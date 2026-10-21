//! Ref locks may be reclaimed only while the exclusive store guard proves no
//! holder or waiter can still have a ref-lock descriptor. Roots stay permanent.
use super::{Candidate, PruneCandidate, PruneKind, PruneReport, Removal, read_entries};
use crate::{LocalRef, Result, discovery, error::io_error, fsutil, locks::StoreGuard};
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

pub(super) fn collect(
    guard: &StoreGuard<'_>,
    committed: &BTreeSet<LocalRef>,
    ref_directories: &[PathBuf],
    report: &mut PruneReport,
) -> Vec<Candidate> {
    // Do not turn this scanner into a read operation usable under a shared lock:
    // an absent ref alone does not prove its lock has no holders or waiters.
    if let Err(error) = guard.require_exclusive() {
        report.scan_error(guard.layout().root(), error);
        return Vec::new();
    }
    let lock_root = guard.layout().root().join("locks/refs");
    let mut candidates = match lock_tree(&lock_root, Path::new(""), committed, report) {
        Ok(tree) => tree.candidates,
        Err(error) => {
            scan_error(report, &lock_root, error);
            Vec::new()
        }
    };
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

struct LockTree {
    candidates: Vec<Candidate>,
    recognized: bool,
    empty_after_cleanup: bool,
}

fn entries(directory: &Path) -> Result<Vec<PathBuf>> {
    fsutil::checkpoint("prune.bookkeeping-enumerate", directory)?;
    read_entries(directory)
}

fn lock_tree(
    root: &Path,
    relative: &Path,
    committed: &BTreeSet<LocalRef>,
    report: &mut PruneReport,
) -> Result<LockTree> {
    let directory = root.join(relative);
    let is_root = relative.as_os_str().is_empty();
    let mut tree = LockTree {
        candidates: Vec::new(),
        recognized: true,
        empty_after_cleanup: true,
    };
    for path in entries(&directory)? {
        fsutil::checkpoint("prune.bookkeeping-metadata", &path)?;
        let metadata = fs::symlink_metadata(&path)
            .map_err(|source| io_error("inspect ref lock bookkeeping", &path, source))?;
        let name = path.file_name().unwrap();
        if name == "ref.lock"
            && metadata.is_file()
            && metadata.len() == 0
            && metadata.nlink() == 1
            && let Ok(reference) = LocalRef::from_encoded_path(relative)
        {
            if committed.contains(&reference) {
                tree.empty_after_cleanup = false;
            } else {
                tree.candidates
                    .push(candidate(&path, PruneKind::RefLock, false));
            }
        } else if metadata.is_dir() && discovery::encoded_child(relative, name) {
            let child = lock_tree(root, &relative.join(name), committed, report)?;
            tree.empty_after_cleanup &= child.empty_after_cleanup;
            // Each encoded subtree below the permanent root is independent.
            // Unknown contents inside one preserve that subtree in full.
            if !child.recognized && !is_root {
                tree.recognized = false;
            }
            tree.candidates.extend(child.candidates);
        } else {
            report.skip(
                &path,
                "unrecognized ref lock entry; preserved without following symlinks",
            );
            tree.empty_after_cleanup = false;
            if !is_root {
                tree.recognized = false;
            }
        }
    }
    if !tree.recognized {
        tree.candidates.clear();
        tree.empty_after_cleanup = false;
        report.skip(
            &directory,
            "ref lock subtree contains unknown contents; preserved in full",
        );
    } else if tree.empty_after_cleanup && !is_root {
        tree.candidates
            .push(candidate(&directory, PruneKind::RefLockDirectory, true));
    }
    Ok(tree)
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
