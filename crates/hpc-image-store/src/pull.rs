//! Registry resolution selects one immutable manifest before any layer transfer.
use crate::{
    ContentMetadata, Error, ImageSelector, LocalRef, Platform, RefMetadata, Result, Sha256Digest,
    Store, graph,
    locks::Operation,
    oci::Descriptor,
    refs,
    staging::{self, AcquisitionStage, CleanupWarning},
    subprocess,
    transfer::{acquired_content, acquisition_destination, oci_argument, require_capability},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

const DOCKER_LIST: &str = "application/vnd.docker.distribution.manifest.list.v2+json";

/// Registry settings are independent of containers-storage. Unset settings use
/// Skopeo's normal configuration and inherited environment. Credentials are
/// read by Skopeo, never copied into ref metadata. Pull never invokes Podman.
#[derive(Clone, Debug)]
pub struct PullContext {
    pub skopeo: PathBuf,
    pub platform: Option<Platform>,
    pub auth_file: Option<PathBuf>,
    pub cert_dir: Option<PathBuf>,
    pub policy: Option<PathBuf>,
    /// None inherits registry configuration; Some overrides it explicitly.
    pub tls_verify: Option<bool>,
}
impl Default for PullContext {
    fn default() -> Self {
        Self {
            skopeo: "skopeo".into(),
            platform: None,
            auth_file: None,
            cert_dir: None,
            policy: None,
            tls_verify: None,
        }
    }
}
impl PullContext {
    fn validate(&self) -> Result<()> {
        if std::iter::once(&self.skopeo)
            .chain(self.auth_file.iter())
            .chain(self.cert_dir.iter())
            .chain(self.policy.iter())
            .any(|path| path.as_os_str().is_empty())
        {
            return Err(Error::InvalidTransfer(
                "pull configuration contains an empty path".into(),
            ));
        }
        if let Some(platform) = &self.platform {
            platform.validate()?;
        }
        Ok(())
    }

    fn command(&self, action: &str) -> Command {
        let mut command = Command::new(&self.skopeo);
        if let Some(policy) = &self.policy {
            command.arg("--policy").arg(policy);
        }
        command.arg(action);
        if let Some(auth_file) = &self.auth_file {
            command.arg("--authfile").arg(auth_file);
        }
        let copying = action == "copy";
        if let Some(cert_dir) = &self.cert_dir {
            command
                .arg(if copying {
                    "--src-cert-dir"
                } else {
                    "--cert-dir"
                })
                .arg(cert_dir);
        }
        if let Some(tls_verify) = self.tls_verify {
            command.arg(format!(
                "{}={tls_verify}",
                if copying {
                    "--src-tls-verify"
                } else {
                    "--tls-verify"
                }
            ));
        }
        // A failure is returned to the caller; neither resolution nor transfer
        // retries a mutable tag or generates extra registry traffic.
        command.args(["--retry-times", "0"]);
        command
    }

    fn check_capabilities(&self) -> Result<()> {
        for (action, required) in [
            ("inspect", &["--raw", "--retry-times"][..]),
            (
                "copy",
                &[
                    "--preserve-digests",
                    "--dest-shared-blob-dir",
                    "--retry-times",
                ][..],
            ),
        ] {
            let output = subprocess::run(
                Command::new(&self.skopeo).args([action, "--help"]),
                "check registry capabilities",
                &self.skopeo,
                None,
            )?;
            let help = String::from_utf8_lossy(&output.stdout);
            for capability in required
                .iter()
                .copied()
                .chain(self.auth_file.as_ref().map(|_| "--authfile"))
                .chain(self.cert_dir.as_ref().map(|_| {
                    if action == "copy" {
                        "--src-cert-dir"
                    } else {
                        "--cert-dir"
                    }
                }))
                .chain(self.tls_verify.map(|_| {
                    if action == "copy" {
                        "--src-tls-verify"
                    } else {
                        "--tls-verify"
                    }
                }))
            {
                require_capability(&help, &self.skopeo, capability)?;
            }
        }
        if self.policy.is_some() {
            let output = subprocess::run(
                Command::new(&self.skopeo).arg("--help"),
                "check registry policy capability",
                &self.skopeo,
                None,
            )?;
            require_capability(
                &String::from_utf8_lossy(&output.stdout),
                &self.skopeo,
                "--policy",
            )?;
        }
        Ok(())
    }

    fn inspect(&self, reference: &str) -> Result<Vec<u8>> {
        let output = subprocess::run(
            self.command("inspect")
                .arg("--raw")
                .arg(format!("docker://{reference}")),
            "inspect raw registry object",
            &self.skopeo,
            None,
        )?;
        Ok(output.stdout)
    }

    fn resolve(&self, source: &ImageSelector) -> Result<Sha256Digest> {
        let selected = match source.digest() {
            Some(digest) => format!("{}@{digest}", source.name()),
            None => source.to_string(),
        };
        let raw = self.inspect(&selected)?;
        resolve_raw(self, source, &raw).map_err(|reason| Error::RegistryResolution {
            reference: source.to_string(),
            reason,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct PullOptions {
    pub replace: bool,
    pub notes: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PullReport {
    pub local_ref: LocalRef,
    pub requested_reference: String,
    pub resolved_digest: Sha256Digest,
    pub stored_root: Sha256Digest,
    pub content: ContentMetadata,
    pub cleanup_warnings: Vec<CleanupWarning>,
    pub ignored_destination_digest: Option<Sha256Digest>,
}

impl Store {
    /// Pull one OCI image manifest from a registry, preserving its digest.
    /// Tag sources may select one manifest through an index. Digest sources
    /// must name the manifest directly; indexes are never stored by pull.
    pub fn pull(
        &self,
        source: &ImageSelector,
        destination: Option<&ImageSelector>,
        context: &PullContext,
        options: &PullOptions,
    ) -> Result<PullReport> {
        let reference = acquisition_destination(source, destination, Operation::Pull)?;
        context.validate()?;
        oci_argument(self.config().root())?;
        let store_guard = self.prepare(Operation::Pull)?;
        let ref_guard = store_guard.lock_ref(&reference)?;
        refs::check_destination(&ref_guard, options.replace)?;
        context.check_capabilities()?;
        let resolved_digest = context.resolve(source)?;
        let stage = AcquisitionStage::new(&ref_guard)?;
        let mut command = context.command("copy");
        command
            .arg("--preserve-digests")
            .arg("--dest-shared-blob-dir")
            .arg(stage.shared_blob_path())
            .arg(format!("docker://{}@{resolved_digest}", source.name()))
            .arg(oci_argument(&stage.layout_path())?);
        subprocess::run(
            &mut command,
            "pull image",
            &context.skopeo,
            Some(ref_guard.transfer_fds()),
        )?;

        // The writer has exited. Only the small root/config objects and the
        // descriptors needed for metadata are read; Skopeo validates transfer.
        let (root, content, objects) = acquired_content(&stage)?;
        if root.digest != resolved_digest {
            return Err(Error::InvalidObject {
                path: stage.layout_path(),
                reason: format!(
                    "completed root {} differs from resolved registry manifest {resolved_digest}",
                    root.digest
                ),
            });
        }
        if let Some(expected) = &context.platform
            && !platform_matches(&content.platform, expected)
        {
            return Err(Error::InvalidObject {
                path: stage.layout_path(),
                reason: format!(
                    "completed configuration platform {} does not match requested {}",
                    platform_name(&content.platform),
                    platform_name(expected)
                ),
            });
        }
        let acquired_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::InvalidTransfer("system clock is before the Unix epoch".into()))?
            .as_secs();
        let metadata = RefMetadata::new_pull(
            reference.clone(),
            root.digest.clone(),
            content.clone(),
            acquired_at,
            source.to_string(),
            options.notes.clone(),
        );
        let completed = stage.complete(&root.digest, metadata, objects)?;
        let directory = completed.directory().to_owned();
        refs::publish(completed, options.replace)?;
        let cleanup_warnings = staging::cleanup_published(&ref_guard, &directory)
            .into_iter()
            .collect();
        Ok(PullReport {
            local_ref: reference,
            requested_reference: source.to_string(),
            resolved_digest,
            stored_root: root.digest,
            content,
            cleanup_warnings,
            ignored_destination_digest: destination.and_then(ImageSelector::digest).cloned(),
        })
    }
}

fn raw_digest(bytes: &[u8]) -> Sha256Digest {
    Sha256Digest::new(format!("sha256:{:x}", Sha256::digest(bytes)))
        .expect("SHA-256 produces a canonical digest")
}

enum RawObject {
    Manifest,
    Index(Vec<Descriptor>),
}

// Interpret only registry JSON and descriptors. This is not a graph traversal.
fn parse_raw(bytes: &[u8]) -> std::result::Result<RawObject, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("malformed registry JSON: {error}"))?;
    let object = value.as_object().ok_or("registry JSON must be an object")?;
    if object
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        != Some(2)
    {
        return Err("unsupported or missing schemaVersion; expected 2".into());
    }
    if object.contains_key("artifactType") || object.contains_key("subject") {
        return Err("artifact and subject-bearing objects are unsupported".into());
    }
    let media_type = object
        .get("mediaType")
        .and_then(serde_json::Value::as_str)
        .ok_or("registry object must specify mediaType")?;
    match media_type {
        graph::MANIFEST => {
            #[derive(Deserialize)]
            struct Manifest {
                config: Descriptor,
                layers: Vec<Descriptor>,
            }
            let manifest: Manifest = serde_json::from_value(value)
                .map_err(|error| format!("invalid image manifest: {error}"))?;
            let mut declarations = BTreeMap::new();
            for (descriptor, supported) in std::iter::once((
                &manifest.config,
                manifest.config.media_type == graph::CONFIG,
            ))
            .chain(
                manifest
                    .layers
                    .iter()
                    .map(|layer| (layer, graph::LAYERS.contains(&layer.media_type.as_str()))),
            ) {
                graph::validate_descriptor(descriptor)?;
                if !supported {
                    return Err(format!(
                        "unsupported image content media type {}",
                        descriptor.media_type
                    ));
                }
                let declaration = (&descriptor.media_type, descriptor.size);
                if declarations
                    .insert(&descriptor.digest, declaration)
                    .is_some_and(|previous| previous != declaration)
                {
                    return Err(format!("conflicting descriptors for {}", descriptor.digest));
                }
            }
            Ok(RawObject::Manifest)
        }
        graph::INDEX | DOCKER_LIST => {
            #[derive(Deserialize)]
            struct Index {
                manifests: Vec<Descriptor>,
            }
            let index: Index = serde_json::from_value(value)
                .map_err(|error| format!("invalid registry index: {error}"))?;
            for descriptor in &index.manifests {
                graph::validate_descriptor(descriptor)?;
            }
            Ok(RawObject::Index(index.manifests))
        }
        other => Err(format!(
            "unsupported registry media type {other}; pull requires an OCI image manifest"
        )),
    }
}

fn resolve_raw(
    context: &PullContext,
    source: &ImageSelector,
    raw: &[u8],
) -> std::result::Result<Sha256Digest, String> {
    let digest = raw_digest(raw);
    if source.digest().is_some_and(|expected| expected != &digest) {
        return Err(format!(
            "raw response digest {digest} does not match requested {}",
            source.digest().unwrap()
        ));
    }
    match parse_raw(raw)? {
        RawObject::Manifest => Ok(digest),
        RawObject::Index(descriptors) => {
            if source.digest().is_some() {
                return Err("digest sources must identify an OCI image manifest directly; index and manifest-list digests are unsupported".into());
            }
            let platform = match &context.platform {
                Some(platform) => platform.clone(),
                None => default_index_platform(std::env::consts::ARCH)?,
            };
            let selected = select_manifest(&descriptors, &platform)?;
            let child_reference = format!("{}@{}", source.name(), selected.digest);
            let child = context
                .inspect(&child_reference)
                .map_err(|error| error.to_string())?;
            if raw_digest(&child) != selected.digest {
                return Err(format!(
                    "selected child response does not match digest {}",
                    selected.digest
                ));
            }
            if child.len() as u64 != selected.size {
                return Err(format!(
                    "selected child {} has size {}, expected {}",
                    selected.digest,
                    child.len(),
                    selected.size
                ));
            }
            if !matches!(parse_raw(&child)?, RawObject::Manifest) {
                return Err(
                    "selected child is an index; nested-index resolution is unsupported".into(),
                );
            }
            Ok(selected.digest.clone())
        }
    }
}

fn default_index_platform(architecture: &str) -> std::result::Result<Platform, String> {
    let architecture = match architecture {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => {
            return Err(format!(
                "index selection on {other} requires an explicit --platform OS/ARCH[/VARIANT]"
            ));
        }
    };
    Ok(Platform {
        os: "linux".into(),
        architecture: architecture.into(),
        variant: None,
    })
}

fn platform_matches(actual: &Platform, expected: &Platform) -> bool {
    actual.os == expected.os
        && actual.architecture == expected.architecture
        && expected
            .variant
            .as_ref()
            .is_none_or(|variant| actual.variant.as_ref() == Some(variant))
}
fn platform_name(platform: &Platform) -> String {
    let mut name = format!("{}/{}", platform.os, platform.architecture);
    if let Some(variant) = &platform.variant {
        name.push('/');
        name.push_str(variant);
    }
    name
}

fn descriptor_platform(descriptor: &Descriptor) -> std::result::Result<Option<Platform>, String> {
    descriptor
        .extra
        .get("platform")
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|error| format!("invalid descriptor platform: {error}"))
        })
        .transpose()
}

fn select_manifest<'a>(
    descriptors: &'a [Descriptor],
    platform: &Platform,
) -> std::result::Result<&'a Descriptor, String> {
    let mut declarations = BTreeMap::new();
    let mut candidates = BTreeMap::new();
    for descriptor in descriptors {
        let actual = descriptor_platform(descriptor)?;
        let declaration = (&descriptor.media_type, descriptor.size, actual.clone());
        if declarations
            .insert(&descriptor.digest, declaration.clone())
            .is_some_and(|previous| previous != declaration)
        {
            return Err(format!(
                "conflicting duplicate index descriptors for {}",
                descriptor.digest
            ));
        }
        if actual
            .as_ref()
            .is_some_and(|actual| platform_matches(actual, platform))
        {
            candidates.insert(&descriptor.digest, descriptor);
        }
    }
    if candidates.len() != 1 {
        return Err(format!(
            "index has {} distinct matching manifest digests for {}; specify --platform OS/ARCH[/VARIANT] selecting exactly one image",
            candidates.len(),
            platform_name(platform)
        ));
    }
    let selected = *candidates.values().next().unwrap();
    if matches!(selected.media_type.as_str(), graph::INDEX | DOCKER_LIST) {
        return Err(
            "selected descriptor requires nested-index resolution, which is unsupported".into(),
        );
    }
    if selected.media_type != graph::MANIFEST {
        return Err(format!(
            "selected descriptor has unsupported image representation {}",
            selected.media_type
        ));
    }
    Ok(selected)
}

#[cfg(test)]
mod tests;
