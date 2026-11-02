//! Inventory and digest resolution use only committed namespace and wrapper
//! files. They make no claim about the health of the referenced image graph.
use crate::{
    Error, ImageSelector, LocalRef, RefMetadata, Result, Sha256Digest, Store, discovery, fsutil,
    locks::{Operation, StoreGuard},
    refs,
};
use serde::{Serialize, Serializer};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ListStatus {
    Unverified,
    Damaged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCategory {
    Namespace,
    Pointer,
    Metadata,
    Layout,
    Io,
    Lock,
    Graph,
}

#[derive(Clone, Debug, Serialize)]
pub struct ListDiagnostic {
    pub category: DiagnosticCategory,
    #[serde(serialize_with = "serialize_path")]
    pub path: PathBuf,
    pub local_ref: Option<LocalRef>,
    pub message: String,
}

impl ListDiagnostic {
    pub(crate) fn from_error(path: &Path, reference: Option<&LocalRef>, error: &Error) -> Self {
        let category = match error {
            Error::InvalidPointer { .. } => DiagnosticCategory::Pointer,
            Error::Metadata { kind: "ref", .. }
            | Error::UnsupportedVersion { kind: "ref", .. }
            | Error::InvalidTransfer(_) => DiagnosticCategory::Metadata,
            Error::InvalidObject { path, .. }
                if path.file_name().is_some_and(|name| name == "metadata.json") =>
            {
                DiagnosticCategory::Metadata
            }
            Error::Metadata { .. } | Error::InvalidObject { .. } => DiagnosticCategory::Layout,
            Error::LockTimeout { .. } | Error::Coordination(_) => DiagnosticCategory::Lock,
            Error::Io { .. } => DiagnosticCategory::Io,
            _ => DiagnosticCategory::Namespace,
        };
        let path = match error {
            Error::Io { path, .. }
            | Error::InvalidObject { path, .. }
            | Error::InvalidPointer { path, .. }
            | Error::InvalidStore { path, .. }
            | Error::LockTimeout { path, .. } => path.clone(),
            _ => path.to_owned(),
        };
        Self {
            category,
            path,
            local_ref: reference.cloned(),
            message: error.to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ListEntry {
    #[serde(serialize_with = "serialize_path")]
    pub path: PathBuf,
    pub local_ref: Option<LocalRef>,
    /// Comes only from the committed OCI layout, never from cached metadata.
    pub stored_root: Option<Sha256Digest>,
    pub metadata: Option<RefMetadata>,
    pub status: ListStatus,
}

/// Keep malformed non-UTF-8 namespace names representable in human and JSON
/// reports. Native paths remain available in the Rust report itself.
pub(crate) fn display_path(path: &Path) -> String {
    path.to_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{path:?}"))
}

pub(crate) fn serialize_path<S: Serializer>(
    path: &Path,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.serialize_str(&display_path(path))
}

#[derive(Clone, Debug, Serialize)]
pub struct ListReport {
    pub entries: Vec<ListEntry>,
    pub diagnostics: Vec<ListDiagnostic>,
    /// Namespace enumeration completed. This is independent of ref health.
    pub complete: bool,
}
impl ListReport {
    pub fn has_errors(&self) -> bool {
        !self.complete
            || !self.diagnostics.is_empty()
            || self
                .entries
                .iter()
                .any(|entry| entry.status == ListStatus::Damaged)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct RemoveReport {
    pub local_ref: LocalRef,
}

impl Store {
    /// Collect committed namespace entries and quick-check their wrapper files.
    /// Per-ref damage is returned in the report; no image content is read.
    pub fn list(&self) -> Result<ListReport> {
        let guard = self.prepare(Operation::List)?;
        let discovered = discovery::discover(&guard)?;
        let mut report = ListReport {
            entries: Vec::new(),
            diagnostics: Vec::new(),
            complete: discovered.complete,
        };
        fsutil::checkpoint("inventory.discovered", self.layout.root())?;
        for item in discovered.entries {
            let mut entry = ListEntry {
                path: item.path,
                local_ref: item.local_ref,
                stored_root: None,
                metadata: None,
                status: ListStatus::Damaged,
            };
            if let Some(error) = item.diagnostic {
                report.diagnostics.push(ListDiagnostic::from_error(
                    &entry.path,
                    entry.local_ref.as_ref(),
                    &error,
                ));
            } else if let Some(reference) = &entry.local_ref {
                match refs::inspect_small(&guard, reference) {
                    Ok(inspected) => {
                        entry.stored_root = inspected.root.map(|root| root.digest);
                        entry.metadata = inspected.metadata;
                        if inspected.errors.is_empty() {
                            entry.status = ListStatus::Unverified;
                        }
                        for error in inspected.errors {
                            report.diagnostics.push(ListDiagnostic::from_error(
                                &entry.path,
                                Some(reference),
                                &error.cause,
                            ));
                        }
                    }
                    Err(Error::RefMissing(_)) => continue,
                    Err(error) => report.diagnostics.push(ListDiagnostic::from_error(
                        &entry.path,
                        Some(reference),
                        &error,
                    )),
                }
            }
            report.entries.push(entry);
        }
        report
            .entries
            .sort_by(|a, b| a.local_ref.cmp(&b.local_ref).then(a.path.cmp(&b.path)));
        Ok(report)
    }

    /// Remove only the explicit committed name. Retired content awaits prune.
    pub fn remove(&self, reference: &LocalRef) -> Result<RemoveReport> {
        let guard = self.prepare(Operation::Remove)?;
        refs::remove(&guard, reference)?;
        Ok(RemoveReport {
            local_ref: reference.clone(),
        })
    }
}

/// Resolve identity under the caller's retained store guard, which protects
/// both discovery and subsequent use from every namespace mutation.
pub(crate) fn select(guard: &StoreGuard<'_>, selector: &ImageSelector) -> Result<LocalRef> {
    let digest = match selector.digest() {
        None => return LocalRef::new(selector.reference()),
        Some(digest) => digest,
    };
    let mut discovered = discovery::discover(guard)?;
    fsutil::checkpoint("selector.discovered", guard.layout().root())?;
    discovered
        .entries
        .sort_by(|a, b| a.local_ref.cmp(&b.local_ref).then(a.path.cmp(&b.path)));
    let mut diagnostics = Vec::new();
    for item in discovered.entries {
        if item
            .local_ref
            .as_ref()
            .is_some_and(|reference| reference.name() != selector.name())
        {
            continue;
        }
        if let Some(error) = item.diagnostic {
            diagnostics.push(ListDiagnostic::from_error(
                &item.path,
                item.local_ref.as_ref(),
                &error,
            ));
            continue;
        }
        let Some(reference) = item.local_ref else {
            continue;
        };
        match refs::read_small(guard, &reference) {
            Ok(resolved) if &resolved.root.digest == digest => {
                drop(resolved);
                fsutil::checkpoint("selector.selected", &item.path)?;
                return Ok(reference);
            }
            Ok(_) | Err(Error::RefMissing(_)) => (),
            Err(error) => diagnostics.push(ListDiagnostic::from_error(
                &item.path,
                Some(&reference),
                &error,
            )),
        }
    }
    if !discovered.complete || !diagnostics.is_empty() {
        Err(Error::ResolutionIncomplete {
            selector: selector.clone(),
            diagnostics,
        })
    } else {
        Err(Error::DigestMissing(selector.clone()))
    }
}
