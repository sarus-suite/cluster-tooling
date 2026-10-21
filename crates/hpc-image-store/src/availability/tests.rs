//! Availability acceptance reuses the publication fixtures and process harness.
use super::*;
use crate::{
    AvailabilityDiagnostic, AvailabilityDiagnosticKind as Kind, CheckScope, DiagnosticCategory,
    ImageAvailability as Availability, ImageSelector, locks, test_snapshot::snapshot,
};
use serde_json::{Value, json};
use std::{io, os::unix::fs::PermissionsExt};

fn selector(value: impl AsRef<str>) -> ImageSelector {
    ImageSelector::new(value.as_ref()).unwrap()
}

fn unusable(handle: &Store, selected: &ImageSelector) -> Vec<AvailabilityDiagnostic> {
    match handle.availability(selected).unwrap() {
        Availability::Unusable { diagnostics } => {
            assert!(!diagnostics.is_empty());
            diagnostics
        }
        result => panic!("expected unusable content, got {result:?}"),
    }
}

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

fn change_manifest(handle: &Store, raw: &Path, change: impl FnOnce(&mut Value)) {
    let path = handle.layout.blob(&root(Image::One));
    let mut value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    change(&mut value);
    let bytes = serde_json::to_vec(&value).unwrap();
    fs::write(path, &bytes).unwrap();
    let index_path = raw.join("index.json");
    let mut index: Value = serde_json::from_slice(&fs::read(&index_path).unwrap()).unwrap();
    index["manifests"][0]["size"] = json!(bytes.len());
    write_json(&index_path, &index);
}

#[test]
fn missing_requires_a_valid_initialized_store() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("store");
    let handle = store(&path);
    let selected = selector(reference().as_str());
    assert!(matches!(
        handle.availability(&selected),
        Err(Error::MissingStore(_))
    ));
    assert!(!path.exists());
    fs::create_dir(&path).unwrap();
    let before = snapshot(&path);
    assert!(matches!(
        handle.availability(&selected),
        Err(Error::MissingStore(_))
    ));
    assert_eq!(snapshot(&path), before);
    drop(handle.prepare(Operation::Import).unwrap());
    for selected in [
        selected,
        selector(format!("analysis/tool@{}", root(Image::One))),
    ] {
        assert!(matches!(
            handle.availability(&selected),
            Ok(Availability::Missing)
        ));
    }
    fs::write(path.join("store.json"), br#"{"version":99}"#).unwrap();
    assert!(matches!(
        handle.availability(&selector(reference().as_str())),
        Err(Error::UnsupportedVersion {
            kind: "store",
            version: 99
        })
    ));
}

#[test]
fn ready_named_and_digest_images_preserve_all_persistent_content() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    publish(&handle, &reference(), Image::Index, false);
    fs::write(handle.layout.staging().join("abandoned"), b"preserve").unwrap();
    fs::write(handle.layout.root().join("unknown"), b"preserve").unwrap();
    let before = snapshot(temporary.path());
    for selected in [
        selector(reference().as_str()),
        selector(format!("analysis/tool@{}", root(Image::Index))),
        selector(format!("analysis/tool:ignored@{}", root(Image::Index))),
    ] {
        assert!(matches!(
            handle.availability(&selected),
            Ok(Availability::Ready)
        ));
    }
    assert_eq!(snapshot(temporary.path()), before);
    handle.remove(&reference()).unwrap();
    assert!(matches!(
        handle.availability(&selector(reference().as_str())),
        Ok(Availability::Missing)
    ));
    assert!(matches!(
        handle.availability(&selector(format!("analysis/tool@{}", root(Image::Index)))),
        Ok(Availability::Missing)
    ));
}

#[test]
fn wrapper_damage_and_unsupported_versions_have_typed_context() {
    for case in [
        "pointer-file",
        "pointer-escape",
        "raw-missing",
        "blobs-missing",
        "metadata-missing",
        "metadata-json",
        "metadata-symlink",
        "metadata-mismatch",
        "layout-missing",
        "layout-json",
        "layout-version",
        "metadata-version",
        "index-missing",
        "index-json",
        "index-schema",
        "root-media-type",
        "ambiguous-root",
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        let raw = publish(&handle, &reference(), Image::One, false);
        let pointer = handle.layout.ref_entry(&reference());
        let metadata_path = raw.join("metadata.json");
        match case {
            "pointer-file" | "pointer-escape" => {
                fs::remove_file(&pointer).unwrap();
                if case == "pointer-file" {
                    fs::write(&pointer, b"preserve").unwrap();
                } else {
                    symlink("/outside", &pointer).unwrap();
                }
            }
            "raw-missing" => {
                fs::rename(&raw, raw.with_extension("retired")).unwrap();
            }
            "blobs-missing" => {
                fs::remove_dir(raw.join("blobs")).unwrap();
            }
            "metadata-missing" => {
                fs::remove_file(&metadata_path).unwrap();
            }
            "metadata-json" => {
                fs::write(&metadata_path, b"{").unwrap();
            }
            "metadata-symlink" => {
                fs::remove_file(&metadata_path).unwrap();
                symlink("/outside", &metadata_path).unwrap();
            }
            "metadata-version" | "metadata-mismatch" => {
                let mut value: Value =
                    serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
                if case == "metadata-version" {
                    value["version"] = json!(99);
                } else {
                    value["stored_root"] = json!(root(Image::Two));
                }
                write_json(&metadata_path, &value);
            }
            "layout-missing" => {
                fs::remove_file(raw.join("oci-layout")).unwrap();
            }
            "layout-json" => {
                fs::write(raw.join("oci-layout"), b"{").unwrap();
            }
            "layout-version" => write_json(
                &raw.join("oci-layout"),
                &json!({"imageLayoutVersion":"9.0.0"}),
            ),
            "index-missing" => {
                fs::remove_file(raw.join("index.json")).unwrap();
            }
            "index-json" => {
                fs::write(raw.join("index.json"), b"{").unwrap();
            }
            "index-schema" | "root-media-type" | "ambiguous-root" => {
                let path = raw.join("index.json");
                let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                match case {
                    "index-schema" => value["schemaVersion"] = json!(99),
                    "root-media-type" => {
                        value["manifests"][0]["mediaType"] = json!("example/unknown")
                    }
                    _ => value["manifests"] = json!([]),
                }
                write_json(&path, &value);
            }
            _ => unreachable!(),
        }
        let before = snapshot(temporary.path());
        let diagnostics = unusable(&handle, &selector(reference().as_str()));
        let expected = if matches!(
            case,
            "metadata-version" | "layout-version" | "index-schema" | "root-media-type"
        ) {
            Kind::Unsupported
        } else {
            Kind::Damage
        };
        assert!(
            diagnostics.iter().any(|finding| finding.kind == expected),
            "{case}: {diagnostics:?}"
        );
        assert!(diagnostics.iter().all(|finding| finding.local_ref == Some(reference()) && finding.path.is_absolute()));
        if case.starts_with("metadata-") {
            assert!(
                diagnostics
                    .iter()
                    .any(|finding| finding.path == metadata_path)
            );
        }
        assert_eq!(snapshot(temporary.path()), before, "{case}");
        assert!(
            handle
                .check(&CheckScope::Image(selector(reference().as_str())))
                .unwrap()
                .has_errors()
        );
    }
}

#[test]
fn graph_findings_keep_multiple_classifications_and_object_context() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    let raw = publish(&handle, &reference(), Image::One, false);
    fs::write(raw.join("metadata.json"), b"{").unwrap();
    fs::remove_file(handle.layout.blob(&digest(CONFIG_DIGEST))).unwrap();
    change_manifest(&handle, &raw, |value| {
        value["artifactType"] = json!("example/artifact");
        value["layers"] =
            json!([{"mediaType":"example/unknown", "digest":digest(&"d".repeat(64)), "size":1}]);
    });
    let before = snapshot(temporary.path());
    let diagnostics = unusable(&handle, &selector(reference().as_str()));
    assert_eq!(diagnostics.len(), 4);
    assert!(
        diagnostics.iter().any(
            |finding| finding.kind == Kind::Damage && finding.path == raw.join("metadata.json")
        )
    );
    assert!(
        diagnostics
            .iter()
            .any(|finding| finding.kind == Kind::Unsupported
                && finding.object_digest == Some(root(Image::One)))
    );
    let missing = diagnostics
        .iter()
        .find(|finding| finding.object_digest == Some(digest(CONFIG_DIGEST)))
        .unwrap();
    assert_eq!(missing.kind, Kind::Damage);
    assert_eq!(missing.category, DiagnosticCategory::Graph);
    assert_eq!(missing.operation, Some("open regular file"));
    assert_eq!(missing.path, handle.layout.blob(&digest(CONFIG_DIGEST)));
    assert_eq!(snapshot(temporary.path()), before);
    assert!(matches!(
        handle.availability(&selector(format!("analysis/tool@{}", root(Image::One)))),
        Err(Error::ResolutionIncomplete { .. })
    ));
}

#[test]
fn malformed_and_unsupported_graph_content_match_checking() {
    for case in [
        "json",
        "schema-missing",
        "schema-unsupported",
        "size",
        "config-directory",
        "config-symlink",
        "descriptor-conflict",
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        let raw = publish(&handle, &reference(), Image::One, false);
        let config = handle.layout.blob(&digest(CONFIG_DIGEST));
        match case {
            "json" => {
                fs::write(&config, b"{").unwrap();
            }
            "schema-missing" => change_manifest(&handle, &raw, |value| {
                value.as_object_mut().unwrap().remove("schemaVersion");
            }),
            "schema-unsupported" => {
                change_manifest(&handle, &raw, |value| value["schemaVersion"] = json!(99))
            }
            "size" => {
                fs::write(&config, b"{}").unwrap();
            }
            "config-directory" | "config-symlink" => {
                fs::remove_file(&config).unwrap();
                if case == "config-directory" {
                    fs::create_dir(&config).unwrap();
                } else {
                    symlink("/outside", &config).unwrap();
                }
            }
            "descriptor-conflict" => change_manifest(&handle, &raw, |value| {
                value["layers"] = json!([{"mediaType":crate::graph::LAYERS[0], "digest":digest(CONFIG_DIGEST), "size":CONFIG.len() + 1}]);
            }),
            _ => unreachable!(),
        }
        let diagnostics = unusable(&handle, &selector(reference().as_str()));
        let kind = if case == "schema-unsupported" {
            Kind::Unsupported
        } else {
            Kind::Damage
        };
        assert!(
            diagnostics.iter().all(|finding| finding.kind == kind),
            "{case}: {diagnostics:?}"
        );
        let report = handle
            .check(&CheckScope::Image(selector(reference().as_str())))
            .unwrap();
        assert!(report.has_errors());
        assert_eq!(diagnostics.len(), report.diagnostics.len());
        for (available, checked) in diagnostics.iter().zip(&report.diagnostics) {
            assert_eq!(available.path, checked.path);
            assert_eq!(available.message, checked.message);
            assert_eq!(available.object_digest, checked.object_digest);
        }
    }
}

#[test]
fn operational_failures_override_damage_and_remain_errors() {
    for point in ["refs.metadata-read", "graph.observe"] {
        for errno in [libc::EIO, libc::EACCES] {
            let temporary = tempfile::tempdir().unwrap();
            let handle = store(temporary.path());
            let raw = publish(&handle, &reference(), Image::One, false);
            fs::write(raw.join("metadata.json"), b"{").unwrap();
            let before = snapshot(temporary.path());
            fsutil::fail_once(point, errno);
            let error = handle
                .availability(&selector(reference().as_str()))
                .unwrap_err();
            assert!(
                matches!(&error, Error::Io { operation, source, path }
                if *operation == point && path.is_absolute()
                && source.kind() == io::Error::from_raw_os_error(errno).kind()
                && source.raw_os_error() == Some(errno)),
                "{error:?}"
            );
            assert_eq!(snapshot(temporary.path()), before);
            fsutil::fail_once(point, errno);
            let report = handle
                .check(&CheckScope::Image(selector(reference().as_str())))
                .unwrap();
            assert!(report.has_errors() && !report.complete);
        }
    }
}

#[test]
fn digest_resolution_retains_literal_boundaries_and_never_guesses_a_miss() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    let broken = LocalRef::new("registry:5000/tool:aaa").unwrap();
    let raw = publish(&handle, &broken, Image::One, false);
    fs::write(raw.join("metadata.json"), b"{").unwrap();
    let selected = selector(format!("registry:5000/tool:ignored@{}", root(Image::One)));
    assert!(matches!(
        handle.availability(&selected),
        Err(Error::ResolutionIncomplete { .. })
    ));
    let healthy = LocalRef::new("registry:5000/tool:bbb").unwrap();
    publish(&handle, &healthy, Image::One, false);
    assert!(matches!(
        handle.availability(&selected),
        Ok(Availability::Ready)
    ));
    for repository in ["tool", "registry/tool", "registry:5000/other"] {
        assert!(matches!(
            handle.availability(&selector(format!("{repository}@{}", root(Image::One)))),
            Ok(Availability::Missing)
        ));
    }
    fs::write(handle.layout.root().join("refs/unknown"), b"preserve").unwrap();
    assert!(matches!(
        handle.availability(&selected),
        Ok(Availability::Ready)
    ));
    assert!(matches!(
        handle.availability(&selector(format!("absent@{}", root(Image::One)))),
        Err(Error::ResolutionIncomplete { .. })
    ));
    fsutil::fail_once("discovery.enumerate", libc::EIO);
    assert!(matches!(
        handle.availability(&selected),
        Err(Error::ResolutionIncomplete { .. })
    ));
}

#[test]
fn duplicate_digest_matches_are_deterministic_and_do_not_use_cached_aliases() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    let early = LocalRef::new("analysis/tool:aaa").unwrap();
    publish(&handle, &reference(), Image::One, false);
    publish(&handle, &early, Image::One, false);
    assert!(matches!(
        handle.availability(&selector(format!(
            "analysis/tool@{}",
            digest(CONFIG_DIGEST)
        ))),
        Ok(Availability::Missing)
    ));
    fs::remove_file(handle.layout.blob(&digest(CONFIG_DIGEST))).unwrap();
    let diagnostics = unusable(
        &handle,
        &selector(format!("analysis/tool:release@{}", root(Image::One))),
    );
    assert!(
        diagnostics
            .iter()
            .all(|finding| finding.local_ref == Some(early.clone()))
    );
}

#[test]
fn availability_retains_complete_internal_index_traversal() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    let raw = publish(&handle, &reference(), Image::Index, false);
    let child = digest(&"d".repeat(64));
    let nested = json!({"schemaVersion":2,"manifests":[{"mediaType":crate::graph::MANIFEST,"digest":root(Image::Two),"size":manifest(Image::Two).len()}]});
    let bytes = serde_json::to_vec(&nested).unwrap();
    fs::write(handle.layout.blob(&child), &bytes).unwrap();
    let mut index: Value = serde_json::from_slice(INDEX).unwrap();
    index["manifests"][1] =
        json!({"mediaType":crate::graph::INDEX,"digest":child,"size":bytes.len()});
    let bytes = serde_json::to_vec(&index).unwrap();
    fs::write(handle.layout.blob(&root(Image::Index)), &bytes).unwrap();
    let wrapper = raw.join("index.json");
    let mut index: Value = serde_json::from_slice(&fs::read(&wrapper).unwrap()).unwrap();
    index["manifests"][0]["size"] = json!(bytes.len());
    write_json(&wrapper, &index);
    assert!(matches!(
        handle.availability(&selector(reference().as_str())),
        Ok(Availability::Ready)
    ));
    fs::remove_file(handle.layout.blob(&root(Image::Two))).unwrap();
    let diagnostics = unusable(&handle, &selector(reference().as_str()));
    assert!(
        diagnostics
            .iter()
            .any(|finding| finding.object_digest == Some(root(Image::Two))
                && finding.kind == Kind::Damage)
    );
}

#[test]
fn availability_holds_both_shared_locks_during_graph_inspection() {
    for selected in ["name", "digest"] {
        for action in ["replace", "remove-public", "check-exclusive"] {
            let temporary = tempfile::tempdir().unwrap();
            let handle = store(temporary.path());
            publish(&handle, &reference(), Image::One, false);
            let mut reader = Worker::start(
                temporary.path(),
                &reference(),
                &format!("availability-held-{selected}"),
            );
            reader.wait_for("PUBLICATION_PAUSED");
            // A second reader can finish while the first remains in graph reading.
            assert!(matches!(
                handle.availability(&selector(reference().as_str())),
                Ok(Availability::Ready)
            ));
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
        }
    }
}

#[test]
fn availability_revalidates_digest_candidates_after_discovery() {
    for action in ["availability-scan-remove", "availability-scan-replace"] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        publish(&handle, &reference(), Image::One, false);
        let mut reader = Worker::start(temporary.path(), &reference(), action);
        reader.wait_for("PUBLICATION_PAUSED");
        if action.ends_with("remove") {
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
fn availability_propagates_store_and_ref_lock_timeouts() {
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    publish(&handle, &reference(), Image::One, false);
    let before = snapshot(temporary.path());
    let _short_waits = locks::short_test_waits();
    {
        let guard = handle.prepare(Operation::Prune).unwrap();
        assert!(
            matches!(handle.availability(&selector(reference().as_str())), Err(Error::LockTimeout { path, .. }) if path == handle.layout.store_lock())
        );
        drop(guard);
    }
    {
        let guard = handle.prepare(Operation::Import).unwrap();
        let ref_guard = guard.lock_ref(&reference()).unwrap();
        assert!(matches!(
            handle.availability(&selector(reference().as_str())),
            Err(Error::LockTimeout { .. })
        ));
        assert!(matches!(
            handle.availability(&selector(format!("analysis/tool@{}", root(Image::One)))),
            Err(Error::ResolutionIncomplete { .. })
        ));
        drop(ref_guard);
    }
    assert_eq!(snapshot(temporary.path()), before);
}

#[test]
fn availability_reports_unprivileged_permission_failures() {
    let temporary = tempfile::tempdir().unwrap();
    let mut worker = Worker::start(temporary.path(), &reference(), "availability-permissions");
    worker.wait_for("PUBLICATION_READY");
    worker.finish();
}

pub(super) fn worker(handle: &Store, reference: &LocalRef, action: &str) {
    if action == "availability-permissions" {
        permission_cases();
        return;
    }
    fsutil::pause_once(if action.starts_with("availability-scan-") {
        "selector.discovered"
    } else {
        "graph.observe"
    });
    let selected = if action == "availability-held-name" {
        selector(reference.as_str())
    } else {
        selector(format!("{}@{}", reference.name(), root(Image::One)))
    };
    let result = handle.availability(&selected).unwrap();
    if action.starts_with("availability-scan-") {
        assert!(matches!(result, Availability::Missing));
    } else {
        assert!(matches!(result, Availability::Ready));
    }
}

fn permission_cases() {
    assert_ne!(unsafe { libc::geteuid() }, 0);
    for file in ["store.json", "metadata.json", "config", "namespace"] {
        let temporary = tempfile::tempdir().unwrap();
        let handle = store(temporary.path());
        let raw = publish(&handle, &reference(), Image::One, false);
        let path = match file {
            "store.json" => handle.layout.root().join(file),
            "metadata.json" => raw.join(file),
            "config" => handle.layout.blob(&digest(CONFIG_DIGEST)),
            _ => handle
                .layout
                .ref_entry(&reference())
                .parent()
                .unwrap()
                .to_owned(),
        };
        let permissions = fs::metadata(&path).unwrap().permissions();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o0)).unwrap();
        let result = handle.availability(&selector(reference().as_str()));
        fs::set_permissions(&path, permissions).unwrap();
        assert!(
            matches!(result, Err(Error::Io { source, .. }) if source.kind() == io::ErrorKind::PermissionDenied),
            "{file}"
        );
    }
}
