//! Persistent state comparison without access times or following symlinks.
use std::{
    collections::BTreeMap,
    fs,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
};

#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    inode: u64,
    mode: u32,
    modified: (i64, i64),
    bytes: Vec<u8>,
}

pub fn snapshot(root: &Path) -> BTreeMap<PathBuf, Entry> {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, Entry>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let bytes = if metadata.file_type().is_symlink() {
            fs::read_link(path).unwrap().as_os_str().as_bytes().to_vec()
        } else if metadata.is_file() {
            fs::read(path).unwrap()
        } else {
            assert!(metadata.is_dir(), "unexpected snapshot entry: {path:?}");
            Vec::new()
        };
        entries.insert(
            path.strip_prefix(root).unwrap().to_owned(),
            Entry {
                inode: metadata.ino(),
                mode: metadata.mode(),
                modified: (metadata.mtime(), metadata.mtime_nsec()),
                bytes,
            },
        );
        if metadata.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), entries);
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}
