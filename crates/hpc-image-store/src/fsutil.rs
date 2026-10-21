//! Small filesystem helpers, with test-only failure checkpoints.
use crate::{
    Error, NamespaceChange, Result, error::io_error, initialization::require_regular_file,
};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

pub(crate) fn open_regular(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| io_error("open regular file", path, source))?;
    require_regular_file(path, &file)?;
    Ok(file)
}

pub(crate) fn read_regular(path: &Path) -> Result<Vec<u8>> {
    let mut file = open_regular(path)?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes)
        .map_err(|source| io_error("read regular file", path, source))?;
    Ok(bytes)
}

pub(crate) fn sized_object(path: &Path, size: u64) -> Result<File> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| io_error("inspect object", path, source))?;
    if !metadata.is_file() {
        return Err(Error::InvalidObject {
            path: path.to_owned(),
            reason: "expected a regular object, not a symlink or other entry".into(),
        });
    }
    let file = open_regular(path)?;
    let actual = file
        .metadata()
        .map_err(|source| io_error("inspect object", path, source))?
        .len();
    if actual != size {
        return Err(Error::InvalidObject {
            path: path.to_owned(),
            reason: format!("expected {size} bytes, found {actual}"),
        });
    }
    Ok(file)
}

pub(crate) fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| io_error("create raw-image file", path, source))?;
    file.write_all(bytes)
        .map_err(|source| io_error("write raw-image file", path, source))?;
    checkpoint("raw_image.file-sync", path)?;
    file.sync_all()
        .map_err(|source| io_error("sync raw-image file", path, source))
}

pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

pub(crate) fn sync_before_commit(path: &Path, point: &'static str) -> Result<()> {
    checkpoint(point, path)?;
    sync_directory(path)
        .map_err(|source| io_error("sync directory before publication", path, source))
}

pub(crate) fn durability(
    change: NamespaceChange,
    namespace: &Path,
    sync_path: &Path,
    source: io::Error,
) -> Error {
    Error::Durability {
        change,
        path: namespace.to_owned(),
        sync_path: sync_path.to_owned(),
        source,
    }
}

pub(crate) fn sync_after_commit(
    path: &Path,
    point: &'static str,
    change: NamespaceChange,
    namespace: &Path,
) -> Result<()> {
    fault(point)
        .and_then(|()| sync_directory(path))
        .map_err(|source| durability(change, namespace, path, source))
}

pub(crate) fn checkpoint(point: &'static str, path: &Path) -> Result<()> {
    fault(point).map_err(|source| io_error(point, path, source))
}

// Production calls are no-ops; no filesystem abstraction or runtime injection
// interface is exposed. Thread-local state keeps parallel tests independent.
pub(crate) fn fault(_point: &'static str) -> io::Result<()> {
    #[cfg(test)]
    {
        let failure = FAILURE.with(|slot| {
            let mut value = slot.borrow_mut();
            if value.as_ref().is_some_and(|(point, _)| *point == _point) {
                if let Some((_, FaultAction::FailAfter { remaining, .. })) = value.as_mut()
                    && *remaining > 0
                {
                    *remaining -= 1;
                    return None;
                }
                if let Some((point, FaultAction::Persistent(errno))) = *value {
                    Some((point, FaultAction::Persistent(errno)))
                } else {
                    value.take()
                }
            } else {
                None
            }
        });
        if let Some((_, action)) = failure {
            match action {
                FaultAction::Fail(errno)
                | FaultAction::Persistent(errno)
                | FaultAction::FailAfter { errno, .. } => {
                    return Err(io::Error::from_raw_os_error(errno));
                }
                FaultAction::Pause => {
                    println!("PUBLICATION_PAUSED");
                    io::stdout().flush()?;
                    let mut instruction = String::new();
                    io::stdin().read_line(&mut instruction)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    static FAILURE: std::cell::RefCell<Option<(&'static str, FaultAction)>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum FaultAction {
    FailAfter { remaining: usize, errno: i32 },
    Persistent(i32),
    Fail(i32),
    Pause,
}

#[cfg(test)]
pub(crate) fn fail_once(point: &'static str, errno: i32) {
    FAILURE.with(|slot| *slot.borrow_mut() = Some((point, FaultAction::Fail(errno))));
}

#[cfg(test)]
pub(crate) fn pause_once(point: &'static str) {
    FAILURE.with(|slot| *slot.borrow_mut() = Some((point, FaultAction::Pause)));
}

pub(crate) fn unique_directory(parent: &Path, prefix: &str) -> Result<PathBuf> {
    tempfile::Builder::new()
        .prefix(prefix)
        .rand_bytes(12)
        .tempdir_in(parent)
        .map(|directory| directory.keep())
        .map_err(|source| io_error("create private directory", parent, source))
}

#[cfg(test)]
pub(crate) fn fail_persistently(point: &'static str, errno: i32) {
    FAILURE.with(|slot| *slot.borrow_mut() = Some((point, FaultAction::Persistent(errno))));
}
#[cfg(test)]
pub(crate) fn clear_failure() {
    FAILURE.with(|slot| *slot.borrow_mut() = None);
}

#[cfg(test)]
pub(crate) fn fail_after(point: &'static str, remaining: usize, errno: i32) {
    FAILURE.with(|slot| {
        *slot.borrow_mut() = Some((point, FaultAction::FailAfter { remaining, errno }))
    });
}
