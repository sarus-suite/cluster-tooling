//! Read-only OCI dependency traversal. Callers retain the store lock; this
//! module does not acquire ref locks and can also serve an exclusive prune scan.
use crate::{Sha256Digest, fsutil, oci::Descriptor};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
};

pub(crate) const MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
pub(crate) const INDEX: &str = "application/vnd.oci.image.index.v1+json";
const CONFIG: &str = "application/vnd.oci.image.config.v1+json";
const LAYERS: [&str; 6] = [
    "application/vnd.oci.image.layer.v1.tar",
    "application/vnd.oci.image.layer.v1.tar+gzip",
    "application/vnd.oci.image.layer.v1.tar+zstd",
    "application/vnd.oci.image.layer.nondistributable.v1.tar",
    "application/vnd.oci.image.layer.nondistributable.v1.tar+gzip",
    "application/vnd.oci.image.layer.nondistributable.v1.tar+zstd",
];

#[derive(Clone, Copy)]
enum Role {
    Image,
    Config,
    Layer,
}
impl Role {
    fn accepts(self, media_type: &str) -> bool {
        match self {
            Self::Image => matches!(media_type, MANIFEST | INDEX),
            Self::Config => media_type == CONFIG,
            Self::Layer => LAYERS.contains(&media_type),
        }
    }
}

#[derive(Clone)]
pub(crate) struct GraphDiagnostic {
    pub path: PathBuf,
    pub digest: Sha256Digest,
    pub message: String,
}

pub(crate) struct GraphReport {
    /// A partial set must never authorize deletion when `complete` is false.
    pub reachable: BTreeSet<Sha256Digest>,
    pub diagnostics: Vec<GraphDiagnostic>,
    pub complete: bool,
}
impl GraphReport {
    fn error(&mut self, path: &Path, digest: &Sha256Digest, message: String) {
        self.complete = false;
        self.diagnostics.push(GraphDiagnostic {
            path: path.to_owned(),
            digest: digest.clone(),
            message,
        });
    }
}

struct Observation {
    size: Option<u64>,
    children: Vec<(Descriptor, Role)>,
    errors: Vec<String>,
}

/// Cached file observations and JSON parsing last only for this operation.
pub(crate) struct GraphReader {
    blobs: PathBuf,
    observations: BTreeMap<Sha256Digest, Observation>,
    declarations: BTreeMap<Sha256Digest, (String, u64)>,
}

enum Work {
    Enter(Descriptor, Role),
    Leave(Sha256Digest),
}

impl GraphReader {
    pub fn new(blobs: &Path) -> Self {
        Self {
            blobs: blobs.to_owned(),
            observations: BTreeMap::new(),
            declarations: BTreeMap::new(),
        }
    }

    pub fn scan(&mut self, root: &Descriptor) -> GraphReport {
        let mut report = GraphReport {
            reachable: BTreeSet::new(),
            diagnostics: Vec::new(),
            complete: true,
        };
        // Visitation is per root so cached failures reach every dependent ref.
        let mut active = BTreeSet::new();
        let mut completed = BTreeSet::new();
        let mut work = vec![Work::Enter(root.clone(), Role::Image)];
        while let Some(item) = work.pop() {
            let (descriptor, role) = match item {
                Work::Leave(digest) => {
                    active.remove(&digest);
                    completed.insert(digest);
                    continue;
                }
                Work::Enter(descriptor, role) => (descriptor, role),
            };
            let digest = &descriptor.digest;
            let path = self.blobs.join(digest.hex());
            report.reachable.insert(digest.clone());
            if let Err(reason) = validate_descriptor(&descriptor) {
                report.error(&path, digest, reason);
            }
            let declaration = (descriptor.media_type.clone(), descriptor.size);
            if let Some(previous) = self.declarations.get(digest) {
                if previous != &declaration {
                    report.error(
                        &path,
                        digest,
                        format!(
                            "conflicting descriptors: previously {} with {} bytes, now {} with {} bytes",
                            previous.0, previous.1, declaration.0, declaration.1
                        ),
                    );
                }
            } else {
                self.declarations.insert(digest.clone(), declaration);
            }
            if !role.accepts(&descriptor.media_type) {
                report.error(
                    &path,
                    digest,
                    format!(
                        "unsupported media type {} for this dependency",
                        descriptor.media_type
                    ),
                );
                continue;
            }
            let observation = self
                .observations
                .entry(digest.clone())
                .or_insert_with(|| observe(&path, &descriptor.media_type));
            if let Some(actual) = observation.size
                && actual != descriptor.size
            {
                report.error(
                    &path,
                    digest,
                    format!("expected {} bytes, found {actual}", descriptor.size),
                );
            }
            if active.contains(digest) {
                report.error(&path, digest, "cycle in OCI dependency graph".into());
                continue;
            }
            // Check each incoming descriptor before skipping repeated edges.
            if completed.contains(digest) {
                continue;
            }
            for error in &observation.errors {
                report.error(&path, digest, error.clone());
            }
            active.insert(digest.clone());
            work.push(Work::Leave(digest.clone()));
            for (child, child_role) in observation.children.iter().rev() {
                work.push(Work::Enter(child.clone(), *child_role));
            }
        }
        report
    }
}

fn validate_descriptor(descriptor: &Descriptor) -> Result<(), String> {
    let extra = &descriptor.extra;
    if extra.contains_key("artifactType") || extra.contains_key("subject") {
        return Err("artifact and subject-bearing descriptors are unsupported".into());
    }
    if let Some(value) = extra.get("urls")
        && !value
            .as_array()
            .is_some_and(|values| values.iter().all(Value::is_string))
    {
        return Err("descriptor urls must be an array of strings".into());
    }
    if let Some(value) = extra.get("data")
        && !value.is_string()
    {
        return Err("descriptor data must be a string".into());
    }
    if let Some(value) = extra.get("annotations")
        && !value
            .as_object()
            .is_some_and(|values| values.values().all(Value::is_string))
    {
        return Err("descriptor annotations must map strings to strings".into());
    }
    if let Some(value) = extra.get("platform") {
        let platform: PlatformFields =
            serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
        platform.validate()?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct PlatformFields {
    os: String,
    architecture: String,
    // These fields are deserialized for type validation only, without host
    // selection or comparison with cached metadata.
    #[serde(rename = "variant")]
    _variant: Option<String>,
    #[serde(rename = "os.version")]
    _os_version: Option<String>,
    #[serde(rename = "os.features")]
    _os_features: Option<Vec<String>>,
    #[serde(rename = "created")]
    _created: Option<String>,
}
impl PlatformFields {
    fn validate(&self) -> Result<(), String> {
        if self.os.is_empty() || self.architecture.is_empty() {
            return Err("image platform must specify nonempty OS and architecture".into());
        }
        Ok(())
    }
}

fn observe(path: &Path, media_type: &str) -> Observation {
    let mut observation = Observation {
        size: None,
        children: Vec::new(),
        errors: Vec::new(),
    };
    if let Err(error) = read_object(path, media_type, &mut observation) {
        observation.errors.push(error);
    }
    observation
}

fn read_object(path: &Path, media_type: &str, observation: &mut Observation) -> Result<(), String> {
    fsutil::checkpoint("graph.observe", path).map_err(|error| error.to_string())?;
    let mut file = fsutil::open_regular(path).map_err(|error| error.to_string())?;
    observation.size = Some(file.metadata().map_err(|error| error.to_string())?.len());
    if LAYERS.contains(&media_type) {
        return Ok(());
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let object = value.as_object().ok_or("OCI JSON must be an object")?;
    if media_type == CONFIG {
        let fields: PlatformFields =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        return fields.validate();
    }
    if object.get("schemaVersion").and_then(Value::as_u64) != Some(2) {
        return Err("unsupported or missing OCI schemaVersion (expected 2)".into());
    }
    if object
        .get("mediaType")
        .is_some_and(|value| value.as_str() != Some(media_type))
    {
        observation
            .errors
            .push("object mediaType disagrees with its descriptor".into());
    }
    if object.contains_key("artifactType") || object.contains_key("subject") {
        observation
            .errors
            .push("artifact and subject-bearing objects are unsupported".into());
    }
    if media_type == MANIFEST {
        child(object.get("config"), Role::Config, "config", observation);
    }
    let (key, role) = if media_type == INDEX {
        ("manifests", Role::Image)
    } else {
        ("layers", Role::Layer)
    };
    match object.get(key).and_then(Value::as_array) {
        Some(values) => {
            for (index, value) in values.iter().enumerate() {
                child(Some(value), role, &format!("{key}[{index}]"), observation);
            }
        }
        None => observation.errors.push(format!("{key} must be an array")),
    }
    Ok(())
}

fn child(value: Option<&Value>, role: Role, field: &str, observation: &mut Observation) {
    match value {
        Some(value) => match serde_json::from_value(value.clone()) {
            Ok(descriptor) => observation.children.push((descriptor, role)),
            Err(error) => observation
                .errors
                .push(format!("invalid {field} descriptor: {error}")),
        },
        None => observation
            .errors
            .push(format!("missing {field} descriptor")),
    }
}

#[cfg(test)]
mod tests;
