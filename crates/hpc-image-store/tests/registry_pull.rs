//! Controlled CLI coverage plus one explicitly enabled Docker Hub smoke test.
use hpc_image_store::{Acquisition, CheckScope, ImageSelector, Store, StoreConfig};
use serde_json::{Value, json};
use std::{fs, process::Command};

#[allow(dead_code)]
#[path = "support/registry.rs"]
mod support;
use support::{DESTINATION, Fixture, SOURCE, descriptor, selector};

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

/// One acquisition only. Never run as part of ordinary tests or a CI matrix.
/// Failures are reported directly; there are no retries or substitute images.
#[test]
#[ignore = "contacts Docker Hub once; run only by this exact test name"]
fn dockerhub_alpine_smoke() {
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
    println!("Pulled Docker Hub Alpine once: {}", metadata.stored_root);
}
