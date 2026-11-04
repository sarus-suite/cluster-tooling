use crate::{
    Error, LocalRef, NamespaceChange, RefMetadata, Result, cas,
    error::io_error,
    fsutil,
    initialization::{create_directory, require_directory},
    inspection::Finding,
    layout::{Layout, valid_raw_image_id},
    locks::StoreGuard,
    oci::{self, Descriptor},
    staging::CompletedStage,
};
use std::{
    fs, io,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
};

fn invalid(path: &Path, reason: &'static str) -> Error {
    Error::InvalidPointer {
        path: path.to_owned(),
        reason,
    }
}

pub(crate) fn namespace_entry_at(
    layout: &Layout,
    reference: &LocalRef,
) -> Result<Option<fs::Metadata>> {
    // Check every parent without following symlinks. Unlinking the terminal
    // entry must never mutate a directory reached through a malformed key.
    let mut parent = layout.root().join("refs");
    require_directory(&parent)?;
    for component in reference.encoded_path().components() {
        parent.push(component);
        match fs::symlink_metadata(&parent) {
            Ok(metadata) if metadata.is_dir() => (),
            Ok(_) => {
                return Err(invalid(
                    &parent,
                    "ref namespace parents must be real directories",
                ));
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(io_error("inspect ref namespace parent", &parent, source)),
        }
    }
    let path = layout.ref_entry(reference);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io_error("inspect committed ref", &path, source)),
    }
}

fn replaceable(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if !metadata.is_file() && !metadata.file_type().is_symlink() {
        return Err(invalid(
            path,
            "only symlinks or regular files can be replaced or unlinked",
        ));
    }
    Ok(())
}

/// Publish only after content and a matching raw image are durable. The
/// exclusive store guard serializes the destination policy and namespace change.
pub(crate) fn check_destination(
    guard: &StoreGuard<'_>,
    reference: &LocalRef,
    replace: bool,
) -> Result<()> {
    guard.require_exclusive()?;
    if let Some(metadata) = namespace_entry_at(guard.layout(), reference)? {
        if !replace {
            return Err(Error::RefExists(reference.clone()));
        }
        replaceable(&guard.layout().ref_entry(reference), &metadata)?;
    }
    Ok(())
}

pub(crate) fn publish(stage: CompletedStage<'_>, replace: bool) -> Result<PathBuf> {
    let guard = stage.guard();
    let reference = stage.reference();
    guard.require_exclusive()?;
    let destination = guard.layout().ref_entry(reference);
    check_destination(guard, reference, replace)?;
    for blob in stage.blobs() {
        cas::insert(guard, blob)?;
    }
    fsutil::checkpoint("raw_image.prepare", &guard.layout().raw_images())?;
    let raw_image = fsutil::unique_directory(&guard.layout().raw_images(), "raw-")?;
    create_directory(&raw_image.join("blobs"))?;
    oci::write_layout(&raw_image, stage.root())?;
    let metadata = serde_json::to_vec(stage.metadata()).map_err(|source| Error::Metadata {
        kind: "ref",
        source,
    })?;
    fsutil::write_new(&raw_image.join("metadata.json"), &metadata)?;
    fsutil::sync_before_commit(&raw_image.join("blobs"), "raw_image.blobs-sync")?;
    fsutil::sync_before_commit(&raw_image, "raw_image.directory-sync")?;
    fsutil::sync_before_commit(&guard.layout().raw_images(), "raw_image.parent-sync")?;

    // Persist the entire namespace path before publishing the ref. The exclusive
    // store guard protects shared prefixes throughout publication.
    let mut parent = guard.layout().root().join("refs");
    let mut parents = vec![parent.clone()];
    for component in reference.encoded_path().components() {
        parent.push(component);
        create_directory(&parent)?;
        parents.push(parent.clone());
    }
    for directory in parents.iter().rev() {
        fsutil::sync_before_commit(directory, "ref.namespace-sync")?;
    }
    let id = raw_image.file_name().unwrap().to_str().unwrap();
    let target = guard.layout().raw_image_target(reference, id);
    let temporary = stage.directory().join("ref-pointer");
    symlink(target, &temporary)
        .map_err(|source| io_error("prepare committed pointer", &temporary, source))?;
    fsutil::sync_before_commit(stage.directory(), "ref.temporary-sync")?;
    fsutil::checkpoint("ref.rename", &destination)?;
    fs::rename(&temporary, &destination)
        .map_err(|source| io_error("publish committed pointer", &destination, source))?;
    // From here onward an error must say that the new ref may be visible. Do
    // not roll back: readers may already have observed the complete raw image.
    fsutil::sync_after_commit(
        &parent,
        "ref.destination-sync",
        NamespaceChange::RefPublished,
        &destination,
    )?;
    fsutil::sync_after_commit(
        stage.directory(),
        "ref.source-sync",
        NamespaceChange::RefPublished,
        &destination,
    )?;
    Ok(raw_image)
}

pub(crate) fn remove(guard: &StoreGuard<'_>, reference: &LocalRef) -> Result<()> {
    guard.require_exclusive()?;
    let path = guard.layout().ref_entry(reference);
    let metadata = namespace_entry_at(guard.layout(), reference)?
        .ok_or_else(|| Error::RefMissing(reference.clone()))?;
    replaceable(&path, &metadata)?;
    fsutil::checkpoint("ref.unlink", &path)?;
    fs::remove_file(&path).map_err(|source| io_error("unlink committed ref", &path, source))?;
    fsutil::sync_after_commit(
        path.parent().unwrap(),
        "ref.remove-sync",
        NamespaceChange::RefRemoved,
        &path,
    )
}

/// Validate the exact relative pointer shape before touching its target.
pub(crate) fn raw_image_path(guard: &StoreGuard<'_>, reference: &LocalRef) -> Result<PathBuf> {
    raw_image_path_at(guard.layout(), reference)
}

fn raw_image_path_at(layout: &Layout, reference: &LocalRef) -> Result<PathBuf> {
    let id =
        pointer_identity(layout, reference)?.ok_or_else(|| Error::RefMissing(reference.clone()))?;
    let raw_image = layout.raw_image(&id);
    require_directory(&raw_image)?;
    Ok(raw_image)
}

/// Snapshot the terminal pointer without accessing its target.
pub(crate) fn pointer_identity(layout: &Layout, reference: &LocalRef) -> Result<Option<String>> {
    let path = layout.ref_entry(reference);
    let Some(metadata) = namespace_entry_at(layout, reference)? else {
        return Ok(None);
    };
    if !metadata.file_type().is_symlink() {
        return Err(invalid(&path, "committed ref must be a symlink"));
    }
    let target =
        fs::read_link(&path).map_err(|source| io_error("read committed pointer", &path, source))?;
    let id = target
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| valid_raw_image_id(name))
        .ok_or_else(|| invalid(&path, "unrecognized raw-image identity"))?;
    let expected = layout.raw_image_target(reference, id);
    if target.as_os_str() != expected.as_os_str() {
        return Err(invalid(
            &path,
            "pointer must be the canonical relative path into raw_images",
        ));
    }
    Ok(Some(id.to_owned()))
}

/// The borrowed guard keeps the selected raw image protected from reclamation for
/// the whole time the caller reads or uses it.
pub(crate) struct ResolvedRef<'a> {
    _guard: &'a StoreGuard<'a>,
    pub raw_image: PathBuf,
    pub root: Descriptor,
    pub metadata: RefMetadata,
}

pub(crate) fn read<'a>(guard: &'a StoreGuard<'a>, reference: &LocalRef) -> Result<ResolvedRef<'a>> {
    let resolved = read_small(guard, reference)?;
    fsutil::sized_object(
        &guard.layout().blob(&resolved.root.digest),
        resolved.root.size,
    )?;
    Ok(resolved)
}

/// Inventory validates only wrapper files; no CAS object is opened here.
pub(crate) fn read_small<'a>(
    guard: &'a StoreGuard<'a>,
    reference: &LocalRef,
) -> Result<ResolvedRef<'a>> {
    let mut inspected = inspect_small(guard, reference)?;
    if !inspected.errors.is_empty() {
        return Err(inspected.errors.remove(0).into_error());
    }
    Ok(ResolvedRef {
        _guard: guard,
        raw_image: inspected.raw_image,
        root: inspected.root.expect("successful root inspection"),
        metadata: inspected.metadata.expect("successful metadata inspection"),
    })
}

pub(crate) struct RefInspection {
    pub raw_image: PathBuf,
    pub root: Option<Descriptor>,
    pub metadata: Option<RefMetadata>,
    pub errors: Vec<Finding>,
}

/// Keep independently readable fields available when another wrapper file is
/// damaged. A cached digest never replaces an unreadable authoritative root.
pub(crate) fn inspect_small(guard: &StoreGuard<'_>, reference: &LocalRef) -> Result<RefInspection> {
    inspect_small_at(guard.layout(), reference)
}

fn inspect_small_at(layout: &Layout, reference: &LocalRef) -> Result<RefInspection> {
    let raw_image = raw_image_path_at(layout, reference)?;
    let mut errors = Vec::new();
    if let Err(error) = require_directory(&raw_image.join("blobs")) {
        errors.push(Finding::wrapper(&raw_image.join("blobs"), error));
    }
    let root = match oci::inspect_single_root(&raw_image) {
        Ok(root) => Some(root),
        Err(error) => {
            errors.push(error);
            None
        }
    };
    let metadata_path = raw_image.join("metadata.json");
    let metadata = match fsutil::checkpoint("refs.metadata-read", &metadata_path)
        .and_then(|()| fsutil::read_regular(&metadata_path))
        .and_then(|bytes| RefMetadata::from_json(&bytes))
    {
        Ok(metadata) => {
            if &metadata.local_ref != reference
                || root
                    .as_ref()
                    .is_some_and(|root| metadata.stored_root != root.digest)
            {
                errors.push(Finding::wrapper(
                    &metadata_path,
                    Error::InvalidObject {
                        path: metadata_path.clone(),
                        reason: "metadata disagrees with logical ref or layout root".into(),
                    },
                ));
            }
            Some(metadata)
        }
        Err(error) => {
            errors.push(Finding::wrapper(&metadata_path, error));
            None
        }
    };
    Ok(RefInspection {
        raw_image,
        root,
        metadata,
        errors,
    })
}
