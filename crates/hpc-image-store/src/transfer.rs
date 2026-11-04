use crate::{
    ContentMetadata, Error, ImageId, ImageSelector, LocalRef, Platform, RefMetadata, Result,
    Sha256Digest, Store, fsutil,
    graph::GraphReader,
    initialization::require_directory,
    locks::Operation,
    mutation::{Kind, Mutation},
    oci::{self, Descriptor},
    refs,
    staging::{AcquisitionStage, BlobSpec},
    subprocess,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    ffi::OsString,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

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
            platform.validate()?;
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
pub(crate) fn require_capability(
    help: &str,
    executable: &Path,
    capability: &'static str,
) -> Result<()> {
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
    /// An explicit destination digest is descriptive input only, never an
    /// assertion about transferred content or a source of a generated tag.
    pub ignored_destination_digest: Option<Sha256Digest>,
}
#[derive(Clone, Debug, Serialize)]
pub struct ExportReport {
    pub local_ref: LocalRef,
    pub stored_manifest: Sha256Digest,
    pub destination: String,
}

/// Naming is resolved before preparing the store or invoking either tool.
pub(crate) fn acquisition_destination(
    source: &ImageSelector,
    destination: Option<&ImageSelector>,
    operation: Operation,
) -> Result<LocalRef> {
    let effective = destination.unwrap_or(source);
    let tag = match effective.tag() {
        Some(tag) => tag,
        None => source
            .digest()
            .map(|digest| &digest.hex()[..12])
            .ok_or_else(|| match operation {
                Operation::Pull => Error::InvalidPullDestination {
                    source_selector: source.to_string(),
                    destination: effective.to_string(),
                },
                _ => Error::InvalidImportDestination {
                    source_selector: source.to_string(),
                    destination: effective.to_string(),
                },
            })?,
    };
    LocalRef::new(format!("{}:{tag}", effective.name()))
}
pub(crate) fn oci_argument(path: &Path) -> Result<OsString> {
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
    /// Import a containers-storage image, retaining an effective destination
    /// tag or deriving one from the explicit source digest's first 12 hex digits.
    pub fn import(
        &self,
        source: &ImageSelector,
        destination: Option<&ImageSelector>,
        context: &TransferContext,
        options: &ImportOptions,
    ) -> Result<ImportReport> {
        let reference = acquisition_destination(source, destination, Operation::Import)?;
        let ignored_destination_digest = destination.and_then(ImageSelector::digest).cloned();
        context.validate()?;
        if context.podman.as_os_str().is_empty() {
            return Err(Error::InvalidTransfer(
                "Podman executable path is empty".into(),
            ));
        }
        // Fail on an unrepresentable OCI path before initializing the store.
        oci_argument(self.config().root())?;
        let store_guard = self.prepare(Operation::Import)?;
        refs::check_destination(&store_guard, &reference, options.replace)?;
        context.check_capabilities("--dest-shared-blob-dir", false)?;

        let selected_source = match source.digest() {
            Some(digest) => format!("{}@{digest}", source.name()),
            None => source.to_string(),
        };
        let output = subprocess::run(
            Command::new(&context.podman).args([
                "image",
                "inspect",
                "--format",
                "{{.Id}}",
                "--",
                &selected_source,
            ]),
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
        let mut mutation = Mutation::begin(&store_guard, &reference, Kind::Import)?;
        let stage = mutation.stage()?;
        let mut command = context.copy_command();
        command
            .arg("--dest-shared-blob-dir")
            .arg(stage.shared_blob_path())
            .arg(storage_argument(if source.digest().is_some() {
                &selected_source
            } else {
                image_id.as_str()
            }))
            .arg(oci_argument(&stage.layout_path()).map_err(|error| mutation.context(error))?);
        subprocess::run(
            &mut command,
            "import image",
            &context.skopeo,
            Some(store_guard.transfer_fd()),
        )
        .map_err(|error| mutation.context(error))?;

        // Skopeo has exited: no writable producer may remain when staged
        // objects become aliases of the immutable CAS.
        let (root, content, objects) =
            acquired_content(&stage).map_err(|error| mutation.context(error))?;
        let acquired_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| {
                mutation.context(Error::InvalidTransfer(
                    "system clock is before the Unix epoch".into(),
                ))
            })?
            .as_secs();
        let metadata = RefMetadata::new_import(
            reference.clone(),
            root.digest.clone(),
            content.clone(),
            acquired_at,
            source.to_string(),
            image_id.clone(),
            options.notes.clone(),
        );
        let completed = stage
            .complete(&root.digest, metadata, objects)
            .map_err(|error| mutation.context(error))?;
        mutation.publish(completed)?;
        // Finish the durable lifecycle under the same exclusive guard. Errors
        // leave recorded work for preparation to recover forward, never rollback.
        mutation.finish()?;
        Ok(ImportReport {
            local_ref: reference.clone(),
            stored_root: root.digest,
            image_id,
            content,
            ignored_destination_digest,
        })
    }

    /// Export a committed name or repository-qualified root. An omitted destination
    /// uses the selector verbatim, including its digest spelling.
    ///
    /// Every export requires Skopeo to preserve the stored manifest digest,
    /// including images whose representation changed during import. Missing
    /// tool support or an inability to preserve that representation is an error;
    /// the operation never retries without preservation.
    pub fn export(
        &self,
        selector: &ImageSelector,
        destination: Option<&ImageSelector>,
        context: &TransferContext,
    ) -> Result<ExportReport> {
        let destination = destination.unwrap_or(selector).as_str();
        // Podman is never used by export, even to probe its availability.
        context.validate()?;
        let store_guard = self.prepare(Operation::Export)?;
        let reference = crate::inventory::select(&store_guard, selector)?;
        let resolved = refs::read(&store_guard, &reference)?;
        require_manifest(&resolved.root, &resolved.raw_image)?;
        context.check_capabilities("--src-shared-blob-dir", true)?;
        let mut command = context.copy_command();
        command
            .arg("--preserve-digests")
            .arg("--src-shared-blob-dir")
            .arg(self.layout.blobs().parent().unwrap())
            .arg(oci_argument(&resolved.raw_image)?)
            .arg(storage_argument(destination));
        subprocess::run(
            &mut command,
            "export image",
            &context.skopeo,
            Some(store_guard.transfer_fd()),
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
        return Err(Error::InvalidObject {
            path: path.to_owned(),
            reason: "transfers require a single OCI manifest; index transfers are not implemented"
                .into(),
        });
    }
    Ok(())
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(rename = "schemaVersion")]
    schema_version: u64,
    config: Descriptor,
    #[serde(default)]
    annotations: Value,
}
#[derive(Deserialize)]
struct Config {
    os: String,
    architecture: String,
    variant: Option<String>,
    created: Option<String>,
    #[serde(default, rename = "config")]
    runtime: Value,
}

fn image_created(config: &Config, manifest: &Manifest) -> Option<String> {
    const CREATED: &str = "org.opencontainers.image.created";
    let annotation = manifest.annotations.get(CREATED).and_then(Value::as_str);
    let label = config
        .runtime
        .get("Labels")
        .and_then(|labels| labels.get(CREATED))
        .and_then(Value::as_str);
    [config.created.as_deref(), annotation, label]
        .into_iter()
        .flatten()
        .find(|value| {
            OffsetDateTime::parse(value, &Rfc3339)
                .is_ok_and(|date| date.unix_timestamp_nanos() != 0)
        })
        .map(str::to_owned)
        .or_else(|| config.created.clone())
}

fn parse_object<T: serde::de::DeserializeOwned>(path: &Path, size: u64) -> Result<T> {
    fsutil::sized_object(path, size)?;
    serde_json::from_slice(&fsutil::read_regular(path)?).map_err(|source| Error::ContentJson {
        path: path.to_owned(),
        source,
    })
}
pub(crate) fn acquired_content(
    stage: &AcquisitionStage<'_>,
) -> Result<(Descriptor, ContentMetadata, Vec<BlobSpec>)> {
    require_directory(&stage.layout_path())?;
    require_directory(&stage.shared_blob_path())?;
    let blobs = stage.shared_blob_path().join("sha256");
    require_directory(&blobs)?;
    let root = oci::single_root(&stage.layout_path())?;
    require_manifest(&root, &stage.layout_path())?;
    // A successful transfer must also be a graph the store can reclaim later.
    // Use recovery's declaration rules while everything is still private; this
    // traversal does not open configuration or layer objects.
    let objects = GraphReader::dependencies(&blobs).dependency_objects(&root)?;
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
    let size_bytes = objects
        .iter()
        .try_fold(0_u64, |total, object| total.checked_add(object.size))
        .ok_or_else(|| Error::InvalidObject {
            path: stage.layout_path(),
            reason: "stored-content size overflows u64".into(),
        })?;
    let created = image_created(&config, &manifest);
    let content = ContentMetadata {
        config_digest: manifest.config.digest,
        platform: Platform {
            os: config.os,
            architecture: config.architecture,
            variant: config.variant,
        },
        created,
        size_bytes,
    };
    Ok((root, content, objects))
}

#[cfg(test)]
mod tests;
