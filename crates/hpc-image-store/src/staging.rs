use crate::{
    Error, RefMetadata, Result, Sha256Digest, fsutil,
    initialization::{create_directory, require_directory},
    locks::RefGuard,
    oci::{self, Descriptor},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// The acquisition producer supplies identities and sizes it has validated.
#[derive(Clone)]
pub(crate) struct BlobSpec {
    pub digest: Sha256Digest,
    pub size: u64,
}

pub(crate) struct StagedBlob {
    pub digest: Sha256Digest,
    pub size: u64,
    pub path: PathBuf,
}

/// Writable paths are exposed only during acquisition. The producer must stop
/// all writers and close their writable handles before calling `complete`.
pub(crate) struct AcquisitionStage<'a> {
    guard: &'a RefGuard<'a>,
    directory: PathBuf,
}

impl<'a> AcquisitionStage<'a> {
    pub fn new(guard: &'a RefGuard<'a>) -> Result<Self> {
        guard.require_exclusive()?;
        let directory = fsutil::unique_directory(&guard.layout().staging(), "operation-")?;
        for relative in ["layout", "layout/blobs", "blobs", "blobs/sha256"] {
            create_directory(&directory.join(relative))?;
        }
        Ok(Self { guard, directory })
    }
    pub fn layout_path(&self) -> PathBuf {
        self.directory.join("layout")
    }
    pub fn shared_blob_path(&self) -> PathBuf {
        self.directory.join("blobs")
    }

    pub fn complete(
        self,
        selected_root: &Sha256Digest,
        metadata: RefMetadata,
        objects: Vec<BlobSpec>,
    ) -> Result<CompletedStage<'a>> {
        // Acquisition must not redirect wrapper or object access outside its
        // private directory through a symlink.
        require_directory(&self.directory)?;
        for relative in ["layout", "layout/blobs", "blobs", "blobs/sha256"] {
            require_directory(&self.directory.join(relative))?;
        }
        if &metadata.local_ref != self.guard.reference() || &metadata.stored_root != selected_root {
            return Err(Error::InvalidObject {
                path: self.layout_path(),
                reason: "metadata disagrees with destination ref or selected root".into(),
            });
        }
        let root = oci::selected_root(&self.layout_path(), selected_root)?;
        let mut sizes = BTreeMap::new();
        let mut blobs = Vec::new();
        for object in objects {
            if let Some(previous) = sizes.insert(object.digest.clone(), object.size) {
                if previous != object.size {
                    return Err(Error::InvalidObject {
                        path: self.shared_blob_path(),
                        reason: "conflicting sizes for one digest".into(),
                    });
                }
                continue;
            }
            let path = self
                .shared_blob_path()
                .join("sha256")
                .join(object.digest.hex());
            fsutil::sized_object(&path, object.size)?;
            blobs.push(StagedBlob {
                digest: object.digest,
                size: object.size,
                path,
            });
        }
        if sizes.get(&root.digest) != Some(&root.size) {
            return Err(Error::InvalidObject {
                path: self.layout_path(),
                reason: "selected root must be a completed staged object of the descriptor's size"
                    .into(),
            });
        }
        Ok(CompletedStage {
            guard: self.guard,
            directory: self.directory,
            root,
            metadata,
            blobs,
        })
    }
}

/// Consumed by publication; no writable acquisition accessors are available.
/// Failure intentionally leaves private data for conservative pruning.
pub(crate) struct CompletedStage<'a> {
    guard: &'a RefGuard<'a>,
    directory: PathBuf,
    root: Descriptor,
    metadata: RefMetadata,
    blobs: Vec<StagedBlob>,
}

impl CompletedStage<'_> {
    pub(super) fn guard(&self) -> &RefGuard<'_> {
        self.guard
    }
    pub(super) fn directory(&self) -> &Path {
        &self.directory
    }
    pub(super) fn root(&self) -> &Descriptor {
        &self.root
    }
    pub(super) fn metadata(&self) -> &RefMetadata {
        &self.metadata
    }
    pub(super) fn blobs(&self) -> &[StagedBlob] {
        &self.blobs
    }
}
