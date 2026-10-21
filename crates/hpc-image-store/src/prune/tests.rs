//! End-to-end maintenance tests reuse publication's synthetic OCI fixtures.
use super::*;
use crate::{
    CheckScope, ImageAvailability, ImageSelector, PruneKind, PruneOptions, PrunePhase, PruneReport,
    locks, test_snapshot::snapshot,
};
use serde_json::{Value, json};
use std::{fs::FileTimes, os::unix::fs::PermissionsExt, time::SystemTime};

fn options(dry_run: bool) -> PruneOptions {
    PruneOptions {
        dry_run,
        grace_period: Duration::ZERO,
    }
}
fn prune(handle: &Store) -> PruneReport {
    let report = handle.prune(&options(false)).unwrap();
    assert!(!report.has_errors(), "{report:?}");
    report
}
fn orphan(handle: &Store) -> PathBuf {
    let path = handle.layout.blobs().join("f".repeat(64));
    fs::write(&path, b"unreferenced bytes").unwrap();
    path
}
fn modify_time(path: &Path, time: SystemTime) {
    fs::File::open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(time))
        .unwrap();
}

#[test]
fn maintenance_never_initializes_or_repairs_a_store() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("store");
    let handle = store(&path);
    for dry_run in [false, true] {
        assert!(matches!(
            handle.prune(&options(dry_run)),
            Err(Error::MissingStore(_))
        ));
        assert!(!path.exists());
    }
    fs::create_dir(&path).unwrap();
    let before = snapshot(&path);
    for dry_run in [false, true] {
        assert!(matches!(
            handle.prune(&options(dry_run)),
            Err(Error::MissingStore(_))
        ));
        assert_eq!(snapshot(&path), before);
    }
    drop(handle.prepare(Operation::Import).unwrap());
    fs::write(handle.layout.marker(), br#"{"version":999}"#).unwrap();
    let before = snapshot(&path);
    for dry_run in [false, true] {
        assert!(matches!(
            handle.prune(&options(dry_run)),
            Err(Error::UnsupportedVersion { .. })
        ));
        assert_eq!(snapshot(&path), before);
    }
}

#[test]
fn dry_run_is_identical_without_ref_lock_files_and_matches_deletion() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    let old_raw = publish(&handle, &reference(), Image::One, false);
    let active_raw = publish(&handle, &reference(), Image::Two, true);
    fs::remove_file(handle.layout.ref_lock(&reference())).unwrap();
    let extra = orphan(&handle);
    let before = snapshot(temporary.path());
    let dry = handle.prune(&options(true)).unwrap();
    assert!(!dry.has_errors());
    assert_eq!(snapshot(temporary.path()), before);
    assert!(dry.candidates.iter().any(|c| c.path == extra && c.eligible));
    assert!(
        dry.candidates
            .iter()
            .any(|c| c.path == old_raw && c.eligible)
    );
    assert!(!dry.candidates.iter().any(|c| c.path == active_raw));
    let actual = prune(&handle);
    for (kind, expected) in &dry.summaries {
        let observed = &actual.summaries[kind];
        assert_eq!(
            (expected.eligible_count, expected.eligible_bytes),
            (observed.eligible_count, observed.eligible_bytes)
        );
    }
    assert_eq!(actual.candidates.len(), dry.candidates.len());
    assert!(
        actual
            .candidates
            .iter()
            .all(|c| c.removed && c.durability_confirmed)
    );
    assert!(active_raw.exists());
    assert!(!handle.layout.ref_lock(&reference()).exists());
    assert!(handle.layout.blob(&root(Image::Two)).exists());
    assert!(handle.layout.blob(&digest(CONFIG_DIGEST)).exists());
    assert!(!handle.layout.blob(&root(Image::One)).exists());
    assert!(!extra.exists());
}

#[test]
fn grace_is_object_age_and_reuse_preserves_blob_age() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    publish(&handle, &reference(), Image::One, false);
    let path = handle.layout.blob(&root(Image::One));
    let old = SystemTime::now() - Duration::from_secs(48 * 60 * 60);
    modify_time(&path, old);
    let before = fs::metadata(&path).unwrap();
    publish(&handle, &reference(), Image::One, true);
    let after = fs::metadata(&path).unwrap();
    assert_eq!(before.ino(), after.ino());
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    handle.remove(&reference()).unwrap();
    let future = handle.layout.blobs().join("e".repeat(64));
    fs::write(&future, b"future bytes").unwrap();
    modify_time(
        &future,
        SystemTime::now() + Duration::from_secs(48 * 60 * 60),
    );
    let report = handle.prune(&PruneOptions::default()).unwrap();
    assert!(!report.has_errors());
    assert!(
        !path.exists(),
        "old blob is eligible immediately after removal"
    );
    assert!(handle.layout.blob(&digest(CONFIG_DIGEST)).exists());
    assert!(
        report
            .candidates
            .iter()
            .filter(|c| c.kind == PruneKind::RawImage)
            .all(|c| !c.eligible)
    );
    prune(&handle);
    assert!(
        future.exists(),
        "zero grace still preserves future timestamps"
    );
}

#[test]
fn unknown_contents_preserve_entire_trees_and_never_follow_symlinks() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    drop(handle.prepare(Operation::Import).unwrap());
    let outside = temporary.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), b"sentinel").unwrap();
    for (name, bad) in [
        ("operation-aaaaaaaaaaaa", "unknown"),
        ("operation-bbbbbbbbbbbb", "symlink"),
        ("operation-cccccccccccc", "wrong-type"),
    ] {
        let directory = handle.layout.staging().join(name);
        fs::create_dir_all(directory.join("layout/blobs")).unwrap();
        fs::write(directory.join("layout/index.json"), b"known but partial").unwrap();
        match bad {
            "unknown" => fs::write(directory.join("unknown"), b"preserve").unwrap(),
            "symlink" => symlink(&outside, directory.join("layout/oci-layout")).unwrap(),
            _ => fs::create_dir(directory.join("layout/oci-layout")).unwrap(),
        }
    }
    let raw = handle.layout.raw_images().join("raw-aaaaaaaaaaaa");
    fs::create_dir_all(raw.join("blobs")).unwrap();
    fs::write(raw.join("metadata.json"), b"partial").unwrap();
    fs::write(raw.join("blobs/unknown"), b"preserve").unwrap();
    fs::write(handle.layout.blobs().join("not-a-digest"), b"preserve").unwrap();
    symlink(&outside, handle.layout.blobs().join("e".repeat(64))).unwrap();
    symlink(
        &outside,
        handle.layout.staging().join("operation-dddddddddddd"),
    )
    .unwrap();
    fs::write(
        handle.layout.staging().join("operation-wrong-length"),
        b"preserve",
    )
    .unwrap();
    fs::write(handle.layout.root().join(".store-init.json"), b"preserve").unwrap();
    let before = snapshot(temporary.path());
    let report = prune(&handle);
    assert_eq!(report.skipped.len(), 8);
    assert!(report.candidates.is_empty());
    assert_eq!(snapshot(temporary.path()), before);
}

#[test]
fn partial_internal_directories_and_ref_pointer_are_reclaimable() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    drop(handle.prepare(Operation::Import).unwrap());
    let stage = handle.layout.staging().join("operation-aaaaaaaaaaaa");
    fs::create_dir_all(stage.join("blobs/sha256")).unwrap();
    let blob = stage.join("blobs/sha256").join("a".repeat(64));
    fs::write(&blob, b"partial blob").unwrap();
    let cas = handle.layout.blobs().join("a".repeat(64));
    fs::hard_link(&blob, &cas).unwrap();
    symlink("/outside/never-follow", stage.join("ref-pointer")).unwrap();
    let raw = handle.layout.raw_images().join("raw-aaaaaaaaaaaa");
    fs::create_dir(&raw).unwrap();
    fs::write(raw.join("metadata.json"), b"{").unwrap();
    let report = prune(&handle);
    assert_eq!(report.summaries[&PruneKind::Staging].removed_bytes, 12);
    assert_eq!(report.summaries[&PruneKind::RawImage].removed_bytes, 1);
    assert_eq!(report.summaries[&PruneKind::Blob].removed_bytes, 12);
    assert_eq!(
        report.candidates.iter().map(|c| c.kind).collect::<Vec<_>>(),
        vec![PruneKind::Staging, PruneKind::RawImage, PruneKind::Blob]
    );
    assert!(report.candidates.iter().all(|c| c.removed));
}

#[test]
fn all_ref_and_graph_errors_block_every_kind_of_cleanup() {
    for damage in [
        "pointer",
        "wrapper",
        "root",
        "child",
        "unsupported",
        "conflict",
        "namespace",
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        let raw = publish(&handle, &reference(), Image::Index, false);
        orphan(&handle);
        match damage {
            "pointer" => {
                let p = handle.layout.ref_entry(&reference());
                fs::remove_file(&p).unwrap();
                symlink("/outside", p).unwrap();
            }
            "wrapper" => fs::write(raw.join("metadata.json"), b"{").unwrap(),
            "root" => fs::write(raw.join("index.json"), b"{").unwrap(),
            "child" => fs::remove_file(handle.layout.blob(&root(Image::Two))).unwrap(),
            "namespace" => {
                fs::write(handle.layout.root().join("refs/unknown"), b"preserve").unwrap()
            }
            _ => {
                let path = handle.layout.blob(&root(Image::Index));
                let mut index: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                if damage == "unsupported" {
                    index["manifests"][1]["mediaType"] = json!("unsupported");
                } else {
                    let mut extra = index["manifests"][0].clone();
                    extra["size"] = json!(1);
                    index["manifests"].as_array_mut().unwrap().push(extra);
                }
                let bytes = serde_json::to_vec(&index).unwrap();
                fs::write(&path, &bytes).unwrap();
                let mut wrapper: Value =
                    serde_json::from_slice(&fs::read(raw.join("index.json")).unwrap()).unwrap();
                wrapper["manifests"][0]["size"] = json!(bytes.len());
                fs::write(
                    raw.join("index.json"),
                    serde_json::to_vec(&wrapper).unwrap(),
                )
                .unwrap();
            }
        }
        let before = snapshot(temporary.path());
        for dry_run in [true, false] {
            let report = handle.prune(&options(dry_run)).unwrap();
            assert!(
                report.has_errors() && !report.scan_complete,
                "{damage}: {report:?}"
            );
            assert!(report.candidates.iter().all(|c| !c.eligible && !c.removed));
            assert!(report.summaries.values().all(|s| s.eligible_count == 0));
            assert_eq!(snapshot(temporary.path()), before, "{damage}");
        }
    }
}

#[test]
fn incomplete_enumeration_and_operational_failures_block_deletion() {
    for point in [
        "discovery.enumerate",
        "refs.metadata-read",
        "graph.observe",
        "prune.enumerate",
        "prune.metadata",
        "prune.before-delete",
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        publish(&handle, &reference(), Image::One, false);
        orphan(&handle);
        let before = snapshot(temporary.path());
        fsutil::fail_once(point, libc::EACCES);
        let report = handle.prune(&options(false)).unwrap();
        assert!(
            report.has_errors() && !report.scan_complete,
            "{point}: {report:?}"
        );
        assert!(report.candidates.iter().all(|c| !c.eligible));
        assert_eq!(snapshot(temporary.path()), before);
    }
}

#[test]
fn deletion_stops_on_first_error_reports_partial_changes_and_can_retry() {
    for (point, after) in [
        ("prune.remove", 0),
        ("prune.remove", 1),
        ("prune.sync", 0),
        ("prune.sync", 3),
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        publish(&handle, &reference(), Image::One, false);
        let extra = orphan(&handle);
        fsutil::fail_after(point, after, libc::EIO);
        let report = handle.prune(&options(false)).unwrap();
        assert!(report.scan_complete && report.has_errors());
        assert_eq!(report.diagnostics.len(), 1);
        assert_eq!(
            report.diagnostics[0].phase,
            if point == "prune.sync" {
                PrunePhase::Sync
            } else {
                PrunePhase::Remove
            }
        );
        assert!(extra.exists(), "later CAS candidate must not be deleted");
        let stage = report
            .candidates
            .iter()
            .find(|c| c.kind == PruneKind::Staging)
            .unwrap();
        if after > 0 || point == "prune.sync" {
            assert!(stage.removed_entries > 0);
        }
        if point == "prune.sync" {
            assert!(!stage.durability_confirmed);
            assert!(report.diagnostics[0].message.contains("visible"));
        } else {
            assert!(stage.durability_confirmed);
        }
        assert!(!prune(&handle).has_errors());
        assert!(!extra.exists());
        assert!(!handle.check(&CheckScope::All).unwrap().has_errors());
    }
    // A candidate root can be absent even though syncing its removal failed.
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    drop(handle.prepare(Operation::Import).unwrap());
    let extra = orphan(&handle);
    fsutil::fail_once("prune.sync", libc::EIO);
    let report = handle.prune(&options(false)).unwrap();
    assert!(report.candidates[0].removed && !report.candidates[0].durability_confirmed);
    assert_eq!(report.summaries[&PruneKind::Blob].removed_count, 1);
    assert!(!extra.exists());
    prune(&handle);
}

#[test]
fn availability_never_authorizes_a_later_prune_scan() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    let raw = publish(&handle, &reference(), Image::One, false);
    let selected = ImageSelector::new(reference().as_str()).unwrap();
    assert!(matches!(
        handle.availability(&selected),
        Ok(ImageAvailability::Ready)
    ));
    fs::write(raw.join("metadata.json"), b"{").unwrap();
    assert!(matches!(
        handle.availability(&selected),
        Ok(ImageAvailability::Unusable { .. })
    ));
    let extra = orphan(&handle);
    let before = snapshot(temporary.path());
    assert!(handle.prune(&options(false)).unwrap().has_errors());
    assert_eq!(snapshot(temporary.path()), before);
    handle.remove(&reference()).unwrap();
    assert!(matches!(
        handle.availability(&selected),
        Ok(ImageAvailability::Missing)
    ));
    prune(&handle);
    assert!(!extra.exists());
}

#[test]
fn shared_layers_and_complete_nested_indexes_survive_pruning() {
    use sha2::{Digest, Sha256};
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    let guard = handle.prepare(Operation::Import).unwrap();
    let selected_ref = reference();
    let ref_guard = guard.lock_ref(&selected_ref).unwrap();
    let stage = AcquisitionStage::new(&ref_guard).unwrap();
    let mut objects = Vec::new();
    let mut object = |media_type: &str, bytes: Vec<u8>| {
        let digest = Sha256Digest::new(format!("sha256:{:x}", Sha256::digest(&bytes))).unwrap();
        let descriptor = Descriptor {
            media_type: media_type.into(),
            digest: digest.clone(),
            size: bytes.len() as u64,
            extra: BTreeMap::new(),
        };
        fs::write(
            stage.shared_blob_path().join("sha256").join(digest.hex()),
            &bytes,
        )
        .unwrap();
        objects.push(BlobSpec {
            digest,
            size: bytes.len() as u64,
        });
        descriptor
    };
    let config = object(crate::graph::CONFIG, CONFIG.to_vec());
    let layer = object(crate::graph::LAYERS[0], b"shared layer bytes".to_vec());
    let first = object(crate::graph::MANIFEST, serde_json::to_vec(&json!({"schemaVersion":2,"config":config,"layers":[layer],"annotations":{"name":"one"}})).unwrap());
    let second = object(crate::graph::MANIFEST, serde_json::to_vec(&json!({"schemaVersion":2,"config":config,"layers":[layer],"annotations":{"name":"two"}})).unwrap());
    let nested = object(
        crate::graph::INDEX,
        serde_json::to_vec(&json!({"schemaVersion":2,"manifests":[first]})).unwrap(),
    );
    let top = object(
        crate::graph::INDEX,
        serde_json::to_vec(&json!({"schemaVersion":2,"manifests":[nested,second]})).unwrap(),
    );
    oci::write_layout(&stage.layout_path(), &top).unwrap();
    let completed = stage
        .complete(
            &top.digest,
            RefMetadata::new(selected_ref.clone(), top.digest.clone()),
            objects.clone(),
        )
        .unwrap();
    refs::publish(completed, false).unwrap();
    drop(ref_guard);
    drop(guard);
    let extra = orphan(&handle);
    prune(&handle);
    assert!(!extra.exists());
    for blob in &objects {
        assert!(handle.layout.blob(&blob.digest).exists());
    }
    assert!(!handle.check(&CheckScope::All).unwrap().has_errors());
    let second_bytes = fs::read(handle.layout.blob(&second.digest)).unwrap();
    fs::remove_file(handle.layout.blob(&second.digest)).unwrap();
    let extra = orphan(&handle);
    assert!(handle.prune(&options(false)).unwrap().has_errors());
    assert!(extra.exists());
    fs::write(handle.layout.blob(&second.digest), second_bytes).unwrap();

    // Another committed image keeps the shared layer after the index ref goes.
    let other = LocalRef::new("analysis/tool:other").unwrap();
    let guard = handle.prepare(Operation::Import).unwrap();
    let ref_guard = guard.lock_ref(&other).unwrap();
    let stage = AcquisitionStage::new(&ref_guard).unwrap();
    oci::write_layout(&stage.layout_path(), &first).unwrap();
    let specs: Vec<_> = objects
        .into_iter()
        .filter(|object| {
            [
                first.digest.clone(),
                config.digest.clone(),
                layer.digest.clone(),
            ]
            .contains(&object.digest)
        })
        .collect();
    for blob in &specs {
        fs::copy(
            handle.layout.blob(&blob.digest),
            stage
                .shared_blob_path()
                .join("sha256")
                .join(blob.digest.hex()),
        )
        .unwrap();
    }
    let completed = stage
        .complete(
            &first.digest,
            RefMetadata::new(other, first.digest.clone()),
            specs,
        )
        .unwrap();
    refs::publish(completed, false).unwrap();
    drop(ref_guard);
    drop(guard);
    handle.remove(&selected_ref).unwrap();
    prune(&handle);
    for descriptor in [&first, &config, &layer] {
        assert!(handle.layout.blob(&descriptor.digest).exists());
    }
    for descriptor in [&top, &nested, &second] {
        assert!(!handle.layout.blob(&descriptor.digest).exists());
    }
    assert!(!handle.check(&CheckScope::All).unwrap().has_errors());
}

#[test]
fn process_coordination_covers_active_readers_writers_and_reverse_exclusion() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    publish(&handle, &reference(), Image::One, false);
    for action in [
        "prune-hold-import",
        "prune-hold-pull",
        "prune-hold-export",
        "availability-held-name",
    ] {
        let mut holder = Worker::start(temporary.path(), &reference(), action);
        holder.wait_for("PUBLICATION_PAUSED");
        let before = snapshot(temporary.path());
        {
            let _short = locks::short_test_waits();
            assert!(matches!(
                handle.prune(&options(false)),
                Err(Error::LockTimeout { .. })
            ));
        }
        assert_eq!(snapshot(temporary.path()), before);
        holder.proceed();
        holder.wait_for("PUBLICATION_READY");
        holder.finish();
    }
    let mut holder = Worker::start(temporary.path(), &reference(), "prune-held");
    holder.wait_for("PUBLICATION_PAUSED");
    {
        let _short = locks::short_test_waits();
        for operation in [
            Operation::Import,
            Operation::Pull,
            Operation::Export,
            Operation::List,
            Operation::Check,
            Operation::Remove,
            Operation::Availability,
            Operation::Prune,
        ] {
            assert!(
                matches!(handle.prepare(operation), Err(Error::LockTimeout { .. })),
                "{operation:?}"
            );
        }
    }
    holder.proceed();
    holder.wait_for("PUBLICATION_READY");
    holder.finish();
}

#[test]
fn unprivileged_permission_failures_block_all_cleanup() {
    let temporary = tempfile::tempdir().unwrap();
    let mut worker = Worker::start(temporary.path(), &reference(), "prune-permissions");
    worker.wait_for("PUBLICATION_READY");
    worker.finish();
}

pub(super) fn worker(handle: &Store, action: &str) {
    if action == "prune-permissions" {
        assert_ne!(unsafe { libc::geteuid() }, 0);
        for case in [
            "namespace",
            "metadata",
            "candidate",
            "candidate-child",
            "lock-namespace",
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let handle = store(temporary.path());
            let raw = publish(&handle, &reference(), Image::One, false);
            let extra = orphan(&handle);
            let path = match case {
                "namespace" => handle.layout.root().join("refs"),
                "metadata" => raw.join("metadata.json"),
                "candidate" => handle.layout.staging(),
                "lock-namespace" => handle.layout.root().join("locks/refs"),
                _ => {
                    let p = handle
                        .layout
                        .staging()
                        .join("operation-aaaaaaaaaaaa/layout");
                    fs::create_dir_all(&p).unwrap();
                    p
                }
            };
            let permission = fs::metadata(&path).unwrap().permissions();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o0)).unwrap();
            let report = handle.prune(&options(false)).unwrap();
            fs::set_permissions(&path, permission).unwrap();
            assert!(
                report.has_errors() && !report.scan_complete,
                "{case}: {report:?}"
            );
            assert!(extra.exists());
            assert!(report.candidates.iter().all(|c| !c.removed));
        }
    } else if action == "prune-namespace-held" {
        fsutil::pause_once("prune.namespace-sync");
        prune(handle);
    } else if action == "prune-held" {
        fsutil::pause_once("prune.before-delete");
        prune(handle);
    } else {
        let operation = match action {
            "prune-hold-import" => Operation::Import,
            "prune-hold-pull" => Operation::Pull,
            _ => Operation::Export,
        };
        let guard = handle.prepare(operation).unwrap();
        let selected_ref = reference();
        let _ref_guard = guard.lock_ref(&selected_ref).unwrap();
        fsutil::pause_once("prune.test-held");
        fsutil::checkpoint("prune.test-held", handle.layout.root()).unwrap();
    }
}

#[test]
fn descriptor_conflicts_across_refs_block_pruning() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    publish(&handle, &reference(), Image::One, false);
    let other = LocalRef::new("other:tag").unwrap();
    let raw = publish(&handle, &other, Image::Two, false);
    let path = handle.layout.blob(&root(Image::Two));
    let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest["config"]["size"] = json!(79);
    let bytes = serde_json::to_vec(&manifest).unwrap();
    fs::write(path, &bytes).unwrap();
    let mut index: Value =
        serde_json::from_slice(&fs::read(raw.join("index.json")).unwrap()).unwrap();
    index["manifests"][0]["size"] = json!(bytes.len());
    fs::write(raw.join("index.json"), serde_json::to_vec(&index).unwrap()).unwrap();
    orphan(&handle);
    let before = snapshot(temporary.path());
    let report = handle.prune(&options(false)).unwrap();
    assert!(report.has_errors() && !report.scan_complete);
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("conflicting descriptors"))
    );
    assert_eq!(snapshot(temporary.path()), before);
}

fn assert_namespace_blocked(report: &PruneReport, path: &Path) {
    assert!(report.has_errors() && !report.scan_complete, "{report:?}");
    assert_eq!(report.diagnostics.len(), 1);
    let diagnostic = &report.diagnostics[0];
    assert_eq!(diagnostic.phase, PrunePhase::Sync);
    assert_eq!(diagnostic.path, path);
    assert!(
        diagnostic
            .message
            .contains("persist ref namespace before pruning")
    );
    assert!(report.candidates.iter().all(|candidate| !candidate.eligible
        && !candidate.removed
        && candidate.removed_entries == 0
        && candidate.removed_bytes == 0));
    assert!(
        report
            .summaries
            .values()
            .all(|summary| summary.eligible_count == 0
                && summary.eligible_bytes == 0
                && summary.removed_count == 0
                && summary.removed_bytes == 0)
    );
}

#[test]
fn namespace_barrier_saves_removed_refs_including_empty_dirs_and_ancestors() {
    for reference in [
        reference(),
        LocalRef::new(format!("{}:release", "a".repeat(180))).unwrap(),
    ] {
        // Every encoded directory remains after removal. Fail each directory's
        // sync in turn, including intermediate chunks and the refs root. If an
        // ancestor or empty leaf were omitted, pruning would wrongly proceed.
        let encoded_depth = reference.encoded_path().components().count();
        for failed_sync in 0..=encoded_depth {
            let temporary = tempfile::tempdir().unwrap();
            let handle = store(temporary.path());
            let raw = publish(&handle, &reference, Image::One, false);
            fsutil::fail_once("ref.remove-sync", libc::EIO);
            assert!(matches!(
                handle.remove(&reference),
                Err(Error::Durability {
                    change: NamespaceChange::RefRemoved,
                    ..
                })
            ));
            assert!(!handle.layout.ref_entry(&reference).exists());
            let leaf = handle
                .layout
                .ref_entry(&reference)
                .parent()
                .unwrap()
                .to_owned();
            let directories: Vec<_> = leaf
                .ancestors()
                .take(encoded_depth + 1)
                .map(Path::to_owned)
                .collect();
            assert_eq!(
                directories.last().unwrap(),
                &handle.layout.root().join("refs")
            );
            let before = snapshot(temporary.path());
            fsutil::fail_after("prune.namespace-sync", failed_sync, libc::EIO);
            let report = handle.prune(&options(false)).unwrap();
            assert_namespace_blocked(&report, &directories[failed_sync]);
            assert!(
                report
                    .candidates
                    .iter()
                    .any(|candidate| candidate.kind == PruneKind::Staging)
            );
            assert_eq!(snapshot(temporary.path()), before);
            let report = prune(&handle);
            assert_eq!(report.summaries[&PruneKind::RawImage].removed_count, 1);
            assert_eq!(report.summaries[&PruneKind::Blob].removed_count, 2);
            assert!(!raw.exists());
            assert!(
                directories[..encoded_depth]
                    .iter()
                    .all(|directory| !directory.exists())
            );
            assert!(directories.last().unwrap().is_dir());
            assert!(!handle.layout.ref_lock(&reference).exists());
        }
    }
    // An initialized store with no historical namespace directories still syncs
    // refs/ before deleting orphaned content.
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    drop(handle.prepare(Operation::Import).unwrap());
    let extra = orphan(&handle);
    fsutil::fail_once("prune.namespace-sync", libc::EIO);
    let report = handle.prune(&options(false)).unwrap();
    assert_namespace_blocked(&report, &handle.layout.root().join("refs"));
    assert!(extra.exists());
    prune(&handle);
    assert!(!extra.exists());
}

#[test]
fn namespace_barrier_protects_failed_and_terminated_replacements() {
    for terminated in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        let old_raw = publish(&handle, &reference(), Image::One, false);
        if terminated {
            let mut writer = Worker::start(temporary.path(), &reference(), "interrupt-after");
            writer.wait_for("PUBLICATION_READY");
            writer.proceed();
            writer.wait_for("PUBLICATION_PAUSED");
            writer.child.kill().unwrap();
            writer.child.wait().unwrap();
        } else {
            let guard = handle.prepare(Operation::Import).unwrap();
            let selected = reference();
            let ref_guard = guard.lock_ref(&selected).unwrap();
            let stage = completed(&ref_guard, Image::Two);
            fsutil::fail_once("ref.destination-sync", libc::EIO);
            assert!(matches!(
                refs::publish(stage, true),
                Err(Error::Durability {
                    change: NamespaceChange::RefPublished,
                    ..
                })
            ));
        }
        let new_raw = {
            let guard = handle.prepare(Operation::List).unwrap();
            let selected = reference();
            let ref_guard = guard.lock_ref(&selected).unwrap();
            let resolved = refs::read(&ref_guard).unwrap();
            assert_eq!(resolved.root.digest, root(Image::Two));
            resolved.raw_image
        };
        let before = snapshot(temporary.path());
        fsutil::fail_once("prune.namespace-sync", libc::EIO);
        let report = handle.prune(&options(false)).unwrap();
        assert_namespace_blocked(
            &report,
            handle.layout.ref_entry(&reference()).parent().unwrap(),
        );
        assert_eq!(snapshot(temporary.path()), before);
        prune(&handle);
        assert!(!old_raw.exists());
        assert!(!handle.layout.blob(&root(Image::One)).exists());
        assert!(new_raw.exists());
        assert!(handle.layout.blob(&root(Image::Two)).exists());
        assert!(handle.layout.blob(&digest(CONFIG_DIGEST)).exists());
        assert!(!handle.check(&CheckScope::All).unwrap().has_errors());
    }
}

#[test]
fn namespace_barrier_is_skipped_for_dry_runs_no_candidates_and_bad_scans() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    drop(handle.prepare(Operation::Import).unwrap());
    let before = snapshot(temporary.path());
    fsutil::fail_once("prune.namespace-sync", libc::EIO);
    assert!(!handle.prune(&options(false)).unwrap().has_errors());
    assert_eq!(snapshot(temporary.path()), before);
    orphan(&handle);
    let before = snapshot(temporary.path());
    let dry = handle.prune(&options(true)).unwrap();
    assert!(!dry.has_errors());
    assert!(dry.candidates[0].eligible);
    assert_eq!(snapshot(temporary.path()), before);
    let unknown = handle.layout.root().join("refs/unknown");
    fs::write(&unknown, b"preserve").unwrap();
    let blocked = handle.prune(&options(false)).unwrap();
    assert!(blocked.has_errors());
    assert!(
        blocked
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.phase == PrunePhase::Scan)
    );
    fs::remove_file(unknown).unwrap();
    // The pending one-shot fault proves none of those paths tried to sync.
    let report = handle.prune(&options(false)).unwrap();
    assert_namespace_blocked(&report, &handle.layout.root().join("refs"));
}

#[test]
fn namespace_barrier_retains_exclusive_lock_until_sync_and_deletion_finish() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    publish(&handle, &reference(), Image::One, false);
    let extra = orphan(&handle);
    let before = snapshot(temporary.path());
    let mut worker = Worker::start(temporary.path(), &reference(), "prune-namespace-held");
    worker.wait_for("PUBLICATION_PAUSED");
    assert_eq!(snapshot(temporary.path()), before);
    {
        let _short = locks::short_test_waits();
        for operation in [
            Operation::Import,
            Operation::Pull,
            Operation::Remove,
            Operation::Availability,
        ] {
            assert!(
                matches!(handle.prepare(operation), Err(Error::LockTimeout { .. })),
                "{operation:?}"
            );
        }
    }
    assert!(extra.exists());
    worker.proceed();
    worker.wait_for("PUBLICATION_READY");
    worker.finish();
    assert!(!extra.exists());
    assert!(!handle.check(&CheckScope::All).unwrap().has_errors());
}
