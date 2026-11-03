//! Public removal's preparation, reclamation, and recovery contract.
use super::*;
use crate::{
    CheckScope, ImageSelector, PruneOptions,
    mutation::{Kind, Mutation, Record},
    test_snapshot::snapshot,
    test_support::{NestedImages, SharedImages, SyntheticImage, complete_image, prefix_names},
};

fn publish_complete(handle: &Store, reference: &LocalRef, image: &SyntheticImage) -> PathBuf {
    let guard = handle.prepare(Operation::Import).unwrap();
    let mut mutation = Mutation::begin(&guard, reference, Kind::Import).unwrap();
    let stage = complete_image(mutation.stage().unwrap(), reference, image);
    let raw = mutation.publish(stage).unwrap();
    mutation.finish().unwrap();
    raw
}

struct RemovalFixture {
    _directory: tempfile::TempDir,
    store: Store,
    target: LocalRef,
    keeper: LocalRef,
    images: SharedImages,
    raw: PathBuf,
    keeper_raw: PathBuf,
}
impl RemovalFixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory.path().join("store"));
        let (target, keeper) = prefix_names();
        let images = SharedImages::new();
        let raw = publish_complete(&store, &target, &images.original);
        let keeper_raw = publish_complete(&store, &keeper, &images.keeper);
        Self {
            _directory: directory,
            store,
            target,
            keeper,
            images,
            raw,
            keeper_raw,
        }
    }
    fn assert_idle(&self) {
        assert!(!crate::recovery::pending(&self.store.layout).unwrap());
        assert_eq!(
            fs::read_dir(self.store.layout.staging()).unwrap().count(),
            0
        );
    }
    fn assert_recovered(&self, removed: bool) {
        self.assert_idle();
        assert_eq!(self.raw.exists(), !removed);
        assert_eq!(
            refs::pointer_identity(&self.store.layout, &self.target)
                .unwrap()
                .is_none(),
            removed
        );
        for digest in &self.images.original_only {
            assert_eq!(self.store.layout.blob(digest).exists(), !removed);
        }
        for digest in self.images.keeper.digests() {
            assert!(self.store.layout.blob(&digest).exists());
        }
    }
}

#[test]
fn remove_never_creates_or_resumes_store_setup() {
    for scenario in [
        "missing",
        "ancestors",
        "empty",
        "locks",
        "bootstrap",
        "interrupted",
        "interrupted-no-lock",
        "unsupported",
        "damaged",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let root = if scenario == "ancestors" {
            directory.path().join("absent/parent/store")
        } else {
            directory.path().join("store")
        };
        let handle = store(&root);
        match scenario {
            "missing" | "ancestors" => (),
            "empty" => fs::create_dir(&root).unwrap(),
            "locks" | "bootstrap" => {
                fs::create_dir(&root).unwrap();
                fs::create_dir(root.join("locks")).unwrap();
                if scenario == "bootstrap" {
                    fs::write(handle.layout.store_lock(), b"").unwrap();
                }
            }
            _ => {
                drop(handle.prepare(Operation::Import).unwrap());
                match scenario {
                    "unsupported" => {
                        fs::write(handle.layout.marker(), br#"{"version":99}"#).unwrap()
                    }
                    "damaged" => fs::remove_dir(handle.layout.raw_images()).unwrap(),
                    _ => {
                        fs::rename(handle.layout.marker(), root.join(".store-init.json")).unwrap();
                        if scenario == "interrupted-no-lock" {
                            fs::remove_file(handle.layout.store_lock()).unwrap();
                        }
                    }
                }
            }
        }
        let before = snapshot(directory.path());
        assert!(handle.remove(&reference()).is_err(), "{scenario}");
        assert_eq!(snapshot(directory.path()), before, "{scenario}");
    }
    let directory = tempfile::tempdir().unwrap();
    let handle = store(directory.path());
    drop(handle.prepare(Operation::Import).unwrap());
    let before = snapshot(directory.path());
    assert!(
        matches!(handle.remove(&reference()), Err(Error::RefMissing(value)) if value == reference())
    );
    assert_eq!(snapshot(directory.path()), before);
}

#[test]
fn removal_reclaims_shared_content_and_prefixes_before_success_without_reading_terminals() {
    let fixture = RemovalFixture::new();
    // Display caches are not authoritative for deletion.
    fs::write(fixture.raw.join("metadata.json"), b"{").unwrap();
    fs::write(fixture.keeper_raw.join("metadata.json"), b"{").unwrap();
    let keeper = snapshot(&fixture.keeper_raw);
    let shared = fixture.images.shared.first().unwrap();
    let shared_before = snapshot(&fixture.store.layout.blob(shared));
    let lock = snapshot(&fixture.store.layout.store_lock());
    fsutil::track_opens();
    let report = fixture.store.remove(&fixture.target).unwrap();
    let opened = fsutil::take_opens();
    assert_eq!(report.local_ref, fixture.target);
    fixture.assert_recovered(true);
    assert_eq!(snapshot(&fixture.keeper_raw), keeper);
    assert_eq!(snapshot(&fixture.store.layout.blob(shared)), shared_before);
    assert!(
        fixture
            .store
            .layout
            .ref_entry(&fixture.target)
            .parent()
            .unwrap()
            .is_dir()
    );
    for image in [&fixture.images.original, &fixture.images.keeper] {
        assert!(!opened.contains(&fixture.raw.join("metadata.json")));
        assert!(!opened.contains(&fixture.keeper_raw.join("metadata.json")));
        for digest in image.digests() {
            if digest != image.root.digest {
                assert!(!opened.contains(&fixture.store.layout.blob(&digest)));
            }
        }
    }
    fixture.store.remove(&fixture.keeper).unwrap();
    fixture.assert_idle();
    for path in [
        fixture.store.layout.blobs(),
        fixture.store.layout.raw_images(),
        fixture.store.config().root().join("refs"),
    ] {
        assert_eq!(fs::read_dir(path).unwrap().count(), 0);
    }
    assert_eq!(snapshot(&fixture.store.layout.store_lock()), lock);
}

#[test]
fn removal_preserves_every_nested_keeper_branch_and_unrecorded_remnants() {
    let directory = tempfile::tempdir().unwrap();
    let handle = store(directory.path());
    let images = NestedImages::new();
    let target = reference();
    let keeper = LocalRef::new("nested:keeper").unwrap();
    let raw = publish_complete(&handle, &target, &images.first);
    let keeper_raw = publish_complete(&handle, &keeper, &images.top);
    let before = snapshot(&keeper_raw);
    let blobs = snapshot(&handle.layout.blobs());
    let unrecorded = handle.layout.staging().join("operation-unrecorded");
    fs::create_dir(&unrecorded).unwrap();
    fs::write(unrecorded.join("unknown"), b"preserve").unwrap();
    let staging = snapshot(&unrecorded);
    fsutil::track_opens();
    handle.remove(&target).unwrap();
    let opened = fsutil::take_opens();
    assert!(!raw.exists());
    assert_eq!(snapshot(&keeper_raw), before);
    assert_eq!(snapshot(&handle.layout.blobs()), blobs);
    assert_eq!(snapshot(&unrecorded), staging);
    assert!(!opened.contains(&handle.layout.blob(&images.config.digest)));
    assert!(!opened.contains(&handle.layout.blob(&images.layer.digest)));
    assert!(opened.contains(&handle.layout.blob(&images.nested.digest)));
    assert!(opened.contains(&handle.layout.blob(&images.second.digest)));
}

#[test]
fn damaged_target_declarations_are_preserved_before_removal_intent() {
    for damage in ["wrapper", "index", "manifest", "unsupported"] {
        let fixture = RemovalFixture::new();
        match damage {
            "wrapper" => fs::rename(
                &fixture.raw,
                fixture._directory.path().join("saved-wrapper"),
            )
            .unwrap(),
            "index" => fs::write(fixture.raw.join("index.json"), b"{").unwrap(),
            "manifest" => fs::remove_file(
                fixture
                    .store
                    .layout
                    .blob(&fixture.images.original.root.digest),
            )
            .unwrap(),
            "unsupported" => {
                let image = fixture.images.original.with_manifest_change(|value| {
                    value["subject"] = serde_json::json!({});
                });
                for (digest, bytes) in &image.objects {
                    fs::write(fixture.store.layout.blob(digest), bytes).unwrap();
                }
                for name in ["oci-layout", "index.json"] {
                    fs::remove_file(fixture.raw.join(name)).unwrap();
                }
                oci::write_layout(&fixture.raw, &image.root).unwrap();
            }
            _ => unreachable!(),
        }
        let before = snapshot(fixture.store.config().root());
        assert!(fixture.store.remove(&fixture.target).is_err(), "{damage}");
        assert_eq!(snapshot(fixture.store.config().root()), before, "{damage}");
        fixture.assert_idle();
    }
}

#[test]
fn unrelated_dependency_damage_blocks_all_operations_after_visible_removal() {
    let fixture = RemovalFixture::new();
    let manifest = fixture
        .store
        .layout
        .blob(&fixture.images.keeper.root.digest);
    let bytes = fs::read(&manifest).unwrap();
    fs::remove_file(&manifest).unwrap();
    let raw = snapshot(&fixture.raw);
    let blobs = snapshot(&fixture.store.layout.blobs());
    let error = fixture.store.remove(&fixture.target).unwrap_err();
    assert!(matches!(
        error,
        Error::Recovery {
            change: Some(NamespaceChange::RefRemoved),
            ..
        }
    ));
    assert_eq!(
        refs::pointer_identity(&fixture.store.layout, &fixture.target).unwrap(),
        None
    );
    assert_eq!(snapshot(&fixture.raw), raw);
    assert_eq!(snapshot(&fixture.store.layout.blobs()), blobs);
    let pending = snapshot(fixture.store.config().root());
    for operation in [
        Operation::List,
        Operation::Check,
        Operation::Remove,
        Operation::Prune,
        Operation::Import,
        Operation::Pull,
        Operation::Export,
        Operation::Availability,
    ] {
        assert!(
            matches!(
                fixture.store.prepare(operation),
                Err(Error::Recovery { .. })
            ),
            "{operation:?}"
        );
        assert_eq!(snapshot(fixture.store.config().root()), pending);
    }
    assert!(fixture.store.check(&CheckScope::All).is_err());
    assert!(fixture.store.prune(&PruneOptions::default()).is_err());
    assert!(
        fixture
            .store
            .availability(&ImageSelector::new(fixture.keeper.as_str()).unwrap())
            .is_err()
    );
    // Restore known fixture bytes only: production has no automatic repair.
    fs::write(manifest, bytes).unwrap();
    fixture.store.list().unwrap();
    fixture.store.list().unwrap();
    fixture.assert_recovered(true);
}

pub(super) fn removal_checkpoint(point: &str) -> &'static str {
    match point {
        "intent" => "mutation.snapshot-durable",
        "unlink" => "ref.unlink",
        "visible" => "ref.remove-sync",
        "namespace" => "recovery.namespace-sync",
        "cleaning" => "recovery.cleaning-durable",
        "deleted" => "recovery.removed",
        "complete" => "recovery.complete-durable",
        "cleared" => "recovery.record-removed",
        _ => panic!("unknown removal checkpoint"),
    }
}

#[test]
fn public_remove_errors_recover_at_every_durable_boundary() {
    for (point, removed) in [
        ("mutation.snapshot-created", false),
        ("mutation.snapshot-write", false),
        ("mutation.snapshot-file-sync", false),
        ("mutation.snapshot-renamed", false),
        ("mutation.snapshot-parent-sync", false),
        ("mutation.snapshot-durable", false),
        ("ref.unlink", false),
        ("ref.remove-sync", true),
        ("recovery.namespace-sync", true),
        ("recovery.cleaning-durable", true),
        ("recovery.remove", true),
        ("recovery.removed", true),
        ("recovery.cleanup-sync", true),
        ("recovery.cleanup-durable", true),
        ("recovery.complete-durable", true),
        ("recovery.record-remove", true),
        ("recovery.record-removed", true),
        ("recovery.record-remove-sync", true),
    ] {
        let fixture = RemovalFixture::new();
        let keeper = snapshot(&fixture.keeper_raw);
        let shared = snapshot(
            &fixture
                .store
                .layout
                .blob(fixture.images.shared.first().unwrap()),
        );
        fsutil::fail_once(point, libc::EIO);
        let error = fixture.store.remove(&fixture.target).unwrap_err();
        fsutil::clear_failure();
        assert!(
            matches!(
                error,
                Error::Recovery {
                    change: Some(NamespaceChange::RefRemoved),
                    ..
                }
            ),
            "{point}: {error}"
        );
        fixture.store.list().unwrap();
        fixture.store.list().unwrap();
        fixture.assert_recovered(removed);
        assert_eq!(snapshot(&fixture.keeper_raw), keeper, "{point}");
        assert_eq!(
            snapshot(
                &fixture
                    .store
                    .layout
                    .blob(fixture.images.shared.first().unwrap())
            ),
            shared,
            "{point}"
        );
    }
}

#[test]
fn public_remove_retries_every_cleanup_deletion_and_parent_sync() {
    for last_ref in [false, true] {
        let fixture_for_case = || {
            let fixture = RemovalFixture::new();
            if last_ref {
                fixture.store.remove(&fixture.keeper).unwrap();
            }
            fixture
        };
        let fixture = fixture_for_case();
        fsutil::fail_once("recovery.cleaning-durable", libc::EIO);
        assert!(fixture.store.remove(&fixture.target).is_err());
        let record: Record =
            serde_json::from_slice(&fs::read(fixture.store.layout.pending_mutation()).unwrap())
                .unwrap();
        let entries = record.cleanup.unwrap().entries;
        if last_ref {
            assert!(
                entries
                    .iter()
                    .any(|entry| matches!(entry.target, crate::mutation::Target::Namespace { .. }))
            );
        }
        for point in [
            "recovery.remove",
            "recovery.removed",
            "recovery.cleanup-sync",
        ] {
            for position in 0..entries.len() {
                let fixture = fixture_for_case();
                fsutil::fail_after(point, position, libc::EIO);
                assert!(
                    fixture.store.remove(&fixture.target).is_err(),
                    "{point}, {position}"
                );
                fsutil::clear_failure();
                assert!(fixture.store.layout.pending_mutation().exists());
                fixture.store.list().unwrap();
                fixture.store.list().unwrap();
                if last_ref {
                    fixture.assert_idle();
                    for path in [
                        fixture.store.layout.blobs(),
                        fixture.store.layout.raw_images(),
                        fixture.store.config().root().join("refs"),
                    ] {
                        assert_eq!(fs::read_dir(path).unwrap().count(), 0);
                    }
                } else {
                    fixture.assert_recovered(true);
                }
            }
        }
    }
}

#[test]
fn terminated_public_remove_recovers_forward_or_abandons_without_unlinking() {
    for (point, removed) in [
        ("intent", false),
        ("unlink", false),
        ("visible", true),
        ("namespace", true),
        ("cleaning", true),
        ("deleted", true),
        ("complete", true),
        ("cleared", true),
    ] {
        let fixture = RemovalFixture::new();
        let keeper = snapshot(&fixture.keeper_raw);
        let lock = snapshot(&fixture.store.layout.store_lock());
        let mut worker = Worker::start(
            fixture.store.config().root(),
            &fixture.target,
            &format!("remove-at-{point}"),
        );
        worker.wait_for("PUBLICATION_PAUSED");
        // Removal retains its single exclusive guard through cleanup/clearing.
        {
            let _short = crate::locks::short_test_waits();
            assert!(matches!(
                fixture.store.prepare(Operation::List),
                Err(Error::LockTimeout { .. })
            ));
        }
        worker.child.kill().unwrap();
        worker.child.wait().unwrap();
        fixture.store.list().unwrap();
        fixture.store.list().unwrap();
        fixture.assert_recovered(removed);
        assert_eq!(snapshot(&fixture.keeper_raw), keeper);
        assert_eq!(snapshot(&fixture.store.layout.store_lock()), lock);
    }
}
