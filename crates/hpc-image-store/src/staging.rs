use crate::{
    Error, LocalRef, RefMetadata, Result, Sha256Digest, fsutil,
    initialization::{create_directory, require_directory},
    locks::StoreGuard,
    oci::{self, Descriptor},
};
use std::{
    collections::BTreeMap,
    fs,
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
    guard: &'a StoreGuard<'a>,
    reference: LocalRef,
    directory: PathBuf,
}

impl<'a> AcquisitionStage<'a> {
    /// Create staging at a path already recorded by the caller
    /// as belonging to its mutation. This function checks exclusive access and
    /// path placement, but does not verify the mutation record.
    /// The caller is responsible for entering the path in the mutation record
    /// before invoking this function.
    pub(crate) fn recorded(
        guard: &'a StoreGuard<'a>,
        reference: &LocalRef,
        directory: PathBuf,
    ) -> Result<Self> {
        guard.require_exclusive()?;
        if directory.parent() != Some(guard.layout().staging().as_path()) {
            return Err(Error::Coordination(
                "recorded staging must be a direct child of tmp",
            ));
        }
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|source| {
                crate::error::io_error("create recorded staging", &directory, source)
            })?;
        for relative in ["layout", "layout/blobs", "blobs", "blobs/sha256"] {
            create_directory(&directory.join(relative))?;
        }
        Ok(Self {
            guard,
            reference: reference.clone(),
            directory,
        })
    }
    pub fn new(guard: &'a StoreGuard<'a>, reference: &LocalRef) -> Result<Self> {
        guard.require_exclusive()?;
        let directory = fsutil::unique_directory(&guard.layout().staging(), "operation-")?;
        for relative in ["layout", "layout/blobs", "blobs", "blobs/sha256"] {
            create_directory(&directory.join(relative))?;
        }
        Ok(Self {
            guard,
            reference: reference.clone(),
            directory,
        })
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
        if metadata.local_ref != self.reference || &metadata.stored_root != selected_root {
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
            reference: self.reference,
            directory: self.directory,
            root,
            metadata,
            blobs,
        })
    }
}

/// Consumed by publication; no writable acquisition accessors are available.
/// Recorded acquisitions leave failed work for recovery; unrecorded fixtures
/// retain their private data for explicit maintenance.
pub(crate) struct CompletedStage<'a> {
    guard: &'a StoreGuard<'a>,
    reference: LocalRef,
    directory: PathBuf,
    root: Descriptor,
    metadata: RefMetadata,
    blobs: Vec<StagedBlob>,
}

impl CompletedStage<'_> {
    pub(super) fn guard(&self) -> &StoreGuard<'_> {
        self.guard
    }
    pub(super) fn reference(&self) -> &LocalRef {
        &self.reference
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
