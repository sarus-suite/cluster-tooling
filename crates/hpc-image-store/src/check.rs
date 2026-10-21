//! Ref checking owns coordination and report attribution; graph reading is
//! independent so prune can reuse it under an exclusive store lock.
use crate::{
    DiagnosticCategory, Error, ImageSelector, ListDiagnostic, LocalRef, Result, Sha256Digest,
    Store, discovery, fsutil,
    graph::GraphReader,
    inspection,
    inventory::{self, serialize_path},
    locks::{Operation, RefGuard},
};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub enum CheckScope {
    Image(ImageSelector),
    All,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passed,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
pub struct CheckEntry {
    #[serde(serialize_with = "serialize_path")]
    pub path: PathBuf,
    pub local_ref: Option<LocalRef>,
    pub stored_root: Option<Sha256Digest>,
    pub status: CheckStatus,
}

#[derive(Clone, Debug, Serialize)]
pub struct CheckDiagnostic {
    pub category: DiagnosticCategory,
    #[serde(serialize_with = "serialize_path")]
    pub path: PathBuf,
    pub local_ref: Option<LocalRef>,
    pub object_digest: Option<Sha256Digest>,
    pub message: String,
}
impl CheckDiagnostic {
    fn from_error(path: &Path, reference: Option<&LocalRef>, error: &Error) -> Self {
        let diagnostic = ListDiagnostic::from_error(path, reference, error);
        Self {
            category: diagnostic.category,
            path: diagnostic.path,
            local_ref: diagnostic.local_ref,
            object_digest: None,
            message: diagnostic.message,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CheckReport {
    pub entries: Vec<CheckEntry>,
    pub diagnostics: Vec<CheckDiagnostic>,
    /// False if namespace enumeration or any requested ref/dependency could
    /// not be fully inspected. This is not a promise of digest verification.
    pub complete: bool,
}
impl CheckReport {
    pub fn has_errors(&self) -> bool {
        !self.complete
            || !self.diagnostics.is_empty()
            || self
                .entries
                .iter()
                .any(|entry| entry.status == CheckStatus::Failed)
    }
}

impl Store {
    /// Check wrappers and supported reachable OCI content without hashing,
    /// unpacking, fetching dependencies, repairing data, or initializing a store.
    pub fn check(&self, scope: &CheckScope) -> Result<CheckReport> {
        let guard = self.prepare(Operation::Check)?;
        let mut reader = GraphReader::new(&self.layout.blobs());
        let mut report = CheckReport {
            entries: Vec::new(),
            diagnostics: Vec::new(),
            complete: true,
        };
        match scope {
            CheckScope::Image(selector) => {
                let ref_guard = inventory::select(&guard, selector)?;
                inspect(&ref_guard, &mut reader, &mut report, false)?;
            }
            CheckScope::All => {
                let discovered = discovery::discover(&guard)?;
                report.complete = discovered.complete;
                fsutil::checkpoint("check.discovered", self.layout.root())?;
                for item in discovered.entries {
                    if let Some(error) = item.diagnostic {
                        failed(&mut report, item.path, item.local_ref, error);
                    } else if let Some(reference) = item.local_ref {
                        match guard.lock_ref(&reference) {
                            Ok(ref_guard) => inspect(&ref_guard, &mut reader, &mut report, true)?,
                            Err(error) => failed(&mut report, item.path, Some(reference), error),
                        }
                    }
                }
            }
        }
        report
            .entries
            .sort_by(|a, b| a.local_ref.cmp(&b.local_ref).then(a.path.cmp(&b.path)));
        Ok(report)
    }
}

fn failed(report: &mut CheckReport, path: PathBuf, reference: Option<LocalRef>, error: Error) {
    report.complete = false;
    report.diagnostics.push(CheckDiagnostic::from_error(
        &path,
        reference.as_ref(),
        &error,
    ));
    report.entries.push(CheckEntry {
        path,
        local_ref: reference,
        stored_root: None,
        status: CheckStatus::Failed,
    });
}

fn inspect(
    guard: &RefGuard<'_>,
    reader: &mut GraphReader,
    report: &mut CheckReport,
    skip_missing: bool,
) -> Result<()> {
    let reference = guard.reference();
    let path = guard.layout().ref_entry(reference);
    let inspected = inspection::inspect(guard, reader, || {
        fsutil::checkpoint("check.selected", &path)
    })?;
    if inspected.missing && skip_missing {
        return Ok(());
    }
    let start = report.diagnostics.len();
    for error in inspected.wrapper_findings {
        report.complete = false;
        report.diagnostics.push(CheckDiagnostic::from_error(
            &path,
            Some(reference),
            &error.cause,
        ));
    }
    if let Some(graph) = inspected.graph {
        report.complete &= graph.complete;
        // Retain reachability in the internal graph report for prune. Checking
        // exposes no partial live set that could accidentally authorize deletion.
        let _ = graph.reachable;
        for diagnostic in graph.diagnostics {
            report.diagnostics.push(CheckDiagnostic {
                category: DiagnosticCategory::Graph,
                path: diagnostic.finding.path,
                local_ref: Some(reference.clone()),
                object_digest: Some(diagnostic.digest),
                message: diagnostic.finding.message,
            });
        }
    } else {
        report.complete = false;
    }
    report.entries.push(CheckEntry {
        path,
        local_ref: Some(reference.clone()),
        stored_root: inspected.stored_root,
        status: if report.diagnostics.len() == start {
            CheckStatus::Passed
        } else {
            CheckStatus::Failed
        },
    });
    Ok(())
}
