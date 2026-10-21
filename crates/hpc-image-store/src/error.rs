use std::{io, path::PathBuf, time::Duration};

pub type Result<T> = std::result::Result<T, Error>;

/// A namespace mutation that may already be visible when durability fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NamespaceChange {
    BlobInserted,
    RefPublished,
    RefRemoved,
}

impl std::fmt::Display for NamespaceChange {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::BlobInserted => "blob insertion",
            Self::RefPublished => "reference publication",
            Self::RefRemoved => "reference removal",
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("no committed ref selects stored root {0}")]
    DigestMissing(crate::ImageSelector),
    #[error(
        "cannot completely resolve stored root {selector}; damaged or unreadable namespace entries remain"
    )]
    ResolutionIncomplete {
        selector: crate::ImageSelector,
        diagnostics: Vec<crate::ListDiagnostic>,
    },
    #[error("invalid OCI transfer JSON at {path}: {source}")]
    ContentJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid transfer argument: {0}")]
    InvalidTransfer(String),
    #[error("invalid image selector {value:?}: {reason}")]
    InvalidSelector { value: String, reason: &'static str },
    #[error(
        "import destination {destination} requires a tag because source {source_selector} has no explicit digest; the destination digest is ignored and cannot supply a generated tag"
    )]
    InvalidImportDestination {
        source_selector: String,
        destination: String,
    },
    #[error(
        "pull destination {destination} requires a tag because source {source_selector} has no explicit digest; the destination digest is ignored and cannot supply a generated tag"
    )]
    InvalidPullDestination {
        source_selector: String,
        destination: String,
    },
    #[error("cannot resolve registry source {reference}: {reason}")]
    RegistryResolution { reference: String, reason: String },
    #[error("{executable} lacks required capability {capability}")]
    ToolCapability {
        executable: PathBuf,
        capability: &'static str,
    },
    #[error("failed to {operation} using {executable}: {source}")]
    ToolIo {
        operation: &'static str,
        executable: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{operation} using {executable} failed ({status}): {stderr}")]
    ToolFailed {
        operation: &'static str,
        executable: PathBuf,
        status: std::process::ExitStatus,
        stderr: String,
    },
    #[error("invalid output from {executable} during {operation}: {reason}")]
    ToolOutput {
        operation: &'static str,
        executable: PathBuf,
        reason: String,
    },
    #[error("reference already exists: {0}")]
    RefExists(crate::LocalRef),
    #[error("reference does not exist: {0}")]
    RefMissing(crate::LocalRef),
    #[error("invalid object {path}: {reason}")]
    InvalidObject { path: PathBuf, reason: String },
    #[error("invalid committed pointer {path}: {reason}")]
    InvalidPointer { path: PathBuf, reason: &'static str },
    #[error("invalid lock use: {0}")]
    Coordination(&'static str),
    #[error("{change} at {path} may already be visible; failed to sync {sync_path}: {source}")]
    Durability {
        change: NamespaceChange,
        path: PathBuf,
        sync_path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid local reference {value:?}: {reason}")]
    InvalidRef { value: String, reason: &'static str },
    #[error("invalid SHA-256 digest {0:?}")]
    InvalidDigest(String),
    #[error("invalid encoded reference: {0}")]
    InvalidEncoding(&'static str),
    #[error("store does not exist or is not initialized: {0}")]
    MissingStore(PathBuf),
    #[error("timed out acquiring lock {path} after {timeout:?}")]
    LockTimeout { path: PathBuf, timeout: Duration },
    #[error("invalid store entry {path}: {reason}")]
    InvalidStore { path: PathBuf, reason: &'static str },
    #[error("unsupported {kind} metadata version {version}")]
    UnsupportedVersion { kind: &'static str, version: u64 },
    #[error("invalid {kind} metadata: {source}")]
    Metadata {
        kind: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to {operation} {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("store marker may already be published; failed to sync {path}: {source}")]
    InitializationDurability {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub(crate) fn io_error(
    operation: &'static str,
    path: &std::path::Path,
    source: io::Error,
) -> Error {
    Error::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}
