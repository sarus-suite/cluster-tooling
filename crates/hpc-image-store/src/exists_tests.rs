//! Presence acceptance reuses publication fixtures and process coordination.
use super::*;
use crate::{
    CheckScope, ImageSelector, locks,
    mutation::{Kind, Mutation},
    test_snapshot::snapshot,
    test_support::prefix_names,
};
use serde_json::{Value, json};
use std::{io, os::unix::fs::PermissionsExt};

fn selector(value: impl AsRef<str>) -> ImageSelector {
    ImageSelector::new(value.as_ref()).unwrap()
}

#[test]
fn exists_never_creates_or_resumes_setup() {
    for case in [
        "missing",
        "ancestors",
        "empty",
        "interrupted",
        "old",
        "unsupported",
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(if case == "ancestors" {
            "absent/parent/store"
        } else {
            "store"
        });
        let handle = store(&path);
        if case == "empty" {
            fs::create_dir(&path).unwrap();
        } else if matches!(case, "interrupted" | "old" | "unsupported") {
            drop(handle.prepare(Operation::Import).unwrap());
            if case == "interrupted" {
                fs::rename(path.join("store.json"), path.join(".store-init.json")).unwrap();
            } else {
                fs::write(
                    path.join("store.json"),
                    if case == "old" {
                        b"{\"version\":1}".as_slice()
                    } else {
                        b"{\"version\":99}".as_slice()
                    },
                )
                .unwrap();
            }
        }
        let before = snapshot(temporary.path());
        for selected in [
            selector(reference().as_str()),
            selector(format!("analysis/tool@{}", root(Image::One))),
        ] {
            assert!(handle.exists(&selected).is_err(), "{case}");
            assert!(handle.resolve(&selected).is_err(), "{case}");
        }
        assert_eq!(snapshot(temporary.path()), before, "{case}");
    }
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    drop(handle.prepare(Operation::Import).unwrap());
    assert!(!handle.exists(&selector(reference().as_str())).unwrap());
    assert_eq!(
        handle.resolve(&selector(reference().as_str())).unwrap(),
        None
    );
    assert!(
        !handle
            .exists(&selector(format!("analysis/tool@{}", root(Image::One))))
            .unwrap()
    );
    assert_eq!(
        handle
            .resolve(&selector(format!("analysis/tool@{}", root(Image::One))))
            .unwrap(),
        None
    );
}

#[test]
fn resolution_reads_exact_tagged_roots_and_removes_tags() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    let (short, long) = prefix_names();
    publish(&handle, &long, Image::One, false);
    assert_eq!(handle.resolve(&selector(short.as_str())).unwrap(), None);
    for (name, image) in [
        (long, Image::One),
        (
            LocalRef::new("REGISTRY:5000/team/tool:Release").unwrap(),
            Image::Index,
        ),
    ] {
        publish(&handle, &name, image, true);
        let expected = format!("{}@{}", name.name(), root(image));
        assert_eq!(
            handle.resolve(&selector(name.as_str())).unwrap(),
            Some(expected.clone())
        );
        assert_eq!(
            handle.resolve(&selector(&expected)).unwrap(),
            Some(expected)
        );
    }
}

#[test]
fn names_require_terminal_entries_and_ignore_pointer_damage() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    let (short, long) = prefix_names();
    publish(&handle, &long, Image::One, false);
    assert!(!handle.exists(&selector(short.as_str())).unwrap());
    assert!(handle.exists(&selector(long.as_str())).unwrap());
    publish(&handle, &short, Image::One, false);
    assert!(handle.exists(&selector(short.as_str())).unwrap());
    handle.remove(&short).unwrap();
    assert!(!handle.exists(&selector(short.as_str())).unwrap());
    assert!(handle.exists(&selector(long.as_str())).unwrap());
    let pointer = handle.layout.ref_entry(&long);
    for case in ["dangling", "escape", "file", "directory"] {
        fs::remove_file(&pointer).unwrap();
        match case {
            "dangling" => symlink(
                handle.layout.raw_image_target(&long, "raw-AbCdEf123456"),
                &pointer,
            )
            .unwrap(),
            "escape" => symlink("/outside", &pointer).unwrap(),
            "file" => fs::write(&pointer, b"preserve").unwrap(),
            _ => fs::create_dir(&pointer).unwrap(),
        }
        let before = snapshot(temporary.path());
        assert!(handle.exists(&selector(long.as_str())).unwrap(), "{case}");
        assert!(handle.resolve(&selector(long.as_str())).is_err(), "{case}");
        assert!(
            handle
                .check(&CheckScope::Image(selector(long.as_str())))
                .unwrap()
                .has_errors()
        );
        assert!(matches!(
            handle.exists(&selector(format!("{}@{}", long.name(), root(Image::One)))),
            Err(Error::ResolutionIncomplete { .. })
        ));
        assert_eq!(snapshot(temporary.path()), before);
        if case == "directory" {
            fs::remove_dir(&pointer).unwrap();
        }
    }
}

#[test]
fn clean_lookup_reads_only_requested_identity_and_reserves_nothing() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    let raw = publish(&handle, &reference(), Image::Index, false);
    let other = LocalRef::new("other/tool:t").unwrap();
    let other_raw = publish(&handle, &other, Image::Two, false);
    fs::write(other_raw.join("index.json"), b"{").unwrap();
    fs::remove_file(raw.join("metadata.json")).unwrap();
    fs::remove_file(raw.join("oci-layout")).unwrap();
    fs::remove_dir(raw.join("blobs")).unwrap();
    for entry in fs::read_dir(handle.layout.blobs()).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
    let before = snapshot(temporary.path());
    for (selected, expected_index) in [
        (selector(reference().as_str()), None),
        (selector("absent:t"), None),
        (
            selector(format!("analysis/tool:ignored@{}", root(Image::Index))),
            Some(raw.join("index.json")),
        ),
    ] {
        fsutil::track_opens();
        let present = handle.exists(&selected).unwrap();
        assert_eq!(present, selected.name() != "absent");
        let opened = fsutil::take_opens();
        assert!(
            opened.iter().all(
                |path| path == &handle.layout.marker() || Some(path) == expected_index.as_ref()
            ),
            "{opened:?}"
        );
        if let Some(index) = expected_index {
            assert!(opened.contains(&index));
        }
    }
    for point in ["refs.metadata-read", "graph.observe"] {
        fsutil::fail_once(point, libc::EIO);
        assert!(
            handle
                .exists(&selector(format!("analysis/tool@{}", root(Image::Index))))
                .unwrap()
        );
        assert!(fsutil::fault(point).is_err(), "lookup consumed {point}");
    }
    for selected in [
        selector(reference().as_str()),
        selector(format!("analysis/tool:ignored@{}", root(Image::Index))),
    ] {
        fsutil::track_opens();
        assert_eq!(
            handle.resolve(&selected).unwrap(),
            Some(format!("analysis/tool@{}", root(Image::Index)))
        );
        let opened = fsutil::take_opens();
        assert!(opened.contains(&raw.join("index.json")));
        assert!(
            opened
                .iter()
                .all(|path| path == &handle.layout.marker() || path == &raw.join("index.json")),
            "{opened:?}"
        );
    }
    for point in ["refs.metadata-read", "graph.observe"] {
        fsutil::fail_once(point, libc::EIO);
        assert!(
            handle
                .resolve(&selector(reference().as_str()))
                .unwrap()
                .is_some()
        );
        assert!(fsutil::fault(point).is_err(), "resolution consumed {point}");
    }
    assert_eq!(snapshot(temporary.path()), before);
}

#[test]
fn digest_lookup_preserves_literal_scope_precedence_and_root_identity() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    let reference = LocalRef::new("registry:5000/tool:aaa").unwrap();
    let raw = publish(&handle, &reference, Image::One, false);
    fs::write(raw.join("metadata.json"), b"{").unwrap();
    assert_eq!(
        handle
            .resolve(&selector(format!(
                "registry:5000/tool:missing@{}",
                root(Image::One)
            )))
            .unwrap(),
        Some(format!("registry:5000/tool@{}", root(Image::One)))
    );
    assert!(
        handle
            .exists(&selector(format!(
                "registry:5000/tool:missing@{}",
                root(Image::One)
            )))
            .unwrap()
    );
    for repository in [
        "tool",
        "registry/tool",
        "registry:5000/other",
        "REGISTRY:5000/tool",
    ] {
        assert_eq!(
            handle
                .resolve(&selector(format!("{repository}@{}", root(Image::One))))
                .unwrap(),
            None
        );
        assert!(
            !handle
                .exists(&selector(format!("{repository}@{}", root(Image::One))))
                .unwrap()
        );
    }
    for digest in [digest(CONFIG_DIGEST), root(Image::Two)] {
        assert_eq!(
            handle
                .resolve(&selector(format!("registry:5000/tool:aaa@{digest}")))
                .unwrap(),
            None
        );
        assert!(
            !handle
                .exists(&selector(format!("registry:5000/tool:aaa@{digest}")))
                .unwrap()
        );
    }
    publish(
        &handle,
        &LocalRef::new("registry:5000/tool:bbb").unwrap(),
        Image::One,
        false,
    );
    assert_eq!(
        handle
            .resolve(&selector(format!(
                "registry:5000/tool@{}",
                root(Image::One)
            )))
            .unwrap(),
        Some(format!("registry:5000/tool@{}", root(Image::One)))
    );
    assert!(
        handle
            .exists(&selector(format!(
                "registry:5000/tool@{}",
                root(Image::One)
            )))
            .unwrap()
    );
}

#[test]
fn digest_damage_cannot_prove_absence_but_a_definite_match_wins() {
    for case in [
        "missing",
        "json",
        "schema",
        "index-type",
        "root-type",
        "roots",
        "digest",
        "size",
        "index-symlink",
        "raw-symlink",
        "pointer",
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        let broken = LocalRef::new("analysis/tool:aaa").unwrap();
        let raw = publish(&handle, &broken, Image::One, false);
        let index = raw.join("index.json");
        match case {
            "missing" => fs::remove_file(&index).unwrap(),
            "json" => fs::write(&index, b"{").unwrap(),
            "index-symlink" => {
                fs::remove_file(&index).unwrap();
                symlink("/outside", &index).unwrap();
            }
            "raw-symlink" => {
                let retired = raw.with_extension("retired");
                fs::rename(&raw, &retired).unwrap();
                symlink(&retired, &raw).unwrap();
            }
            "pointer" => {
                let pointer = handle.layout.ref_entry(&broken);
                fs::remove_file(&pointer).unwrap();
                symlink("/outside", pointer).unwrap();
            }
            _ => {
                let mut value: Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
                match case {
                    "schema" => value["schemaVersion"] = json!(99),
                    "index-type" => value["mediaType"] = json!("example/unknown"),
                    "root-type" => value["manifests"][0]["mediaType"] = json!("example/unknown"),
                    "roots" => value["manifests"] = json!([]),
                    "digest" => value["manifests"][0]["digest"] = json!("sha256:bad"),
                    _ => value["manifests"][0]["size"] = json!("invalid"),
                }
                fs::write(index, serde_json::to_vec(&value).unwrap()).unwrap();
            }
        }
        let before = snapshot(temporary.path());
        assert!(
            handle.resolve(&selector(broken.as_str())).is_err(),
            "{case}"
        );
        for digest in [root(Image::One), root(Image::Two)] {
            assert!(
                matches!(handle.resolve(&selector(format!("analysis/tool@{digest}"))), Err(Error::ResolutionIncomplete { diagnostics, .. }) if !diagnostics.is_empty()),
                "{case}"
            );
            let error = handle
                .exists(&selector(format!("analysis/tool@{digest}")))
                .unwrap_err();
            assert!(
                matches!(error, Error::ResolutionIncomplete { diagnostics, .. } if !diagnostics.is_empty()),
                "{case}"
            );
        }
        assert!(
            !handle
                .exists(&selector(format!("another/repo@{}", root(Image::One))))
                .unwrap()
        );
        assert_eq!(
            handle
                .resolve(&selector(format!("another/repo@{}", root(Image::One))))
                .unwrap(),
            None
        );
        assert_eq!(snapshot(temporary.path()), before);
        publish(
            &handle,
            &LocalRef::new("analysis/tool:bbb").unwrap(),
            Image::One,
            false,
        );
        assert_eq!(
            handle
                .resolve(&selector(format!("analysis/tool@{}", root(Image::One))))
                .unwrap(),
            Some(format!("analysis/tool@{}", root(Image::One))),
            "{case}"
        );
        assert!(
            handle
                .exists(&selector(format!("analysis/tool@{}", root(Image::One))))
                .unwrap(),
            "{case}"
        );
    }
}

#[test]
fn incomplete_discovery_is_an_error_unless_a_match_is_known() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    publish(&handle, &reference(), Image::One, false);
    fs::write(handle.layout.root().join("refs/unknown"), b"preserve").unwrap();
    assert!(
        handle
            .resolve(&selector(format!("analysis/tool@{}", root(Image::One))))
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        handle.resolve(&selector(format!("absent@{}", root(Image::One)))),
        Err(Error::ResolutionIncomplete { .. })
    ));
    fsutil::fail_once("discovery.enumerate", libc::EIO);
    assert!(matches!(
        handle.resolve(&selector(format!("analysis/tool@{}", root(Image::One)))),
        Err(Error::ResolutionIncomplete { .. })
    ));
    assert!(
        handle
            .exists(&selector(format!("analysis/tool@{}", root(Image::One))))
            .unwrap()
    );
    assert!(matches!(
        handle.exists(&selector(format!("absent@{}", root(Image::One)))),
        Err(Error::ResolutionIncomplete { .. })
    ));
    fsutil::fail_once("discovery.enumerate", libc::EIO);
    assert!(matches!(
        handle.exists(&selector(format!("analysis/tool@{}", root(Image::One)))),
        Err(Error::ResolutionIncomplete { .. })
    ));
}

#[test]
fn exists_recovers_recorded_work_before_observing_and_blocks_on_unsafe_recovery() {
    for resolve in [false, true] {
        let observe = |handle: &Store, selected: &ImageSelector| {
            if resolve {
                handle
                    .resolve(selected)
                    .map(|reference| reference.is_some())
            } else {
                handle.exists(selected)
            }
        };
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        publish(&handle, &reference(), Image::One, false);
        {
            let guard = handle.prepare(Operation::Import).unwrap();
            let mutation = Mutation::begin(&guard, &reference(), Kind::Import).unwrap();
            drop(mutation.stage().unwrap());
        }
        for _ in 0..2 {
            assert!(observe(&handle, &selector(reference().as_str())).unwrap());
        }
        assert!(!crate::recovery::pending(&handle.layout).unwrap());
        fsutil::fail_once("ref.remove-sync", libc::EIO);
        assert!(handle.remove(&reference()).is_err());
        assert!(crate::recovery::pending(&handle.layout).unwrap());
        for _ in 0..2 {
            assert!(!observe(&handle, &selector(reference().as_str())).unwrap());
        }
        assert!(!crate::recovery::pending(&handle.layout).unwrap());
        fs::write(handle.layout.root().join("pending-mutation.json"), b"{").unwrap();
        let before = snapshot(temporary.path());
        assert!(matches!(
            observe(&handle, &selector("absent:t")),
            Err(Error::Recovery { .. })
        ));
        assert_eq!(snapshot(temporary.path()), before);
    }
}

#[test]
fn exists_holds_shared_access_through_name_and_digest_lookup() {
    for action in [
        "exists-held-name",
        "exists-held-digest",
        "exists-scan-remove",
        "exists-scan-replace",
        "resolve-held-name",
        "resolve-held-digest",
        "resolve-scan-remove",
        "resolve-scan-replace",
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        publish(&handle, &reference(), Image::One, false);
        let mut reader = Worker::start(temporary.path(), &reference(), action);
        reader.wait_for("PUBLICATION_PAUSED");
        assert!(handle.exists(&selector(reference().as_str())).unwrap());
        assert!(
            handle
                .resolve(&selector(reference().as_str()))
                .unwrap()
                .is_some()
        );
        finish_reader_before_mutation(temporary.path(), &mut reader, !action.ends_with("replace"));
    }
}

#[test]
fn exists_propagates_short_lock_timeouts_without_changes() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    publish(&handle, &reference(), Image::One, false);
    let before = snapshot(temporary.path());
    let _short = locks::short_test_waits();
    let _writer = handle.prepare(Operation::Import).unwrap();
    for selected in [
        selector(reference().as_str()),
        selector(format!("analysis/tool@{}", root(Image::One))),
    ] {
        assert!(matches!(
            handle.exists(&selected),
            Err(Error::LockTimeout { .. })
        ));
        assert!(matches!(
            handle.resolve(&selected),
            Err(Error::LockTimeout { .. })
        ));
    }
    assert_eq!(snapshot(temporary.path()), before);
}

#[test]
fn exists_reports_unprivileged_permission_failures() {
    let temporary = tempfile::tempdir().unwrap();
    let mut worker = Worker::start(temporary.path(), &reference(), "exists-permissions");
    worker.wait_for("PUBLICATION_READY");
    worker.finish();
}

pub(super) fn worker(handle: &Store, reference: &LocalRef, action: &str) {
    if action == "exists-permissions" {
        permission_cases();
        return;
    }
    fsutil::pause_once(if action.contains("-scan-") {
        "exists.discovered"
    } else if action == "resolve-held-name" {
        "resolve.selected"
    } else {
        "exists.selected"
    });
    let selected = if action.ends_with("-held-name") {
        selector(reference.as_str())
    } else {
        selector(format!("{}@{}", reference.name(), root(Image::One)))
    };
    if action.starts_with("resolve-") {
        assert_eq!(
            handle.resolve(&selected).unwrap(),
            Some(format!("{}@{}", reference.name(), root(Image::One)))
        );
    } else {
        assert!(handle.exists(&selected).unwrap());
    }
}

fn permission_cases() {
    assert_ne!(unsafe { libc::geteuid() }, 0);
    for file in ["store.json", "index.json", "namespace", "store.lock"] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        let raw = publish(&handle, &reference(), Image::One, false);
        let path = match file {
            "store.json" => handle.layout.marker(),
            "index.json" => raw.join(file),
            "store.lock" => handle.layout.store_lock(),
            _ => handle
                .layout
                .ref_entry(&reference())
                .parent()
                .unwrap()
                .to_owned(),
        };
        let permissions = fs::metadata(&path).unwrap().permissions();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o0)).unwrap();
        let named = handle.exists(&selector(reference().as_str()));
        let by_digest = handle.exists(&selector(format!("analysis/tool@{}", root(Image::One))));
        let resolved_name = handle.resolve(&selector(reference().as_str()));
        let resolved_digest =
            handle.resolve(&selector(format!("analysis/tool@{}", root(Image::One))));
        fs::set_permissions(&path, permissions).unwrap();
        if file == "index.json" {
            assert!(named.unwrap());
        } else {
            assert!(
                matches!(named, Err(Error::Io { source, .. }) if source.kind() == io::ErrorKind::PermissionDenied),
                "{file}"
            );
        }
        assert!(by_digest.is_err(), "{file}");
        assert!(
            matches!(resolved_name, Err(Error::Io { source, .. }) if source.kind() == io::ErrorKind::PermissionDenied),
            "{file}"
        );
        assert!(resolved_digest.is_err(), "{file}");
    }
}
