//! Empty namespace reclamation and permanent store-lock coordination.
use super::*;
use crate::{
    ImageSelector, PruneKind, PruneOptions, PrunePhase, PruneReport, locks, test_snapshot::snapshot,
};

fn prune(handle: &Store) -> PruneReport {
    let report = handle.prune(&PruneOptions::default()).unwrap();
    assert!(!report.has_errors(), "{report:?}");
    report
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
        legacy_unlink(&handle, reference).unwrap();
    }
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
    assert_eq!(dry.summaries[&PruneKind::RefDirectory].eligible_count, 2);
    assert_eq!(dry.summaries[&PruneKind::RefDirectory].eligible_bytes, 0);

    let report = prune(&handle);
    assert_eq!(report.summaries[&PruneKind::RefDirectory].removed_count, 2);

    for reference in &dead {
        assert!(
            !handle
                .layout
                .ref_entry(reference)
                .parent()
                .unwrap()
                .exists()
        );
    }
    assert_eq!(
        fs::metadata(handle.layout.store_lock()).unwrap().ino(),
        store_inode
    );
    assert_eq!(snapshot(&raw), raw_before);
    assert_eq!(snapshot(&handle.layout.blobs()), blobs_before);
    assert!(handle.layout.root().join("refs").is_dir());

    assert!(
        prune(&handle)
            .summaries
            .values()
            .all(|summary| summary.eligible_count == 0)
    );
}

#[test]
fn missing_lookups_create_no_bookkeeping_and_namespace_cleanup_requires_the_barrier() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    drop(handle.prepare(Operation::Import).unwrap());
    let selected = ImageSelector::new("missing:tag").unwrap();
    let before = snapshot(temporary.path());
    assert!(matches!(handle.exists(&selected), Ok(false)));
    prune(&handle);
    assert_eq!(snapshot(temporary.path()), before);
    assert!(!temporary.path().join("locks/refs").exists());

    // A legacy unlink leaves namespace directories eligible at default grace.
    publish(&handle, &reference(), Image::One, false);
    legacy_unlink(&handle, &reference()).unwrap();
    let before = snapshot(temporary.path());
    fsutil::fail_once("prune.namespace-sync", libc::EIO);
    let blocked = handle.prune(&PruneOptions::default()).unwrap();
    assert!(blocked.has_errors() && !blocked.scan_complete);
    assert_eq!(blocked.diagnostics[0].phase, PrunePhase::Sync);
    assert!(
        blocked
            .summaries
            .values()
            .all(|s| s.eligible_count == 0 && s.removed_count == 0)
    );
    assert_eq!(snapshot(temporary.path()), before);
    assert_eq!(
        prune(&handle).summaries[&PruneKind::RefDirectory].removed_count,
        1
    );
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
        let live_inode = fs::metadata(handle.layout.ref_entry(&live)).unwrap().ino();
        legacy_unlink(&handle, &dead).unwrap();
        prune(&handle);
        assert!(handle.layout.ref_entry(&live).exists());
        assert_eq!(
            fs::metadata(handle.layout.ref_entry(&live)).unwrap().ino(),
            live_inode
        );
        assert!(!handle.layout.ref_entry(&dead).parent().unwrap().exists());
        legacy_unlink(&handle, &live).unwrap();
        let report = prune(&handle);
        let dirs: Vec<_> = report
            .candidates
            .iter()
            .filter(|candidate| candidate.kind == PruneKind::RefDirectory)
            .collect();
        assert!(
            dirs.iter()
                .all(|candidate| candidate.removed && candidate.durability_confirmed)
        );
        assert!(
            dirs.windows(2)
                .all(|pair| pair[0].path.components().count() >= pair[1].path.components().count())
        );

        assert_eq!(
            fs::read_dir(handle.layout.root().join("refs"))
                .unwrap()
                .count(),
            0
        );
    }
}

#[test]
fn unexpected_lock_contents_are_preserved_without_adoption() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    drop(handle.prepare(Operation::Import).unwrap());
    let foreign = temporary.path().join("locks/refs/foreign");
    fs::create_dir_all(&foreign).unwrap();
    fs::write(foreign.join("ref.lock"), b"preserve").unwrap();
    symlink("/outside/never-follow", foreign.join("pointer")).unwrap();
    let before = snapshot(&temporary.path().join("locks"));
    prune(&handle);
    assert_eq!(snapshot(&temporary.path().join("locks")), before);
}

#[test]
fn incomplete_bookkeeping_scan_blocks_all_reclamation() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    publish(&handle, &reference(), Image::One, false);
    legacy_unlink(&handle, &reference()).unwrap();
    let before = snapshot(temporary.path());
    fsutil::fail_once("prune.bookkeeping-enumerate", libc::EACCES);
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

#[test]
fn bookkeeping_remove_and_sync_errors_stop_with_visible_progress_and_allow_retry() {
    for point in ["prune.bookkeeping-remove", "prune.bookkeeping-sync"] {
        for completed in 0..3 {
            let temporary = tempfile::tempdir().unwrap();
            let handle = store(temporary.path());
            let reference = LocalRef::new(format!("{}:tag", "x".repeat(130))).unwrap();
            publish(&handle, &reference, Image::One, false);
            legacy_unlink(&handle, &reference).unwrap();
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

            assert!(
                !handle
                    .layout
                    .ref_entry(&reference)
                    .parent()
                    .unwrap()
                    .exists()
            );
            assert_eq!(snapshot(&handle.layout.blobs()), blob_before);
        }
    }
}

#[test]
fn store_lock_holders_exclude_cleanup_and_waiters_retain_permanent_identity() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    drop(handle.prepare(Operation::Import).unwrap());
    let selected = reference();
    for _ in 0..2 {
        let guard = handle.prepare(Operation::Import).unwrap();
        let lock = handle.layout.store_lock();
        let inode = fs::metadata(&lock).unwrap().ino();
        let mut waiter = Worker::start(temporary.path(), &selected, "bookkeeping-waiter");
        // The checkpoint proves the waiter has opened the permanent lock
        // and a real flock has returned WouldBlock.
        waiter.wait_for("PUBLICATION_PAUSED");
        {
            let _short = locks::short_test_waits();
            assert!(matches!(
                handle.prune(&PruneOptions::default()),
                Err(Error::LockTimeout { .. })
            ));
        }
        drop(guard);
        // A waiting descriptor has no lock yet. Prune can run while it is
        // paused, but must preserve the inode on which it will retry.
        prune(&handle);
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
        prune(&handle);
        assert_eq!(fs::metadata(&lock).unwrap().ino(), inode);
    }
}

pub(super) fn worker(handle: &Store, _selected: &LocalRef) {
    fsutil::pause_once("lock.contended");
    let _guard = handle.prepare(Operation::Export).unwrap();
    println!("BOOKKEEPING_ACQUIRED");
    std::io::stdout().flush().unwrap();
    let mut instruction = String::new();
    std::io::stdin().read_line(&mut instruction).unwrap();
}
