//! Structural regressions.
use super::*;
use crate::{
    CheckDiagnostic, CheckScope, DiagnosticCategory, ImageSelector, test_snapshot::snapshot,
};
use serde_json::{Value, json};

fn selector(value: impl AsRef<str>) -> ImageSelector {
    ImageSelector::new(value.as_ref()).unwrap()
}
fn findings(handle: &Store, selected: &ImageSelector) -> Vec<CheckDiagnostic> {
    let report = handle.check(&CheckScope::Image(selected.clone())).unwrap();
    assert!(report.has_errors());
    assert!(!report.diagnostics.is_empty());
    report.diagnostics
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
        let diagnostics = findings(&handle, &selector(reference().as_str()));
        assert!(diagnostics.iter().all(|finding| finding.local_ref == Some(reference()) && finding.path.is_absolute()));
        if case.starts_with("metadata-") {
            assert!(
                diagnostics
                    .iter()
                    .any(|finding| finding.path == metadata_path
                        || finding.category == DiagnosticCategory::Metadata),
                "{case}: {diagnostics:?}"
            );
        }
        assert_eq!(snapshot(temporary.path()), before, "{case}");
    }
}
#[test]
fn check_retains_multiple_findings_and_object_context() {
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
    let diagnostics = findings(&handle, &selector(reference().as_str()));
    assert_eq!(diagnostics.len(), 4);
    assert!(
        diagnostics
            .iter()
            .any(|finding| finding.category == DiagnosticCategory::Metadata)
    );
    assert!(
        diagnostics
            .iter()
            .any(|finding| finding.object_digest == Some(root(Image::One)))
    );
    let missing = diagnostics
        .iter()
        .find(|finding| finding.object_digest == Some(digest(CONFIG_DIGEST)))
        .unwrap();
    assert_eq!(missing.category, DiagnosticCategory::Graph);
    assert_eq!(missing.path, handle.layout.blob(&digest(CONFIG_DIGEST)));
    assert_eq!(snapshot(temporary.path()), before);
}
#[test]
fn check_reports_malformed_and_unsupported_graph_content() {
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
        let diagnostics = findings(&handle, &selector(reference().as_str()));
        assert!(
            diagnostics
                .iter()
                .any(|finding| finding.category == DiagnosticCategory::Graph
                    && finding.object_digest.is_some()),
            "{case}: {diagnostics:?}"
        );
    }
}
#[test]
fn check_reports_operational_failures_alongside_damage() {
    for point in ["refs.metadata-read", "graph.observe"] {
        for errno in [libc::EIO, libc::EACCES] {
            let temporary = tempfile::tempdir().unwrap();
            let handle = store(temporary.path());
            let raw = publish(&handle, &reference(), Image::One, false);
            fs::write(raw.join("metadata.json"), b"{").unwrap();
            let before = snapshot(temporary.path());
            fsutil::fail_once(point, errno);
            let report = handle
                .check(&CheckScope::Image(selector(reference().as_str())))
                .unwrap();
            assert!(report.has_errors() && !report.complete);
            assert_eq!(snapshot(temporary.path()), before);
        }
    }
}
#[test]
fn check_retains_complete_internal_index_traversal() {
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
    assert!(
        !handle
            .check(&CheckScope::Image(selector(reference().as_str())))
            .unwrap()
            .has_errors()
    );
    fs::remove_file(handle.layout.blob(&root(Image::Two))).unwrap();
    let diagnostics = findings(&handle, &selector(reference().as_str()));
    assert!(
        diagnostics
            .iter()
            .any(|finding| finding.object_digest == Some(root(Image::Two)))
    );
}
