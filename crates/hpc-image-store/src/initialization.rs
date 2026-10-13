use crate::{
    Error, Result, StoreMetadata,
    error::io_error,
    layout::{DIRECTORIES, INIT_TEMP, Layout},
    locks::initialization_lock,
};
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
};

fn invalid(path: &Path, reason: &'static str) -> Error {
    Error::InvalidStore {
        path: path.to_owned(),
        reason,
    }
}

pub(crate) fn require_directory(path: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| io_error("inspect directory", path, source))?;
    if !metadata.is_dir() {
        return Err(invalid(
            path,
            "expected a directory, not a symlink or other entry",
        ));
    }
    Ok(())
}

pub(crate) fn create_directory(path: &Path) -> Result<()> {
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => require_directory(path),
        Err(source) => Err(io_error("create directory", path, source)),
    }
}

pub(crate) fn require_regular_file(path: &Path, file: &File) -> Result<()> {
    if !file
        .metadata()
        .map_err(|source| io_error("inspect file", path, source))?
        .is_file()
    {
        return Err(invalid(path, "expected a regular file"));
    }
    Ok(())
}

fn open_regular(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| io_error("open file", path, source))?;
    require_regular_file(path, &file)?;
    Ok(file)
}

fn marker_exists(layout: &Layout) -> Result<bool> {
    match fs::symlink_metadata(layout.marker()) {
        Ok(_) => Ok(true),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error("inspect marker", &layout.marker(), source)),
    }
}

pub(crate) fn validate_initialized(layout: &Layout) -> Result<()> {
    require_directory(layout.root())?;
    if !marker_exists(layout)? {
        return Err(Error::MissingStore(layout.root().to_owned()));
    }
    let file = open_regular(&layout.marker())?;
    let bytes = std::io::read_to_string(file)
        .map_err(|source| io_error("read marker", &layout.marker(), source))?;
    StoreMetadata::from_json(bytes.as_bytes())?;
    for directory in DIRECTORIES {
        require_directory(&layout.root().join(directory))?;
    }
    open_regular(&layout.store_lock())?;
    Ok(())
}

pub(crate) fn validate_read_prerequisites(layout: &Layout) -> Result<()> {
    for path in [
        layout.root().to_owned(),
        layout.root().join("locks"),
        layout.store_lock(),
    ] {
        match fs::symlink_metadata(&path) {
            Ok(_) => (),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Err(Error::MissingStore(layout.root().to_owned()));
            }
            Err(source) => return Err(io_error("inspect store", &path, source)),
        }
    }
    require_directory(layout.root())?;
    require_directory(&layout.root().join("locks"))?;
    // A bootstrap lock can exist while another process is initializing. Wait on
    // that lock before deciding whether the marker is missing.
    open_regular(&layout.store_lock())?;
    Ok(())
}

// No image data is permitted without a marker. Only our empty bootstrap tree
// and the reserved temporary marker are safe to resume.
fn validate_scaffolding(layout: &Layout) -> Result<()> {
    fn visit(root: &Path, relative: &Path) -> Result<()> {
        let directory = root.join(relative);
        require_directory(&directory)?;
        for entry in fs::read_dir(&directory)
            .map_err(|source| io_error("enumerate scaffold", &directory, source))?
        {
            let entry =
                entry.map_err(|source| io_error("read scaffold entry", &directory, source))?;
            let child = relative.join(entry.file_name());
            let path = root.join(&child);
            if DIRECTORIES.iter().any(|known| child == Path::new(known)) {
                visit(root, &child)?;
            } else if child == Path::new("locks/store.lock")
                || child == Path::new(INIT_TEMP)
                || child == Path::new("store.json")
            {
                open_regular(&path)?;
            } else {
                return Err(invalid(
                    &path,
                    "unrecognized or populated unversioned store",
                ));
            }
        }
        Ok(())
    }
    visit(layout.root(), Path::new(""))
}

pub(crate) fn initialize(layout: &Layout) -> Result<()> {
    // Preflight prevents adding bootstrap entries to a foreign or unsupported store.
    match fs::symlink_metadata(layout.root()) {
        Ok(_) => {
            require_directory(layout.root())?;
            if marker_exists(layout)? {
                return validate_initialized(layout);
            } else if let Err(error) = validate_scaffolding(layout) {
                // A concurrent initializer may have committed during enumeration.
                if marker_exists(layout)? {
                    validate_initialized(layout)?;
                } else {
                    return Err(error);
                }
            }
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(layout.root())
                .map_err(|source| io_error("create store root", layout.root(), source))?;
            require_directory(layout.root())?;
        }
        Err(source) => return Err(io_error("inspect store root", layout.root(), source)),
    }
    create_directory(&layout.root().join("locks"))?;
    let _guard = initialization_lock(layout)?;
    if marker_exists(layout)? {
        return validate_initialized(layout);
    }
    validate_scaffolding(layout)?;
    for directory in DIRECTORIES {
        create_directory(&layout.root().join(directory))?;
    }
    let temporary = layout.root().join(INIT_TEMP);
    // An interrupted initializer may have left this exact private file. No other
    // temporary files are recognized, and no lock file is ever replaced.
    if fs::symlink_metadata(&temporary).is_ok() {
        open_regular(&temporary)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&temporary)
        .map_err(|source| io_error("write temporary store marker", &temporary, source))?;
    require_regular_file(&temporary, &file)?;
    let bytes =
        serde_json::to_vec(&StoreMetadata::default()).map_err(|source| Error::Metadata {
            kind: "store",
            source,
        })?;
    file.write_all(&bytes)
        .map_err(|source| io_error("write store marker", &temporary, source))?;
    file.sync_all()
        .map_err(|source| io_error("sync temporary marker", &temporary, source))?;
    // Persist the lock identity and scaffolding before the marker makes them live.
    open_regular(&layout.store_lock())?
        .sync_all()
        .map_err(|source| io_error("sync store lock", &layout.store_lock(), source))?;
    for directory in DIRECTORIES.iter().rev() {
        sync_directory(&layout.root().join(directory), false)?;
    }
    sync_directory(layout.root(), false)?;
    fs::rename(&temporary, layout.marker())
        .map_err(|source| io_error("publish store marker", &layout.marker(), source))?;
    sync_directory(layout.root(), true)?;
    // Also persist a newly created root and any newly created parent directories.
    for parent in layout.root().ancestors().skip(1) {
        sync_directory(parent, true)?;
    }
    Ok(())
}

fn sync_directory(path: &Path, published: bool) -> Result<()> {
    let result = File::open(path).and_then(|file| file.sync_all());
    result.map_err(|source| {
        if published {
            Error::InitializationDurability {
                path: path.to_owned(),
                source,
            }
        } else {
            io_error("sync directory", path, source)
        }
    })
}
