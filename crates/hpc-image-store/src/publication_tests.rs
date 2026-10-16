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
        2
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
#[ignore = "subprocess helper"]
fn publication_worker() {
    let root_path = std::env::var_os("HPC_PUBLICATION_ROOT").unwrap();
    let handle = store(Path::new(&root_path));
    let reference = LocalRef::new(std::env::var("HPC_PUBLICATION_REF").unwrap()).unwrap();
    let action = std::env::var("HPC_PUBLICATION_ACTION").unwrap();
    println!("PUBLICATION_STARTED");
    std::io::stdout().flush().unwrap();
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
