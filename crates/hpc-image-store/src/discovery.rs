use crate::{Error, LocalRef, Result, error::io_error, fsutil, locks::StoreGuard};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(crate) struct NamespaceEntry {
    pub path: PathBuf,
    pub local_ref: Option<LocalRef>,
    pub diagnostic: Option<Error>,
}

pub(crate) struct Discovery {
    pub entries: Vec<NamespaceEntry>,
    pub complete: bool,
    /// Scanned namespace directories, including empty ones, children first.
    /// Only a complete scan under an exclusive lock can authorize syncing them.
    pub directories: Vec<PathBuf>,
}

pub(crate) fn discover(guard: &StoreGuard<'_>) -> Result<Discovery> {
    guard.require_shared()?;
    Ok(discover_locked(guard))
}

pub(crate) fn discover_exclusive(guard: &StoreGuard<'_>) -> Result<Discovery> {
    guard.require_exclusive()?;
    Ok(discover_locked(guard))
}

fn discover_locked(guard: &StoreGuard<'_>) -> Discovery {
    let mut result = Discovery {
        entries: Vec::new(),
        complete: true,
        directories: Vec::new(),
    };
    visit(guard, Path::new(""), &mut result);
    result
        .entries
        .sort_by(|left, right| left.path.cmp(&right.path));
    result
}

fn unexpected(result: &mut Discovery, path: PathBuf, diagnostic: Error) {
    result.complete = false;
    result.entries.push(NamespaceEntry {
        path,
        local_ref: None,
        diagnostic: Some(diagnostic),
    });
}

fn visit(guard: &StoreGuard<'_>, relative: &Path, result: &mut Discovery) {
    let path = guard.layout().root().join("refs").join(relative);
    let entries = match fsutil::fault("discovery.enumerate").and_then(|()| fs::read_dir(&path)) {
        Ok(entries) => entries,
        Err(source) => {
            unexpected(
                result,
                path.clone(),
                io_error("enumerate ref namespace", &path, source),
            );
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(source) => {
                unexpected(
                    result,
                    path.clone(),
                    io_error("read ref namespace entry", &path, source),
                );
                continue;
            }
        };
        let child = relative.join(entry.file_name());
        let child_path = entry.path();
        if entry.file_name() == "ref" {
            let reference = match LocalRef::from_encoded_path(relative) {
                Ok(reference) => reference,
                Err(error) => {
                    unexpected(result, child_path, error);
                    continue;
                }
            };
            result.entries.push(NamespaceEntry {
                path: child_path,
                local_ref: Some(reference),
                diagnostic: None,
            });
            continue;
        }

        let name = entry.file_name();
        let metadata = match fs::symlink_metadata(&child_path) {
            Ok(metadata) => metadata,
            Err(source) => {
                unexpected(
                    result,
                    child_path.clone(),
                    io_error("inspect namespace entry", &child_path, source),
                );
                continue;
            }
        };
        // Bound malformed trees as well as refusing symlinks. A logical ref has
        // at most 768 hex characters across seven directory components.
        if encoded_child(relative, &name) && metadata.is_dir() {
            visit(guard, &child, result);
        } else {
            unexpected(
                result,
                child_path.clone(),
                Error::InvalidStore {
                    path: child_path,
                    reason: "unexpected ref namespace entry",
                },
            );
        }
    }
    result.directories.push(path);
}

/// Directory geometry shared by namespace discovery and lock bookkeeping.
/// Terminal ref/lock entries additionally require a complete decoded LocalRef.
pub(crate) fn encoded_child(relative: &Path, name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        !name.is_empty()
            && name.len() <= 120
            && name.len() % 2 == 0
            && name
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }) && relative
        .components()
        .all(|part| part.as_os_str().len() == 120)
        && relative.components().count() < 7
}
