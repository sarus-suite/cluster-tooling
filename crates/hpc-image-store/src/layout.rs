use crate::LocalRef;
use std::path::{Path, PathBuf};

pub(crate) const DIRECTORIES: &[&str] = &[
    "blobs",
    "blobs/sha256",
    "refs",
    "generations",
    "tmp",
    "locks",
    "locks/refs",
];
pub(crate) const INIT_TEMP: &str = ".store-init.json";

#[derive(Debug)]
pub(crate) struct Layout {
    root: PathBuf,
}
impl Layout {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn marker(&self) -> PathBuf {
        self.root.join("store.json")
    }
    pub fn store_lock(&self) -> PathBuf {
        self.root.join("locks/store.lock")
    }
    pub fn ref_entry(&self, reference: &LocalRef) -> PathBuf {
        self.root
            .join("refs")
            .join(reference.encoded_path())
            .join("ref")
    }
    pub fn ref_lock(&self, reference: &LocalRef) -> PathBuf {
        self.root
            .join("locks/refs")
            .join(reference.encoded_path())
            .join("ref.lock")
    }
}
