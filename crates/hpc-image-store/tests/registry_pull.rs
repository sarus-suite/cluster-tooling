//! Controlled CLI coverage plus one explicitly enabled Docker Hub smoke test.
use hpc_image_store::{
    Acquisition, CheckScope, ImageId, ImageSelector, Store, StoreConfig, TransferContext,
};
use serde_json::{Value, json};
use std::{fs, process::Command};

#[allow(dead_code)]
#[path = "support/registry.rs"]
mod support;
use support::{DESTINATION, Fixture, SOURCE, descriptor, selector};

#[path = "support/snapshot.rs"]
mod snapshots;
use snapshots::snapshot;

#[path = "support/storage.rs"]
mod storage;

const CLI: &str = env!("CARGO_BIN_EXE_hpc-image-store");

#[test]
fn pull_cli_reports_pinned_identity_notices_and_configuration() {
    let fixture = Fixture::new();
    let mut entry = descriptor(&fixture.manifest, support::MANIFEST_TYPE);
    entry["platform"] = json!({"os":"linux","architecture":"amd64"});
    fixture.set_index(vec![entry]);
    let destination = format!("{DESTINATION}@{}", fixture.layer);
    let output = fixture
        .cli(CLI)
        .args([
            "--json",
            "--podman",
            "/unavailable/podman",
            "--platform",
            "linux/amd64",
            "pull",
            SOURCE,
            &destination,
            "--notes",
            "from CLI",
            "--authfile",
            "/private/auth.json",
            "--cert-dir",
            "/private/certs",
            "--policy",
            "/private/policy.json",
            "--tls-verify=false",
        ])
        .env("HPC_PULL_TEST_ENV", "inherited CLI environment")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["stored_root"], json!(fixture.root));
    assert_eq!(report["resolved_digest"], report["stored_root"]);
    assert_eq!(report["ignored_destination_digest"], json!(fixture.layer));
    assert!(report.get("image_id").is_none());
    assert!(String::from_utf8_lossy(&output.stderr).contains("ignoring pull destination digest"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("tool progress"));
    assert_eq!(fixture.metadata().notes.as_deref(), Some("from CLI"));
    assert!(
        fixture
            .calls()
            .iter()
            .any(|entry| entry["environment"] == "inherited CLI environment")
    );
    for entry in fixture
        .calls()
        .iter()
        .filter(|entry| !entry["args"].as_array().unwrap().contains(&json!("--help")))
    {
        let args = entry["args"].as_array().unwrap();
        for value in [
            "--policy",
            "/private/policy.json",
            "--authfile",
            "/private/auth.json",
            "/private/certs",
        ] {
            assert!(args.contains(&json!(value)));
        }
    }
    let output = fixture
        .cli(CLI)
        .args(["pull", SOURCE, &destination, "--replace"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("Pulled "));
}

#[test]
fn pull_cli_optional_destinations_and_digest_sources_match_library() {
    for combined in [false, true] {
        let fixture = Fixture::new();
        let source = format!(
            "registry.example/image{}@{}",
            if combined { ":old" } else { "" },
            fixture.root
        );
        let output = fixture
            .cli(CLI)
            .args(["--json", "pull", &source])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            report["local_ref"],
            json!(format!(
                "registry.example/image:{}",
                if combined {
                    "old"
                } else {
                    &fixture.root.hex()[..12]
                }
            ))
        );
        assert_eq!(report["requested_reference"], source);
        assert_eq!(report["ignored_destination_digest"], Value::Null);
    }
}

#[test]
fn invalid_pull_usage_does_not_prepare_store_or_run_tools() {
    let fixture = Fixture::new();
    let tagless = format!("renamed@{}", fixture.root);
    for arguments in [
        vec!["pull", SOURCE, &tagless],
        vec!["pull", "alpine"],
        vec!["pull", "docker://alpine:tag"],
        vec!["pull", SOURCE, "--tls-verify=invalid"],
        vec!["--platform", "linux", "pull", SOURCE],
    ] {
        let output = fixture.cli(CLI).args(arguments).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        assert!(!fixture.store.config().root().exists());
        assert!(fixture.calls().is_empty());
    }
    let output = fixture.cli(CLI).args(["pull", "--help"]).output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(
        help.contains("[DESTINATION]")
            && help.contains("--authfile")
            && help.contains("--tls-verify")
    );
}

#[test]
fn pull_cli_operation_failure_preserves_ref_and_returns_status_one() {
    let mut fixture = Fixture::new();
    fixture.pull(false).unwrap();
    let pointer = fs::read_link(fixture.pointer()).unwrap();
    fixture.config["copy_failure"] = json!("digest preservation failed");
    fixture.save();
    let output = fixture
        .cli(CLI)
        .args(["--json", "pull", SOURCE, DESTINATION, "--replace"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("digest preservation failed"));
    assert_eq!(fs::read_link(fixture.pointer()).unwrap(), pointer);
}

#[test]
fn pulled_export_cli_preserves_identity_and_reports_failures_without_modification() {
    let mut fixture = Fixture::new();
    fixture.pull(false).unwrap();
    let qualified = format!("cached/image:ignored@{}", fixture.root);
    let matching = format!("localhost/output:tag@{}", fixture.root);
    let mismatch = format!("localhost/output:tag@sha256:{}", "f".repeat(64));
    let before = snapshot(fixture.store.config().root());
    for (source, destination) in [
        (DESTINATION, None),
        (qualified.as_str(), None),
        (qualified.as_str(), Some(matching.as_str())),
    ] {
        let mut command = fixture.cli(CLI);
        command.args([
            "--json",
            "--podman",
            "/unavailable/podman",
            "export",
            source,
        ]);
        if let Some(destination) = destination {
            command.arg(destination);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["local_ref"], DESTINATION);
        assert_eq!(report["stored_manifest"], json!(fixture.root));
        assert_eq!(report["destination"], destination.unwrap_or(source));
        assert!(
            fixture.calls().last().unwrap()["args"]
                .as_array()
                .unwrap()
                .contains(&json!("--preserve-digests"))
        );
        assert_eq!(snapshot(fixture.store.config().root()), before);
    }
    for (key, value, destination, diagnostic, expected_calls) in [
        (
            "help",
            "--src-shared-blob-dir",
            DESTINATION,
            "--preserve-digests",
            1,
        ),
        (
            "help",
            "--preserve-digests",
            DESTINATION,
            "--src-shared-blob-dir",
            1,
        ),
        (
            "export_failure",
            "cannot preserve manifest",
            DESTINATION,
            "cannot preserve manifest",
            2,
        ),
        (
            "unused",
            "",
            mismatch.as_str(),
            "does not match expected digest",
            2,
        ),
    ] {
        fixture.config.as_object_mut().unwrap().remove("help");
        fixture
            .config
            .as_object_mut()
            .unwrap()
            .remove("export_failure");
        fixture.config[key] = json!(value);
        fixture.save();
        let calls = fixture.calls().len();
        let output = fixture
            .cli(CLI)
            .args(["--json", "export", &qualified, destination])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains(diagnostic));
        assert_eq!(fixture.calls().len() - calls, expected_calls);
        assert_eq!(snapshot(fixture.store.config().root()), before);
    }
}

/// One acquisition only. Never run as part of ordinary tests or a CI matrix.
/// Failures are reported directly; there are no retries or substitute images.
#[test]
#[ignore = "contacts Docker Hub once; run only by this exact test name"]
fn dockerhub_alpine_smoke() {
    if storage::run_if_needed("dockerhub_alpine_smoke", true) {
        return;
    }
    let configuration = std::env::var_os("CONTAINERS_STORAGE_CONF").unwrap();
    let configuration_before = fs::read(&configuration).unwrap();
    for tool in ["podman", "skopeo"] {
        println!(
            "{}",
            String::from_utf8_lossy(&storage::run(Command::new(tool).arg("--version")))
        );
    }
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("store");
    let output = Command::new(CLI)
        .arg("--store")
        .arg(&root)
        .args([
            "--json",
            "--platform",
            "linux/amd64",
            "pull",
            "docker.io/library/alpine:3.23",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Docker Hub pull unavailable or failed; do not retry automatically: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["resolved_digest"], report["stored_root"]);
    assert_eq!(report["content"]["platform"]["architecture"], "amd64");
    assert_eq!(report["content"]["platform"]["os"], "linux");
    let store = Store::new(StoreConfig::new(root).unwrap());
    let inventory = store.list().unwrap();
    assert!(!inventory.has_errors());
    assert_eq!(inventory.entries.len(), 1);
    let metadata = inventory.entries[0].metadata.as_ref().unwrap();
    assert_eq!(metadata.version(), 3);
    assert!(
        matches!(&metadata.acquisition,Acquisition::Pull{requested_reference,resolved_digest} if requested_reference=="docker.io/library/alpine:3.23" && resolved_digest==&metadata.stored_root)
    );
    for selector in [
        selector("docker.io/library/alpine:3.23"),
        ImageSelector::new(format!("docker.io/library/alpine@{}", metadata.stored_root)).unwrap(),
    ] {
        assert!(
            !store
                .check(&CheckScope::Image(selector))
                .unwrap()
                .has_errors()
        );
    }
    assert!(report["cleanup_warnings"].as_array().unwrap().is_empty());

    let stored_manifest = fs::read(
        store
            .config()
            .root()
            .join("blobs/sha256")
            .join(metadata.stored_root.hex()),
    )
    .unwrap();
    assert_eq!(support::digest(&stored_manifest), metadata.stored_root);
    let before = snapshot(store.config().root());
    let named = selector("docker.io/library/alpine:3.23");
    let qualified = selector(format!("docker.io/library/alpine@{}", metadata.stored_root));
    let combined = selector(format!(
        "docker.io/library/alpine:ignored@{}",
        metadata.stored_root
    ));
    let matching = selector(format!(
        "localhost/hpc-export:matching@{}",
        metadata.stored_root
    ));
    let plain = selector("localhost/hpc-export:plain");
    for (source, destination, cli) in [
        (&named, None, false),
        (&qualified, None, false),
        (&combined, Some(&matching), true),
        (&named, Some(&plain), true),
    ] {
        let effective = destination.unwrap_or(source);
        if cli {
            let mut command = Command::new(CLI);
            command.arg("--store").arg(store.config().root()).args([
                "--json",
                "export",
                source.as_str(),
            ]);
            if let Some(destination) = destination {
                command.arg(destination.as_str());
            }
            let exported: Value = serde_json::from_slice(&storage::run(&mut command)).unwrap();
            assert_eq!(exported["stored_manifest"], json!(metadata.stored_root));
            assert_eq!(exported["local_ref"], named.as_str());
            assert_eq!(exported["destination"], effective.as_str());
        } else {
            let exported = store
                .export(source, destination, &TransferContext::default())
                .unwrap();
            assert_eq!(exported.stored_manifest, metadata.stored_root);
            assert_eq!(exported.local_ref.as_str(), named.as_str());
            assert_eq!(exported.destination, effective.as_str());
        }
        let raw = storage::run(
            Command::new("skopeo")
                .args(["inspect", "--raw"])
                .arg(format!("containers-storage:{effective}")),
        );
        assert_eq!(
            raw, stored_manifest,
            "export must preserve exact manifest bytes"
        );
        assert_eq!(support::digest(&raw), metadata.stored_root);
        let image_id = storage::run(Command::new("podman").args([
            "image",
            "inspect",
            "--format",
            "{{.Id}}",
            "--",
            effective.as_str(),
        ]));
        assert_eq!(
            ImageId::new(String::from_utf8(image_id).unwrap().trim())
                .unwrap()
                .as_str(),
            metadata.content.config_digest.hex()
        );
        assert_eq!(snapshot(store.config().root()), before);
    }
    let mismatch = format!("localhost/hpc-export:mismatch@sha256:{}", "f".repeat(64));
    let failed = Command::new(CLI)
        .arg("--store")
        .arg(store.config().root())
        .args(["--json", "export", qualified.as_str(), &mismatch])
        .output()
        .unwrap();
    assert_eq!(failed.status.code(), Some(1));
    assert!(failed.stdout.is_empty());
    let diagnostic = String::from_utf8_lossy(&failed.stderr);
    assert!(
        diagnostic.contains("does not match expected digest")
            || diagnostic.contains("would not match destination reference"),
        "{diagnostic}"
    );
    assert_eq!(snapshot(store.config().root()), before);
    assert_eq!(fs::read(&configuration).unwrap(), configuration_before);
    println!(
        "Pulled Docker Hub Alpine once and preserved manifest {} through library and CLI exports; config ID {}",
        metadata.stored_root, metadata.content.config_digest
    );
}
