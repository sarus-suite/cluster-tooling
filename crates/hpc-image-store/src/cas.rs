use crate::{
    NamespaceChange, Result, error::io_error, fsutil, locks::RefGuard, staging::StagedBlob,
};
use std::fs;

/// Insert without overwrite. The caller's completed stage guarantees that the
/// source has no live writers; its inode becomes immutable store content.
pub(crate) fn insert(guard: &RefGuard<'_>, blob: &StagedBlob) -> Result<()> {
    guard.require_exclusive()?;
    let source = fsutil::sized_object(&blob.path, blob.size)?;
    fsutil::checkpoint("blob.source-sync", &blob.path)?;
    source
        .sync_all()
        .map_err(|source| io_error("sync staged object", &blob.path, source))?;
    let target = guard.layout().blob(&blob.digest);
    fsutil::checkpoint("blob.link", &target)?;
    let inserted = match fs::hard_link(&blob.path, &target) {
        Ok(()) => true,
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(source) => {
            return Err(io_error(
                "insert immutable object with hard link",
                &target,
                source,
            ));
        }
    };

    if inserted {
        fsutil::fault("blob.target-sync")
            .and_then(|()| source.sync_all())
            .map_err(|source| {
                fsutil::durability(NamespaceChange::BlobInserted, &target, &target, source)
            })?;
        fsutil::sync_after_commit(
            &guard.layout().blobs(),
            "blob.directory-sync",
            NamespaceChange::BlobInserted,
            &target,
        )?;
    } else {
        // Another publisher may have linked the object but not yet synced its
        // directory. Confirm and sync the winning object before relying on it.
        let existing = fsutil::sized_object(&target, blob.size)?;
        fsutil::checkpoint("blob.reuse-sync", &target)?;
        existing
            .sync_all()
            .map_err(|source| io_error("sync reused object", &target, source))?;
        fsutil::sync_before_commit(&guard.layout().blobs(), "blob.reuse-directory-sync")?;
    }
    Ok(())
}
