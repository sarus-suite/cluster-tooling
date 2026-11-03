//! Public pull coverage of the shared recorded mutation protocol.
use super::*;
use crate::{
    LocalRef,
    locks::Operation,
    test_snapshot::snapshot,
    test_support::{NestedImages, SharedImages, SyntheticImage, prefix_names, publish_image},
};

fn select(fixture: &mut Fixture, image: &SyntheticImage) {
    fixture.select_image(
        &image.objects[&image.root.digest],
        image.objects.values().map(Vec::as_slice),
    );
}

fn images() -> SharedImages {
    let mut images = SharedImages::new();
    // Registry inspection requires mediaType in the raw manifest itself.
    for image in [
        &mut images.original,
        &mut images.keeper,
        &mut images.replacement,
    ] {
        *image = image.with_manifest_change(|value| value["mediaType"] = json!(graph::MANIFEST));
    }
    images.original_only = images
        .original
        .digests()
        .difference(&images.shared)
        .cloned()
        .collect();
    images.keeper_only = images
        .keeper
        .digests()
        .difference(&images.shared)
        .cloned()
        .collect();
    images.replacement_only = images
        .replacement
        .digests()
        .difference(&images.shared)
        .cloned()
        .collect();
    images
}

fn pull_as(fixture: &Fixture, reference: &LocalRef) {
    fixture
        .store
        .pull(
            &selector(SOURCE),
            Some(&selector(reference.as_str())),
            &fixture.context,
            &PullOptions::default(),
        )
        .unwrap();
}

fn raw_path(fixture: &Fixture, reference: &LocalRef) -> PathBuf {
    let pointer = fs::read_link(fixture.store.layout.ref_entry(reference)).unwrap();
    fixture
        .store
        .layout
        .raw_image(pointer.file_name().unwrap().to_str().unwrap())
}

fn shared_fixture() -> (Fixture, SharedImages, PathBuf) {
    let mut fixture = Fixture::new();
    let images = images();
    select(&mut fixture, &images.original);
    fixture.pull(false).unwrap();
    select(&mut fixture, &images.keeper);
    let keeper = LocalRef::new("cached/keeper:tag").unwrap();
    pull_as(&fixture, &keeper);
    let keeper_raw = raw_path(&fixture, &keeper);
    select(&mut fixture, &images.replacement);
    (fixture, images, keeper_raw)
}

pub(super) fn assert_idle(fixture: &Fixture) {
    assert!(!crate::recovery::pending(&fixture.store.layout).unwrap());
    assert_eq!(
        fs::read_dir(fixture.store.layout.staging())
            .unwrap()
            .count(),
        0
    );
}

fn underlying(error: &Error) -> &Error {
    match error {
        Error::Recovery { source, .. } => underlying(source),
        other => other,
    }
}

#[test]
fn replacement_finishes_cleanup_and_preserves_shared_prefix_content() {
    let mut fixture = Fixture::new();
    let images = images();
    let (short, longer) = prefix_names();
    select(&mut fixture, &images.original);
    pull_as(&fixture, &short);
    let old = raw_path(&fixture, &short);
    select(&mut fixture, &images.keeper);
    pull_as(&fixture, &longer);
    let keeper = raw_path(&fixture, &longer);
    let keeper_before = snapshot(&keeper);
    let shared = fixture.store.layout.blob(images.shared.first().unwrap());
    let shared_before = snapshot(&shared);
    select(&mut fixture, &images.replacement);
    for _ in 0..2 {
        let previous = raw_path(&fixture, &short);
        let report = fixture
            .store
            .pull(
                &selector(SOURCE),
                Some(&selector(short.as_str())),
                &fixture.context,
                &PullOptions {
                    replace: true,
                    notes: None,
                },
            )
            .unwrap();
        assert_eq!(report.resolved_digest, images.replacement.root.digest);
        assert_eq!(report.stored_root, report.resolved_digest);
        assert!(!previous.exists());
        assert_idle(&fixture);
        assert_eq!(snapshot(&keeper), keeper_before);
        assert_eq!(snapshot(&shared), shared_before);
    }
    assert!(!old.exists());
    for digest in &images.original_only {
        assert!(!fixture.store.layout.blob(digest).exists());
    }
    for digest in images.keeper.digests().union(&images.replacement.digests()) {
        assert!(fixture.store.layout.blob(digest).is_file());
    }
    assert_eq!(
        fs::read_dir(fixture.store.layout.raw_images())
            .unwrap()
            .count(),
        2
    );
    assert_eq!(fixture.store.list().unwrap().entries.len(), 2);
    assert!(!fixture.store.check(&CheckScope::All).unwrap().has_errors());
}

#[test]
fn pull_failures_recover_without_harming_prior_or_shared_content() {
    let before_commit = [
        "mutation.before-staging",
        "mutation.snapshot-write",
        "mutation.intent-durable",
        "blob.link",
        "blob.directory-sync",
        "raw_image.file-sync",
        "raw_image.directory-sync",
        "ref.namespace-sync",
        "ref.temporary-sync",
        "ref.rename",
    ];
    let after_commit = [
        "ref.destination-sync",
        "ref.source-sync",
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
    ];
    for (point, published) in before_commit
        .into_iter()
        .map(|p| (p, false))
        .chain(after_commit.into_iter().map(|p| (p, true)))
    {
        let (fixture, images, keeper) = shared_fixture();
        let target = LocalRef::new(DESTINATION).unwrap();
        let old = raw_path(&fixture, &target);
        let old_before = snapshot(&old);
        let keeper_before = snapshot(&keeper);
        let shared = fixture.store.layout.blob(images.shared.first().unwrap());
        let shared_before = snapshot(&shared);
        fsutil::fail_once(
            point,
            if point == "blob.link" {
                libc::EXDEV
            } else {
                libc::EIO
            },
        );
        let error = fixture.pull(true).unwrap_err();
        assert!(
            matches!(
                error,
                Error::Recovery {
                    reference: Some(_),
                    ..
                }
            ),
            "{point}: {error}"
        );
        if point == "blob.link" {
            assert!(
                matches!(underlying(&error), Error::Io { source, .. } if source.raw_os_error() == Some(libc::EXDEV))
            );
        }
        if matches!(point, "ref.destination-sync" | "ref.source-sync") {
            assert!(matches!(
                underlying(&error),
                Error::Durability {
                    change: NamespaceChange::RefPublished,
                    ..
                }
            ));
        }
        assert_eq!(raw_path(&fixture, &target) != old, published, "{point}");
        if !published {
            assert_eq!(snapshot(&old), old_before);
        }
        let calls = fixture.calls().len();
        fs::remove_file(&fixture.context.skopeo).unwrap();
        fixture.store.list().unwrap();
        fixture.store.list().unwrap();
        assert_eq!(fixture.calls().len(), calls);
        assert_idle(&fixture);
        assert_eq!(old.exists(), !published, "{point}");
        for digest in if published {
            &images.original_only
        } else {
            &images.replacement_only
        } {
            assert!(
                !fixture.store.layout.blob(digest).exists(),
                "{point}: {digest}"
            );
        }
        assert_eq!(snapshot(&keeper), keeper_before);
        assert_eq!(snapshot(&shared), shared_before);
    }
}

#[test]
fn acquisition_errors_remain_typed_and_recover_without_registry_requests() {
    for case in ["copy", "root", "platform", "subject", "descriptor"] {
        let (mut fixture, images, keeper) = shared_fixture();
        let old = raw_path(&fixture, &LocalRef::new(DESTINATION).unwrap());
        let old_before = snapshot(&old);
        let keeper_before = snapshot(&keeper);
        match case {
            "copy" => {
                fixture.config["copy_failure"] = json!("cannot preserve digest");
                fixture.save();
            }
            "root" => {
                fixture.set_raw(&images.original.objects[&images.original.root.digest]);
            }
            "platform" => {
                fixture.context.platform = Some(platform("arm64", None));
            }
            // Raw inspection remains supported, but the completed producer output
            // includes a declaration that acquired_content must reject privately.
            "subject" | "descriptor" => {
                let changed = images.replacement.with_manifest_change(|value| {
                    if case == "subject" {
                        value["subject"] = value["config"].clone();
                    } else {
                        value["layers"][0]["urls"] = json!([123]);
                    }
                });
                select(&mut fixture, &changed);
                fixture.set_raw(&images.replacement.objects[&images.replacement.root.digest]);
            }
            _ => unreachable!(),
        }
        let error = fixture.pull(true).unwrap_err();
        assert!(
            matches!(
                error,
                Error::Recovery {
                    phase: "acquiring",
                    ..
                }
            ),
            "{case}: {error}"
        );
        if case == "copy" {
            assert!(matches!(
                underlying(&error),
                Error::ToolFailed {
                    operation: "pull image",
                    ..
                }
            ));
        } else {
            assert!(
                matches!(underlying(&error), Error::InvalidObject { .. }),
                "{case}: {error}"
            );
        }
        assert_eq!(snapshot(&old), old_before);
        assert_eq!(snapshot(&keeper), keeper_before);
        let calls = fixture.calls().len();
        fs::remove_file(&fixture.context.skopeo).unwrap();
        fixture.store.list().unwrap();
        fixture.store.list().unwrap();
        assert_idle(&fixture);
        assert_eq!(fixture.calls().len(), calls);
        assert_eq!(snapshot(&old), old_before);
        assert_eq!(snapshot(&keeper), keeper_before);
    }
}

#[test]
fn damaged_old_identities_or_dependencies_are_preserved() {
    use std::os::unix::fs::symlink;
    for damage in [
        "file",
        "malformed",
        "escape",
        "missing-wrapper",
        "missing-index",
        "missing-manifest",
        "unsupported",
    ] {
        let (fixture, images, keeper) = shared_fixture();
        let reference = LocalRef::new(DESTINATION).unwrap();
        let old = raw_path(&fixture, &reference);
        let entry = fixture.store.layout.ref_entry(&reference);
        match damage {
            "file" | "malformed" | "escape" => {
                fs::remove_file(&entry).unwrap();
                match damage {
                    "file" => fs::write(&entry, b"preserve damaged entry").unwrap(),
                    "malformed" => symlink("missing", &entry).unwrap(),
                    _ => symlink(fixture.temporary.path().join("outside"), &entry).unwrap(),
                }
            }
            "missing-wrapper" => {
                fs::rename(&old, fixture.temporary.path().join("saved-wrapper")).unwrap()
            }
            "missing-index" => fs::remove_file(old.join("index.json")).unwrap(),
            "missing-manifest" => {
                fs::remove_file(fixture.store.layout.blob(&images.original.root.digest)).unwrap()
            }
            "unsupported" => {
                let path = fixture.store.layout.blob(&images.original.root.digest);
                let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                value["layers"][0]["mediaType"] = json!("unsupported");
                let bytes = serde_json::to_vec(&value).unwrap();
                fs::write(path, &bytes).unwrap();
                let index = old.join("index.json");
                let mut value: Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
                value["manifests"][0]["size"] = json!(bytes.len());
                fs::write(index, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            _ => unreachable!(),
        }
        let entry_before = snapshot(&entry);
        let old_before = old.exists().then(|| snapshot(&old));
        let blobs_before = snapshot(&fixture.store.layout.blobs());
        let keeper_before = snapshot(&keeper);
        assert!(fixture.pull(true).is_err(), "{damage}");
        fixture.store.list().unwrap();
        assert_idle(&fixture);
        assert_eq!(snapshot(&entry), entry_before, "{damage}");
        assert_eq!(old.exists().then(|| snapshot(&old)), old_before, "{damage}");
        assert_eq!(
            snapshot(&fixture.store.layout.blobs()),
            blobs_before,
            "{damage}"
        );
        assert_eq!(snapshot(&keeper), keeper_before, "{damage}");
    }
}

#[test]
fn metadata_damage_does_not_block_reclamation_and_unknown_live_dependencies_do() {
    for damaged_dependencies in [false, true] {
        let (fixture, _, keeper) = shared_fixture();
        let old = raw_path(&fixture, &LocalRef::new(DESTINATION).unwrap());
        fs::write(old.join("metadata.json"), b"invalid cache").unwrap();
        fs::write(keeper.join("metadata.json"), b"invalid cache").unwrap();
        let keeper_before = snapshot(&keeper);
        if damaged_dependencies {
            let root = crate::oci::single_root(&keeper).unwrap();
            fs::remove_file(fixture.store.layout.blob(&root.digest)).unwrap();
            assert!(fixture.pull(true).is_err());
            assert!(old.exists());
            let before = snapshot(fixture.store.config().root());
            assert!(fixture.store.list().is_err());
            assert_eq!(snapshot(fixture.store.config().root()), before);
        } else {
            fixture.pull(true).unwrap();
            assert!(!old.exists());
            assert_idle(&fixture);
        }
        assert_eq!(snapshot(&keeper), keeper_before);
    }
}

struct PausedPull(std::process::Child);
impl Drop for PausedPull {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn terminated_pull_recovers_at_publication_and_cleanup_boundaries() {
    use std::io::{BufRead, BufReader};
    for point in [
        "mutation.intent-durable",
        "ref.destination-sync",
        "recovery.cleaning-durable",
        "recovery.removed",
        "recovery.complete-durable",
    ] {
        let (fixture, images, keeper) = shared_fixture();
        let reference = LocalRef::new(DESTINATION).unwrap();
        let old = raw_path(&fixture, &reference);
        let keeper_before = snapshot(&keeper);
        let mut worker = PausedPull(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "pull::tests::pull_worker",
                    "--nocapture",
                ])
                .env("HPC_PULL_TEST_DIRECTORY", fixture.temporary.path())
                .env("HPC_PULL_PAUSE", point)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let stdout = worker.0.stdout.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let line = receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("pull checkpoint readiness");
            if line == "PUBLICATION_PAUSED" {
                break;
            }
        }
        assert!(!try_exclusive(&fixture.store.layout.store_lock()));
        worker.0.kill().unwrap();
        worker.0.wait().unwrap();
        let published = point != "mutation.intent-durable";
        assert_eq!(raw_path(&fixture, &reference) != old, published);
        let calls = fixture.calls().len();
        fixture.store.list().unwrap();
        fixture.store.list().unwrap();
        assert_idle(&fixture);
        assert_eq!(old.exists(), !published);
        let guard = fixture.store.prepare(Operation::Export).unwrap();
        assert_eq!(
            refs::read(&guard, &reference).unwrap().root.digest,
            if published {
                images.replacement.root.digest
            } else {
                images.original.root.digest
            }
        );
        assert_eq!(snapshot(&keeper), keeper_before);
        assert_eq!(fixture.calls().len(), calls);
    }
}

#[test]
fn pull_reclamation_preserves_every_nested_keeper_branch() {
    for damaged in [false, true] {
        let mut fixture = Fixture::new();
        let images = NestedImages::new();
        let first = images
            .first
            .with_manifest_change(|value| value["mediaType"] = json!(graph::MANIFEST));
        select(&mut fixture, &first);
        fixture.pull(false).unwrap();
        let old = raw_path(&fixture, &LocalRef::new(DESTINATION).unwrap());
        let keeper = LocalRef::new("nested/keeper:tag").unwrap();
        let raw = publish_image(&fixture.store, &keeper, &images.top, false);
        let keeper_before = snapshot(&raw);
        let shared_before = snapshot(&fixture.store.layout.blob(&images.layer.digest));
        if damaged {
            fs::remove_file(fixture.store.layout.blob(&images.nested.digest)).unwrap();
        }
        select(&mut fixture, &self::images().replacement);
        let result = fixture.pull(true);
        if damaged {
            assert!(result.is_err());
            assert!(old.exists());
            assert!(fixture.store.list().is_err());
        } else {
            result.unwrap();
            assert!(!old.exists());
            for digest in images.top.digests() {
                assert!(fixture.store.layout.blob(&digest).exists());
            }
            // The primitive keeper fixture intentionally leaves unrecorded
            // staging; public pull must preserve that unrelated tree.
            assert!(!crate::recovery::pending(&fixture.store.layout).unwrap());
        }
        assert_eq!(snapshot(&raw), keeper_before);
        assert_eq!(
            snapshot(&fixture.store.layout.blob(&images.layer.digest)),
            shared_before
        );
    }
}
