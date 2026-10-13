use std::{io, path::PathBuf, time::Duration};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
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
