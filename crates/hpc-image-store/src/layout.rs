use crate::{LocalRef, Sha256Digest};
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
    pub fn blobs(&self) -> PathBuf {
        self.root.join("blobs/sha256")
    }
    pub fn blob(&self, digest: &Sha256Digest) -> PathBuf {
        self.blobs().join(digest.hex())
    }
    pub fn generations(&self) -> PathBuf {
        self.root.join("generations")
    }
    pub fn generation(&self, id: &str) -> PathBuf {
        self.generations().join(id)
    }
    pub fn staging(&self) -> PathBuf {
        self.root.join("tmp")
    }

    pub fn generation_target(&self, reference: &LocalRef, id: &str) -> PathBuf {
        let mut target = PathBuf::new();
        // From refs/<key components>/ back to the store root.
        for _ in 0..=reference.encoded_path().components().count() {
            target.push("..");
        }
        target.push("generations");
        target.push(id);
        target
    }
}

pub(crate) fn valid_generation_id(id: &str) -> bool {
    id.strip_prefix("generation-").is_some_and(|suffix| {
        suffix.len() == 12 && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
    })
}
