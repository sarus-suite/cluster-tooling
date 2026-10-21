use crate::{
    Error, LocalRef, NamespaceChange, RefMetadata, Sha256Digest, Store, StoreConfig, cas,
    discovery, fsutil,
    locks::{Operation, RefGuard},
    oci::{self, Descriptor},
    refs,
    staging::{AcquisitionStage, BlobSpec, CompletedStage},
};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::fs::{MetadataExt, symlink},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

// Precomputed SHA-256 identities for complete, zero-layer OCI fixtures. Runtime
// publication deliberately does not hash content; these bytes are fixture inputs.
const CONFIG: &[u8] =
    br#"{"architecture":"amd64","os":"linux","rootfs":{"type":"layers","diff_ids":[]}}"#;
const CONFIG_DIGEST: &str = "c5b1d63604f273462ef36fadac3182d43ae6a6138731cf594b314835cf1c034f";
const ONE_DIGEST: &str = "7edeb1b1354d2b2044357c1ecf167324f8523d390f2bcd3496341b97d95c8851";
const TWO_DIGEST: &str = "020acf43630d6b1d288384f240ea91edab826e95e8cbdd0f7ae1938f98ec8139";
const INDEX_DIGEST: &str = "ace808964803c6d4becde72507e63d41314cb9165f116010d1e5f24dee6b1d82";
const INDEX: &[u8] = br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:7edeb1b1354d2b2044357c1ecf167324f8523d390f2bcd3496341b97d95c8851","size":279},{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:020acf43630d6b1d288384f240ea91edab826e95e8cbdd0f7ae1938f98ec8139","size":279}]}"#;

#[derive(Clone, Copy)]
enum Image {
    One,
    Two,
    Index,
}

fn digest(hex: &str) -> Sha256Digest {
    Sha256Digest::new(format!("sha256:{hex}")).unwrap()
}
fn store(path: &Path) -> Store {
    Store::new(StoreConfig::new(path).unwrap())
}
fn reference() -> LocalRef {
    LocalRef::new("analysis/tool:release").unwrap()
}
fn root(image: Image) -> Sha256Digest {
    digest(match image {
        Image::One => ONE_DIGEST,
        Image::Two => TWO_DIGEST,
        Image::Index => INDEX_DIGEST,
    })
}

fn manifest(image: Image) -> Vec<u8> {
    let name = match image {
        Image::One => "one",
        Image::Two => "two",
        Image::Index => unreachable!(),
    };
    format!("{{\"schemaVersion\":2,\"mediaType\":\"application/vnd.oci.image.manifest.v1+json\",\"config\":{{\"mediaType\":\"application/vnd.oci.image.config.v1+json\",\"digest\":\"sha256:{CONFIG_DIGEST}\",\"size\":78}},\"layers\":[],\"annotations\":{{\"fixture\":\"{name}\"}}}}").into_bytes()
}

fn fixture<'a>(
    guard: &'a RefGuard<'a>,
    image: Image,
) -> (AcquisitionStage<'a>, Descriptor, Vec<BlobSpec>) {
    let stage = AcquisitionStage::new(guard).unwrap();
    let mut objects = vec![(digest(CONFIG_DIGEST), CONFIG.to_vec())];
    match image {
        Image::One | Image::Two => objects.push((root(image), manifest(image))),
        Image::Index => {
            objects.push((root(Image::One), manifest(Image::One)));
            objects.push((root(Image::Two), manifest(Image::Two)));
            objects.push((root(Image::Index), INDEX.to_vec()));
        }
    }
    let size = objects.last().unwrap().1.len() as u64;
    let descriptor = Descriptor {
        media_type: if matches!(image, Image::Index) {
            "application/vnd.oci.image.index.v1+json"
        } else {
            "application/vnd.oci.image.manifest.v1+json"
        }
        .into(),
        digest: root(image),
        size,
        extra: BTreeMap::new(),
    };
    oci::write_layout(&stage.layout_path(), &descriptor).unwrap();
    let mut specs = Vec::new();
    for (digest, bytes) in objects {
        fsutil::write_new(
            &stage.shared_blob_path().join("sha256").join(digest.hex()),
            &bytes,
        )
        .unwrap();
        specs.push(BlobSpec {
            digest,
            size: bytes.len() as u64,
        });
    }
    (stage, descriptor, specs)
}

fn completed<'a>(guard: &'a RefGuard<'a>, image: Image) -> CompletedStage<'a> {
    let (stage, descriptor, objects) = fixture(guard, image);
    stage
        .complete(
            &descriptor.digest,
            RefMetadata::new(guard.reference().clone(), descriptor.digest.clone()),
            objects,
        )
        .unwrap()
}

fn publish(handle: &Store, reference: &LocalRef, image: Image, replace: bool) -> PathBuf {
    let store_guard = handle.prepare(Operation::Import).unwrap();
    let ref_guard = store_guard.lock_ref(reference).unwrap();
    refs::publish(completed(&ref_guard, image), replace).unwrap()
}

#[test]
fn export_rejects_a_complete_stored_index_before_invoking_tools() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    publish(&handle, &reference(), Image::Index, false);
    assert!(!handle.check(&crate::CheckScope::All).unwrap().has_errors());
    let before = crate::test_snapshot::snapshot(temporary.path());
    let context = crate::TransferContext {
        podman: "/unavailable/podman".into(),
        skopeo: "/unavailable/skopeo".into(),
        platform: None,
    };
    for selector in [
        reference().to_string(),
        format!("analysis/tool@{}", root(Image::Index)),
    ] {
        assert!(matches!(
            handle.export(&crate::ImageSelector::new(selector).unwrap(), None, &context),
            Err(Error::InvalidObject { reason, .. }) if reason.contains("single OCI manifest")
        ));
        assert_eq!(crate::test_snapshot::snapshot(temporary.path()), before);
    }
}

#[test]
fn publication_replacement_and_removal_preserve_retired_data_and_lock_identities() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    let reference = reference();
    let first = publish(&handle, &reference, Image::One, false);
    let lock = handle.layout.ref_lock(&reference);
    let inode = fs::metadata(&lock).unwrap().ino();
    let guard = handle.prepare(Operation::Import).unwrap();
    let ref_guard = guard.lock_ref(&reference).unwrap();
    let old = refs::read(&ref_guard).unwrap();
    assert_eq!(old.root.digest, root(Image::One));
    assert_eq!(old.metadata.local_ref, reference);
    assert!(
        fs::read_link(handle.layout.ref_entry(&reference))
            .unwrap()
            .is_relative()
    );
    assert_eq!(fs::read_dir(first.join("blobs")).unwrap().count(), 0);
    assert!(matches!(
        refs::publish(completed(&ref_guard, Image::Two), false),
        Err(Error::RefExists(_))
    ));
    assert_eq!(
        refs::read(&ref_guard).unwrap().root.digest,
        root(Image::One)
    );
    let second = refs::publish(completed(&ref_guard, Image::Two), true).unwrap();
    assert_ne!(first, second);
    assert!(first.exists());
    assert_eq!(refs::read(&ref_guard).unwrap().raw_image, second);
    refs::remove(&ref_guard).unwrap();
    assert!(matches!(refs::read(&ref_guard), Err(Error::RefMissing(_))));
    assert!(matches!(
        refs::remove(&ref_guard),
        Err(Error::RefMissing(_))
    ));
    assert!(first.exists() && second.exists());
    assert!(handle.layout.blob(&root(Image::One)).exists());
    assert_eq!(fs::metadata(lock).unwrap().ino(), inode);
}

#[test]
fn shared_guards_reject_mutation_and_metadata_cannot_change_the_destination() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    publish(&handle, &reference(), Image::One, false);
    let guard = handle.prepare(Operation::List).unwrap();
    let ref_guard = guard.lock_ref(&reference()).unwrap();
    assert!(matches!(
        AcquisitionStage::new(&ref_guard),
        Err(Error::Coordination(_))
    ));
    assert!(matches!(
        refs::remove(&ref_guard),
        Err(Error::Coordination(_))
    ));
    drop(ref_guard);
    drop(guard);
    let guard = handle.prepare(Operation::Pull).unwrap();
    let ref_guard = guard.lock_ref(&reference()).unwrap();
    let (stage, descriptor, objects) = fixture(&ref_guard, Image::Two);
    let wrong_metadata = RefMetadata::new(
        LocalRef::new("another:tag").unwrap(),
        descriptor.digest.clone(),
    );
    assert!(
        stage
            .complete(&descriptor.digest, wrong_metadata, objects)
            .is_err()
    );
    assert_eq!(
        refs::read(&ref_guard).unwrap().root.digest,
        root(Image::One)
    );
}

#[test]
fn cas_reuse_preserves_bytes_inode_and_age() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    publish(&handle, &reference(), Image::One, false);
    let path = handle.layout.blob(&root(Image::One));
    let before = fs::metadata(&path).unwrap();
    publish(
        &handle,
        &LocalRef::new("another:tag").unwrap(),
        Image::One,
        false,
    );
    let after = fs::metadata(&path).unwrap();
    assert_eq!(before.ino(), after.ino());
    assert_eq!(
        (before.mtime(), before.mtime_nsec()),
        (after.mtime(), after.mtime_nsec())
    );
    assert_eq!(fs::read(path).unwrap(), manifest(Image::One));
}

#[test]
fn incompatible_cas_entries_are_never_overwritten() {
    for kind in ["size", "symlink", "directory"] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        let guard = handle.prepare(Operation::Import).unwrap();
        let ref_guard = guard.lock_ref(&reference()).unwrap();
        let stage = completed(&ref_guard, Image::One);
        let target = handle.layout.blob(&root(Image::One));
        let outside = temporary.path().join("outside");
        fs::write(&outside, b"keep").unwrap();
        match kind {
            "size" => fs::write(&target, b"wrong size").unwrap(),
            "symlink" => symlink(&outside, &target).unwrap(),
            "directory" => fs::create_dir(&target).unwrap(),
            _ => unreachable!(),
        }
        let inode = fs::symlink_metadata(&target).unwrap().ino();
        assert!(matches!(
            refs::publish(stage, false),
            Err(Error::InvalidObject { .. })
        ));
        assert_eq!(fs::symlink_metadata(target).unwrap().ino(), inode);
        assert_eq!(fs::read(outside).unwrap(), b"keep");
        assert!(!handle.layout.ref_entry(&reference()).exists());
    }
}

#[test]
fn interrupted_acquisition_and_invalid_layouts_preserve_previous_refs() {
    for damage in [
        "version",
        "schema",
        "ambiguous",
        "missing",
        "size",
        "digest",
        "symlink",
        "root-size",
        "unsupported-root",
        "descriptor",
        "blob-directory",
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        publish(&handle, &reference(), Image::One, false);
        let guard = handle.prepare(Operation::Import).unwrap();
        let ref_guard = guard.lock_ref(&reference()).unwrap();
        let (stage, descriptor, mut objects) = fixture(&ref_guard, Image::Two);
        let mut metadata = RefMetadata::new(reference(), descriptor.digest.clone());
        match damage {
            "version" => fs::write(
                stage.layout_path().join("oci-layout"),
                br#"{"imageLayoutVersion":"2.0.0"}"#,
            )
            .unwrap(),
            "schema" => fs::write(
                stage.layout_path().join("index.json"),
                br#"{"schemaVersion":1,"manifests":[]}"#,
            )
            .unwrap(),
            "ambiguous" => fs::write(
                stage.layout_path().join("index.json"),
                serde_json::to_vec(
                    &serde_json::json!({"schemaVersion":2,"manifests":[descriptor, descriptor]}),
                )
                .unwrap(),
            )
            .unwrap(),
            "missing" => {
                objects.pop();
            }
            "size" => fs::write(
                stage
                    .shared_blob_path()
                    .join("sha256")
                    .join(descriptor.digest.hex()),
                b"incomplete",
            )
            .unwrap(),
            "digest" => metadata.stored_root = root(Image::One),
            "symlink" => {
                let path = stage
                    .shared_blob_path()
                    .join("sha256")
                    .join(descriptor.digest.hex());
                fs::remove_file(&path).unwrap();
                symlink(handle.layout.blob(&root(Image::One)), path).unwrap();
            }
            "root-size" => {
                let mut descriptor = descriptor.clone();
                descriptor.size += 1;
                fs::write(stage.layout_path().join("index.json"), serde_json::to_vec(&serde_json::json!({"schemaVersion":2,"manifests":[descriptor]})).unwrap()).unwrap();
            }
            "unsupported-root" => {
                let mut descriptor = descriptor.clone();
                descriptor.media_type = "unsupported".into();
                fs::write(stage.layout_path().join("index.json"), serde_json::to_vec(&serde_json::json!({"schemaVersion":2,"manifests":[descriptor]})).unwrap()).unwrap();
            }
            "descriptor" => fs::write(stage.layout_path().join("index.json"), serde_json::to_vec(&serde_json::json!({"schemaVersion":2,"manifests":[{"digest":descriptor.digest,"size":-1}]})).unwrap()).unwrap(),
            "blob-directory" => {
                let blobs = stage.shared_blob_path();
                fs::rename(&blobs, temporary.path().join("retired-stage-blobs")).unwrap();
                symlink(temporary.path().join("retired-stage-blobs"), blobs).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            stage
                .complete(&root(Image::Two), metadata, objects)
                .is_err(),
            "accepted {damage}"
        );
        assert_eq!(
            refs::read(&ref_guard).unwrap().root.digest,
            root(Image::One)
        );
    }
}

#[test]
fn layout_normalization_preserves_content_and_descriptor_extensions() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    let guard = handle.prepare(Operation::Import).unwrap();
    let ref_guard = guard.lock_ref(&reference()).unwrap();
    let (stage, mut descriptor, objects) = fixture(&ref_guard, Image::Index);
    descriptor.extra.insert(
        "annotations".into(),
        serde_json::json!({"fixture":"selected"}),
    );
    fs::write(stage.layout_path().join("index.json"), serde_json::to_vec(&serde_json::json!({
        "schemaVersion":2,"manifests":[descriptor, {"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":root(Image::One),"size":279}]
    })).unwrap()).unwrap();
    let stage = stage
        .complete(
            &root(Image::Index),
            RefMetadata::new(reference(), root(Image::Index)),
            objects,
        )
        .unwrap();
    let raw_image = refs::publish(stage, false).unwrap();
    let descriptors = oci::roots(&raw_image).unwrap();
    assert_eq!(descriptors.len(), 1);
    assert_eq!(descriptors[0], descriptor);
    assert_eq!(
        fs::read(handle.layout.blob(&root(Image::Index))).unwrap(),
        INDEX
    );
    assert_eq!(
        refs::read(&ref_guard).unwrap().root.digest,
        root(Image::Index)
    );
}

#[test]
fn failed_precommit_steps_preserve_the_previous_ref() {
    let points = [
        "blob.source-sync",
        "blob.link",
        "blob.reuse-sync",
        "blob.reuse-directory-sync",
        "raw_image.prepare",
        "raw_image.file-sync",
        "raw_image.blobs-sync",
        "raw_image.directory-sync",
        "raw_image.parent-sync",
        "ref.namespace-sync",
        "ref.temporary-sync",
        "ref.rename",
    ];
    for point in points {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        publish(&handle, &reference(), Image::One, false);
        let guard = handle.prepare(Operation::Import).unwrap();
        let ref_guard = guard.lock_ref(&reference()).unwrap();
        let stage = completed(&ref_guard, Image::Two);
        fsutil::fail_once(
            point,
            if point == "blob.link" {
                libc::EXDEV
            } else {
                libc::EIO
            },
        );
        let result = refs::publish(stage, true);
        assert!(
            matches!(result, Err(Error::Io { .. })),
            "wrong result at {point}"
        );
        assert_eq!(
            refs::read(&ref_guard).unwrap().root.digest,
            root(Image::One),
            "changed ref at {point}"
        );
    }
}

#[test]
fn durability_errors_identify_visible_namespace_changes() {
    for (point, expected_change, expected_root) in [
        (
            "blob.target-sync",
            NamespaceChange::BlobInserted,
            Image::One,
        ),
        (
            "blob.directory-sync",
            NamespaceChange::BlobInserted,
            Image::One,
        ),
        (
            "ref.destination-sync",
            NamespaceChange::RefPublished,
            Image::Two,
        ),
        ("ref.source-sync", NamespaceChange::RefPublished, Image::Two),
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        publish(&handle, &reference(), Image::One, false);
        let guard = handle.prepare(Operation::Import).unwrap();
        let ref_guard = guard.lock_ref(&reference()).unwrap();
        let stage = completed(&ref_guard, Image::Two);
        fsutil::fail_once(point, libc::EIO);
        assert!(
            matches!(refs::publish(stage, true), Err(Error::Durability { change, .. }) if change == expected_change)
        );
        assert_eq!(
            refs::read(&ref_guard).unwrap().root.digest,
            root(expected_root)
        );
        assert!(handle.layout.blob(&root(Image::Two)).exists());
    }
}

#[test]
fn unlink_failure_and_post_unlink_sync_failure_have_different_states() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    let raw_image = publish(&handle, &reference(), Image::One, false);
    let guard = handle.prepare(Operation::Remove).unwrap();
    let ref_guard = guard.lock_ref(&reference()).unwrap();
    fsutil::fail_once("ref.unlink", libc::EIO);
    assert!(matches!(refs::remove(&ref_guard), Err(Error::Io { .. })));
    assert!(refs::read(&ref_guard).is_ok());
    fsutil::fail_once("ref.remove-sync", libc::EIO);
    assert!(matches!(
        refs::remove(&ref_guard),
        Err(Error::Durability {
            change: NamespaceChange::RefRemoved,
            ..
        })
    ));
    assert!(matches!(refs::read(&ref_guard), Err(Error::RefMissing(_))));
    assert!(raw_image.exists());
}

#[test]
fn damaged_refs_can_be_replaced_or_unlinked_without_following_them() {
    for kind in ["file", "broken", "escape", "directory"] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        publish(&handle, &reference(), Image::One, false);
        let path = handle.layout.ref_entry(&reference());
        fs::remove_file(&path).unwrap();
        let outside = temporary.path().join("outside");
        fs::write(&outside, b"keep").unwrap();
        match kind {
            "file" => fs::write(&path, b"damaged").unwrap(),
            "broken" => symlink("missing", &path).unwrap(),
            "escape" => symlink(&outside, &path).unwrap(),
            "directory" => fs::create_dir(&path).unwrap(),
            _ => unreachable!(),
        }
        let guard = handle.prepare(Operation::Remove).unwrap();
        let ref_guard = guard.lock_ref(&reference()).unwrap();
        assert!(refs::read(&ref_guard).is_err());
        assert!(matches!(
            refs::publish(completed(&ref_guard, Image::Two), false),
            Err(Error::RefExists(_))
        ));
        if kind == "directory" {
            assert!(refs::publish(completed(&ref_guard, Image::Two), true).is_err());
            assert!(refs::remove(&ref_guard).is_err());
        } else {
            refs::remove(&ref_guard).unwrap();
            assert_eq!(fs::read(&outside).unwrap(), b"keep");
            // Explicit replacement itself also accepts malformed entries.
            fs::write(&path, b"damaged again").unwrap();
            refs::publish(completed(&ref_guard, Image::Two), true).unwrap();
            assert_eq!(
                refs::read(&ref_guard).unwrap().root.digest,
                root(Image::Two)
            );
        }
    }
}

#[test]
fn readers_reject_escaping_pointers_and_symlinked_raw_image_files() {
    for damage in [
        "absolute",
        "traversal",
        "noncanonical",
        "raw_image",
        "metadata",
        "layout",
        "index",
        "blobs",
        "identity",
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        let raw_image = publish(&handle, &reference(), Image::One, false);
        let path = handle.layout.ref_entry(&reference());
        match damage {
            "absolute" | "traversal" | "noncanonical" => {
                let original = fs::read_link(&path).unwrap();
                fs::remove_file(&path).unwrap();
                let target = match damage {
                    "absolute" => raw_image.clone(),
                    "traversal" => PathBuf::from("../../../tmp/raw-abcdefghijkl"),
                    _ => PathBuf::from(format!("./{}", original.display())),
                };
                symlink(target, &path).unwrap();
            }
            "raw_image" => {
                fs::remove_dir_all(&raw_image).unwrap();
                symlink(outside.path(), &raw_image).unwrap();
            }
            "identity" => fs::write(
                raw_image.join("metadata.json"),
                serde_json::to_vec(&RefMetadata::new(reference(), root(Image::Two))).unwrap(),
            )
            .unwrap(),
            name => {
                let file = match name {
                    "metadata" => "metadata.json",
                    "layout" => "oci-layout",
                    "index" => "index.json",
                    "blobs" => "blobs",
                    _ => unreachable!(),
                };
                let target = raw_image.join(file);
                if file == "blobs" {
                    fs::remove_dir(&target).unwrap();
                } else {
                    fs::remove_file(&target).unwrap();
                }
                symlink(outside.path(), target).unwrap();
            }
        }
        let guard = handle.prepare(Operation::List).unwrap();
        let ref_guard = guard.lock_ref(&reference()).unwrap();
        assert!(refs::read(&ref_guard).is_err(), "accepted {damage}");
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}

#[test]
fn symlinked_namespace_parents_cannot_redirect_reads_or_mutations() {
    let temporary = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    publish(&handle, &reference(), Image::One, false);
    let path = handle.layout.ref_entry(&reference());
    let parent = path.parent().unwrap();
    fs::rename(parent, temporary.path().join("retired-namespace")).unwrap();
    fs::write(outside.path().join("ref"), b"preserve").unwrap();
    symlink(outside.path(), parent).unwrap();
    let guard = handle.prepare(Operation::Import).unwrap();
    let ref_guard = guard.lock_ref(&reference()).unwrap();
    assert!(refs::read(&ref_guard).is_err());
    assert!(refs::remove(&ref_guard).is_err());
    assert!(refs::publish(completed(&ref_guard, Image::Two), true).is_err());
    assert_eq!(fs::read(outside.path().join("ref")).unwrap(), b"preserve");
    assert!(!handle.layout.blob(&root(Image::Two)).exists());
}

#[test]
fn long_and_prefix_sharing_keys_publish_independently() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    let references = [
        LocalRef::new(format!("{}:t", "a".repeat(58))).unwrap(),
        LocalRef::new(format!("{}:tag", "a".repeat(58))).unwrap(),
        LocalRef::new(format!("{}:{}", "a".repeat(255), "T".repeat(128))).unwrap(),
    ];
    for reference in &references {
        publish(&handle, reference, Image::One, false);
    }
    let guard = handle.prepare(Operation::List).unwrap();
    let inventory = discovery::discover(&guard).unwrap();
    assert!(inventory.complete);
    assert_eq!(inventory.entries.len(), 3);
    assert!(
        inventory
            .entries
            .iter()
            .all(|entry| entry.diagnostic.is_none())
    );
    for reference in references {
        let ref_guard = guard.lock_ref(&reference).unwrap();
        assert_eq!(
            refs::read(&ref_guard).unwrap().metadata.local_ref,
            reference
        );
    }
}

#[test]
fn discovery_keeps_damage_and_reports_incomplete_enumeration() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    publish(&handle, &reference(), Image::One, false);
    for (name, file) in [("broken:t", false), ("malformed:t", true)] {
        let path = handle.layout.ref_entry(&LocalRef::new(name).unwrap());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        if file {
            fs::write(path, b"malformed").unwrap();
        } else {
            symlink("missing", path).unwrap();
        }
    }
    let guard = handle.prepare(Operation::List).unwrap();
    let result = discovery::discover(&guard).unwrap();
    assert!(result.complete);
    assert_eq!(result.entries.len(), 3);
    assert_eq!(
        result
            .entries
            .iter()
            .filter(|entry| entry.diagnostic.is_some())
            .count(),
        0 // Enumeration preserves entries; inventory validates under ref locks.
    );
    assert!(result.entries.iter().all(|entry| entry.local_ref.is_some()));
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), handle.layout.root().join("refs/unknown")).unwrap();
    let result = discovery::discover(&guard).unwrap();
    assert!(!result.complete);
    assert_eq!(result.entries.len(), 4);
    fsutil::fail_once("discovery.enumerate", libc::EACCES);
    let result = discovery::discover(&guard).unwrap();
    assert!(!result.complete);
    assert_eq!(result.entries.len(), 1);
    assert!(matches!(
        result.entries[0].diagnostic,
        Some(Error::Io { .. })
    ));
}

#[test]
fn public_inventory_is_empty_or_unverified_without_reading_cas() {
    use crate::ListStatus;
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    drop(handle.prepare(Operation::Import).unwrap());
    let empty = handle.list().unwrap();
    assert!(empty.complete && !empty.has_errors() && empty.entries.is_empty());
    publish(&handle, &reference(), Image::One, false);
    // Even an unusable root blob is outside inventory's quick check.
    fs::remove_file(handle.layout.blob(&root(Image::One))).unwrap();
    symlink(
        "/unavailable/outside",
        handle.layout.blob(&root(Image::One)),
    )
    .unwrap();
    let report = handle.list().unwrap();
    assert!(report.complete && !report.has_errors());
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.entries[0].status, ListStatus::Unverified);
    assert_eq!(report.entries[0].stored_root, Some(root(Image::One)));
    assert!(report.entries[0].metadata.is_some());
}

#[test]
fn public_inventory_preserves_damage_and_independently_readable_fields() {
    use crate::ListStatus;
    use serde_json::{Value, json};
    for damage in [
        "metadata-missing",
        "metadata-json",
        "metadata-version",
        "config-digest-missing",
        "identity",
        "layout-version",
        "index-empty",
        "index-ambiguous",
        "index-json",
        "root-type",
        "blobs-missing",
        "pointer",
        "file",
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        let raw = publish(&handle, &reference(), Image::One, false);
        let healthy = LocalRef::new("healthy:t").unwrap();
        publish(&handle, &healthy, Image::Two, false);
        let metadata_path = raw.join("metadata.json");
        match damage {
            "metadata-missing" => fs::remove_file(&metadata_path).unwrap(),
            "metadata-json" => fs::write(&metadata_path, b"{").unwrap(),
            "metadata-version" | "config-digest-missing" | "identity" => {
                let mut value: Value =
                    serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
                match damage {
                    "metadata-version" => value["version"] = json!(2),
                    "config-digest-missing" => {
                        value["content"]
                            .as_object_mut()
                            .unwrap()
                            .remove("config_digest");
                    }
                    _ => value["stored_root"] = json!(root(Image::Two)),
                }
                fs::write(&metadata_path, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            "layout-version" => {
                fs::write(raw.join("oci-layout"), br#"{"imageLayoutVersion":"9"}"#).unwrap()
            }
            "index-json" => fs::write(raw.join("index.json"), b"[").unwrap(),
            "index-empty" | "index-ambiguous" | "root-type" => {
                let path = raw.join("index.json");
                let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                match damage {
                    "index-empty" => value["manifests"] = json!([]),
                    "index-ambiguous" => {
                        let first = value["manifests"][0].clone();
                        value["manifests"].as_array_mut().unwrap().push(first);
                    }
                    _ => value["manifests"][0]["mediaType"] = json!("unsupported"),
                }
                fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            "blobs-missing" => fs::remove_dir(raw.join("blobs")).unwrap(),
            "pointer" | "file" => {
                let path = handle.layout.ref_entry(&reference());
                fs::remove_file(&path).unwrap();
                if damage == "file" {
                    fs::write(path, b"damaged").unwrap();
                } else {
                    symlink("/unavailable/outside", path).unwrap();
                }
            }
            _ => unreachable!(),
        }
        let before = fs::read(&metadata_path).ok();
        let report = handle.list().unwrap();
        assert!(report.complete && report.has_errors(), "{damage}");
        assert_eq!(report.entries.len(), 2, "{damage}");
        let bad = report
            .entries
            .iter()
            .find(|entry| entry.local_ref.as_ref() == Some(&reference()))
            .unwrap();
        assert_eq!(bad.status, ListStatus::Damaged, "{damage}");
        assert!(
            report
                .entries
                .iter()
                .any(|entry| entry.local_ref.as_ref() == Some(&healthy)
                    && entry.status == ListStatus::Unverified)
        );
        assert!(!report.diagnostics.is_empty());
        if damage.starts_with("metadata") || damage == "config-digest-missing" {
            assert_eq!(bad.stored_root, Some(root(Image::One)));
        }
        if damage.starts_with("index") || damage == "layout-version" || damage == "root-type" {
            assert!(bad.stored_root.is_none() && bad.metadata.is_some());
        }
        assert_eq!(
            fs::read(&metadata_path).ok(),
            before,
            "inventory modified {damage}"
        );
    }
}

#[test]
fn public_inventory_reports_unknown_namespace_and_incomplete_enumeration() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    publish(&handle, &reference(), Image::One, false);
    let unknown = handle.layout.root().join("refs/unknown");
    symlink("/unavailable/outside", &unknown).unwrap();
    let report = handle.list().unwrap();
    assert!(!report.complete && report.has_errors());
    assert!(
        report
            .entries
            .iter()
            .any(|entry| entry.path == unknown && entry.local_ref.is_none())
    );
    fsutil::fail_once("discovery.enumerate", libc::EACCES);
    let report = handle.list().unwrap();
    assert!(!report.complete && report.has_errors());
    assert_eq!(report.diagnostics.len(), 1);
}

#[test]
fn public_remove_preserves_retired_content_and_reports_unlink_durability() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    let raw = publish(&handle, &reference(), Image::One, false);
    let lock = handle.layout.ref_lock(&reference());
    let inode = fs::metadata(&lock).unwrap().ino();
    fsutil::fail_once("ref.unlink", libc::EIO);
    assert!(matches!(handle.remove(&reference()), Err(Error::Io { .. })));
    assert_eq!(handle.list().unwrap().entries.len(), 1);
    fsutil::fail_once("ref.remove-sync", libc::EIO);
    assert!(matches!(
        handle.remove(&reference()),
        Err(Error::Durability {
            change: NamespaceChange::RefRemoved,
            ..
        })
    ));
    assert!(handle.list().unwrap().entries.is_empty());
    assert!(raw.join("metadata.json").is_file());
    assert!(handle.layout.blob(&root(Image::One)).is_file());
    assert_eq!(fs::metadata(&lock).unwrap().ino(), inode);
    assert!(matches!(
        handle.remove(&reference()),
        Err(Error::RefMissing(_))
    ));
}

#[test]
fn public_remove_unlinks_damaged_refs_without_following_them() {
    for kind in ["file", "broken", "escape", "directory", "fifo", "parent"] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        publish(&handle, &reference(), Image::One, false);
        let path = handle.layout.ref_entry(&reference());
        fs::remove_file(&path).unwrap();
        let outside = temporary.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("ref"), b"preserve").unwrap();
        match kind {
            "file" => fs::write(&path, b"bad").unwrap(),
            "broken" => symlink("missing", &path).unwrap(),
            "escape" => symlink(outside.join("ref"), &path).unwrap(),
            "directory" => fs::create_dir(&path).unwrap(),
            "fifo" => {
                let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
                // SAFETY: NUL-terminated path and a valid permission mask.
                assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
            }
            "parent" => {
                fs::remove_dir(path.parent().unwrap()).unwrap();
                symlink(&outside, path.parent().unwrap()).unwrap();
            }
            _ => unreachable!(),
        }
        let result = handle.remove(&reference());
        if matches!(kind, "directory" | "fifo" | "parent") {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap().local_ref, reference());
        }
        assert_eq!(fs::read(outside.join("ref")).unwrap(), b"preserve");
    }
}

#[test]
#[ignore = "subprocess helper"]
fn publication_worker() {
    let root_path = std::env::var_os("HPC_PUBLICATION_ROOT").unwrap();
    let handle = store(Path::new(&root_path));
    let reference = LocalRef::new(std::env::var("HPC_PUBLICATION_REF").unwrap()).unwrap();
    let action = std::env::var("HPC_PUBLICATION_ACTION").unwrap();
    println!("PUBLICATION_STARTED");
    std::io::stdout().flush().unwrap();
    if action == "check-exclusive" {
        let _guard = handle.prepare(Operation::Prune).unwrap();
        println!("PUBLICATION_READY");
        return;
    }
    if action.starts_with("check-") {
        use crate::{CheckScope, CheckStatus, ImageSelector};
        fsutil::pause_once(if action.starts_with("check-scan-") {
            "check.discovered"
        } else {
            "check.selected"
        });
        let scope = if action.starts_with("check-scan-") {
            CheckScope::All
        } else {
            CheckScope::Image(
                ImageSelector::new(format!("{}@{}", reference.name(), root(Image::One))).unwrap(),
            )
        };
        let report = handle.check(&scope).unwrap();
        assert!(!report.has_errors(), "{report:?}");
        if action == "check-scan-remove" {
            assert!(report.entries.is_empty());
        } else {
            assert_eq!(report.entries.len(), 1);
            assert_eq!(report.entries[0].status, CheckStatus::Passed);
            assert_eq!(
                report.entries[0].stored_root,
                Some(root(if action == "check-scan-replace" {
                    Image::Two
                } else {
                    Image::One
                }))
            );
        }
        println!("PUBLICATION_READY");
        return;
    }
    if action.starts_with("inventory-") {
        fsutil::pause_once("inventory.discovered");
        let report = handle.list().unwrap();
        assert!(!report.has_errors());
        if action == "inventory-remove" {
            assert!(report.entries.is_empty());
        } else {
            assert_eq!(report.entries.len(), 1);
            assert_eq!(report.entries[0].stored_root, Some(root(Image::Two)));
            assert_eq!(
                report.entries[0].metadata.as_ref().unwrap().stored_root,
                root(Image::Two)
            );
        }
        println!("PUBLICATION_READY");
        return;
    }
    if action.starts_with("select-") {
        use crate::ImageSelector;
        fsutil::pause_once(if action == "select-held" {
            "selector.selected"
        } else {
            "selector.discovered"
        });
        let guard = handle.prepare(Operation::Export).unwrap();
        let selected = crate::inventory::select(
            &guard,
            &ImageSelector::new(format!("{}@{}", reference.name(), root(Image::One))).unwrap(),
        );
        if action == "select-after-remove" {
            assert!(matches!(selected, Err(Error::DigestMissing(_))));
        } else {
            let selected = selected.unwrap();
            if action == "select-after-replace" {
                assert_eq!(selected.reference().as_str(), "analysis/tool:zzzz");
            }
            assert_eq!(refs::read(&selected).unwrap().root.digest, root(Image::One));
        }
        println!("PUBLICATION_READY");
        return;
    }
    if action == "remove-public" {
        handle.remove(&reference).unwrap();
        println!("PUBLICATION_READY");
        return;
    }
    let guard = handle
        .prepare(if action == "read" {
            Operation::List
        } else {
            Operation::Import
        })
        .unwrap();
    let ref_guard = guard.lock_ref(&reference).unwrap();
    if action == "read" {
        let resolved = refs::read(&ref_guard).unwrap();
        assert_eq!(resolved.root.digest, root(Image::Two));
        assert_eq!(resolved.metadata.stored_root, resolved.root.digest);
        println!("PUBLICATION_READY");
        return;
    }
    let image = if action == "race" {
        Image::One
    } else {
        Image::Two
    };
    let stage = completed(&ref_guard, image);
    println!("PUBLICATION_READY");
    std::io::stdout().flush().unwrap();
    let mut instruction = String::new();
    std::io::stdin().read_line(&mut instruction).unwrap();
    if action == "interrupt" {
        fsutil::pause_once("ref.rename");
    } else if action == "interrupt-after" {
        fsutil::pause_once("ref.destination-sync");
    }
    refs::publish(stage, action != "race").unwrap();
}

#[test]
fn inventory_revalidates_refs_replaced_or_removed_after_enumeration() {
    for action in ["inventory-remove", "inventory-replace"] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        publish(&handle, &reference(), Image::One, false);
        let mut reader = Worker::start(temporary.path(), &reference(), action);
        reader.wait_for("PUBLICATION_PAUSED");
        if action == "inventory-remove" {
            handle.remove(&reference()).unwrap();
        } else {
            publish(&handle, &reference(), Image::Two, true);
        }
        reader.proceed();
        reader.wait_for("PUBLICATION_READY");
        reader.finish();
    }
}

#[test]
fn digest_resolution_revalidates_candidates_after_namespace_scan() {
    for action in ["select-after-remove", "select-after-replace"] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        publish(&handle, &reference(), Image::One, false);
        if action == "select-after-replace" {
            publish(
                &handle,
                &LocalRef::new("analysis/tool:zzzz").unwrap(),
                Image::One,
                false,
            );
        }
        let mut reader = Worker::start(temporary.path(), &reference(), action);
        reader.wait_for("PUBLICATION_PAUSED");
        if action == "select-after-remove" {
            handle.remove(&reference()).unwrap();
        } else {
            publish(&handle, &reference(), Image::Two, true);
        }
        reader.proceed();
        reader.wait_for("PUBLICATION_READY");
        reader.finish();
    }
}

#[test]
fn selected_digest_guard_excludes_replacement_and_removal_until_use_finishes() {
    for action in ["replace", "remove-public"] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        publish(&handle, &reference(), Image::One, false);
        let mut reader = Worker::start(temporary.path(), &reference(), "select-held");
        reader.wait_for("PUBLICATION_PAUSED");
        let mut writer = Worker::start(temporary.path(), &reference(), action);
        assert!(matches!(
            writer.events.recv_timeout(Duration::from_millis(150)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        reader.proceed();
        reader.wait_for("PUBLICATION_READY");
        reader.finish();
        writer.wait_for("PUBLICATION_READY");
        if action == "replace" {
            writer.proceed();
        }
        writer.finish();
        let inventory = handle.list().unwrap();
        if action == "replace" {
            assert_eq!(inventory.entries[0].stored_root, Some(root(Image::Two)));
        } else {
            assert!(inventory.entries.is_empty());
        }
    }
}

struct Worker {
    child: Child,
    events: Receiver<String>,
}
impl Worker {
    fn start(root: &Path, reference: &LocalRef, action: &str) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "publication_tests::publication_worker",
                "--nocapture",
            ])
            .env("HPC_PUBLICATION_ROOT", root)
            .env("HPC_PUBLICATION_REF", reference.as_str())
            .env("HPC_PUBLICATION_ACTION", action)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let output = child.stdout.take().unwrap();
        let (sender, events) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let worker = Self { child, events };
        worker.wait_for("PUBLICATION_STARTED");
        worker
    }
    fn wait_for(&self, event: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let line = self
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("publication worker did not reach expected state");
            if line.contains(event) {
                return;
            }
        }
    }
    fn proceed(&mut self) {
        writeln!(self.child.stdin.as_mut().unwrap()).unwrap();
    }
    fn finish(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            assert!(Instant::now() < deadline, "publication worker did not exit");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn separate_processes_race_to_insert_identical_content() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    drop(handle.prepare(Operation::Import).unwrap());
    let mut workers: Vec<_> = (0..6)
        .map(|i| {
            Worker::start(
                temporary.path(),
                &LocalRef::new(format!("race{i}:tag")).unwrap(),
                "race",
            )
        })
        .collect();
    for worker in &workers {
        worker.wait_for("PUBLICATION_READY");
    }
    for worker in &mut workers {
        worker.proceed();
    }
    for worker in &mut workers {
        worker.finish();
    }
    assert_eq!(fs::read_dir(handle.layout.blobs()).unwrap().count(), 2);
    assert_eq!(
        fs::read(handle.layout.blob(&root(Image::One))).unwrap(),
        manifest(Image::One)
    );
    let guard = handle.prepare(Operation::List).unwrap();
    assert_eq!(discovery::discover(&guard).unwrap().entries.len(), 6);
}

#[test]
fn readers_wait_for_replacement_and_observe_matching_complete_raw_images() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    let old = publish(&handle, &reference(), Image::One, false);
    let mut writer = Worker::start(temporary.path(), &reference(), "replace");
    writer.wait_for("PUBLICATION_READY");
    let mut reader = Worker::start(temporary.path(), &reference(), "read");
    assert!(matches!(
        reader.events.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    let mut second_writer = Worker::start(temporary.path(), &reference(), "replace");
    assert!(matches!(
        second_writer
            .events
            .recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    writer.proceed();
    writer.finish();
    second_writer.wait_for("PUBLICATION_READY");
    second_writer.proceed();
    second_writer.finish();
    reader.wait_for("PUBLICATION_READY");
    reader.finish();
    assert!(old.exists());
    assert_eq!(fs::read_dir(handle.layout.raw_images()).unwrap().count(), 3);
}

#[test]
fn killed_publication_exposes_only_complete_refs_and_retains_abandoned_data() {
    for (action, expected_image) in [("interrupt", Image::One), ("interrupt-after", Image::Two)] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        let old = publish(&handle, &reference(), Image::One, false);
        let mut writer = Worker::start(temporary.path(), &reference(), action);
        writer.wait_for("PUBLICATION_READY");
        writer.proceed();
        writer.wait_for("PUBLICATION_PAUSED");
        writer.child.kill().unwrap();
        writer.child.wait().unwrap();
        let guard = handle.prepare(Operation::List).unwrap();
        let ref_guard = guard.lock_ref(&reference()).unwrap();
        let resolved = refs::read(&ref_guard).unwrap();
        assert_eq!(resolved.root.digest, root(expected_image));
        if action == "interrupt" {
            assert_eq!(resolved.raw_image, old);
        }
        assert!(old.exists());
        assert_eq!(fs::read_dir(handle.layout.raw_images()).unwrap().count(), 2);
        assert_eq!(fs::read_dir(handle.layout.staging()).unwrap().count(), 2);
        assert_eq!(discovery::discover(&guard).unwrap().entries.len(), 1);
    }
}

#[test]
fn unsupported_hard_link_does_not_fall_back_to_copying() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    let guard = handle.prepare(Operation::Import).unwrap();
    let ref_guard = guard.lock_ref(&reference()).unwrap();
    let stage = completed(&ref_guard, Image::One);
    fsutil::fail_once("blob.link", libc::EXDEV);
    assert!(
        matches!(cas::insert(&ref_guard, &stage.blobs()[0]), Err(Error::Io { source, .. }) if source.raw_os_error() == Some(libc::EXDEV))
    );
    assert_eq!(fs::read_dir(handle.layout.blobs()).unwrap().count(), 0);
}

#[test]
fn public_check_covers_indexes_selectors_and_preserves_store_data() {
    use crate::{CheckScope, CheckStatus, ImageSelector};
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    assert!(matches!(
        handle.check(&CheckScope::All),
        Err(Error::MissingStore(_))
    ));
    {
        let _guard = handle.prepare(Operation::Import).unwrap();
    }
    let empty = handle.check(&CheckScope::All).unwrap();
    assert!(empty.entries.is_empty() && !empty.has_errors());
    publish(&handle, &reference(), Image::Index, false);
    let duplicate = LocalRef::new("analysis/tool:zzzz").unwrap();
    publish(&handle, &duplicate, Image::Index, false);
    let unrelated = LocalRef::new("other/tool:release").unwrap();
    publish(&handle, &unrelated, Image::One, false);

    // All these files, directories, symlinks, staging remnants, and existing
    // lock identities must survive checks byte-for-byte and with the same age.
    fn snapshot(path: &Path, result: &mut BTreeMap<PathBuf, (u64, i64, u32, Vec<u8>)>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let bytes = if metadata.file_type().is_symlink() {
            use std::os::unix::ffi::OsStrExt;
            fs::read_link(path).unwrap().as_os_str().as_bytes().to_vec()
        } else if metadata.is_file() {
            fs::read(path).unwrap()
        } else {
            Vec::new()
        };
        result.insert(
            path.to_owned(),
            (metadata.ino(), metadata.mtime(), metadata.mode(), bytes),
        );
        if metadata.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                snapshot(&entry.unwrap().path(), result);
            }
        }
    }
    let mut before = BTreeMap::new();
    snapshot(temporary.path(), &mut before);
    for selector in [
        reference().to_string(),
        format!("analysis/tool@{}", root(Image::Index)),
        format!("analysis/tool:ignored@{}", root(Image::Index)),
    ] {
        let report = handle
            .check(&CheckScope::Image(ImageSelector::new(selector).unwrap()))
            .unwrap();
        assert!(!report.has_errors(), "{report:?}");
        assert_eq!(report.entries[0].local_ref, Some(reference()));
        assert_eq!(report.entries[0].stored_root, Some(root(Image::Index)));
        assert_eq!(report.entries[0].status, CheckStatus::Passed);
    }
    let report = handle.check(&CheckScope::All).unwrap();
    assert!(!report.has_errors() && report.entries.len() == 3);
    let mut after = BTreeMap::new();
    snapshot(temporary.path(), &mut after);
    assert_eq!(before, after);
    assert!(matches!(
        handle.check(&CheckScope::Image(
            ImageSelector::new(format!("analysis/tool@{}", root(Image::One))).unwrap()
        )),
        Err(Error::DigestMissing(_))
    ));
    let missing = handle
        .check(&CheckScope::Image(
            ImageSelector::new("missing:tag").unwrap(),
        ))
        .unwrap();
    assert!(missing.has_errors());
}

#[test]
fn public_check_continues_after_ref_damage_and_reports_graph_despite_bad_metadata() {
    use crate::{CheckScope, CheckStatus, DiagnosticCategory, ImageSelector};
    for damage in [
        "metadata",
        "pointer",
        "layout",
        "schema",
        "root-missing",
        "config-missing",
        "root-size",
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        let raw = publish(&handle, &reference(), Image::One, false);
        let healthy = LocalRef::new("healthy:tag").unwrap();
        publish(&handle, &healthy, Image::Two, false);
        match damage {
            "metadata" => {
                fs::write(raw.join("metadata.json"), b"{").unwrap();
                fs::remove_file(handle.layout.blob(&root(Image::One))).unwrap();
            }
            "pointer" => {
                let path = handle.layout.ref_entry(&reference());
                fs::remove_file(&path).unwrap();
                symlink("/outside/unavailable", path).unwrap();
            }
            "layout" => fs::write(raw.join("index.json"), b"{").unwrap(),
            "schema" => {
                let path = raw.join("metadata.json");
                let mut value: serde_json::Value =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                value["version"] = serde_json::json!(2);
                fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            "root-missing" => fs::remove_file(handle.layout.blob(&root(Image::One))).unwrap(),
            "config-missing" => {
                fs::remove_file(handle.layout.blob(&digest(CONFIG_DIGEST))).unwrap()
            }
            "root-size" => fs::write(handle.layout.blob(&root(Image::One)), b"{}").unwrap(),
            _ => unreachable!(),
        }
        let report = handle.check(&CheckScope::All).unwrap();
        assert!(report.has_errors() && !report.complete, "{damage}");
        assert_eq!(report.entries.len(), 2);
        assert!(
            report
                .entries
                .iter()
                .any(|entry| entry.local_ref.as_ref() == Some(&reference())
                    && entry.status == CheckStatus::Failed)
        );
        // A shared missing config must fail both refs, including a cache hit.
        let expected = if damage == "config-missing" {
            CheckStatus::Failed
        } else {
            CheckStatus::Passed
        };
        assert!(report.entries.iter().any(|entry| entry.local_ref.as_ref() == Some(&healthy) && entry.status == expected));
        if damage == "metadata" {
            assert!(
                report
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.category == DiagnosticCategory::Metadata)
            );
            assert!(
                report
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.object_digest == Some(root(Image::One)))
            );
            let selector =
                ImageSelector::new(format!("analysis/tool@{}", root(Image::One))).unwrap();
            assert!(matches!(
                handle.check(&CheckScope::Image(selector)),
                Err(Error::ResolutionIncomplete { .. })
            ));
        }
        let selector = ImageSelector::new(reference().as_str()).unwrap();
        assert!(
            handle
                .check(&CheckScope::Image(selector))
                .unwrap()
                .has_errors()
        );
    }
}

#[test]
fn public_check_preserves_unknown_namespace_and_reports_incomplete_scans() {
    use crate::{CheckScope, CheckStatus};
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    publish(&handle, &reference(), Image::One, false);
    let unknown = handle.layout.root().join("refs/unknown");
    fs::write(&unknown, b"preserve").unwrap();
    let report = handle.check(&CheckScope::All).unwrap();
    assert!(report.has_errors() && !report.complete);
    assert!(report.entries.iter().any(|entry| entry.local_ref.is_none()));
    assert!(
        report
            .entries
            .iter()
            .any(|entry| entry.status == CheckStatus::Passed)
    );
    assert_eq!(fs::read(unknown).unwrap(), b"preserve");
    fsutil::fail_once("discovery.enumerate", libc::EIO);
    let report = handle.check(&CheckScope::All).unwrap();
    assert!(report.has_errors() && !report.complete);
    assert!(
        report
            .diagnostics
            .iter()
            .any(|error| error.message.contains("enumerate"))
    );
}

#[test]
fn checking_holds_selected_ref_and_store_locks_until_graph_reading_finishes() {
    for action in ["replace", "remove-public", "check-exclusive"] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        publish(&handle, &reference(), Image::One, false);
        let mut checker = Worker::start(temporary.path(), &reference(), "check-held");
        checker.wait_for("PUBLICATION_PAUSED");
        let mut writer = Worker::start(temporary.path(), &reference(), action);
        assert!(matches!(
            writer.events.recv_timeout(Duration::from_millis(150)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        checker.proceed();
        checker.wait_for("PUBLICATION_READY");
        checker.finish();
        writer.wait_for("PUBLICATION_READY");
        if action == "replace" {
            writer.proceed();
        }
        writer.finish();
    }
}

#[test]
fn checking_revalidates_refs_after_namespace_discovery() {
    for action in ["check-scan-remove", "check-scan-replace"] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        publish(&handle, &reference(), Image::One, false);
        let mut checker = Worker::start(temporary.path(), &reference(), action);
        checker.wait_for("PUBLICATION_PAUSED");
        if action == "check-scan-remove" {
            handle.remove(&reference()).unwrap();
        } else {
            publish(&handle, &reference(), Image::Two, true);
        }
        checker.proceed();
        checker.wait_for("PUBLICATION_READY");
        checker.finish();
    }
}
