use crate::{Error, LocalRef, Result, error::io_error, fsutil, locks::StoreGuard, refs};
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
}

pub(crate) fn discover(guard: &StoreGuard<'_>) -> Result<Discovery> {
    guard.require_shared()?;
    let mut result = Discovery {
        entries: Vec::new(),
        complete: true,
    };
    visit(guard, Path::new(""), &mut result);
    result
        .entries
        .sort_by(|left, right| left.path.cmp(&right.path));
    Ok(result)
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
            let diagnostic = match guard.lock_ref(&reference) {
                Ok(ref_guard) => match refs::raw_image_path(&ref_guard) {
                    Ok(_) => None,
                    // A ref can disappear between enumeration and acquisition.
                    Err(Error::RefMissing(_)) => continue,
                    Err(error) => Some(error),
                },
                Err(error) => Some(error),
            };
            result.entries.push(NamespaceEntry {
                path: child_path,
                local_ref: Some(reference),
                diagnostic,
            });
            continue;
        }

        let name = entry.file_name();
        let canonical_chunk = name.to_str().is_some_and(|name| {
            !name.is_empty()
                && name.len() <= 120
                && name.len() % 2 == 0
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        let canonical_parent = relative
            .components()
            .all(|part| part.as_os_str().len() == 120);
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
        if canonical_chunk
            && canonical_parent
            && relative.components().count() < 7
            && metadata.is_dir()
        {
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
}
