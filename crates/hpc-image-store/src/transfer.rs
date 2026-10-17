use crate::{
    Acquisition, ContentMetadata, Error, ImageId, LocalRef, Platform, RefMetadata, Result,
    Sha256Digest, Store, fsutil,
    initialization::require_directory,
    locks::Operation,
    oci::{self, Descriptor},
    refs,
    staging::{self, AcquisitionStage, BlobSpec, CleanupWarning},
    subprocess,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const OCI_CONFIG: &str = "application/vnd.oci.image.config.v1+json";

/// Both tools inherit the caller's environment, storage configuration and
/// namespace. No storage settings are reconstructed or overridden.
#[derive(Clone, Debug)]
pub struct TransferContext {
    pub podman: PathBuf,
    pub skopeo: PathBuf,
    pub platform: Option<Platform>,
}
impl Default for TransferContext {
    fn default() -> Self {
        Self {
            podman: "podman".into(),
            skopeo: "skopeo".into(),
            platform: None,
        }
    }
}
impl TransferContext {
    fn validate(&self) -> Result<()> {
        if self.skopeo.as_os_str().is_empty() {
            return Err(Error::InvalidTransfer(
                "tool executable path is empty".into(),
            ));
        }
        if let Some(platform) = &self.platform {
            for value in [&platform.os, &platform.architecture]
                .into_iter()
                .chain(platform.variant.iter())
            {
                if value.is_empty()
                    || !value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
                {
                    return Err(Error::InvalidTransfer(
                        "platform fields must be nonempty identifiers".into(),
                    ));
                }
            }
        }
        Ok(())
    }
    fn copy_command(&self) -> Command {
        let mut command = Command::new(&self.skopeo);
        if let Some(platform) = &self.platform {
            command.args([
                "--override-os",
                &platform.os,
                "--override-arch",
                &platform.architecture,
            ]);
            if let Some(variant) = &platform.variant {
                command.args(["--override-variant", variant]);
            }
        }
        command.arg("copy");
        command
    }
    fn check_capabilities(&self, shared_option: &'static str, preserve: bool) -> Result<()> {
        let help = subprocess::run(
            Command::new(&self.skopeo).args(["copy", "--help"]),
            "check copy capabilities",
            &self.skopeo,
            None,
        )?;
        let text = String::from_utf8_lossy(&help.stdout);
        for capability in [
            Some(shared_option),
            preserve.then_some("--preserve-digests"),
        ]
        .into_iter()
        .flatten()
        {
            require_capability(&text, &self.skopeo, capability)?;
        }
        if let Some(platform) = &self.platform {
            let help = subprocess::run(
                Command::new(&self.skopeo).arg("--help"),
                "check platform capabilities",
                &self.skopeo,
                None,
            )?;
            let text = String::from_utf8_lossy(&help.stdout);
            for capability in ["--override-os", "--override-arch"]
                .into_iter()
                .chain(platform.variant.as_ref().map(|_| "--override-variant"))
            {
                require_capability(&text, &self.skopeo, capability)?;
            }
        }
        Ok(())
    }
}
fn require_capability(help: &str, executable: &Path, capability: &'static str) -> Result<()> {
    if !help.split_whitespace().any(|word| word == capability) {
        return Err(Error::ToolCapability {
            executable: executable.to_owned(),
            capability,
        });
    }
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub struct ImportOptions {
    pub replace: bool,
    pub notes: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct ImportReport {
    pub local_ref: LocalRef,
    pub stored_root: Sha256Digest,
    pub image_id: ImageId,
    pub content: ContentMetadata,
    pub cleanup_warnings: Vec<CleanupWarning>,
}
#[derive(Clone, Debug, Serialize)]
pub struct ExportReport {
    pub local_ref: LocalRef,
    pub stored_manifest: Sha256Digest,
    pub destination: String,
}

/// Arguments are image names or IDs in containers-storage. A name such as
/// `oci:latest` is a repository and tag, not a transport selection. Explicit
/// URLs and embedded storage settings are rejected; tools parse the full name.
pub(crate) fn validate_image_argument(value: &str) -> Result<()> {
    if value.is_empty()
        || value.starts_with('-')
        || value.contains(['[', ']'])
        || value.contains("://")
        || value
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return Err(Error::InvalidTransfer("expected a plain containers-storage image name or ID, without a transport or storage specifier".into()));
    }
    Ok(())
}
fn oci_argument(path: &Path) -> Result<OsString> {
    if path.as_os_str().as_bytes().contains(&b':') {
        return Err(Error::InvalidTransfer(
            "Skopeo's OCI transport cannot address a layout path containing ':'".into(),
        ));
    }
    let mut argument = OsString::from("oci:");
    argument.push(path);
    Ok(argument)
}
fn storage_argument(image: &str) -> String {
    format!("containers-storage:{image}")
}

impl Store {
    /// Import a containers-storage image. An omitted ref uses the source verbatim,
    /// which must be a valid explicitly tagged local ref.
    pub fn import(
        &self,
        source: &str,
        reference: Option<&LocalRef>,
        context: &TransferContext,
        options: &ImportOptions,
    ) -> Result<ImportReport> {
        validate_image_argument(source)?;
        let default_reference;
        let reference = match reference {
            Some(reference) => reference,
            None => {
                default_reference = LocalRef::new(source)?;
                &default_reference
            }
        };
        context.validate()?;
        if context.podman.as_os_str().is_empty() {
            return Err(Error::InvalidTransfer(
                "Podman executable path is empty".into(),
            ));
        }
        // Fail on an unrepresentable OCI path before initializing the store.
        oci_argument(self.config().root())?;
        let store_guard = self.prepare(Operation::Import)?;
        let ref_guard = store_guard.lock_ref(reference)?;
        refs::check_destination(&ref_guard, options.replace)?;
        context.check_capabilities("--dest-shared-blob-dir", false)?;

        let output = subprocess::run(
            Command::new(&context.podman)
                .args(["image", "inspect", "--format", "{{.Id}}", "--", source]),
            "inspect source image ID",
            &context.podman,
            None,
        )?;
        let image_id = std::str::from_utf8(&output.stdout)
            .ok()
            .and_then(|text| ImageId::new(text.trim()).ok())
            .ok_or_else(|| Error::ToolOutput {
                operation: "inspect source image ID",
                executable: context.podman.clone(),
                reason: "expected one complete containers-storage image ID".into(),
            })?;
        let stage = AcquisitionStage::new(&ref_guard)?;
        let mut command = context.copy_command();
        command
            .arg("--dest-shared-blob-dir")
            .arg(stage.shared_blob_path())
            .arg(storage_argument(image_id.as_str()))
            .arg(oci_argument(&stage.layout_path())?);
        subprocess::run(
            &mut command,
            "import image",
            &context.skopeo,
            Some(ref_guard.transfer_fds()),
        )?;

        // Skopeo has exited: no writable producer may remain when staged
        // objects become aliases of the immutable CAS.
        let (root, content, objects) = import_content(&stage)?;
        let acquired_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::InvalidTransfer("system clock is before the Unix epoch".into()))?
            .as_secs();
        let metadata = RefMetadata::new_import(
            reference.clone(),
            root.digest.clone(),
            content.clone(),
            acquired_at,
            source.into(),
            image_id.clone(),
            options.notes.clone(),
        );
        let completed = stage.complete(&root.digest, metadata, objects)?;
        let staging_directory = completed.directory().to_owned();
        refs::publish(completed, options.replace)?;
        // Publication (including both rename-parent syncs) has succeeded. Keep
        // the same locks while removing only this operation's private aliases.
        let cleanup_warnings = staging::cleanup_published(&ref_guard, &staging_directory)
            .into_iter()
            .collect();
        Ok(ImportReport {
            local_ref: reference.clone(),
            stored_root: root.digest,
            image_id,
            content,
            cleanup_warnings,
        })
    }

    /// Export a stored image, using the local ref verbatim when no destination
    /// is supplied.
    pub fn export(
        &self,
        reference: &LocalRef,
        destination: Option<&str>,
        context: &TransferContext,
    ) -> Result<ExportReport> {
        let destination = destination.unwrap_or(reference.as_str());
        validate_image_argument(destination)?;
        // Podman is never used by export, even to probe its availability.
        context.validate()?;
        let store_guard = self.prepare(Operation::Export)?;
        let ref_guard = store_guard.lock_ref(reference)?;
        let resolved = refs::read(&ref_guard)?;
        require_manifest(&resolved.root, &resolved.raw_image)?;
        let preserve = matches!(resolved.metadata.acquisition, Acquisition::Pull { .. });
        context.check_capabilities("--src-shared-blob-dir", preserve)?;
        let mut command = context.copy_command();
        if preserve {
            command.arg("--preserve-digests");
        }
        command
            .arg("--src-shared-blob-dir")
            .arg(self.layout.blobs().parent().unwrap())
            .arg(oci_argument(&resolved.raw_image)?)
            .arg(storage_argument(destination));
        subprocess::run(
            &mut command,
            "export image",
            &context.skopeo,
            Some(ref_guard.transfer_fds()),
        )?;
        Ok(ExportReport {
            local_ref: reference.clone(),
            stored_manifest: resolved.root.digest.clone(),
            destination: destination.into(),
        })
    }
}
fn require_manifest(root: &Descriptor, path: &Path) -> Result<()> {
    if root.media_type != OCI_MANIFEST {
        return Err(Error::InvalidObject { path: path.to_owned(), reason: "stage 3 transfers require a single OCI manifest; index transfers are not implemented".into() });
    }
    Ok(())
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(rename = "schemaVersion")]
    schema_version: u64,
    config: Descriptor,
    layers: Vec<Descriptor>,
}
#[derive(Deserialize)]
struct Config {
    os: String,
    architecture: String,
    variant: Option<String>,
    created: Option<String>,
}
fn parse_object<T: serde::de::DeserializeOwned>(path: &Path, size: u64) -> Result<T> {
    fsutil::sized_object(path, size)?;
    serde_json::from_slice(&fsutil::read_regular(path)?).map_err(|source| Error::ContentJson {
        path: path.to_owned(),
        source,
    })
}
fn import_content(
    stage: &AcquisitionStage<'_>,
) -> Result<(Descriptor, ContentMetadata, Vec<BlobSpec>)> {
    require_directory(&stage.layout_path())?;
    require_directory(&stage.shared_blob_path())?;
    let blobs = stage.shared_blob_path().join("sha256");
    require_directory(&blobs)?;
    let root = oci::single_root(&stage.layout_path())?;
    require_manifest(&root, &stage.layout_path())?;
    let manifest: Manifest = parse_object(&blobs.join(root.digest.hex()), root.size)?;
    if manifest.schema_version != 2 || manifest.config.media_type != OCI_CONFIG {
        return Err(Error::InvalidObject {
            path: stage.layout_path(),
            reason: "unsupported manifest schema or config media type".into(),
        });
    }
    let config: Config = parse_object(
        &blobs.join(manifest.config.digest.hex()),
        manifest.config.size,
    )?;
    if config.os.is_empty() || config.architecture.is_empty() {
        return Err(Error::InvalidObject {
            path: blobs.join(manifest.config.digest.hex()),
            reason: "image config must specify OS and architecture".into(),
        });
    }
    let mut sizes = BTreeMap::new();
    for descriptor in std::iter::once(&root)
        .chain(std::iter::once(&manifest.config))
        .chain(manifest.layers.iter())
    {
        if let Some(previous) = sizes.insert(descriptor.digest.clone(), descriptor.size)
            && previous != descriptor.size
        {
            return Err(Error::InvalidObject {
                path: stage.layout_path(),
                reason: "conflicting descriptor sizes for one digest".into(),
            });
        }
    }
    let size_bytes = sizes
        .values()
        .try_fold(0_u64, |total, size| total.checked_add(*size))
        .ok_or_else(|| Error::InvalidObject {
            path: stage.layout_path(),
            reason: "stored-content size overflows u64".into(),
        })?;
    let objects = sizes
        .into_iter()
        .map(|(digest, size)| BlobSpec { digest, size })
        .collect();
    let content = ContentMetadata {
        platform: Platform {
            os: config.os,
            architecture: config.architecture,
            variant: config.variant,
        },
        created: config.created,
        size_bytes,
    };
    Ok((root, content, objects))
}

#[cfg(test)]
mod tests;
