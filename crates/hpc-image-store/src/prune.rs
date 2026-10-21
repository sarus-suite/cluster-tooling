//! Conservative maintenance. The exclusive store lock protects the entire fresh
//! scan and the deletion plan; no per-ref locks or external tools are needed.
use crate::{
    Error, LocalRef, Result, Sha256Digest, Store, discovery, error::io_error, fsutil,
    graph::GraphReader, inventory::serialize_path, layout::valid_raw_image_id, locks::Operation,
    refs,
};
use serde::Serialize;

mod bookkeeping;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

#[derive(Clone, Debug)]
pub struct PruneOptions {
    pub dry_run: bool,
    /// Content age, not time since the last ref was removed. Zero is supported.
    /// Unused lock files and empty ref/lock directories are always eligible.
    pub grace_period: Duration,
}
impl Default for PruneOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            grace_period: Duration::from_secs(24 * 60 * 60),
        }
    }
}

/// Deletion order: private work, retired layouts, CAS, then unused bookkeeping.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PruneKind {
    Staging,
    RawImage,
    Blob,
    RefLock,
    RefLockDirectory,
    RefDirectory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrunePhase {
    Scan,
    Remove,
    Sync,
}

#[derive(Clone, Debug, Serialize)]
pub struct PruneDiagnostic {
    pub phase: PrunePhase,
    #[serde(serialize_with = "serialize_path")]
    pub path: PathBuf,
    pub local_ref: Option<LocalRef>,
    pub object_digest: Option<Sha256Digest>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct PruneSkipped {
    #[serde(serialize_with = "serialize_path")]
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct PruneCandidate {
    #[serde(serialize_with = "serialize_path")]
    pub path: PathBuf,
    pub kind: PruneKind,
    /// Sum of regular-file lengths, counting each directory entry separately.
    /// Hard links, sparse files and filesystem allocation affect actual savings.
    pub size_bytes: u64,
    /// Always false for every candidate when the complete scan is blocked.
    pub eligible: bool,
    /// True once the top-level candidate name has been removed, even if its
    /// parent could not be synced. Consult durability_confirmed as well.
    pub removed: bool,
    /// Visible namespace removals, including partial directory cleanup.
    pub removed_entries: u64,
    pub removed_bytes: u64,
    /// False if a removal occurred whose parent could not be synced. No rollback
    /// is attempted. True with zero removed_entries means nothing was changed.
    pub durability_confirmed: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct PruneSummary {
    pub eligible_count: u64,
    pub eligible_bytes: u64,
    /// Complete candidate names removed, possibly with uncertain durability.
    pub removed_count: u64,
    /// Includes bytes removed from partially cleaned candidates.
    pub removed_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct PruneReport {
    pub dry_run: bool,
    pub grace_period: Duration,
    /// The scan established safe deletion, including the namespace durability
    /// barrier when destructive pruning has eligible candidates. Dry-run does
    /// not attempt that barrier or certify that it would succeed.
    pub scan_complete: bool,
    pub candidates: Vec<PruneCandidate>,
    pub summaries: BTreeMap<PruneKind, PruneSummary>,
    pub skipped: Vec<PruneSkipped>,
    pub diagnostics: Vec<PruneDiagnostic>,
}
impl PruneReport {
    pub fn has_errors(&self) -> bool {
        !self.scan_complete || !self.diagnostics.is_empty()
    }

    fn scan_error(&mut self, path: &Path, error: Error) {
        self.scan_complete = false;
        self.diagnostics.push(PruneDiagnostic {
            phase: PrunePhase::Scan,
            path: path.to_owned(),
            local_ref: None,
            object_digest: None,
            message: error.to_string(),
        });
    }

    fn skip(&mut self, path: &Path, reason: &str) {
        self.skipped.push(PruneSkipped {
            path: path.to_owned(),
            reason: reason.into(),
        });
    }
}

// Exact, preflighted entries in postorder. Never use recursive blind removal:
// unknown contents must preserve the entire candidate, and partial progress
// must remain reportable when an individual removal or sync fails.
struct Removal {
    path: PathBuf,
    directory: bool,
    bytes: u64,
}
struct Candidate {
    report: PruneCandidate,
    removals: Vec<Removal>,
}

impl Store {
    /// Reclaim recognized unreferenced content after a complete authoritative
    /// scan. Both modes require an initialized store. Dry-run creates no files.
    /// Collected scan and deletion failures are returned in the report; store
    /// preparation and lock acquisition failures are returned as errors.
    pub fn prune(&self, options: &PruneOptions) -> Result<PruneReport> {
        let guard = self.prepare(Operation::Prune)?;
        let now = SystemTime::now();
        fsutil::checkpoint("prune.locked", self.layout.root())?;
        let mut report = PruneReport {
            dry_run: options.dry_run,
            grace_period: options.grace_period,
            scan_complete: true,
            candidates: Vec::new(),
            summaries: [
                PruneKind::Staging,
                PruneKind::RawImage,
                PruneKind::Blob,
                PruneKind::RefLock,
                PruneKind::RefLockDirectory,
                PruneKind::RefDirectory,
            ]
            .into_iter()
            .map(|kind| (kind, PruneSummary::default()))
            .collect(),
            skipped: Vec::new(),
            diagnostics: Vec::new(),
        };
        let discovered = discovery::discover_exclusive(&guard)?;
        report.scan_complete = discovered.complete;
        let mut reader = GraphReader::new(&self.layout.blobs());
        let mut live = BTreeSet::new();
        let mut active_raw = BTreeSet::new();
        let mut committed = BTreeSet::new();
        for item in discovered.entries {
            if let Some(error) = item.diagnostic {
                report.scan_error(&item.path, error);
                continue;
            }
            let Some(reference) = item.local_ref else {
                continue;
            };
            committed.insert(reference.clone());
            let inspected = match refs::inspect_small_exclusive(&guard, &reference) {
                Ok(inspected) => inspected,
                Err(error) => {
                    report.scan_error(&item.path, error);
                    continue;
                }
            };
            active_raw.insert(inspected.raw_image);
            for finding in inspected.errors {
                report.scan_complete = false;
                report.diagnostics.push(PruneDiagnostic {
                    phase: PrunePhase::Scan,
                    path: finding.path,
                    local_ref: Some(reference.clone()),
                    object_digest: None,
                    message: finding.message,
                });
            }
            if let Some(root) = inspected.root {
                let graph = reader.scan(&root);
                report.scan_complete &= graph.complete;
                live.extend(graph.reachable);
                for diagnostic in graph.diagnostics {
                    report.diagnostics.push(PruneDiagnostic {
                        phase: PrunePhase::Scan,
                        path: diagnostic.finding.path,
                        local_ref: Some(reference.clone()),
                        object_digest: Some(diagnostic.digest),
                        message: diagnostic.finding.message,
                    });
                }
            } else {
                report.scan_complete = false;
            }
        }
        let mut candidates = Vec::new();
        for (kind, parent) in [
            (PruneKind::Staging, self.layout.staging()),
            (PruneKind::RawImage, self.layout.raw_images()),
            (PruneKind::Blob, self.layout.blobs()),
        ] {
            let found = enumerate(
                &parent,
                kind,
                &live,
                &active_raw,
                now,
                options.grace_period,
                &mut report,
            );
            candidates.extend(found);
        }
        candidates.extend(bookkeeping::collect(
            &guard,
            &committed,
            &discovered.directories,
            &mut report,
        ));
        candidates.sort_by(|a, b| {
            a.report
                .kind
                .cmp(&b.report.kind)
                .then_with(|| {
                    if matches!(
                        a.report.kind,
                        PruneKind::RefDirectory | PruneKind::RefLockDirectory
                    ) {
                        // Siblings have stable ordering, but parents must wait
                        // until their planned child removals have completed.
                        b.report
                            .path
                            .components()
                            .count()
                            .cmp(&a.report.path.components().count())
                    } else {
                        std::cmp::Ordering::Equal
                    }
                })
                .then(a.report.path.cmp(&b.report.path))
        });
        // Check aggregate byte arithmetic before authorizing any mutation.
        for candidate in &candidates {
            if candidate.report.eligible {
                let summary = report.summaries.get_mut(&candidate.report.kind).unwrap();
                if let Some(bytes) = summary
                    .eligible_bytes
                    .checked_add(candidate.report.size_bytes)
                {
                    summary.eligible_count += 1;
                    summary.eligible_bytes = bytes;
                } else {
                    report.scan_error(
                        &candidate.report.path,
                        invalid(&candidate.report.path, "candidate bytes overflow u64"),
                    );
                }
            }
        }
        if report.scan_complete
            && !options.dry_run
            && candidates.iter().any(|candidate| candidate.report.eligible)
        {
            // Visible removals/replacements may have outlived a failed parent
            // sync or a terminated publisher. Save the scanned namespace before
            // reclaiming anything an older ref could otherwise select on crash.
            // Discovery includes empty directories and lists children first.
            for directory in &discovered.directories {
                let sync = fsutil::checkpoint("prune.namespace-sync", directory).and_then(|()| {
                    fsutil::sync_directory(directory).map_err(|source| {
                        io_error("sync ref namespace before pruning", directory, source)
                    })
                });
                if let Err(error) = sync {
                    report.scan_complete = false;
                    report.diagnostics.push(PruneDiagnostic {
                        phase: PrunePhase::Sync,
                        path: directory.clone(),
                        local_ref: None,
                        object_digest: None,
                        message: format!("cannot persist ref namespace before pruning; nothing was removed: {error}"),
                    });
                    break;
                }
            }
        }
        if report.scan_complete && !options.dry_run {
            // Before the first mutation and still under the exclusive lock.
            if let Err(error) = fsutil::checkpoint("prune.before-delete", self.layout.root()) {
                report.scan_error(self.layout.root(), error);
            }
        }
        if !report.scan_complete {
            for candidate in &mut candidates {
                candidate.report.eligible = false;
            }
            for summary in report.summaries.values_mut() {
                *summary = PruneSummary::default();
            }
        } else if !options.dry_run {
            for candidate in &mut candidates {
                if !candidate.report.eligible {
                    continue;
                }
                let failure = remove_candidate(candidate);
                let summary = report.summaries.get_mut(&candidate.report.kind).unwrap();
                summary.removed_count += u64::from(candidate.report.removed);
                summary.removed_bytes += candidate.report.removed_bytes;
                if let Some(diagnostic) = failure {
                    report.diagnostics.push(diagnostic);
                    break;
                }
            }
        }
        report.candidates = candidates
            .into_iter()
            .map(|candidate| candidate.report)
            .collect();
        report.skipped.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(report)
    }
}

fn invalid(path: &Path, reason: &str) -> Error {
    Error::InvalidObject {
        path: path.to_owned(),
        reason: reason.into(),
    }
}

fn canonical_digest(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        name.len() == 64
            && name
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn operation_id(name: &str) -> bool {
    name.strip_prefix("operation-").is_some_and(|suffix| {
        suffix.len() == 12 && suffix.bytes().all(|b| b.is_ascii_alphanumeric())
    })
}
fn old_enough(now: SystemTime, modified: SystemTime, grace: Duration) -> bool {
    now.duration_since(modified).is_ok_and(|age| age >= grace)
}

fn read_entries(path: &Path) -> Result<Vec<PathBuf>> {
    fsutil::checkpoint("prune.enumerate", path)?;
    let entries =
        fs::read_dir(path).map_err(|source| io_error("enumerate prune data", path, source))?;
    let mut paths = Vec::new();
    for entry in entries {
        paths.push(
            entry
                .map_err(|source| io_error("read prune directory entry", path, source))?
                .path(),
        );
    }
    paths.sort();
    Ok(paths)
}

fn enumerate(
    parent: &Path,
    kind: PruneKind,
    live: &BTreeSet<Sha256Digest>,
    active_raw: &BTreeSet<PathBuf>,
    now: SystemTime,
    grace: Duration,
    report: &mut PruneReport,
) -> Vec<Candidate> {
    let paths = match read_entries(parent) {
        Ok(paths) => paths,
        Err(error) => {
            report.scan_error(parent, error);
            return Vec::new();
        }
    };
    let mut candidates = Vec::new();
    for path in paths {
        let name = path.file_name().unwrap();
        let active = match kind {
            PruneKind::RawImage => active_raw.contains(&path),
            PruneKind::Blob => name
                .to_str()
                .and_then(|hex| Sha256Digest::new(format!("sha256:{hex}")).ok())
                .is_some_and(|digest| live.contains(&digest)),
            PruneKind::Staging => false,
            PruneKind::RefLock | PruneKind::RefLockDirectory | PruneKind::RefDirectory => {
                unreachable!("bookkeeping has a separate scanner")
            }
        };
        if active {
            continue;
        }
        let inspect = || -> Result<Option<Candidate>> {
            fsutil::checkpoint("prune.metadata", &path)?;
            let metadata = fs::symlink_metadata(&path)
                .map_err(|source| io_error("inspect prune entry", &path, source))?;
            let name = path.file_name().unwrap();
            let recognized = match kind {
                PruneKind::Blob => canonical_digest(name) && metadata.is_file(),
                PruneKind::RawImage => {
                    name.to_str().is_some_and(valid_raw_image_id) && metadata.is_dir()
                }
                PruneKind::Staging => name.to_str().is_some_and(operation_id) && metadata.is_dir(),
                PruneKind::RefLock | PruneKind::RefLockDirectory | PruneKind::RefDirectory => {
                    unreachable!("bookkeeping has a separate scanner")
                }
            };
            if !recognized {
                return Ok(None);
            }
            let modified = metadata
                .modified()
                .map_err(|source| io_error("read prune modification time", &path, source))?;
            let mut removals = Vec::new();
            if metadata.is_dir() {
                if !inspect_tree(&path, Path::new(""), kind, &mut removals)? {
                    return Ok(None);
                }
            } else {
                removals.push(Removal {
                    path: path.clone(),
                    directory: false,
                    bytes: metadata.len(),
                });
            }
            let size_bytes = removals
                .iter()
                .try_fold(0_u64, |total, entry| total.checked_add(entry.bytes))
                .ok_or_else(|| invalid(&path, "candidate bytes overflow u64"))?;
            Ok(Some(Candidate {
                report: PruneCandidate {
                    path: path.clone(),
                    kind,
                    size_bytes,
                    eligible: old_enough(now, modified, grace),
                    removed: false,
                    removed_entries: 0,
                    removed_bytes: 0,
                    durability_confirmed: true,
                },
                removals,
            }))
        };
        match inspect() {
            Ok(Some(candidate)) => candidates.push(candidate),
            Ok(None) => report.skip(
                &path,
                "unrecognized entry or directory contents; preserved in full",
            ),
            Err(error) => report.scan_error(&path, error),
        }
    }
    candidates
}

/// Only known directory paths recurse, so malformed/unknown trees are bounded.
/// File contents need not be valid: abandoned partial writes are reclaimable.
fn inspect_tree(
    root: &Path,
    relative: &Path,
    kind: PruneKind,
    removals: &mut Vec<Removal>,
) -> Result<bool> {
    let directory = root.join(relative);
    let mut recognized = true;
    for path in read_entries(&directory)? {
        let child = relative.join(path.file_name().unwrap());
        let metadata = fs::symlink_metadata(&path)
            .map_err(|source| io_error("inspect cleanup tree entry", &path, source))?;
        let (is_directory, is_file, is_pointer) = match kind {
            PruneKind::RawImage => (
                child == Path::new("blobs"),
                matches!(
                    child.to_str(),
                    Some("oci-layout" | "index.json" | "metadata.json")
                ),
                false,
            ),
            PruneKind::Staging => (
                matches!(
                    child.to_str(),
                    Some(
                        "layout"
                            | "layout/blobs"
                            | "layout/blobs/sha256"
                            | "blobs"
                            | "blobs/sha256"
                    )
                ),
                matches!(
                    child.to_str(),
                    Some("layout/oci-layout" | "layout/index.json")
                ) || (matches!(
                    child.parent().and_then(Path::to_str),
                    Some("blobs/sha256" | "layout/blobs/sha256")
                ) && canonical_digest(child.file_name().unwrap())),
                child == Path::new("ref-pointer"),
            ),
            PruneKind::Blob
            | PruneKind::RefLock
            | PruneKind::RefLockDirectory
            | PruneKind::RefDirectory => {
                unreachable!("only staged and raw-image trees are scanned here")
            }
        };
        if is_directory && metadata.is_dir() {
            recognized &= inspect_tree(root, &child, kind, removals)?;
        } else if is_file && metadata.is_file() {
            removals.push(Removal {
                path,
                directory: false,
                bytes: metadata.len(),
            });
        } else if is_pointer && metadata.file_type().is_symlink() {
            removals.push(Removal {
                path,
                directory: false,
                bytes: 0,
            });
        } else {
            recognized = false;
        }
    }
    removals.push(Removal {
        path: directory,
        directory: true,
        bytes: 0,
    });
    Ok(recognized)
}

fn remove_candidate(candidate: &mut Candidate) -> Option<PruneDiagnostic> {
    for entry in &candidate.removals {
        let bookkeeping = matches!(
            candidate.report.kind,
            PruneKind::RefLock | PruneKind::RefLockDirectory | PruneKind::RefDirectory
        );
        let remove_point = if bookkeeping {
            "prune.bookkeeping-remove"
        } else {
            "prune.remove"
        };
        let sync_point = if bookkeeping {
            "prune.bookkeeping-sync"
        } else {
            "prune.sync"
        };
        let remove = fsutil::fault(remove_point).and_then(|()| {
            if entry.directory {
                fs::remove_dir(&entry.path)
            } else {
                fs::remove_file(&entry.path)
            }
        });
        if let Err(source) = remove {
            return Some(PruneDiagnostic {
                phase: PrunePhase::Remove,
                path: entry.path.clone(),
                local_ref: None,
                object_digest: None,
                message: io_error(
                    "remove prune entry (earlier removals remain visible)",
                    &entry.path,
                    source,
                )
                .to_string(),
            });
        }
        candidate.report.removed_entries += 1;
        candidate.report.removed_bytes += entry.bytes;
        candidate.report.removed = entry.path == candidate.report.path;
        let parent = entry.path.parent().unwrap();
        if let Err(source) = fsutil::fault(sync_point).and_then(|()| fsutil::sync_directory(parent))
        {
            candidate.report.durability_confirmed = false;
            return Some(PruneDiagnostic {
                phase: PrunePhase::Sync,
                path: parent.to_owned(),
                local_ref: None,
                object_digest: None,
                message: format!(
                    "removal of {} is visible but may not survive a crash; failed to sync parent: {source}",
                    entry.path.display()
                ),
            });
        }
    }
    None
}

#[cfg(test)]
mod age_tests {
    use super::*;
    #[test]
    fn exact_grace_boundary_and_future_times() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100_000);
        let grace = Duration::from_secs(86_400);
        assert!(old_enough(now, now - grace, grace));
        assert!(!old_enough(
            now,
            now - grace + Duration::from_nanos(1),
            grace
        ));
        assert!(old_enough(now, now, Duration::ZERO));
        assert!(!old_enough(
            now,
            now + Duration::from_nanos(1),
            Duration::ZERO
        ));
    }
}
