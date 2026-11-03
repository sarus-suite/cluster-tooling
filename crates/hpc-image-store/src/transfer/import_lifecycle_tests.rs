//! Public-import coverage for the recorded lifecycle. Producers use the same
//! synthetic object bytes as the storage/recovery fixtures.
use super::*;
use crate::{
    test_snapshot::snapshot,
    test_support::{NestedImages, SharedImages, prefix_names, publish_image},
};

fn raw_path(fixture: &Fixture) -> PathBuf {
    let pointer = fs::read_link(fixture.store.layout.ref_entry(&fixture.reference)).unwrap();
    fixture
        .store
        .layout
        .raw_image(pointer.file_name().unwrap().to_str().unwrap())
}

fn shared_fixture() -> (Fixture, SharedImages, LocalRef, PathBuf) {
    let mut fixture = Fixture::new();
    let images = SharedImages::new();
    fixture.select_image(&images.original);
    fixture.import(false).unwrap();
    let target = fixture.reference.clone();
    fixture.reference = LocalRef::new("example/keeper:tag").unwrap();
    fixture.select_image(&images.keeper);
    fixture.import(false).unwrap();
    let keeper = fixture.reference.clone();
    let keeper_raw = raw_path(&fixture);
    fixture.reference = target;
    fixture.select_image(&images.replacement);
    (fixture, images, keeper, keeper_raw)
}

#[test]
fn unsupported_staged_declarations_are_rejected_before_fresh_or_replacement_publication() {
    for replace in [false, true] {
        for (case, expected) in [
            ("subject", "subject"),
            ("artifact", "artifact"),
            ("layer-role", "unsupported media type"),
            ("media-type", "disagrees"),
            ("urls", "urls"),
            ("descriptor-subject", "subject"),
            ("conflicting-size", "conflicting"),
            ("descriptor-json", "config"),
            ("root-subject", "subject"),
        ] {
            let mut fixture = Fixture::new();
            let images = SharedImages::new();
            let target = fixture.reference.clone();
            fixture.reference = LocalRef::new("example/keeper:tag").unwrap();
            fixture.select_image(&images.keeper);
            fixture.import(false).unwrap();
            fixture.reference = target;
            if replace {
                fixture.select_image(&images.original);
                fixture.import(false).unwrap();
            }
            let mut image = images
                .replacement
                .with_manifest_change(|manifest| match case {
                    "subject" => manifest["subject"] = manifest["config"].clone(),
                    "artifact" => manifest["artifactType"] = json!("application/example"),
                    "layer-role" => manifest["layers"][0]["mediaType"] = json!(OCI_CONFIG),
                    "media-type" => manifest["mediaType"] = json!(crate::graph::INDEX),
                    "urls" => manifest["layers"][0]["urls"] = json!([123]),
                    "descriptor-subject" => manifest["layers"][0]["subject"] = json!({}),
                    "conflicting-size" => {
                        let mut other = manifest["layers"][0].clone();
                        other["size"] = json!(other["size"].as_u64().unwrap() + 1);
                        manifest["layers"].as_array_mut().unwrap().push(other);
                    }
                    "descriptor-json" => manifest["config"] = json!(false),
                    "root-subject" => (),
                    _ => unreachable!(),
                });
            if case == "root-subject" {
                image.root.extra.insert("subject".into(), json!({}));
            }
            fixture.select_image(&image);
            let refs = snapshot(&fixture.store.layout.root().join("refs"));
            let raw = snapshot(&fixture.store.layout.raw_images());
            let blobs = snapshot(&fixture.store.layout.blobs());
            let error = fixture.import(replace).unwrap_err();
            assert!(
                matches!(&error, Error::Recovery { phase: "acquiring", change: None, source, .. }
                if matches!(source.as_ref(), Error::InvalidObject { path, reason }
                    if path.starts_with(fixture.store.layout.staging()) && reason.contains(expected))),
                "{case}, replace={replace}: {error}"
            );
            let record: crate::mutation::Record =
                serde_json::from_slice(&fs::read(fixture.store.layout.pending_mutation()).unwrap())
                    .unwrap();
            assert_eq!(record.phase, crate::mutation::Phase::Acquiring);
            assert!(record.new_raw.is_none() && record.owned.is_empty());
            assert_eq!(snapshot(&fixture.store.layout.root().join("refs")), refs);
            assert_eq!(snapshot(&fixture.store.layout.raw_images()), raw);
            assert_eq!(snapshot(&fixture.store.layout.blobs()), blobs);
            let calls = fixture.log().len();
            // Aborted acquisition only removes its private tree. It must not
            // reparse the rejected declaration, nor scan other committed refs.
            fsutil::fail_once("dependencies.observe", libc::EIO);
            let result = fixture.store.list();
            fsutil::clear_failure();
            assert!(!result.unwrap().has_errors());
            assert!(!fixture.store.list().unwrap().has_errors());
            assert_import_idle(&fixture.store);
            assert_eq!(fixture.log().len(), calls);
            assert_eq!(snapshot(&fixture.store.layout.root().join("refs")), refs);
            assert_eq!(snapshot(&fixture.store.layout.raw_images()), raw);
            assert_eq!(snapshot(&fixture.store.layout.blobs()), blobs);
        }
    }
}

#[test]
fn public_replacement_reclaims_unique_content_and_preserves_shared_prefixes() {
    let mut fixture = Fixture::new();
    let images = SharedImages::new();
    let (short, long) = prefix_names();
    fixture.reference = short.clone();
    fixture.select_image(&images.original);
    fixture.import(false).unwrap();
    let retired = raw_path(&fixture);
    fixture.reference = long.clone();
    fixture.select_image(&images.keeper);
    fixture.import(false).unwrap();
    let keeper_raw = raw_path(&fixture);
    let keeper_before = snapshot(&keeper_raw);
    let shared = images.shared.first().unwrap();
    let shared_before = snapshot(&fixture.store.layout.blob(shared));
    let lock_before = snapshot(&fixture.store.layout.store_lock());
    fixture.reference = short;
    fixture.select_image(&images.replacement);
    fsutil::track_opens();
    let report = fixture.import(true).unwrap();
    let opened = fsutil::take_opens();
    for image in [&images.original, &images.keeper] {
        for digest in image.digests() {
            if digest != image.root.digest && !images.replacement.objects.contains_key(&digest) {
                assert!(
                    !opened.contains(&fixture.store.layout.blob(&digest)),
                    "dependency traversal opened terminal {digest}"
                );
            }
        }
    }
    assert!(!opened.contains(&retired.join("metadata.json")));
    assert!(!opened.contains(&keeper_raw.join("metadata.json")));
    assert_eq!(report.stored_root, images.replacement.root.digest);
    assert!(!retired.exists());
    for digest in &images.original_only {
        assert!(!fixture.store.layout.blob(digest).exists());
    }
    for digest in images.keeper.digests().union(&images.replacement.digests()) {
        assert!(fixture.store.layout.blob(digest).is_file());
    }
    assert_eq!(snapshot(&keeper_raw), keeper_before);
    assert_eq!(snapshot(&fixture.store.layout.blob(shared)), shared_before);
    assert_eq!(snapshot(&fixture.store.layout.store_lock()), lock_before);
    assert!(fixture.store.layout.ref_entry(&long).is_symlink());
    assert_import_idle(&fixture.store);
    let previous = raw_path(&fixture);
    fixture.import(true).unwrap();
    assert!(!previous.exists());
    assert_eq!(
        fs::read_dir(fixture.store.layout.raw_images())
            .unwrap()
            .count(),
        2
    );
    assert_eq!(snapshot(&fixture.store.layout.blob(shared)), shared_before);
    assert_import_idle(&fixture.store);
}

#[test]
fn public_import_failures_recover_without_harming_prior_or_shared_content() {
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
        let (fixture, images, _, keeper_raw) = shared_fixture();
        let old = raw_path(&fixture);
        let old_before = snapshot(&old);
        let keeper_before = snapshot(&keeper_raw);
        let shared = images.shared.first().unwrap();
        let shared_before = snapshot(&fixture.store.layout.blob(shared));
        fsutil::fail_once(
            point,
            if point == "blob.link" {
                libc::EXDEV
            } else {
                libc::EIO
            },
        );
        let error = fixture.import(true).unwrap_err();
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
        let current = raw_path(&fixture);
        assert_eq!(current != old, published, "{point}: {error}");
        if !published {
            assert_eq!(snapshot(&old), old_before);
        }
        assert_eq!(snapshot(&keeper_raw), keeper_before);
        assert_eq!(snapshot(&fixture.store.layout.blob(shared)), shared_before);
        let calls = fixture.log().len();
        fixture.store.list().unwrap();
        fixture.store.list().unwrap();
        assert_eq!(fixture.log().len(), calls);
        assert_import_idle(&fixture.store);
        assert_eq!(old.exists(), !published, "{point}");
        let discarded = if published {
            &images.original_only
        } else {
            &images.replacement_only
        };
        for digest in discarded {
            assert!(
                !fixture.store.layout.blob(digest).exists(),
                "{point}: {digest}"
            );
        }
        assert_eq!(snapshot(&keeper_raw), keeper_before);
        assert_eq!(snapshot(&fixture.store.layout.blob(shared)), shared_before);
    }
}

#[test]
fn damaged_old_identity_or_dependencies_are_rejected_and_preserved() {
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
        let (fixture, images, _, keeper_raw) = shared_fixture();
        let old = raw_path(&fixture);
        let entry = fixture.store.layout.ref_entry(&fixture.reference);
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
                fs::rename(&old, fixture.temporary.path().join("saved-wrapper")).unwrap();
            }
            "missing-index" => {
                fs::remove_file(old.join("index.json")).unwrap();
            }
            "missing-manifest" => {
                fs::remove_file(fixture.store.layout.blob(&images.original.root.digest)).unwrap();
            }
            "unsupported" => {
                let path = fixture.store.layout.blob(&images.original.root.digest);
                let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                value["layers"][0]["mediaType"] = json!("unsupported");
                let bytes = serde_json::to_vec(&value).unwrap();
                fs::write(&path, &bytes).unwrap();
                let path = old.join("index.json");
                let mut index: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                index["manifests"][0]["size"] = json!(bytes.len());
                fs::write(path, serde_json::to_vec(&index).unwrap()).unwrap();
            }
            _ => unreachable!(),
        }
        let before = snapshot(&entry);
        let raw_before = old.exists().then(|| snapshot(&old));
        let blobs_before = snapshot(&fixture.store.layout.blobs());
        let keeper_before = snapshot(&keeper_raw);
        assert!(fixture.import(true).is_err(), "{damage}");
        assert_eq!(snapshot(&entry), before, "{damage}");
        assert_eq!(old.exists().then(|| snapshot(&old)), raw_before, "{damage}");
        assert_eq!(
            snapshot(&fixture.store.layout.blobs()),
            blobs_before,
            "{damage}"
        );
        // Acquiring records have no live additions, so even damaged old refs do
        // not prevent safe cleanup of this failed attempt's private staging.
        fixture.store.list().unwrap();
        assert_import_idle(&fixture.store);
        assert_eq!(snapshot(&entry), before);
        assert_eq!(snapshot(&keeper_raw), keeper_before);
    }
}

#[test]
fn replacement_uses_dependencies_despite_damaged_display_metadata() {
    let (fixture, images, _, keeper_raw) = shared_fixture();
    let old = raw_path(&fixture);
    fs::write(old.join("metadata.json"), b"invalid cache").unwrap();
    fs::write(keeper_raw.join("metadata.json"), b"invalid cache").unwrap();
    let keeper_before = snapshot(&keeper_raw);
    fixture.import(true).unwrap();
    assert!(!old.exists());
    for digest in &images.original_only {
        assert!(!fixture.store.layout.blob(digest).exists());
    }
    assert_eq!(snapshot(&keeper_raw), keeper_before);
    assert_import_idle(&fixture.store);
}

#[test]
fn public_import_preserves_nested_branches_and_blocks_unprovable_reclamation() {
    for damaged in [false, true] {
        let mut fixture = Fixture::new();
        let images = NestedImages::new();
        // Public acquisition remains manifest-only; install an internal index
        // fixture as another live ref, then replace a public manifest import.
        fixture.select_image(&images.first);
        fixture.import(false).unwrap();
        let old = raw_path(&fixture);
        let keeper = LocalRef::new("nested/keeper:tag").unwrap();
        let raw = publish_image(&fixture.store, &keeper, &images.top, false);
        let keeper_before = snapshot(&raw);
        let shared_before = snapshot(&fixture.store.layout.blob(&images.layer.digest));
        if damaged {
            fs::remove_file(fixture.store.layout.blob(&images.nested.digest)).unwrap();
        }
        let replacement = SharedImages::new();
        fixture.select_image(&replacement.replacement);
        let result = fixture.import(true);
        if damaged {
            assert!(result.is_err());
            assert!(crate::recovery::pending(&fixture.store.layout).unwrap());
            assert!(old.exists());
            assert!(fixture.store.list().is_err());
        } else {
            result.unwrap();
            assert!(!old.exists());
            for digest in images.top.digests() {
                assert!(fixture.store.layout.blob(&digest).exists());
            }
            assert_eq!(snapshot(&raw), keeper_before);
            assert!(!crate::recovery::pending(&fixture.store.layout).unwrap());
        }
        assert_eq!(
            snapshot(&fixture.store.layout.blob(&images.layer.digest)),
            shared_before
        );
    }
}

struct PausedImport(std::process::Child);
impl Drop for PausedImport {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn terminated_public_import_recovers_at_publication_and_cleanup_boundaries() {
    use std::io::{BufRead, BufReader};
    for point in [
        "mutation.intent-durable",
        "ref.destination-sync",
        "recovery.cleaning-durable",
        "recovery.removed",
        "recovery.complete-durable",
    ] {
        let (fixture, images, _, keeper_raw) = shared_fixture();
        let old = raw_path(&fixture);
        let keeper_before = snapshot(&keeper_raw);
        let mut worker = PausedImport(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "transfer::tests::transfer_worker",
                    "--nocapture",
                ])
                .env("HPC_TRANSFER_TEST_DIRECTORY", fixture.temporary.path())
                .env("HPC_TRANSFER_TEST_OPERATION", "import")
                .env("HPC_IMPORT_PAUSE", point)
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
                .expect("import checkpoint readiness");
            if line == "PUBLICATION_PAUSED" {
                break;
            }
        }
        assert!(!try_exclusive(&fixture.store.layout.store_lock()));
        worker.0.kill().unwrap();
        worker.0.wait().unwrap();
        let published = point != "mutation.intent-durable";
        assert_eq!(raw_path(&fixture) != old, published);
        fixture.store.list().unwrap();
        fixture.store.list().unwrap();
        assert_import_idle(&fixture.store);
        assert_eq!(old.exists(), !published);
        let guard = fixture.store.prepare(Operation::Export).unwrap();
        assert_eq!(
            refs::read(&guard, &fixture.reference).unwrap().root.digest,
            if published {
                images.replacement.root.digest
            } else {
                images.original.root.digest
            }
        );
        assert_eq!(snapshot(&keeper_raw), keeper_before);
    }
}
