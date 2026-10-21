//! Bookkeeping reclamation and the ref-lock lifetime contract.
use super::*;
use crate::{
    ImageAvailability, ImageSelector, PruneKind, PruneOptions, PrunePhase, PruneReport, locks,
    test_snapshot::snapshot,
};

fn prune(handle: &Store) -> PruneReport {
    let report = handle.prune(&PruneOptions::default()).unwrap();
    assert!(!report.has_errors(), "{report:?}");
    report
}
fn unused_lock(handle: &Store, reference: &LocalRef) -> PathBuf {
    let guard = handle.prepare(Operation::List).unwrap();
    drop(guard.lock_ref(reference).unwrap());
    handle.layout.ref_lock(reference)
}

#[test]
fn default_prune_cleans_the_example_without_touching_live_identity_or_young_content() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    let live = LocalRef::new("alpine:3.22").unwrap();
    let dead = [
        LocalRef::new("alpine:latest").unwrap(),
        LocalRef::new("ubuntu:26.04").unwrap(),
    ];
    let raw = publish(&handle, &live, Image::One, false);
    for reference in &dead {
        publish(&handle, reference, Image::One, false);
        handle.remove(reference).unwrap();
    }
    let live_lock = handle.layout.ref_lock(&live);
    let live_inode = fs::metadata(&live_lock).unwrap().ino();
    let store_inode = fs::metadata(handle.layout.store_lock()).unwrap().ino();
    let raw_before = snapshot(&raw);
    let blobs_before = snapshot(&handle.layout.blobs());
    let before = snapshot(temporary.path());
    let dry = handle
        .prune(&PruneOptions {
            dry_run: true,
            ..PruneOptions::default()
        })
        .unwrap();
    assert!(!dry.has_errors());
    assert_eq!(snapshot(temporary.path()), before);
    for kind in [
        PruneKind::RefLock,
        PruneKind::RefLockDirectory,
        PruneKind::RefDirectory,
    ] {
        assert_eq!(dry.summaries[&kind].eligible_count, 2);
        assert_eq!(dry.summaries[&kind].eligible_bytes, 0);
    }
    let report = prune(&handle);
    for kind in [
        PruneKind::RefLock,
        PruneKind::RefLockDirectory,
        PruneKind::RefDirectory,
    ] {
        assert_eq!(report.summaries[&kind].removed_count, 2);
    }
    for reference in &dead {
        assert!(
            !handle
                .layout
                .ref_entry(reference)
                .parent()
                .unwrap()
                .exists()
        );
        assert!(!handle.layout.ref_lock(reference).parent().unwrap().exists());
    }
    assert_eq!(fs::metadata(&live_lock).unwrap().ino(), live_inode);
    assert_eq!(
        fs::metadata(handle.layout.store_lock()).unwrap().ino(),
        store_inode
    );
    assert_eq!(snapshot(&raw), raw_before);
    assert_eq!(snapshot(&handle.layout.blobs()), blobs_before);
    for root in ["refs", "locks/refs"] {
        assert!(handle.layout.root().join(root).is_dir());
    }
    assert!(
        prune(&handle)
            .summaries
            .values()
            .all(|summary| summary.eligible_count == 0)
    );
}

#[test]
fn missing_lookups_create_reclaimable_locks_and_bookkeeping_requires_the_barrier() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    drop(handle.prepare(Operation::Import).unwrap());
    let selected = ImageSelector::new("missing:tag").unwrap();
    let reference = LocalRef::new("missing:tag").unwrap();
    assert!(matches!(
        handle.availability(&selected),
        Ok(ImageAvailability::Missing)
    ));
    let lock = handle.layout.ref_lock(&reference);
    assert!(lock.exists());
    let before = snapshot(temporary.path());
    fsutil::fail_once("prune.namespace-sync", libc::EIO);
    let blocked = handle.prune(&PruneOptions::default()).unwrap();
    assert!(blocked.has_errors() && !blocked.scan_complete);
    assert_eq!(blocked.diagnostics[0].phase, PrunePhase::Sync);
    assert!(
        blocked
            .summaries
            .values()
            .all(|summary| summary.eligible_count == 0 && summary.removed_count == 0)
    );
    assert_eq!(snapshot(temporary.path()), before);
    assert_eq!(
        prune(&handle).summaries[&PruneKind::RefLock].removed_count,
        1
    );
    assert!(!lock.exists());
    assert!(matches!(
        handle.availability(&selected),
        Ok(ImageAvailability::Missing)
    ));
    assert!(lock.exists());
    prune(&handle);
    assert!(!lock.exists());
}

#[test]
fn shared_encoded_prefixes_retain_live_ancestors_and_remove_children_first() {
    for (live, dead) in [
        (
            format!("{}:live", "a".repeat(60)),
            format!("{}:dead", "a".repeat(60)),
        ),
        (
            format!("{}:live", "b".repeat(55)),
            format!("{}:live_more", "b".repeat(55)),
        ),
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        let live = LocalRef::new(live).unwrap();
        let dead = LocalRef::new(dead).unwrap();
        publish(&handle, &live, Image::One, false);
        publish(&handle, &dead, Image::Two, false);
        let live_inode = fs::metadata(handle.layout.ref_lock(&live)).unwrap().ino();
        handle.remove(&dead).unwrap();
        prune(&handle);
        assert!(handle.layout.ref_entry(&live).exists());
        assert_eq!(
            fs::metadata(handle.layout.ref_lock(&live)).unwrap().ino(),
            live_inode
        );
        assert!(!handle.layout.ref_lock(&dead).exists());
        assert!(!handle.layout.ref_entry(&dead).parent().unwrap().exists());
        handle.remove(&live).unwrap();
        let report = prune(&handle);
        for kind in [PruneKind::RefDirectory, PruneKind::RefLockDirectory] {
            let dirs: Vec<_> = report
                .candidates
                .iter()
                .filter(|candidate| candidate.kind == kind)
                .collect();
            assert!(
                dirs.iter()
                    .all(|candidate| candidate.removed && candidate.durability_confirmed)
            );
            assert!(
                dirs.windows(2)
                    .all(|pair| pair[0].path.components().count()
                        >= pair[1].path.components().count())
            );
        }
        for root in ["refs", "locks/refs"] {
            assert_eq!(
                fs::read_dir(handle.layout.root().join(root))
                    .unwrap()
                    .count(),
                0
            );
        }
    }
}

#[test]
fn unknown_lock_subtrees_are_preserved_while_independent_known_locks_are_cleaned() {
    for case in [
        "extra-file",
        "symlink",
        "nonempty",
        "directory",
        "hard-link",
        "malformed",
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        drop(handle.prepare(Operation::Import).unwrap());
        let bad = LocalRef::new(format!("{}:bad", "a".repeat(60))).unwrap();
        let path = unused_lock(&handle, &bad);
        let top = handle
            .layout
            .root()
            .join("locks/refs")
            .join(bad.encoded_path().components().next().unwrap());
        match case {
            "extra-file" => fs::write(top.join("unknown"), b"preserve").unwrap(),
            "nonempty" => fs::write(&path, b"not library bookkeeping").unwrap(),
            "hard-link" => fs::hard_link(&path, temporary.path().join("outside-link")).unwrap(),
            "symlink" | "directory" => {
                fs::remove_file(&path).unwrap();
                if case == "directory" {
                    fs::create_dir(&path).unwrap();
                } else {
                    symlink("/outside/never-follow", &path).unwrap();
                }
            }
            _ => {
                let malformed = handle.layout.root().join("locks/refs/ff");
                fs::create_dir(&malformed).unwrap();
                fs::write(malformed.join("ref.lock"), b"").unwrap();
                fs::write(top.join("unknown"), b"preserve").unwrap();
            }
        }
        let independent = LocalRef::new("independent:unused").unwrap();
        let good = unused_lock(&handle, &independent);
        let before = snapshot(&top);
        let report = prune(&handle);
        assert!(!report.skipped.is_empty(), "{case}");
        assert_eq!(snapshot(&top), before, "{case}");
        assert!(!good.exists());
        assert!(fs::symlink_metadata(&path).is_ok());
        if case == "malformed" {
            assert!(handle.layout.root().join("locks/refs/ff/ref.lock").exists());
        }
    }
}

#[test]
fn incomplete_bookkeeping_scan_blocks_all_reclamation() {
    for point in ["prune.bookkeeping-enumerate", "prune.bookkeeping-metadata"] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        publish(&handle, &reference(), Image::One, false);
        handle.remove(&reference()).unwrap();
        let before = snapshot(temporary.path());
        fsutil::fail_once(point, libc::EACCES);
        let report = handle
            .prune(&PruneOptions {
                grace_period: Duration::ZERO,
                ..PruneOptions::default()
            })
            .unwrap();
        assert!(report.has_errors() && !report.scan_complete);
        assert!(
            report
                .candidates
                .iter()
                .all(|candidate| !candidate.eligible && !candidate.removed)
        );
        assert_eq!(snapshot(temporary.path()), before);
    }
}

#[test]
fn bookkeeping_remove_and_sync_errors_stop_with_visible_progress_and_allow_retry() {
    for point in ["prune.bookkeeping-remove", "prune.bookkeeping-sync"] {
        for completed in 0..3 {
            let temporary = tempfile::tempdir().unwrap();
            let handle = store(temporary.path());
            publish(&handle, &reference(), Image::One, false);
            handle.remove(&reference()).unwrap();
            let blob_before = snapshot(&handle.layout.blobs());
            fsutil::fail_after(point, completed, libc::EIO);
            let report = handle.prune(&PruneOptions::default()).unwrap();
            assert!(report.has_errors() && report.scan_complete);
            assert_eq!(report.diagnostics.len(), 1);
            assert_eq!(
                report.diagnostics[0].phase,
                if point.ends_with("sync") {
                    PrunePhase::Sync
                } else {
                    PrunePhase::Remove
                }
            );
            let removed: u64 = report
                .candidates
                .iter()
                .map(|candidate| candidate.removed_entries)
                .sum();
            assert_eq!(
                removed,
                completed as u64 + u64::from(point.ends_with("sync"))
            );
            assert!(
                report
                    .candidates
                    .iter()
                    .all(|candidate| candidate.removed_bytes == 0)
            );
            if point.ends_with("sync") {
                assert!(
                    report
                        .candidates
                        .iter()
                        .any(|candidate| !candidate.durability_confirmed)
                );
            }
            prune(&handle);
            assert!(!handle.layout.ref_lock(&reference()).exists());
            assert!(
                !handle
                    .layout
                    .ref_entry(&reference())
                    .parent()
                    .unwrap()
                    .exists()
            );
            assert_eq!(snapshot(&handle.layout.blobs()), blob_before);
        }
    }
}

#[test]
fn ref_lock_holders_and_actual_waiters_exclude_cleanup_including_after_recreation() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    drop(handle.prepare(Operation::Import).unwrap());
    let selected = reference();
    for _ in 0..2 {
        let guard = handle.prepare(Operation::Import).unwrap();
        let ref_guard = guard.lock_ref(&selected).unwrap();
        let lock = handle.layout.ref_lock(&selected);
        let inode = fs::metadata(&lock).unwrap().ino();
        let mut waiter = Worker::start(temporary.path(), &selected, "bookkeeping-waiter");
        // The checkpoint is reached only after a real flock returns WouldBlock;
        // the waiter already owns its store guard and open ref-lock descriptor.
        waiter.wait_for("PUBLICATION_PAUSED");
        {
            let _short = locks::short_test_waits();
            assert!(matches!(
                handle.prune(&PruneOptions::default()),
                Err(Error::LockTimeout { .. })
            ));
        }
        drop(ref_guard);
        drop(guard);
        {
            let _short = locks::short_test_waits();
            assert!(matches!(
                handle.prune(&PruneOptions::default()),
                Err(Error::LockTimeout { .. })
            ));
        }
        assert_eq!(fs::metadata(&lock).unwrap().ino(), inode);
        waiter.proceed();
        waiter.wait_for("BOOKKEEPING_ACQUIRED");
        {
            let _short = locks::short_test_waits();
            assert!(matches!(
                handle.prune(&PruneOptions::default()),
                Err(Error::LockTimeout { .. })
            ));
        }
        waiter.proceed();
        waiter.wait_for("PUBLICATION_READY");
        waiter.finish();
        assert_eq!(
            prune(&handle).summaries[&PruneKind::RefLock].removed_count,
            1
        );
        assert!(!lock.exists());
    }
}

pub(super) fn worker(handle: &Store, selected: &LocalRef) {
    let guard = handle.prepare(Operation::Export).unwrap();
    fsutil::pause_once("lock.contended");
    let _ref_guard = guard.lock_ref(selected).unwrap();
    println!("BOOKKEEPING_ACQUIRED");
    std::io::stdout().flush().unwrap();
    let mut instruction = String::new();
    std::io::stdin().read_line(&mut instruction).unwrap();
}
