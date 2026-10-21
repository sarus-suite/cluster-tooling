//! Shared selected-ref inspection. Findings retain causes until each caller
//! decides whether to collect a diagnostic or propagate an operational error.
use crate::{
    DiagnosticCategory, Error, ListDiagnostic, Sha256Digest,
    error::io_error,
    graph::{GraphReader, GraphReport},
    locks::RefGuard,
    refs,
};
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FailureKind {
    Damage,
    Unsupported,
    Operational,
}

#[derive(Clone, Debug)]
pub(crate) struct Finding {
    pub kind: FailureKind,
    pub category: DiagnosticCategory,
    pub path: PathBuf,
    pub operation: Option<&'static str>,
    pub cause: Arc<Error>,
    // Keep the checker's established presentation independently of the cause.
    pub message: String,
}

impl Finding {
    pub fn wrapper(path: &Path, error: Error) -> Self {
        let diagnostic = ListDiagnostic::from_error(path, None, &error);
        let kind = match &error {
            Error::UnsupportedVersion { .. } => FailureKind::Unsupported,
            Error::Io { path, source, .. } => {
                // Missing required image files are damage. O_NOFOLLOW can
                // report ELOOP for a symlink rather than opening it; confirm
                // the entry type before treating that error as damage.
                if matches!(
                    source.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) || (matches!(source.raw_os_error(), Some(libc::ELOOP | libc::ENXIO))
                    && std::fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.is_file()))
                {
                    FailureKind::Damage
                } else {
                    FailureKind::Operational
                }
            }
            Error::InvalidPointer { .. }
            | Error::InvalidStore { .. }
            | Error::InvalidObject { .. }
            | Error::Metadata { .. }
            | Error::ContentJson { .. }
            | Error::InvalidTransfer(_)
            | Error::RefMissing(_) => FailureKind::Damage,
            _ => FailureKind::Operational,
        };
        let operation = match &error {
            Error::Io { operation, .. } => Some(*operation),
            _ => None,
        };
        Self {
            kind,
            category: diagnostic.category,
            path: diagnostic.path,
            operation,
            message: diagnostic.message,
            cause: Arc::new(error),
        }
    }

    pub fn unsupported_wrapper(path: &Path, error: Error) -> Self {
        let mut finding = Self::wrapper(path, error);
        finding.kind = FailureKind::Unsupported;
        finding
    }

    pub fn graph(path: &Path, kind: FailureKind, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            kind,
            category: DiagnosticCategory::Graph,
            path: path.to_owned(),
            operation: Some("validate OCI object"),
            cause: Arc::new(Error::InvalidObject {
                path: path.to_owned(),
                reason: message.clone(),
            }),
            message,
        }
    }

    pub fn graph_error(path: &Path, error: Error) -> Self {
        let mut finding = Self::wrapper(path, error);
        finding.category = DiagnosticCategory::Graph;
        finding
    }

    pub fn graph_io(path: &Path, operation: &'static str, source: io::Error) -> Self {
        let message = source.to_string();
        let mut finding = Self::graph_error(path, io_error(operation, path, source));
        finding.message = message;
        finding
    }

    pub fn graph_json(path: &Path, source: serde_json::Error, prefix: &str) -> Self {
        let message = format!("{prefix}{source}");
        let mut finding = Self::graph_error(
            path,
            Error::ContentJson {
                path: path.to_owned(),
                source,
            },
        );
        finding.operation = Some("parse OCI object");
        finding.message = message;
        finding
    }

    pub fn into_error(self) -> Error {
        match Arc::try_unwrap(self.cause) {
            Ok(error) => error,
            Err(shared) => {
                // Cached graph failures can be attributed to several refs.
                // Keep the original typed cause in the I/O source chain.
                let Error::Io {
                    operation,
                    path,
                    source,
                } = shared.as_ref()
                else {
                    unreachable!("only operational I/O errors need shared propagation");
                };
                io_error(
                    operation,
                    path,
                    io::Error::new(source.kind(), shared.clone()),
                )
            }
        }
    }
}

pub(crate) struct ImageInspection {
    pub missing: bool,
    pub stored_root: Option<Sha256Digest>,
    pub wrapper_findings: Vec<Finding>,
    pub graph: Option<GraphReport>,
}

pub(crate) fn inspect(
    guard: &RefGuard<'_>,
    reader: &mut GraphReader,
    after_wrappers: impl FnOnce() -> crate::Result<()>,
) -> crate::Result<ImageInspection> {
    let inspected = match refs::inspect_small(guard) {
        Ok(inspected) => inspected,
        Err(error) => {
            return Ok(ImageInspection {
                missing: matches!(error, Error::RefMissing(_)),
                stored_root: None,
                wrapper_findings: vec![Finding::wrapper(
                    &guard.layout().ref_entry(guard.reference()),
                    error,
                )],
                graph: None,
            });
        }
    };
    after_wrappers()?;
    Ok(ImageInspection {
        missing: false,
        stored_root: inspected.root.as_ref().map(|root| root.digest.clone()),
        wrapper_findings: inspected.errors,
        graph: inspected.root.as_ref().map(|root| reader.scan(root)),
    })
}
