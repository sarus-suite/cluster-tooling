use super::*;
use crate::{
    LocalRef, Store, StoreConfig,
    locks::{self, Operation},
    mutation::{Kind, Mutation},
    test_snapshot::snapshot,
    test_support::{
        MutationFixture, NestedImages, SharedImages, complete_image, prefix_names, publish_image,
    },
};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::fs::{MetadataExt, symlink},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

fn assert_clean(store: &Store) {
    assert!(!pending(&store.layout).unwrap());
    assert!(!store.config().root().join("pending-mutation.json").exists());
    assert!(
        !store
            .config()
            .root()
            .join(".pending-mutation-next.json")
            .exists()
    );
}
fn published_fixture(point: Option<&'static str>) -> MutationFixture {
    let fixture = MutationFixture::new();
    let guard = fixture.store.prepare(Operation::Import).unwrap();
    let mut mutation = Mutation::begin(&guard, &fixture.target, Kind::Import).unwrap();
    let stage = complete_image(
        mutation.stage().unwrap(),
        &fixture.target,
        &fixture.images.replacement,
    );
    if let Some(point) = point {
        fsutil::fail_once(point, libc::EIO);
    }
    let result = mutation.publish(stage);
    if point.is_some() {
        assert!(result.is_err());
    } else {
        result.unwrap();
    }
    drop(mutation);
    drop(guard);
    fixture
}

#[test]
fn already_published_unsupported_declarations_block_actions_without_panicking_or_deleting() {
    let fixture = MutationFixture::new();
    let unsupported = fixture.images.replacement.with_manifest_change(|manifest| {
        manifest["subject"] = serde_json::to_value(&fixture.images.original.root).unwrap();
    });
    // Recreate an operation published by the old acquisition path. The
    // internal fixture publication deliberately bypasses acquired_content.
    let guard = fixture.store.prepare(Operation::Import).unwrap();
    let mut mutation = Mutation::begin(&guard, &fixture.target, Kind::Import).unwrap();
    let stage = complete_image(mutation.stage().unwrap(), &fixture.target, &unsupported);
    mutation.publish(stage).unwrap();
    drop(mutation);
    drop(guard);
    let before = snapshot(fixture.store.config().root());
    for _ in 0..2 {
        let list_error = fixture.store.list().unwrap_err();
        let remove_error = fixture.store.remove(&fixture.target).unwrap_err();
        for error in [list_error, remove_error] {
            assert!(
                matches!(&error, Error::Recovery {
                phase: "intent", change: Some(crate::NamespaceChange::RefPublished), source, ..
            } if matches!(source.as_ref(), Error::InvalidObject { path, reason }
                if path == &fixture.store.layout.blob(&unsupported.root.digest) && reason.contains("subject"))),
                "{error}"
            );
        }
        assert_eq!(snapshot(fixture.store.config().root()), before);
    }
}

#[test]
fn ownership_is_recorded_before_staging_and_shared_objects_remain_immutable() {
    let fixture = MutationFixture::new();
    let shared = fixture.images.shared.first().unwrap();
    let before = snapshot(&fixture.store.layout.blob(shared));
    let guard = fixture.store.prepare(Operation::Import).unwrap();
    let mutation = Mutation::begin(&guard, &fixture.target, Kind::Pull).unwrap();
    assert!(
        fixture
            .store
            .config()
            .root()
            .join("pending-mutation.json")
            .is_file()
    );
    assert!(
        !fixture
            .store
            .config()
            .root()
            .join(".pending-mutation-next.json")
            .exists()
    );
    assert!(Mutation::begin(&guard, &fixture.keeper, Kind::Import).is_err());
    let record = load(&fixture.store.layout).unwrap().unwrap();
    assert_eq!(record.phase, Phase::Acquiring);
    let staging = fixture.store.layout.staging().join(record.staging.unwrap());
    assert!(!staging.exists());
    let stage = mutation.stage().unwrap();
    assert!(staging.is_dir());
    drop(stage);
    mutation.finish().unwrap();
    assert!(!staging.exists());
    assert_eq!(snapshot(&fixture.store.layout.blob(shared)), before);
    assert_clean(&fixture.store);
}

#[test]
fn recorded_skopeo_temporaries_recover_and_resume_after_deletion_or_sync_failure() {
    for point in [None, Some("plan"), Some("removed"), Some("sync")] {
        let fixture = MutationFixture::new();
        let original = snapshot(&fixture.original_raw);
        let keeper = snapshot(&fixture.keeper_raw);
        let blobs = snapshot(&fixture.store.layout.blobs());
        let lock = snapshot(&fixture.store.layout.store_lock());
        let guard = fixture.store.prepare(Operation::Import).unwrap();
        let mutation = Mutation::begin(&guard, &fixture.target, Kind::Import).unwrap();
        let stage = mutation.stage().unwrap();
        let temporary = stage.layout_path().join("oci-put-blob123456789");
        fs::write(stage.layout_path().join("oci-put-blob0"), b"").unwrap();
        fs::write(&temporary, b"partial producer bytes").unwrap();
        let directory = stage.layout_path().parent().unwrap().to_owned();
        // Similar files outside this operation's recorded tree are not adopted.
        let unrecorded = fixture.store.layout.staging().join("operation-unrecorded");
        fs::create_dir_all(unrecorded.join("layout")).unwrap();
        fs::write(unrecorded.join("layout/oci-put-blob123"), b"preserve").unwrap();
        let unrecorded_before = snapshot(&unrecorded);
        drop(stage);
        drop(mutation);
        drop(guard);

        if let Some(point) = point {
            fsutil::fail_once("recovery.cleaning-durable", libc::EIO);
            assert!(fixture.store.list().is_err());
            // Reloading also validates the temporary-file entries in a saved
            // cleanup plan, not just the original staging tree.
            let record = load(&fixture.store.layout).unwrap().unwrap();
            assert_eq!(record.phase, Phase::Cleaning);
            let position = record.cleanup.unwrap().entries.iter().position(|entry| {
                matches!(&entry.target, Target::Staging { member } if member == "layout/oci-put-blob123456789")
            }).unwrap();
            if point != "plan" {
                let checkpoint = if point == "removed" {
                    "recovery.removed"
                } else {
                    "recovery.cleanup-sync"
                };
                fsutil::fail_after(checkpoint, position, libc::EIO);
                let result = fixture.store.list();
                fsutil::clear_failure();
                assert!(result.is_err());
                assert!(!temporary.exists());
                assert!(pending(&fixture.store.layout).unwrap());
            }
        }
        assert!(!fixture.store.list().unwrap().has_errors());
        assert!(!fixture.store.list().unwrap().has_errors());
        assert!(!directory.exists());
        assert_clean(&fixture.store);
        assert_eq!(snapshot(&fixture.original_raw), original);
        assert_eq!(snapshot(&fixture.keeper_raw), keeper);
        assert_eq!(snapshot(&fixture.store.layout.blobs()), blobs);
        assert_eq!(snapshot(&fixture.store.layout.store_lock()), lock);
        assert_eq!(snapshot(&unrecorded), unrecorded_before);
    }
}

#[test]
fn unknown_or_redirected_producer_temporaries_block_recorded_cleanup() {
    for case in [
        "empty-suffix",
        "non-digit",
        "unicode-digit",
        "signed",
        "extra-suffix",
        "wrong-parent",
        "nested",
        "raw",
        "symlink",
        "directory",
        "fifo",
        "unrelated-sibling",
        "redirected-layout",
    ] {
        let fixture = published_fixture(Some("ref.rename"));
        let record = load(&fixture.store.layout).unwrap().unwrap();
        let stage = fixture.store.layout.staging().join(record.staging.unwrap());
        let layout = stage.join("layout");
        let recognized = layout.join("oci-put-blob100");
        fs::write(&recognized, b"known producer file").unwrap();
        let outside = fixture.directory.path().join("protected");
        fs::write(&outside, b"preserve outside contents").unwrap();
        let path = match case {
            "empty-suffix" => layout.join("oci-put-blob"),
            "non-digit" => layout.join("oci-put-blobabc"),
            "unicode-digit" => layout.join("oci-put-blob١"),
            "signed" => layout.join("oci-put-blob+123"),
            "extra-suffix" => layout.join("oci-put-blob123.tmp"),
            "wrong-parent" => stage.join("oci-put-blob123"),
            "nested" => layout.join("blobs/oci-put-blob123"),
            "raw" => fixture
                .store
                .layout
                .raw_image(record.new_raw.as_ref().unwrap())
                .join("oci-put-blob123"),
            "unrelated-sibling" => layout.join("unrelated"),
            "redirected-layout" => layout.clone(),
            _ => layout.join("oci-put-blob123"),
        };
        match case {
            "symlink" => symlink(&outside, &path).unwrap(),
            "directory" => {
                fs::create_dir(&path).unwrap();
                fs::write(path.join("sentinel"), b"preserve directory").unwrap();
            }
            "fifo" => {
                use std::os::unix::ffi::OsStrExt;
                let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                // SAFETY: name is NUL-terminated and the mode is valid.
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            }
            "redirected-layout" => {
                let saved = fixture.directory.path().join("saved-layout");
                fs::rename(&layout, &saved).unwrap();
                symlink(saved, &layout).unwrap();
            }
            _ => fs::write(&path, b"unknown contents").unwrap(),
        }
        let bad = fs::symlink_metadata(&path).unwrap();
        // The snapshot helper intentionally cannot read FIFOs. Compare its
        // identity/type separately, and snapshot every protected regular tree.
        let entry_before = (case != "fifo").then(|| snapshot(&path));
        let known_before = snapshot(&recognized);
        let raw_before = snapshot(&fixture.store.layout.raw_images());
        let blobs_before = snapshot(&fixture.store.layout.blobs());
        let record_before = snapshot(&fixture.store.layout.pending_mutation());
        assert!(fixture.store.list().is_err(), "{case}");
        assert!(fixture.store.list().is_err(), "{case}");
        assert_eq!(
            fs::symlink_metadata(&path).unwrap().ino(),
            bad.ino(),
            "{case}"
        );
        assert_eq!(
            fs::symlink_metadata(&path).unwrap().mode(),
            bad.mode(),
            "{case}"
        );
        assert_eq!(
            (case != "fifo").then(|| snapshot(&path)),
            entry_before,
            "{case}"
        );
        assert_eq!(snapshot(&recognized), known_before, "{case}");
        assert_eq!(
            snapshot(&fixture.store.layout.raw_images()),
            raw_before,
            "{case}"
        );
        assert_eq!(
            snapshot(&fixture.store.layout.blobs()),
            blobs_before,
            "{case}"
        );
        assert_eq!(
            snapshot(&fixture.store.layout.pending_mutation()),
            record_before,
            "{case}"
        );
        assert_eq!(fs::read(&outside).unwrap(), b"preserve outside contents");
    }
}

#[test]
fn aborted_publication_recovers_owned_additions_and_preserves_legacy_remnants() {
    for point in [
        "mutation.intent-durable",
        "blob.directory-sync",
        "raw_image.file-sync",
        "ref.rename",
    ] {
        let fixture = published_fixture(Some(point));
        let old = snapshot(&fixture.original_raw);
        let keeper = snapshot(&fixture.keeper_raw);
        let record = load(&fixture.store.layout).unwrap().unwrap();
        assert_eq!(record.reused.len(), 1);
        assert_eq!(record.owned.len(), fixture.images.replacement_only.len());
        let stage = fixture.store.layout.staging().join(record.staging.unwrap());
        let new_raw = fixture
            .store
            .layout
            .raw_image(record.new_raw.as_ref().unwrap());
        let lock_inode = fs::metadata(fixture.store.layout.store_lock())
            .unwrap()
            .ino();
        drop(fixture.store.prepare(Operation::List).unwrap());
        drop(fixture.store.prepare(Operation::List).unwrap());
        assert!(!stage.exists());
        assert!(!new_raw.exists());
        for digest in &fixture.images.replacement_only {
            assert!(!fixture.store.layout.blob(digest).exists());
        }
        assert_eq!(snapshot(&fixture.original_raw), old);
        assert_eq!(snapshot(&fixture.keeper_raw), keeper);
        assert_eq!(
            fs::read_dir(fixture.store.layout.staging())
                .unwrap()
                .count(),
            2
        );
        assert_eq!(
            fs::metadata(fixture.store.layout.store_lock())
                .unwrap()
                .ino(),
            lock_inode
        );
        assert_clean(&fixture.store);
    }
}

#[test]
fn visible_replacement_recovers_forward_without_a_completion_update() {
    for point in [None, Some("ref.destination-sync"), Some("ref.source-sync")] {
        let fixture = published_fixture(point);
        let keeper = snapshot(&fixture.keeper_raw);
        let record = load(&fixture.store.layout).unwrap().unwrap();
        assert_eq!(record.phase, Phase::Intent);
        let guard = fixture.store.prepare(Operation::List).unwrap();
        assert_eq!(
            refs::read_small(&guard, &fixture.target)
                .unwrap()
                .root
                .digest,
            fixture.images.replacement.root.digest
        );
        assert!(!fixture.original_raw.exists());
        for digest in &fixture.images.original_only {
            assert!(!fixture.store.layout.blob(digest).exists());
        }
        for digest in fixture.images.replacement.digests() {
            assert!(fixture.store.layout.blob(&digest).exists());
        }
        assert_eq!(snapshot(&fixture.keeper_raw), keeper);
        assert_clean(&fixture.store);
    }
}

#[test]
fn removal_reclaims_only_unused_content_and_preserves_longer_names() {
    let images = SharedImages::new();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(StoreConfig::new(dir.path().join("store")).unwrap());
    let (short, long) = prefix_names();
    let raw = publish_image(&store, &short, &images.original, false);
    let keeper = publish_image(&store, &long, &images.keeper, false);
    let before = snapshot(&keeper);
    let guard = store.prepare(Operation::Remove).unwrap();
    let mut mutation = Mutation::begin(&guard, &short, Kind::Remove).unwrap();
    fsutil::fail_once("ref.remove-sync", libc::EIO);
    assert!(mutation.remove().is_err());
    mutation.finish().unwrap();
    assert!(!raw.exists());
    assert_eq!(refs::pointer_identity(&store.layout, &short).unwrap(), None);
    assert_eq!(snapshot(&keeper), before);
    assert!(store.layout.ref_entry(&short).parent().unwrap().is_dir());
    drop(guard);
    let guard = store.prepare(Operation::Remove).unwrap();
    let mut mutation = Mutation::begin(&guard, &long, Kind::Remove).unwrap();
    mutation.remove().unwrap();
    mutation.finish().unwrap();
    assert!(!keeper.exists());
    assert!(!store.layout.ref_entry(&short).parent().unwrap().exists());
    assert_eq!(fs::read_dir(store.layout.blobs()).unwrap().count(), 0);
    assert_clean(&store);
}

#[test]
fn namespace_sync_failure_blocks_all_reclamation_and_keeps_the_visible_ref() {
    let fixture = published_fixture(None);
    let raw = snapshot(&fixture.store.layout.raw_images());
    let blobs = snapshot(&fixture.store.layout.blobs());
    let stage = snapshot(&fixture.store.layout.staging());
    fsutil::fail_once("recovery.namespace-sync", libc::EIO);
    let error = fixture.store.prepare(Operation::List).err().unwrap();
    assert!(matches!(
        error,
        Error::Recovery {
            change: Some(crate::NamespaceChange::RefPublished),
            ..
        }
    ));
    assert_eq!(snapshot(&fixture.store.layout.raw_images()), raw);
    assert_eq!(snapshot(&fixture.store.layout.blobs()), blobs);
    assert_eq!(snapshot(&fixture.store.layout.staging()), stage);
    assert_eq!(
        refs::pointer_identity(&fixture.store.layout, &fixture.target).unwrap(),
        load(&fixture.store.layout).unwrap().unwrap().new_raw
    );
    drop(fixture.store.prepare(Operation::List).unwrap());
    assert_clean(&fixture.store);
}

#[test]
fn cleanup_retries_after_each_removal_and_failed_sync_without_retired_manifests() {
    let count = {
        let fixture = published_fixture(None);
        fsutil::fail_once("recovery.cleaning-durable", libc::EIO);
        assert!(fixture.store.prepare(Operation::List).is_err());
        load(&fixture.store.layout)
            .unwrap()
            .unwrap()
            .cleanup
            .unwrap()
            .entries
            .len()
    };
    // Exercise every planned removal without assuming fixture tree sizes.
    for remaining in 0..count {
        let fixture = published_fixture(None);
        fsutil::fail_after("recovery.removed", remaining, libc::EIO);
        let result = fixture.store.prepare(Operation::List);
        fsutil::clear_failure();
        if result.is_err() {
            assert_eq!(
                load(&fixture.store.layout).unwrap().unwrap().phase,
                Phase::Cleaning
            );
        }
        drop(result);
        drop(fixture.store.prepare(Operation::List).unwrap());
        assert!(!fixture.original_raw.exists());
        assert_clean(&fixture.store);
    }
    for point in [
        "recovery.cleanup-sync",
        "recovery.cleanup-durable",
        "recovery.complete-durable",
        "recovery.record-remove",
        "recovery.record-removed",
        "recovery.record-remove-sync",
    ] {
        let fixture = published_fixture(None);
        fsutil::fail_once(point, libc::EIO);
        let error = fixture.store.prepare(Operation::List).err().unwrap();
        assert!(matches!(error, Error::Recovery { .. }), "{point}: {error}");
        drop(fixture.store.prepare(Operation::List).unwrap());
        assert_clean(&fixture.store);
    }
}

#[test]
fn failed_snapshots_never_authorize_unrecorded_live_mutations() {
    for point in [
        "mutation.snapshot-created",
        "mutation.snapshot-write",
        "mutation.snapshot-file-sync",
        "mutation.snapshot-parent-sync",
        "mutation.snapshot-written",
        "mutation.snapshot-synced",
        "mutation.snapshot-renamed",
        "mutation.snapshot-durable",
    ] {
        let fixture = MutationFixture::new();
        let before = snapshot(&fixture.original_raw);
        let guard = fixture.store.prepare(Operation::Import).unwrap();
        fsutil::fail_once(point, libc::EIO);
        assert!(Mutation::begin(&guard, &fixture.target, Kind::Import).is_err());
        let root = fixture.store.config().root();
        if matches!(
            point,
            "mutation.snapshot-parent-sync"
                | "mutation.snapshot-renamed"
                | "mutation.snapshot-durable"
        ) {
            assert!(root.join("pending-mutation.json").is_file());
            assert!(!root.join(".pending-mutation-next.json").exists());
        } else {
            assert!(!root.join("pending-mutation.json").exists());
            assert!(root.join(".pending-mutation-next.json").is_file());
        }
        recover(&guard).unwrap();
        assert_clean(&fixture.store);
        assert_eq!(snapshot(&fixture.original_raw), before);
        let mut mutation = Mutation::begin(&guard, &fixture.target, Kind::Import).unwrap();
        let stage = complete_image(
            mutation.stage().unwrap(),
            &fixture.target,
            &fixture.images.replacement,
        );
        fsutil::fail_once(point, libc::EIO);
        assert!(mutation.publish(stage).is_err());
        mutation.finish().unwrap();
        assert_eq!(snapshot(&fixture.original_raw), before);
        assert_clean(&fixture.store);
    }
}

#[test]
fn malformed_records_and_reserved_entries_are_preserved() {
    let fixture = MutationFixture::new();
    let guard = fixture.store.prepare(Operation::Import).unwrap();
    let mutation = Mutation::begin(&guard, &fixture.target, Kind::Import).unwrap();
    let record = load(&fixture.store.layout).unwrap().unwrap();
    drop(mutation);
    let valid = serde_json::to_value(record).unwrap();
    let mut cases = vec![serde_json::json!({}), serde_json::json!({"version":99})];
    for (field, value) in [
        ("version", serde_json::json!(99)),
        ("id", serde_json::json!("../escape")),
        ("staging", serde_json::json!("../outside")),
        ("old_raw", serde_json::json!("raw-../escape")),
        ("new_raw", serde_json::json!("raw-123456789012")),
        ("unexpected", serde_json::json!(true)),
    ] {
        let mut next = valid.clone();
        next[field] = value;
        cases.push(next);
    }
    for value in cases {
        fs::write(
            fixture.store.layout.pending_mutation(),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        fs::write(
            fixture.store.layout.pending_mutation_next(),
            b"partial next record",
        )
        .unwrap();
        let before = snapshot(fixture.store.layout.root());
        assert!(recover(&guard).is_err());
        assert_eq!(snapshot(fixture.store.layout.root()), before);
    }
    fs::write(fixture.store.layout.pending_mutation(), b"{broken").unwrap();
    assert!(recover(&guard).is_err());
    fs::remove_file(fixture.store.layout.pending_mutation()).unwrap();
    fs::remove_file(fixture.store.layout.pending_mutation_next()).unwrap();
    symlink(
        fixture.store.layout.store_lock(),
        fixture.store.layout.pending_mutation_next(),
    )
    .unwrap();
    let before = snapshot(fixture.store.layout.root());
    assert!(recover(&guard).is_err());
    assert_eq!(snapshot(fixture.store.layout.root()), before);
}

#[test]
fn inconsistent_namespace_unknown_tree_and_escaping_pointer_block_cleanup() {
    for case in [
        "ref",
        "tree",
        "parent",
        "pointer",
        "blob",
        "raw-root",
        "stage-root",
    ] {
        let fixture = published_fixture(Some("ref.rename"));
        let record = load(&fixture.store.layout).unwrap().unwrap();
        let stage = fixture
            .store
            .layout
            .staging()
            .join(record.staging.as_ref().unwrap());
        match case {
            "ref" => {
                fs::remove_file(fixture.store.layout.ref_entry(&fixture.target)).unwrap();
                symlink(
                    "../../outside",
                    fixture.store.layout.ref_entry(&fixture.target),
                )
                .unwrap();
            }
            "tree" => fs::write(stage.join("unexpected"), b"preserve").unwrap(),
            "parent" => {
                fs::remove_dir(stage.join("layout/blobs")).unwrap();
                symlink(&fixture.keeper_raw, stage.join("layout/blobs")).unwrap();
            }
            "pointer" => {
                fs::remove_file(stage.join("ref-pointer")).unwrap();
                symlink("/outside", stage.join("ref-pointer")).unwrap();
            }
            "blob" => {
                let path = fixture.store.layout.blob(&record.owned[0].digest);
                fs::remove_file(&path).unwrap();
                symlink(&fixture.keeper_raw, path).unwrap();
            }
            "raw-root" => {
                let raw = fixture
                    .store
                    .layout
                    .raw_image(record.new_raw.as_ref().unwrap());
                fs::rename(&raw, fixture.directory.path().join("saved-raw")).unwrap();
                symlink(&fixture.keeper_raw, raw).unwrap();
            }
            "stage-root" => {
                fs::rename(&stage, fixture.directory.path().join("saved-stage")).unwrap();
                symlink(&fixture.keeper_raw, stage).unwrap();
            }
            _ => unreachable!(),
        }
        let before = snapshot(fixture.store.layout.root());
        assert!(fixture.store.prepare(Operation::List).is_err(), "{case}");
        assert_eq!(snapshot(fixture.store.layout.root()), before, "{case}");
    }
}

#[test]
fn cleanup_snapshot_rejects_escaping_unauthorized_and_omitted_entries() {
    for case in ["escape", "owned", "omitted", "order"] {
        let fixture = published_fixture(None);
        fsutil::fail_once("recovery.cleaning-durable", libc::EIO);
        assert!(fixture.store.prepare(Operation::List).is_err());
        let mut record = load(&fixture.store.layout).unwrap().unwrap();
        let entries = &mut record.cleanup.as_mut().unwrap().entries;
        match case {
            "escape" => {
                entries[0].target = Target::Staging {
                    member: "../../store.json".into(),
                }
            }
            "owned" => {
                entries[0].target = Target::Raw {
                    id: fixture
                        .keeper_raw
                        .file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .into(),
                    member: "".into(),
                }
            }
            "omitted" => entries.clear(),
            "order" => entries.reverse(),
            _ => unreachable!(),
        }
        fs::write(
            fixture.store.layout.pending_mutation(),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        let before = snapshot(fixture.store.layout.root());
        assert!(fixture.store.prepare(Operation::List).is_err(), "{case}");
        assert_eq!(snapshot(fixture.store.layout.root()), before, "{case}");
    }
}

#[test]
fn nested_reachability_preserves_all_live_branches_without_opening_terminals_or_metadata() {
    let fixture = MutationFixture::new();
    let nested = NestedImages::new();
    let top = LocalRef::new("nested:top").unwrap();
    let first = LocalRef::new("nested:first").unwrap();
    let raw = publish_image(&fixture.store, &top, &nested.top, false);
    let first_raw = publish_image(&fixture.store, &first, &nested.first, false);
    let before = snapshot(&first_raw);
    // Display metadata is not reachability authority.
    fs::write(raw.join("metadata.json"), b"not metadata").unwrap();
    let guard = fixture.store.prepare(Operation::Remove).unwrap();
    fsutil::track_opens();
    fsutil::fail_persistently("graph.observe", libc::EIO);
    let mut mutation = Mutation::begin(&guard, &top, Kind::Remove).unwrap();
    mutation.remove().unwrap();
    mutation.finish().unwrap();
    fsutil::clear_failure();
    let opened = fsutil::take_opens();
    for image in [
        &nested.top,
        &fixture.images.original,
        &fixture.images.keeper,
    ] {
        let manifest: serde_json::Value =
            serde_json::from_slice(&image.objects[&image.root.digest]).unwrap();
        if let Some(config) = manifest.get("config") {
            let digest = Sha256Digest::new(config["digest"].as_str().unwrap()).unwrap();
            assert!(!opened.contains(&fixture.store.layout.blob(&digest)));
        }
    }
    for digest in [&nested.config.digest, &nested.layer.digest] {
        assert!(!opened.contains(&fixture.store.layout.blob(digest)));
    }
    assert!(
        !opened
            .iter()
            .any(|path| path.file_name().unwrap() == "metadata.json")
    );
    for digest in nested.first.digests() {
        assert!(fixture.store.layout.blob(&digest).exists());
    }
    for digest in [
        &nested.top.root.digest,
        &nested.nested.digest,
        &nested.second.digest,
    ] {
        assert!(!fixture.store.layout.blob(digest).exists());
    }
    assert_eq!(snapshot(&first_raw), before);
    assert_clean(&fixture.store);
}

#[test]
fn unreadable_or_unsupported_live_dependencies_block_reclamation() {
    for damage in ["missing", "unsupported", "conflict", "discovery"] {
        let fixture = published_fixture(None);
        let path = fixture
            .store
            .layout
            .blob(&fixture.images.keeper.root.digest);
        if damage == "missing" {
            fs::remove_file(&path).unwrap();
        }
        if damage == "unsupported" {
            fs::write(&path, br#"{"schemaVersion":3}"#).unwrap();
        }
        if damage == "conflict" {
            let mut manifest: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            manifest["layers"][0]["size"] = serde_json::json!(999);
            fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        }
        let before = snapshot(fixture.store.layout.root());
        if damage == "discovery" {
            fsutil::fail_once("discovery.enumerate", libc::EIO);
        }
        assert!(fixture.store.prepare(Operation::List).is_err(), "{damage}");
        assert_eq!(snapshot(fixture.store.layout.root()), before, "{damage}");
    }
}

const BOUNDARIES: &[&str] = &[
    "mutation.snapshot-created",
    "mutation.snapshot-write",
    "mutation.snapshot-file-sync",
    "mutation.snapshot-parent-sync",
    "mutation.snapshot-written",
    "mutation.snapshot-synced",
    "mutation.snapshot-renamed",
    "mutation.snapshot-durable",
    "mutation.before-staging",
    "mutation.intent-durable",
    "blob.source-sync",
    "blob.link",
    "blob.target-sync",
    "blob.directory-sync",
    "raw_image.file-sync",
    "raw_image.directory-sync",
    "ref.rename",
    "ref.destination-sync",
    "ref.source-sync",
    "ref.remove-sync",
    "recovery.record-sync",
    "recovery.namespace-sync",
    "recovery.cleaning-durable",
    "recovery.remove",
    "recovery.removed",
    "recovery.cleanup-sync",
    "recovery.cleanup-durable",
    "recovery.complete-durable",
    "recovery.record-remove",
    "recovery.record-removed",
    "recovery.record-remove-sync",
    "recovery.reader-released",
    "recovery.reader-retry",
];
fn handshake(event: &str) {
    println!("{event}");
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
}
#[test]
#[ignore = "subprocess helper invoked by recovery tests"]
fn recovery_worker() {
    let root = std::env::var_os("HPC_RECOVERY_ROOT").unwrap();
    let store = Store::new(StoreConfig::new(root).unwrap());
    let reference = LocalRef::new("analysis/tool:release").unwrap();
    let action = std::env::var("HPC_RECOVERY_ACTION").unwrap();
    let point = std::env::var("HPC_RECOVERY_POINT").unwrap_or_default();
    if let Some(point) = BOUNDARIES.iter().find(|candidate| **candidate == point) {
        fsutil::pause_once(point);
    }
    match action.as_str() {
        "publish" | "remove" => {
            let guard = store.prepare(Operation::Import).unwrap();
            let kind = if action == "remove" {
                Kind::Remove
            } else {
                Kind::Import
            };
            let mut mutation = Mutation::begin(&guard, &reference, kind).unwrap();
            if kind == Kind::Remove {
                mutation.remove().unwrap();
            } else {
                let images = SharedImages::new();
                let stage =
                    complete_image(mutation.stage().unwrap(), &reference, &images.replacement);
                mutation.publish(stage).unwrap();
            }
            mutation.finish().unwrap();
        }
        "recover" | "reader" => {
            let _guard = store.prepare(Operation::List).unwrap();
            if action == "reader" {
                handshake("READER_READY");
            }
        }
        "reader-invalid-store" => {
            assert!(matches!(
                store.prepare(Operation::List),
                Err(Error::UnsupportedVersion {
                    kind: "store",
                    version: 99
                })
            ));
            handshake("READER_REJECTED");
        }
        "orphan" => {
            let guard = store.prepare(Operation::Import).unwrap();
            let mutation = Mutation::begin(&guard, &reference, Kind::Import).unwrap();
            let _stage = mutation.stage().unwrap();
            let script = r#"
import os, pathlib, socket, sys
listener = socket.socket(socket.AF_UNIX)
listener.bind(sys.argv[1])
listener.listen(1)
pathlib.Path(sys.argv[2]).write_text(str(os.getpid()))
connection, address = listener.accept()
connection.close()
listener.close()
"#;
            crate::subprocess::run(
                Command::new("python3")
                    .args(["-c", script])
                    .arg(std::env::var_os("HPC_RECOVERY_SOCKET").unwrap())
                    .arg(std::env::var_os("HPC_RECOVERY_READY").unwrap()),
                "controlled recorded acquisition",
                Path::new("python3"),
                Some(guard.transfer_fd()),
            )
            .unwrap();
        }
        _ => panic!("unknown recovery worker action"),
    }
}
struct Worker {
    child: Child,
    events: Receiver<String>,
}
impl Worker {
    fn start(root: &Path, action: &str, point: &str, socket: Option<(&Path, &Path)>) -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--ignored",
                "--exact",
                "recovery::tests::recovery_worker",
                "--nocapture",
            ])
            .env("HPC_RECOVERY_ROOT", root)
            .env("HPC_RECOVERY_ACTION", action)
            .env("HPC_RECOVERY_POINT", point)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        if let Some((socket, ready)) = socket {
            command
                .env("HPC_RECOVERY_SOCKET", socket)
                .env("HPC_RECOVERY_READY", ready);
        }
        let mut child = command.spawn().unwrap();
        let output = child.stdout.take().unwrap();
        let (sender, events) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        Self { child, events }
    }
    fn wait_for(&self, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let line = self
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("worker did not reach readiness checkpoint");
            if line.contains(expected) {
                return;
            }
        }
    }
    fn proceed(&mut self) {
        writeln!(self.child.stdin.as_mut().unwrap()).unwrap();
    }
    fn terminate(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
    fn finish(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            assert!(Instant::now() < deadline, "worker exit timed out");
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
fn termination_at_record_publication_and_cleanup_boundaries_is_recoverable() {
    for point in BOUNDARIES
        .iter()
        .filter(|point| !point.starts_with("recovery.reader-") && **point != "ref.remove-sync")
    {
        let fixture = MutationFixture::new();
        let mut worker = Worker::start(fixture.store.layout.root(), "publish", point, None);
        worker.wait_for("PUBLICATION_PAUSED");
        worker.terminate();
        let before = refs::pointer_identity(&fixture.store.layout, &fixture.target).unwrap();
        drop(fixture.store.prepare(Operation::List).unwrap());
        assert_eq!(
            refs::pointer_identity(&fixture.store.layout, &fixture.target).unwrap(),
            before,
            "{point}"
        );
        assert_clean(&fixture.store);
        assert!(fixture.keeper_raw.exists());
    }
    let fixture = MutationFixture::new();
    let mut worker = Worker::start(
        fixture.store.layout.root(),
        "remove",
        "ref.remove-sync",
        None,
    );
    worker.wait_for("PUBLICATION_PAUSED");
    worker.terminate();
    drop(fixture.store.prepare(Operation::List).unwrap());
    assert_eq!(
        refs::pointer_identity(&fixture.store.layout, &fixture.target).unwrap(),
        None
    );
    assert!(!fixture.original_raw.exists());
    assert_clean(&fixture.store);
}

#[test]
fn recovery_itself_can_be_terminated_and_repeated() {
    for point in [
        "recovery.cleaning-durable",
        "recovery.removed",
        "recovery.cleanup-sync",
        "recovery.complete-durable",
        "recovery.record-removed",
    ] {
        let fixture = published_fixture(None);
        let mut worker = Worker::start(fixture.store.layout.root(), "recover", point, None);
        worker.wait_for("PUBLICATION_PAUSED");
        worker.terminate();
        drop(fixture.store.prepare(Operation::List).unwrap());
        assert_clean(&fixture.store);
        assert!(!fixture.original_raw.exists());
    }
}

#[test]
fn readers_recheck_after_releasing_shared_and_after_recovery() {
    for point in ["recovery.reader-released", "recovery.reader-retry"] {
        for action in ["reader", "reader-invalid-store"] {
            let fixture = published_fixture(None);
            let mut reader = Worker::start(fixture.store.layout.root(), action, point, None);
            reader.wait_for("PUBLICATION_PAUSED");
            // The reader holds neither shared nor exclusive access at these points.
            // Another action can recover first, and can leave a new record to find.
            let guard = fixture.store.prepare(Operation::Import).unwrap();
            let mutation = Mutation::begin(&guard, &fixture.target, Kind::Import).unwrap();
            let stage = mutation.stage().unwrap();
            drop(stage);
            drop(mutation);
            if action == "reader-invalid-store" {
                fs::write(fixture.store.layout.marker(), br#"{"version":99}"#).unwrap();
            }
            drop(guard);
            let before = snapshot(fixture.store.layout.root());
            reader.proceed();
            if action == "reader-invalid-store" {
                reader.wait_for("READER_REJECTED");
                assert_eq!(snapshot(fixture.store.layout.root()), before);
            } else {
                reader.wait_for("READER_READY");
                assert_clean(&fixture.store);
                let shared = fixture.store.prepare(Operation::List).unwrap();
                let _short = locks::short_test_waits();
                assert!(matches!(
                    fixture.store.prepare(Operation::Remove),
                    Err(Error::LockTimeout { .. })
                ));
                drop(shared);
            }
            reader.proceed();
            reader.finish();
        }
    }
}

#[test]
fn a_surviving_transfer_prevents_recovery_from_removing_recorded_staging() {
    let fixture = MutationFixture::new();
    let control = tempfile::tempdir().unwrap();
    let socket = control.path().join("release.sock");
    let ready = control.path().join("ready");
    let mut worker = Worker::start(
        fixture.store.layout.root(),
        "orphan",
        "",
        Some((&socket, &ready)),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "transfer readiness timed out");
        assert!(
            worker.child.try_wait().unwrap().is_none(),
            "wrapper exited before transfer readiness"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    struct Orphan(i32);
    impl Drop for Orphan {
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.0, libc::SIGKILL);
            }
        }
    }
    let orphan = Orphan(fs::read_to_string(&ready).unwrap().parse().unwrap());
    worker.terminate();
    let before = snapshot(fixture.store.layout.root());
    {
        let _short = locks::short_test_waits();
        assert!(matches!(
            fixture.store.prepare(Operation::List),
            Err(Error::LockTimeout { .. })
        ));
    }
    assert_eq!(snapshot(fixture.store.layout.root()), before);
    let _release = std::os::unix::net::UnixStream::connect(&socket).unwrap();
    drop(fixture.store.prepare(Operation::List).unwrap());
    std::mem::forget(orphan); // the transfer exited normally after release
    assert_clean(&fixture.store);
}

#[test]
fn fresh_publication_and_abort_finish_owned_namespace_directories() {
    for abort in [false, true] {
        let fixture = MutationFixture::new();
        let target = LocalRef::new(format!("{}:tag", "a".repeat(125))).unwrap();
        let guard = fixture.store.prepare(Operation::Import).unwrap();
        let mut mutation = Mutation::begin(&guard, &target, Kind::Import).unwrap();
        let stage = complete_image(
            mutation.stage().unwrap(),
            &target,
            &fixture.images.replacement,
        );
        if abort {
            fsutil::fail_once("ref.rename", libc::EIO);
        }
        let result = mutation.publish(stage);
        assert_eq!(result.is_err(), abort);
        let record = load(&fixture.store.layout).unwrap().unwrap();
        assert!(record.namespace_created.len() > 1);
        mutation.finish().unwrap();
        assert_clean(&fixture.store);
        assert_eq!(
            refs::pointer_identity(&fixture.store.layout, &target)
                .unwrap()
                .is_none(),
            abort
        );
        if abort {
            assert!(!record.namespace(&fixture.store.layout, 1).exists());
            for digest in fixture.images.replacement_only {
                assert!(!fixture.store.layout.blob(&digest).exists());
            }
        }
    }
}

#[test]
fn clean_preparation_never_traverses_dependencies_or_runs_check() {
    let fixture = MutationFixture::new();
    fsutil::fail_persistently("dependencies.observe", libc::EIO);
    drop(fixture.store.prepare(Operation::List).unwrap());
    fsutil::clear_failure();
    fsutil::fail_persistently("graph.observe", libc::EIO);
    drop(fixture.store.prepare(Operation::List).unwrap());
    fsutil::clear_failure();
    assert_clean(&fixture.store);
}

#[test]
fn completed_record_can_be_cleared_after_a_later_namespace_change() {
    let fixture = published_fixture(None);
    fsutil::fail_once("recovery.record-remove", libc::EIO);
    assert!(fixture.store.prepare(Operation::List).is_err());
    assert_eq!(
        load(&fixture.store.layout).unwrap().unwrap().phase,
        Phase::Complete
    );
    // A completed record resurrected after an uncertain unlink authorizes no
    // more content deletion, even if a subsequent legacy mutation changed refs.
    fs::remove_file(fixture.store.layout.ref_entry(&fixture.target)).unwrap();
    let blobs = snapshot(&fixture.store.layout.blobs());
    drop(fixture.store.prepare(Operation::List).unwrap());
    assert_eq!(snapshot(&fixture.store.layout.blobs()), blobs);
    assert_clean(&fixture.store);
}

#[test]
fn dependency_cycles_and_unknown_branches_cannot_authorize_cleanup() {
    use crate::{graph::INDEX, oci::Descriptor};
    for unsupported in [false, true] {
        let fixture = published_fixture(None);
        let digest = Sha256Digest::new(format!("sha256:{}", "a".repeat(64))).unwrap();
        let mut descriptor = Descriptor {
            digest: digest.clone(),
            size: 0,
            media_type: INDEX.into(),
            extra: BTreeMap::new(),
        };
        // Stable length establishes matching descriptors without hashing; the
        // traversal's job is to reject cycles, not certify content hashes.
        let bytes = loop {
            let value = if unsupported {
                serde_json::json!({"schemaVersion":2,"manifests":[{"digest":digest,"size":5,"mediaType":"unsupported"}]})
            } else {
                serde_json::json!({"schemaVersion":2,"manifests":[descriptor]})
            };
            let bytes = serde_json::to_vec(&value).unwrap();
            if descriptor.size == bytes.len() as u64 {
                break bytes;
            }
            descriptor.size = bytes.len() as u64;
        };
        fs::write(fixture.store.layout.blob(&digest), bytes).unwrap();
        let mut index: serde_json::Value =
            serde_json::from_slice(&fs::read(fixture.keeper_raw.join("index.json")).unwrap())
                .unwrap();
        index["manifests"] = serde_json::json!([descriptor]);
        fs::write(
            fixture.keeper_raw.join("index.json"),
            serde_json::to_vec(&index).unwrap(),
        )
        .unwrap();
        let before = snapshot(fixture.store.layout.root());
        assert!(fixture.store.prepare(Operation::List).is_err());
        assert_eq!(snapshot(fixture.store.layout.root()), before);
    }
}

#[test]
fn snapshot_failures_during_cleanup_and_completion_remain_recoverable() {
    for point in [
        "mutation.snapshot-created",
        "mutation.snapshot-write",
        "mutation.snapshot-written",
        "mutation.snapshot-file-sync",
        "mutation.snapshot-synced",
        "mutation.snapshot-renamed",
        "mutation.snapshot-parent-sync",
        "mutation.snapshot-durable",
    ] {
        for remaining in [0, 1] {
            let fixture = published_fixture(None);
            fsutil::fail_after(point, remaining, libc::EIO);
            assert!(
                fixture.store.prepare(Operation::List).is_err(),
                "{point} after {remaining}"
            );
            drop(fixture.store.prepare(Operation::List).unwrap());
            assert_clean(&fixture.store);
            assert!(!fixture.original_raw.exists());
        }
    }
}
