//! Empty ref-directory reclamation under the exclusive store guard.
use super::{Candidate, PruneCandidate, PruneKind, PruneReport, Removal, read_entries};
use crate::{Result, fsutil, locks::StoreGuard};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

pub(super) fn collect(
    guard: &StoreGuard<'_>,
    ref_directories: &[PathBuf],
    report: &mut PruneReport,
) -> Vec<Candidate> {
    if let Err(error) = guard.require_exclusive() {
        report.scan_error(guard.layout().root(), error);
        return Vec::new();
    }
    let mut candidates = Vec::new();
    let ref_root = guard.layout().root().join("refs");
    let mut removed_directories = BTreeSet::new();
    // Discovery supplies postorder. A directory becomes empty only if all its
    // children are directories already planned for removal. A live ref, unknown
    // entry, or retained descendant prevents removing every ancestor above it.
    for directory in ref_directories {
        if directory == &ref_root {
            continue;
        }
        match entries(directory) {
            Ok(children)
                if children
                    .iter()
                    .all(|child| removed_directories.contains(child)) =>
            {
                removed_directories.insert(directory.clone());
                candidates.push(candidate(directory, PruneKind::RefDirectory, true));
            }
            Ok(_) => (),
            Err(error) => scan_error(report, directory, error),
        }
    }
    candidates
}

fn entries(directory: &Path) -> Result<Vec<PathBuf>> {
    fsutil::checkpoint("prune.bookkeeping-enumerate", directory)?;
    read_entries(directory)
}

fn candidate(path: &Path, kind: PruneKind, directory: bool) -> Candidate {
    Candidate {
        report: PruneCandidate {
            path: path.to_owned(),
            kind,
            size_bytes: 0,
            eligible: true,
            removed: false,
            removed_entries: 0,
            removed_bytes: 0,
            durability_confirmed: true,
        },
        removals: vec![Removal {
            path: path.to_owned(),
            directory,
            bytes: 0,
        }],
    }
}

fn scan_error(report: &mut PruneReport, fallback: &Path, error: crate::Error) {
    let path = match &error {
        crate::Error::Io { path, .. } => path.clone(),
        _ => fallback.to_owned(),
    };
    report.scan_error(&path, error);
}
