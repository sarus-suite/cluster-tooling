use std::process::Command;

use snapshots::snapshot;

#[allow(dead_code)]
#[path = "support/registry.rs"]
mod registry;

const PRODUCER: &str = r#"#!/usr/bin/python3
import hashlib, json, os, pathlib, sys
assert pathlib.Path(os.environ['CONTAINERS_STORAGE_CONF']).read_text() == 'caller configuration sentinel\n'
a = sys.argv[1:]
if '--help' in a:
    print('--src-shared-blob-dir --dest-shared-blob-dir --preserve-digests')
elif pathlib.Path(__file__).name == 'podman':
    print('a' * 64)
elif a[-1].startswith('oci:'):
    layout = pathlib.Path(a[-1][4:])
    blobs = pathlib.Path(a[a.index('--dest-shared-blob-dir') + 1]) / 'sha256'
    def blob(data):
        digest = hashlib.sha256(data).hexdigest()
        (blobs / digest).write_bytes(data)
        return {'digest': 'sha256:' + digest, 'size': len(data)}
    config = blob(b'{"os":"linux","architecture":"amd64","rootfs":{"type":"layers","diff_ids":[]}}')
    config['mediaType'] = 'application/vnd.oci.image.config.v1+json'
    manifest = blob(json.dumps({'schemaVersion':2, 'config':config, 'layers':[]}).encode())
    manifest['mediaType'] = 'application/vnd.oci.image.manifest.v1+json'
    (layout / 'oci-layout').write_text('{"imageLayoutVersion":"1.0.0"}')
    (layout / 'index.json').write_text(json.dumps({'schemaVersion':2, 'manifests':[manifest]}))
    if os.getenv('HPC_TEST_DENY_CLEANUP') == 'remove':
        layout.chmod(0o500)
    elif os.getenv('HPC_TEST_DENY_CLEANUP') == 'sync':
        layout.parent.parent.chmod(0o300)
    print('subprocess output must not leak into result JSON')
"#;

// Construct legacy leftovers for prune without depending on public removal.
fn unlink_fixture_ref(root: &std::path::Path, name: &str) {
    let reference = hpc_image_store::LocalRef::new(name).unwrap();
    std::fs::remove_file(root.join("refs").join(reference.encoded_path()).join("ref")).unwrap();
}

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hpc-image-store"))
}

#[test]
fn invalid_implicit_import_destination_is_usage_failure() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("store");
    for source in [
        "alpine",
        &"a".repeat(64),
        &format!("sha256:{}", "a".repeat(64)),
    ] {
        let output = cli()
            .arg("--store")
            .arg(&root)
            .args([
                "--podman",
                "/unavailable/podman",
                "--skopeo",
                "/unavailable/skopeo",
                "import",
                source,
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid value"));
        assert!(!root.exists());
    }
}

#[test]
fn command_help_describes_optional_destinations() {
    for (action, positional) in [("import", "[DESTINATION]"), ("export", "[DESTINATION]")] {
        let output = cli().args([action, "--help"]).output().unwrap();
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains(positional), "{help}");
        assert!(help.contains("defaults to the source"), "{help}");
    }
}

#[test]
fn invalid_cli_usage_does_not_initialize_a_store() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("store");
    for arguments in [
        vec!["list", "--format", "invalid"],
        vec!["import", "source", "no-tag"],
        vec!["import", "docker://example/image", "tool:tag"],
        vec!["import", "", "tool:tag"],
        vec!["import", "image with spaces:tag", "tool:tag"],
        vec!["import", "image:\n", "tool:tag"],
        vec!["export", "tool:tag", "containers-storage:[vfs@/tmp]image"],
        vec!["export", "tool:tag", "[vfs@/tmp]image"],
        vec!["--storage-driver", "vfs", "import", "source", "tool:tag"],
        vec!["--platform", "linux", "import", "source", "tool:tag"],
    ] {
        let output = cli()
            .arg("--store")
            .arg(&root)
            .args(arguments)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        assert!(!root.exists());
    }
}
#[test]
fn help_is_successful_and_lists_implemented_commands() {
    let output = cli().arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("import") && help.contains("export"));
    assert!(help.contains("list") && help.contains("remove"));
    assert!(help.contains("check"));
    assert!(help.contains("pull"));
    assert!(help.contains("exists"));
    assert!(help.contains("prune"));
    assert!(!help.contains("--storage-driver"));
    assert!(output.stderr.is_empty());
}

#[test]
fn check_usage_and_missing_store_fail_without_initialization() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("store");
    for args in [
        vec!["check"],
        vec!["check", "--all", "tool:t"],
        vec!["check", "alpine"],
        vec!["check", "tool@sha256:abcd"],
        vec!["check", "--deep"],
    ] {
        let output = cli().arg("--store").arg(&root).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!root.exists());
    }
    for args in [vec!["check", "--all"], vec!["--json", "check", "tool:t"]] {
        let output = cli().arg("--store").arg(&root).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(!root.exists());
    }
}

#[test]
fn check_reports_human_and_json_results_without_external_tools() {
    use std::fs;
    let fixture = InventoryFixture::new();
    let inventory = fixture.report();
    let root_digest = inventory["entries"][0]["stored_root"].as_str().unwrap();
    let raw = fs::canonicalize(inventory["entries"][0]["path"].as_str().unwrap()).unwrap();
    let selector = format!("tool:ignored@{root_digest}");
    // Removing the producer executables proves checks make no tool calls.
    for tool in ["podman", "skopeo"] {
        fs::remove_file(fixture.directory.path().join(tool)).unwrap();
    }
    for args in [
        vec!["check", "tool:t"],
        vec!["check", "--all"],
        vec!["check", &selector],
    ] {
        let output = fixture.command().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("tool:t: passed") && text.contains("1 passed, 0 failed"));
        assert!(output.stderr.is_empty());
    }
    let output = fixture
        .command()
        .args(["check", "--all", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["complete"], true);
    assert_eq!(report["entries"][0]["status"], "passed");
    let manifest_path = fixture
        .root()
        .join("blobs/sha256")
        .join(root_digest.strip_prefix("sha256:").unwrap());
    let manifest_bytes = fs::read(&manifest_path).unwrap();
    fs::remove_file(&manifest_path).unwrap();
    fs::write(raw.join("metadata.json"), b"{").unwrap();
    for args in [
        vec!["--json", "check", "--all"],
        vec!["check", "tool:t", "--json"],
    ] {
        let output = fixture.command().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["complete"], false);
        assert_eq!(report["entries"][0]["status"], "failed");
        assert!(
            report["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|diagnostic| diagnostic["object_digest"] == root_digest)
        );
        assert!(!output.stderr.is_empty());
    }
    let output = fixture.command().args(["check", "--all"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("tool:t: failed"));
    // Unreadable authoritative dependencies block removal before unlinking.
    let rejected = fixture
        .command()
        .args(["remove", "tool:t"])
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(1));
    assert!(rejected.stdout.is_empty());
    assert!(!fixture.root().join("pending-mutation.json").exists());
    assert!(raw.exists());
    // Restore known fixture bytes; damaged cached metadata alone is harmless.
    fs::write(&manifest_path, manifest_bytes).unwrap();
    assert!(
        fixture
            .command()
            .args(["remove", "tool:t"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let output = fixture
        .command()
        .args(["--json", "check", "--all"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["entries"], serde_json::json!([]));
    assert_eq!(report["complete"], true);
}

#[test]
fn check_keeps_malformed_non_utf8_namespace_entries_in_partial_json() {
    use std::{ffi::OsString, fs, os::unix::ffi::OsStringExt};
    let fixture = InventoryFixture::new();
    let path = fixture
        .root()
        .join("refs")
        .join(OsString::from_vec(vec![0xff]));
    fs::write(&path, b"preserve").unwrap();
    let output = fixture
        .command()
        .args(["--json", "check", "--all"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["entries"].as_array().unwrap().len(), 2);
    assert_eq!(report["complete"], false);
    assert!(
        report["diagnostics"][0]["path"]
            .as_str()
            .unwrap()
            .contains("\\xFF")
    );
    assert_eq!(fs::read(path).unwrap(), b"preserve");
}
#[test]
fn missing_store_export_is_an_operation_failure_and_does_not_initialize() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("store");
    let output = cli()
        .arg("--store")
        .arg(&root)
        .args(["--json", "export", "tool:tag", "localhost/output:tag"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("store does not exist"));
    assert!(!root.exists());
}

struct InventoryFixture {
    directory: tempfile::TempDir,
}

#[test]
fn malformed_non_utf8_namespace_paths_remain_visible_in_json() {
    use std::{ffi::OsString, fs, os::unix::ffi::OsStringExt};
    let fixture = InventoryFixture::new();
    let path = fixture
        .root()
        .join("refs")
        .join(OsString::from_vec(vec![0xff]));
    fs::write(&path, b"preserve").unwrap();
    let output = fixture
        .command()
        .args(["list", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["entries"].as_array().unwrap().len(), 2);
    assert!(
        report["diagnostics"][0]["path"]
            .as_str()
            .unwrap()
            .contains("\\xFF")
    );
    assert_eq!(fs::read(path).unwrap(), b"preserve");
}
impl InventoryFixture {
    fn new() -> Self {
        use std::{fs, os::unix::fs::PermissionsExt};
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("storage.conf"),
            "caller configuration sentinel\n",
        )
        .unwrap();
        for tool in ["podman", "skopeo"] {
            let path = directory.path().join(tool);
            fs::write(&path, PRODUCER).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let fixture = Self { directory };
        fixture.import("tool:t");
        fixture
    }
    fn root(&self) -> std::path::PathBuf {
        self.directory.path().join("store")
    }
    fn command(&self) -> Command {
        let mut command = cli();
        command
            .arg("--store")
            .arg(self.root())
            .arg("--podman")
            .arg(self.directory.path().join("podman"))
            .arg("--skopeo")
            .arg(self.directory.path().join("skopeo"))
            .env(
                "CONTAINERS_STORAGE_CONF",
                self.directory.path().join("storage.conf"),
            );
        command
    }
    fn import(&self, reference: &str) {
        let output = self
            .command()
            .args(["import", "source:tag", reference])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn report(&self) -> serde_json::Value {
        let output = self
            .command()
            .args(["list", "--format", "json"])
            .output()
            .unwrap();
        assert!(output.status.success());
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

#[test]
fn accepted_sha256_repository_name_remains_exportable() {
    let fixture = InventoryFixture::new();
    let reference = format!("sha256:{}", "g".repeat(64));
    fixture.import(&reference);
    let inventory = fixture.report();
    let entry = inventory["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["local_ref"] == reference)
        .unwrap();
    assert_eq!(entry["status"], "unverified");
    for destination in [Some("restored:tag"), None] {
        let mut command = fixture.command();
        command.args(["--json", "export", &reference]);
        if let Some(destination) = destination {
            command.arg(destination);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let exported: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(exported["local_ref"], reference);
        assert_eq!(exported["stored_manifest"], entry["stored_root"]);
        assert_eq!(exported["destination"], destination.unwrap_or(&reference));
    }
}

#[test]
fn inventory_formats_and_digest_export_use_the_collected_identities() {
    let fixture = InventoryFixture::new();
    let report = fixture.report();
    let root = report["entries"][0]["stored_root"].as_str().unwrap();
    let config = report["entries"][0]["metadata"]["content"]["config_digest"]
        .as_str()
        .unwrap();
    assert_eq!(report["entries"][0]["metadata"]["version"], 3);
    assert_eq!(report["entries"][0]["status"], "unverified");
    for arguments in [
        vec!["list"],
        vec!["list", "--format", "podman"],
        vec!["list", "--digests"],
        vec!["list", "--format", "full"],
    ] {
        let output = fixture.command().args(&arguments).output().unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.starts_with("REPOSITORY") && text.contains("IMAGE ID"));
        assert!(text.contains(&config[7..19]));
        assert!(!text.contains("aaaaaaaaaaaa"));
        if arguments.contains(&"--digests") || arguments.contains(&"full") {
            assert!(text.contains(root));
        }
        if arguments.contains(&"full") {
            assert!(text.contains("unverified") && text.contains("SIZE BYTES"));
        }
    }
    for arguments in [
        vec!["--json", "list"],
        vec!["list", "--json"],
        vec!["--json", "list", "--format", "json"],
    ] {
        let output = fixture.command().args(arguments).output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
            report
        );
    }
    let output = fixture
        .command()
        .args(["--json", "export", &format!("tool:ignored@{root}")])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let exported: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(exported["local_ref"], "tool:t");
    assert_eq!(exported["destination"], format!("tool:ignored@{root}"));
    let absent = fixture
        .command()
        .args(["export", &format!("tool@{config}"), "output:tag"])
        .output()
        .unwrap();
    assert_eq!(absent.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&absent.stderr).contains("no committed ref"));
}

#[test]
fn inventory_usage_errors_and_missing_store_are_distinct() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("missing");
    let digest = format!("sha256:{}", "a".repeat(64));
    for arguments in [
        vec!["list", "--format", "invalid"],
        vec!["list", "--format", "full", "--digests"],
        vec!["list", "--format", "json", "--digests"],
        vec!["--json", "list", "--digests"],
        vec!["--json", "list", "--format", "podman"],
        vec!["--json", "list", "--format", "full"],
        vec!["remove", &digest],
        vec!["remove", "untagged"],
        vec!["export", "tool@sha256:abcd"],
    ] {
        let output = cli()
            .arg("--store")
            .arg(&root)
            .args(&arguments)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        assert!(!root.exists());
    }
    let output = cli()
        .arg("--store")
        .arg(&root)
        .arg("list")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(!root.exists());
}

#[test]
fn damaged_inventory_blocks_cleanup_after_visible_removal() {
    use std::fs;
    let fixture = InventoryFixture::new();
    fixture.import("healthy:t");
    let reference = hpc_image_store::LocalRef::new("tool:t").unwrap();
    let pointer = fixture
        .root()
        .join("refs")
        .join(reference.encoded_path())
        .join("ref");
    let raw = fs::canonicalize(&pointer).unwrap();
    fs::write(raw.join("metadata.json"), b"{").unwrap();
    let unknown = fixture.root().join("refs/unknown");
    fs::write(&unknown, b"preserve").unwrap();
    for arguments in [
        vec!["list"],
        vec!["list", "--format", "full"],
        vec!["list", "--format", "json"],
    ] {
        let output = fixture.command().args(&arguments).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(!output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("healthy") && text.contains("tool") && text.contains("unknown"));
        if arguments.contains(&"json") {
            let report: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(report["complete"], false);
            assert_eq!(report["entries"].as_array().unwrap().len(), 3);
            assert_eq!(report["diagnostics"].as_array().unwrap().len(), 2);
        }
    }
    let removed = fixture
        .command()
        .args(["--json", "remove", "tool:t"])
        .output()
        .unwrap();
    assert_eq!(removed.status.code(), Some(1));
    assert!(removed.stdout.is_empty());
    assert!(String::from_utf8_lossy(&removed.stderr).contains("recovery"));
    assert!(fixture.root().join("pending-mutation.json").exists());
    for action in ["list", "check", "prune", "remove"] {
        let mut command = fixture.command();
        command.arg(action);
        if action == "remove" {
            command.arg("healthy:t");
        } else if action == "check" {
            command.arg("--all");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
    }
    assert!(raw.exists());
    assert!(fs::symlink_metadata(pointer).is_err());
    assert_eq!(fs::read(unknown).unwrap(), b"preserve");
}

#[test]
fn cli_round_trip_has_clean_json_and_preserves_callers_configuration() {
    use std::{fs, os::unix::fs::PermissionsExt};
    for implicit in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("store");
        let config = temporary.path().join("storage.conf");
        fs::write(&config, "caller configuration sentinel\n").unwrap();

        let podman = temporary.path().join("podman");
        let skopeo = temporary.path().join("skopeo");
        for tool in [&podman, &skopeo] {
            fs::write(tool, PRODUCER).unwrap();
            fs::set_permissions(tool, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let reference = if implicit {
            "localhost/source:tag"
        } else {
            "tool:tag"
        };
        let destination = if implicit {
            reference
        } else {
            "localhost/output:tag"
        };
        let mut imported = cli();
        imported
            .arg("--store")
            .arg(&root)
            .arg("--podman")
            .arg(&podman)
            .arg("--skopeo")
            .arg(&skopeo)
            .args([
                "--json",
                "import",
                "localhost/source:tag",
                "--notes",
                "my notes",
            ])
            .env("CONTAINERS_STORAGE_CONF", &config);
        if !implicit {
            imported.arg(reference);
        }
        let imported = imported.output().unwrap();
        assert!(
            imported.status.success(),
            "{}",
            String::from_utf8_lossy(&imported.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&imported.stdout).unwrap();
        assert_eq!(report["local_ref"], reference);
        assert!(report.get("cleanup_warnings").is_none());
        assert_eq!(fs::read_dir(root.join("tmp")).unwrap().count(), 0);
        assert!(imported.stderr.is_empty());
        let replaced = cli()
            .arg("--store")
            .arg(&root)
            .arg("--podman")
            .arg(&podman)
            .arg("--skopeo")
            .arg(&skopeo)
            .args([
                "--json",
                "import",
                "localhost/source:tag",
                reference,
                "--replace",
            ])
            .env("CONTAINERS_STORAGE_CONF", &config)
            .output()
            .unwrap();
        assert!(
            replaced.status.success(),
            "{}",
            String::from_utf8_lossy(&replaced.stderr)
        );
        let replaced: serde_json::Value = serde_json::from_slice(&replaced.stdout).unwrap();
        assert!(replaced.get("cleanup_warnings").is_none());
        assert_eq!(fs::read_dir(root.join("raw_images")).unwrap().count(), 1);
        assert_eq!(fs::read_dir(root.join("tmp")).unwrap().count(), 0);
        assert!(!root.join("pending-mutation.json").exists());
        let mut exported = cli();
        exported
            .arg("--store")
            .arg(&root)
            .args(["--podman", "/unavailable/podman"])
            .arg("--skopeo")
            .arg(&skopeo)
            .args(["--json", "export", reference])
            .env("CONTAINERS_STORAGE_CONF", &config);
        if !implicit {
            exported.arg(destination);
        }
        let exported = exported.output().unwrap();
        assert!(
            exported.status.success(),
            "{}",
            String::from_utf8_lossy(&exported.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&exported.stdout).unwrap();
        assert_eq!(report["destination"], destination);
        assert!(exported.stderr.is_empty());
        assert_eq!(
            fs::read_to_string(config).unwrap(),
            "caller configuration sentinel\n"
        );
    }
}

#[test]
fn cli_transfers_accept_transport_named_and_unqualified_images() {
    use std::{fs, os::unix::fs::PermissionsExt};
    let temporary = tempfile::tempdir().unwrap();
    let config = temporary.path().join("storage.conf");
    fs::write(&config, "caller configuration sentinel\n").unwrap();
    let podman = temporary.path().join("podman");
    let skopeo = temporary.path().join("skopeo");
    for tool in [&podman, &skopeo] {
        fs::write(tool, PRODUCER).unwrap();
        fs::set_permissions(tool, fs::Permissions::from_mode(0o700)).unwrap();
    }
    for (index, image) in [
        "docker:latest",
        "oci:latest",
        "oci:path",
        "containers-storage:latest",
        "registry.example:5000/team/tool:latest",
        "alpine:latest",
    ]
    .into_iter()
    .enumerate()
    {
        let root = temporary.path().join(format!("store-{index}"));
        let imported = cli()
            .arg("--store")
            .arg(&root)
            .arg("--podman")
            .arg(&podman)
            .arg("--skopeo")
            .arg(&skopeo)
            .args(["--json", "import", image, "tool:tag"])
            .env("CONTAINERS_STORAGE_CONF", &config)
            .output()
            .unwrap();
        assert_eq!(
            imported.status.code(),
            Some(0),
            "{image}: {}",
            String::from_utf8_lossy(&imported.stderr)
        );
        let import_report: serde_json::Value = serde_json::from_slice(&imported.stdout).unwrap();
        assert_eq!(import_report["local_ref"], "tool:tag");
        assert!(imported.stderr.is_empty());
        let exported = cli()
            .arg("--store")
            .arg(&root)
            .arg("--skopeo")
            .arg(&skopeo)
            .args([
                "--podman",
                "/unavailable/podman",
                "--json",
                "export",
                "tool:tag",
                image,
            ])
            .env("CONTAINERS_STORAGE_CONF", &config)
            .output()
            .unwrap();
        assert_eq!(
            exported.status.code(),
            Some(0),
            "{image}: {}",
            String::from_utf8_lossy(&exported.stderr)
        );
        let export_report: serde_json::Value = serde_json::from_slice(&exported.stdout).unwrap();
        assert_eq!(export_report["destination"], image);
        assert_eq!(
            export_report["stored_manifest"],
            import_report["stored_root"]
        );
        assert!(exported.stderr.is_empty());
    }
}

#[test]
fn cleanup_failure_is_an_error_and_recovery_keeps_json_clean() {
    // Permission failures must be exercised without root's DAC override. Match
    // the existing initialization permission tests' subprocess approach.
    if unsafe { libc::geteuid() } == 0 {
        use std::os::unix::process::CommandExt;
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "cleanup_failure_worker",
                "--nocapture",
            ])
            .gid(65534)
            .uid(65534)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    } else {
        exercise_cleanup_failure();
    }
}

#[test]
#[ignore = "unprivileged subprocess helper"]
fn cleanup_failure_worker() {
    exercise_cleanup_failure();
}

fn exercise_cleanup_failure() {
    assert_ne!(unsafe { libc::geteuid() }, 0);
    exercise_exists_permissions();
    for (json, failure) in [
        (false, "remove"),
        (true, "remove"),
        (false, "sync"),
        (true, "sync"),
    ] {
        exercise_import_cleanup_failure(json, failure);
        exercise_pull_cleanup_failure(json, failure);
        exercise_remove_cleanup_failure(json, failure);
    }
}

fn exercise_remove_cleanup_failure(json: bool, failure: &str) {
    use std::{fs, os::unix::fs::PermissionsExt};
    let fixture = InventoryFixture::new();
    let root = fixture.root();
    let reference = hpc_image_store::LocalRef::new("tool:t").unwrap();
    let pointer = root.join("refs").join(reference.encoded_path()).join("ref");
    let raw = fs::canonicalize(&pointer).unwrap();
    let blocked = if failure == "remove" {
        raw.clone()
    } else {
        root.join("raw_images")
    };
    fs::set_permissions(
        &blocked,
        fs::Permissions::from_mode(if failure == "remove" { 0o500 } else { 0o300 }),
    )
    .unwrap();
    let mut command = fixture.command();
    if json {
        command.arg("--json");
    }
    let output = command.args(["remove", "tool:t"]).output().unwrap();
    let blocked_exists = fixture
        .command()
        .args(["exists", "tool:t"])
        .output()
        .unwrap();
    // Restore access before assertions so teardown and recovery remain possible.
    if blocked.exists() {
        fs::set_permissions(blocked, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert_exists_output(&blocked_exists, 125);
    assert_eq!(output.status.code(), Some(1), "{diagnostic}");
    assert!(output.stdout.is_empty());
    assert!(
        diagnostic.contains("mutation recovery") && diagnostic.contains("RefRemoved"),
        "{diagnostic}"
    );
    assert!(fs::symlink_metadata(pointer).is_err());
    assert!(root.join("pending-mutation.json").is_file());
    for tool in ["podman", "skopeo"] {
        fs::remove_file(fixture.directory.path().join(tool)).unwrap();
    }
    for _ in 0..2 {
        let output = fixture
            .command()
            .args(["--json", "exists", "tool:t"])
            .output()
            .unwrap();
        assert_exists_output(&output, 1);
    }
    for _ in 0..2 {
        let output = fixture.command().args(["--json", "list"]).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(report["entries"].as_array().unwrap().is_empty());
    }
    assert_cleanup_complete(&root);
    assert!(!raw.exists());
    assert_eq!(fs::read_dir(root.join("blobs/sha256")).unwrap().count(), 0);
}

fn exercise_import_cleanup_failure(json: bool, failure: &str) {
    use std::{fs, os::unix::fs::PermissionsExt};
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("store");
    let config = temporary.path().join("storage.conf");
    fs::write(&config, "caller configuration sentinel\n").unwrap();
    let podman = temporary.path().join("podman");
    let skopeo = temporary.path().join("skopeo");
    for tool in [&podman, &skopeo] {
        fs::write(tool, PRODUCER).unwrap();
        fs::set_permissions(tool, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut command = cli();
    command
        .arg("--store")
        .arg(&root)
        .arg("--podman")
        .arg(&podman)
        .arg("--skopeo")
        .arg(&skopeo);
    if json {
        command.arg("--json");
    }
    let imported = command
        .args(["import", "localhost/source:tag", "tool:tag"])
        .env("CONTAINERS_STORAGE_CONF", &config)
        .env("HPC_TEST_DENY_CLEANUP", failure)
        .output()
        .unwrap();
    restore_staging_permissions(&root);
    assert_published_cleanup_failure(&root, &imported);
    let exported = cli()
        .arg("--store")
        .arg(&root)
        .arg("--skopeo")
        .arg(&skopeo)
        .args([
            "--podman",
            "/unavailable/podman",
            "--json",
            "export",
            "tool:tag",
            "localhost/output:tag",
        ])
        .env("CONTAINERS_STORAGE_CONF", &config)
        .output()
        .unwrap();
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let _: serde_json::Value = serde_json::from_slice(&exported.stdout).unwrap();
    assert!(exported.stderr.is_empty());
    assert_cleanup_complete(&root);
}

fn exercise_pull_cleanup_failure(json: bool, failure: &str) {
    use std::fs;
    let mut fixture = registry::Fixture::new();
    fixture.pull(false).unwrap();
    let previous = fs::read_link(fixture.pointer()).unwrap();
    fixture.config["deny_cleanup"] = serde_json::json!(failure);
    fixture.save();
    let mut command = fixture.cli(env!("CARGO_BIN_EXE_hpc-image-store"));
    if json {
        command.arg("--json");
    }
    let output = command
        .args(["pull", registry::SOURCE, registry::DESTINATION, "--replace"])
        .output()
        .unwrap();
    let root = fixture.store.config().root();
    restore_staging_permissions(root);
    assert_published_cleanup_failure(root, &output);
    assert_ne!(fs::read_link(fixture.pointer()).unwrap(), previous);
    let calls = fixture.calls().len();
    fs::remove_file(&fixture.context.skopeo).unwrap();
    for _ in 0..2 {
        let output = cli()
            .arg("--store")
            .arg(root)
            .args(["--json", "list"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let _: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(output.stderr.is_empty());
    }
    assert_eq!(fixture.calls().len(), calls);
    assert_cleanup_complete(root);
    assert_eq!(fs::read_dir(root.join("raw_images")).unwrap().count(), 1);
}

fn restore_staging_permissions(root: &std::path::Path) {
    use std::{fs, io::ErrorKind, os::unix::fs::PermissionsExt, path::Path};

    fn restore_directory_permissions(path: &Path) {
        match fs::set_permissions(path, fs::Permissions::from_mode(0o700)) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => panic!("restore permissions on {}: {error}", path.display()),
        }
    }

    // Restore deliberately blocked directories before assertions, so recovery
    // and test teardown can proceed even if an assertion subsequently fails.
    let staging = root.join("tmp");
    restore_directory_permissions(&staging);
    let entries = match fs::read_dir(&staging) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return,
        Err(error) => panic!("read staging directory {}: {error}", staging.display()),
    };
    for entry in entries {
        let entry = entry
            .unwrap_or_else(|error| panic!("read staging entry in {}: {error}", staging.display()));
        restore_directory_permissions(&entry.path().join("layout"));
    }
}

fn assert_published_cleanup_failure(root: &std::path::Path, output: &std::process::Output) {
    let diagnostic = std::str::from_utf8(&output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1), "{diagnostic}");
    assert!(output.stdout.is_empty(), "{diagnostic}");
    assert!(
        diagnostic.contains("mutation recovery") && diagnostic.contains("RefPublished"),
        "{diagnostic}"
    );
    assert!(root.join("pending-mutation.json").is_file());
}

fn assert_cleanup_complete(root: &std::path::Path) {
    assert!(!root.join("pending-mutation.json").exists());
    assert!(!root.join(".pending-mutation-next.json").exists());
    assert_eq!(std::fs::read_dir(root.join("tmp")).unwrap().count(), 0);
}

#[test]
fn import_digest_notices_keep_json_clean_and_invalid_naming_is_usage_failure() {
    let fixture = InventoryFixture::new();
    let digest = format!("sha256:{}", "b".repeat(64));
    for (index, json) in [false, true].into_iter().enumerate() {
        let destination = format!("renamed:release{index}@{digest}");
        let mut command = fixture.command();
        if json {
            command.arg("--json");
        }
        let output = command
            .args(["import", "source:tag", &destination])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let notice = String::from_utf8_lossy(&output.stderr);
        assert!(notice.contains("ignoring import destination digest") && notice.contains(&digest));
        if json {
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["ignored_destination_digest"], digest);
            assert_eq!(report["local_ref"], format!("renamed:release{index}"));
        }
    }
    let root = fixture.directory.path().join("invalid-store");
    let output = cli()
        .arg("--store")
        .arg(&root)
        .args([
            "--podman",
            "/unavailable/podman",
            "--skopeo",
            "/unavailable/skopeo",
            "import",
            "source:tag",
            &format!("renamed@{digest}"),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("destination digest is ignored"));
    assert!(!root.exists());
}

#[test]
fn cli_digest_import_generates_source_tag_and_remove_rejects_digest_selectors() {
    let fixture = InventoryFixture::new();
    let digest = format!("sha256:{}", "b".repeat(64));
    let source = format!("alpine@{digest}");
    let output = fixture
        .command()
        .args(["--json", "import", &source])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["local_ref"], "alpine:bbbbbbbbbbbb");
    assert!(report["ignored_destination_digest"].is_null());
    for reference in [source, format!("alpine:bbbbbbbbbbbb@{digest}")] {
        let output = fixture
            .command()
            .args(["remove", &reference])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
    let output = fixture
        .command()
        .args(["remove", "alpine:bbbbbbbbbbbb"])
        .output()
        .unwrap();
    assert!(output.status.success());
}

#[test]
fn prune_duration_usage_and_missing_store_preserve_the_filesystem() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("store");
    for duration in [
        "-1",
        "+1",
        "1.5h",
        "",
        "h",
        "1H",
        "1h30m",
        "18446744073709551616",
        "18446744073709551615d",
    ] {
        let output = cli()
            .arg("--store")
            .arg(&root)
            .args(["prune", &format!("--grace-period={duration}")])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{duration}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!root.exists());
    }
    for duration in ["0", "5", "1s", "2m", "24h", "7d", "18446744073709551615s"] {
        let output = cli()
            .arg("--store")
            .arg(&root)
            .args(["prune", "--dry-run", "--grace-period", duration])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{duration}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("not initialized"));
        assert!(!root.exists());
    }
}

#[test]
fn prune_cli_json_reports_eligibility_skips_and_blocked_scans_without_tools() {
    use hpc_image_store::{PruneOptions, Store, StoreConfig};
    use std::{fs, os::unix::fs::PermissionsExt};
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path();
    let config = directory.join("storage.conf");
    fs::write(&config, "caller configuration sentinel\n").unwrap();
    let podman = directory.join("podman");
    let skopeo = directory.join("skopeo");
    for executable in [&podman, &skopeo] {
        fs::write(executable, PRODUCER).unwrap();
        fs::set_permissions(executable, fs::Permissions::from_mode(0o700)).unwrap();
    }
    // Use the CLI producer in its child environment, never change process env.
    let root = directory.join("store");
    let acquired = cli()
        .arg("--store")
        .arg(&root)
        .arg("--podman")
        .arg(&podman)
        .arg("--skopeo")
        .arg(&skopeo)
        .args(["import", "source:tag", "stored:tag"])
        .env("CONTAINERS_STORAGE_CONF", &config)
        .output()
        .unwrap();
    assert!(
        acquired.status.success(),
        "{}",
        String::from_utf8_lossy(&acquired.stderr)
    );
    let handle = Store::new(StoreConfig::new(&root).unwrap());
    let orphan = root.join("blobs/sha256").join("f".repeat(64));
    fs::write(&orphan, b"extra bytes").unwrap();
    fs::write(root.join("tmp/unknown"), b"preserve").unwrap();
    let run = |args: &[&str]| {
        cli()
            .arg("--store")
            .arg(&root)
            .args([
                "--skopeo",
                "/unavailable/skopeo",
                "--podman",
                "/unavailable/podman",
            ])
            .args(args)
            .output()
            .unwrap()
    };
    let output = run(&["--json", "prune", "--dry-run", "--grace-period", "0"]);
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let library = handle
        .prune(&PruneOptions {
            dry_run: true,
            grace_period: std::time::Duration::ZERO,
        })
        .unwrap();
    assert_eq!(serde_json::to_value(library).unwrap(), report);
    assert_eq!(report["scan_complete"], true);
    assert_eq!(report["summaries"]["blob"]["eligible_bytes"], 11);
    assert_eq!(report["summaries"]["blob"]["removed_count"], 0);
    assert!(String::from_utf8_lossy(&output.stderr).contains("skipped"));
    assert!(orphan.exists());
    let output = run(&["prune", "--grace-period", "0"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Pruning complete"));
    assert!(!orphan.exists());
    fs::write(&orphan, b"extra bytes").unwrap();
    fs::write(root.join("refs/unknown"), b"damage").unwrap();
    let output = run(&["prune", "--json", "--grace-period", "0"]);
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["scan_complete"], false);
    assert_eq!(report["summaries"]["blob"]["eligible_count"], 0);
    assert!(!report["diagnostics"].as_array().unwrap().is_empty());
    assert!(orphan.exists());
}

#[path = "support/snapshot.rs"]
mod snapshots;

// Deny only fsync in this command's child, allowing the real CLI to scan and
// then receive EIO at the namespace barrier. No production fault hook or runtime
// option is introduced. The parent and subsequent retry remain unaffected.
fn fail_child_fsync(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: the pre-exec closure uses stack data and libc prctl only. It does
    // not allocate or acquire locks. The BPF program is valid for this native
    // Linux child; seccomp copies it before prctl returns.
    unsafe {
        command.pre_exec(|| {
            let mut filter = [
                libc::sock_filter {
                    code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                    jt: 0,
                    jf: 0,
                    k: 0,
                },
                libc::sock_filter {
                    code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                    jt: 0,
                    jf: 1,
                    k: libc::SYS_fsync as u32,
                },
                libc::sock_filter {
                    code: (libc::BPF_RET | libc::BPF_K) as u16,
                    jt: 0,
                    jf: 0,
                    k: libc::SECCOMP_RET_ERRNO | libc::EIO as u32,
                },
                libc::sock_filter {
                    code: (libc::BPF_RET | libc::BPF_K) as u16,
                    jt: 0,
                    jf: 0,
                    k: libc::SECCOMP_RET_ALLOW,
                },
            ];
            let program = libc::sock_fprog {
                len: filter.len() as u16,
                filter: filter.as_mut_ptr(),
            };
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[test]
fn prune_namespace_sync_failure_has_human_and_json_reports_without_deletion() {
    use std::fs;
    let fixture = InventoryFixture::new();
    let root = fixture.root();
    unlink_fixture_ref(&root, "tool:t");
    let stage = root.join("tmp/operation-aaaaaaaaaaaa");
    fs::create_dir(&stage).unwrap();
    let before = snapshots::snapshot(&root);
    for json in [false, true] {
        let mut command = fixture.command();
        if json {
            command.arg("--json");
        }
        command.args(["prune", "--grace-period", "0"]);
        fail_child_fsync(&mut command);
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("sync ref namespace before pruning"),
            "{stderr}"
        );
        if json {
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["scan_complete"], false);
            assert_eq!(report["diagnostics"][0]["phase"], "sync");
            assert!(
                report["diagnostics"][0]["path"]
                    .as_str()
                    .unwrap()
                    .starts_with(root.join("refs").to_str().unwrap())
            );
            for summary in report["summaries"].as_object().unwrap().values() {
                for field in [
                    "eligible_count",
                    "eligible_bytes",
                    "removed_count",
                    "removed_bytes",
                ] {
                    assert_eq!(summary[field], 0);
                }
            }
            assert!(report["candidates"].as_array().unwrap().iter().all(
                |candidate| candidate["eligible"] == false && candidate["removed_entries"] == 0
            ));
        } else {
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                stdout.contains("Pruning blocked") && stdout.contains("nothing was removed"),
                "{stdout}"
            );
        }
        assert_eq!(snapshots::snapshot(&root), before);
    }
    let mut dry = fixture.command();
    dry.args(["--json", "prune", "--dry-run", "--grace-period", "0"]);
    fail_child_fsync(&mut dry);
    assert!(dry.output().unwrap().status.success());
    assert_eq!(snapshots::snapshot(&root), before);
    let retry = fixture
        .command()
        .args(["--json", "prune", "--grace-period", "0"])
        .output()
        .unwrap();
    assert!(
        retry.status.success(),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&retry.stdout).unwrap();
    assert_eq!(report["summaries"]["raw_image"]["removed_count"], 1);
    assert_eq!(report["summaries"]["blob"]["removed_count"], 2);
    assert_eq!(report["summaries"]["staging"]["removed_count"], 1);
    assert!(!stage.exists());
    let mut empty = fixture.command();
    empty.args(["prune", "--grace-period", "0"]);
    fail_child_fsync(&mut empty);
    assert!(empty.output().unwrap().status.success());
}

#[test]
fn prune_cli_cleans_bookkeeping_with_default_grace_and_matches_library_reports() {
    use hpc_image_store::{LocalRef, PruneOptions, Store, StoreConfig};
    use std::{fs, os::unix::fs::MetadataExt};
    let fixture = InventoryFixture::new();
    fixture.import("unused:tag");
    let root = fixture.root();
    unlink_fixture_ref(&root, "unused:tag");
    let dead = LocalRef::new("unused:tag").unwrap();
    let store_lock = root.join("locks/store.lock");
    let inode = fs::metadata(&store_lock).unwrap().ino();
    let before = snapshots::snapshot(&root);
    let output = fixture
        .command()
        .args(["--json", "prune", "--dry-run"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let store = Store::new(StoreConfig::new(&root).unwrap());
    let library = store
        .prune(&PruneOptions {
            dry_run: true,
            ..PruneOptions::default()
        })
        .unwrap();
    assert_eq!(serde_json::to_value(library).unwrap(), json);
    assert_eq!(json["summaries"].as_object().unwrap().len(), 4);
    assert!(json["summaries"].get("ref_lock").is_none());
    assert!(json["summaries"].get("ref_lock_directory").is_none());
    assert_eq!(json["summaries"]["ref_directory"]["eligible_count"], 1);
    assert_eq!(json["summaries"]["ref_directory"]["eligible_bytes"], 0);

    for kind in ["blob", "raw_image", "staging"] {
        assert_eq!(json["summaries"][kind]["eligible_count"], 0);
    }
    let human = fixture
        .command()
        .args(["prune", "--dry-run"])
        .output()
        .unwrap();
    assert!(human.status.success());
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(text.contains("empty ref directories"));
    assert!(!text.contains("unused ref locks"));
    assert!(!text.contains("empty lock directories"));

    assert_eq!(snapshots::snapshot(&root), before);
    let actual = fixture
        .command()
        .args(["--json", "prune"])
        .output()
        .unwrap();
    assert!(
        actual.status.success(),
        "{}",
        String::from_utf8_lossy(&actual.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&actual.stdout).unwrap();
    assert_eq!(json["summaries"]["ref_directory"]["removed_count"], 1);

    assert!(!root.join("refs").join(dead.encoded_path()).exists());
    assert!(!root.join("locks/refs").exists());
    assert_eq!(fs::metadata(&store_lock).unwrap().ino(), inode);
}

#[test]
fn unsupported_store_formats_are_rejected_without_modification() {
    let fixture = InventoryFixture::new();
    let root = fixture.root();
    for version in [1, 99] {
        std::fs::write(
            root.join("store.json"),
            format!("{{\"version\":{version}}}"),
        )
        .unwrap();
        let before = snapshots::snapshot(&root);
        for args in [
            vec!["list"],
            vec!["check", "--all"],
            vec!["export", "tool:t"],
            vec!["remove", "tool:t"],
            vec!["prune", "--dry-run"],
            vec!["import", "source:tag", "dest:tag"],
            vec!["pull", "registry.example/image:tag", "dest:tag"],
        ] {
            let output = fixture.command().args(&args).output().unwrap();
            assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
            assert!(output.stdout.is_empty());
            assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported store"));
            assert_eq!(snapshots::snapshot(&root), before, "{args:?}");
        }
    }
}

// This fixture intentionally writes the private versioned wire format. It
// exercises preparation through the shipped CLI without adding a public API.
fn initialize_fixture_store(root: &std::path::Path) {
    // Import preparation initializes before the deliberately unavailable producer.
    let output = cli()
        .arg("--store")
        .arg(root)
        .args(["--podman", "/unavailable/podman", "import", "setup:missing"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(root.join("store.json").is_file());
}

fn recorded_acquisition(root: &std::path::Path) -> std::path::PathBuf {
    initialize_fixture_store(root);
    let stage = root.join("tmp/operation-123456789012");
    std::fs::create_dir(&stage).unwrap();
    std::fs::create_dir(stage.join("layout")).unwrap();
    std::fs::write(stage.join("layout/index.json"), b"interrupted acquisition").unwrap();
    let record = serde_json::json!({
        "version":1,"id":"123456789012","kind":"import","reference":"tool:tag",
        "phase":"acquiring","old_raw":null,"new_raw":null,"staging":"operation-123456789012",
        "owned":[],"reused":[],"retired":[],"namespace_created":[],"cleanup":null
    });
    std::fs::write(
        root.join("pending-mutation.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    stage
}

#[test]
fn commands_finish_recorded_recovery_before_execution_and_keep_json_clean() {
    use std::fs;
    for action in [
        "list", "check", "prune", "remove", "import", "pull", "export",
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("store");
        let stage = recorded_acquisition(&root);
        let mut command = cli();
        command.arg("--store").arg(&root).args([
            "--json",
            "--podman",
            "/unavailable/podman",
            "--skopeo",
            "/unavailable/skopeo",
            action,
        ]);
        match action {
            "prune" => {
                command.arg("--dry-run");
            }
            "check" => {
                command.arg("--all");
            }
            "list" => (),
            _ => {
                command.arg("tool:tag");
            }
        }
        let output = command.output().unwrap();
        assert!(
            !root.join("pending-mutation.json").exists(),
            "{action}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !root.join(".pending-mutation-next.json").exists(),
            "{action}"
        );
        assert!(!stage.exists(), "{action}");
        if matches!(action, "list" | "check" | "prune") {
            assert!(
                output.status.success(),
                "{action}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
            assert!(output.stderr.is_empty());
        } else {
            assert_eq!(output.status.code(), Some(1), "{action}");
            assert!(output.stdout.is_empty(), "{action}");
        }
        assert_eq!(fs::read_dir(root.join("tmp")).unwrap().count(), 0);
    }
}

#[test]
fn recovery_errors_block_commands_and_preserve_pending_evidence() {
    use std::fs;
    for action in [
        "list", "check", "prune", "remove", "import", "pull", "export",
    ] {
        for json in [false, true] {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path().join("store");
            recorded_acquisition(&root);
            fs::write(root.join("pending-mutation.json"), b"{invalid record").unwrap();
            fs::write(
                root.join(".pending-mutation-next.json"),
                b"preserve this draft too",
            )
            .unwrap();
            let before = snapshots::snapshot(&root);
            let mut command = cli();
            command.arg("--store").arg(&root).args([
                "--podman",
                "/unavailable/podman",
                "--skopeo",
                "/unavailable/skopeo",
            ]);
            if json {
                command.arg("--json");
            }
            command.arg(action);
            match action {
                "check" => {
                    command.arg("--all");
                }
                "list" => (),
                "prune" => {
                    command.arg("--dry-run");
                }
                _ => {
                    command.arg("tool:tag");
                }
            }
            let output = command.output().unwrap();
            assert_eq!(output.status.code(), Some(1), "{action}");
            assert!(output.stdout.is_empty());
            let error = String::from_utf8(output.stderr).unwrap();
            assert!(error.contains("mutation recovery"), "{action}: {error}");
            assert!(error.contains("pending-mutation.json"));
            assert!(!error.contains("using /unavailable"));
            assert_eq!(snapshots::snapshot(&root), before);
        }
    }
}

#[test]
fn remove_requires_completed_setup_and_preserves_every_uninitialized_entry() {
    use std::fs;
    for scenario in [
        "missing",
        "ancestors",
        "empty",
        "locks",
        "bootstrap",
        "interrupted",
        "interrupted-no-lock",
        "unsupported",
        "damaged",
        "missing-ref",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let root = if scenario == "ancestors" {
            directory.path().join("missing/parent/store")
        } else {
            directory.path().join("store")
        };
        match scenario {
            "missing" | "ancestors" => (),
            "empty" => fs::create_dir(&root).unwrap(),
            "locks" | "bootstrap" => {
                fs::create_dir(&root).unwrap();
                fs::create_dir(root.join("locks")).unwrap();
                if scenario == "bootstrap" {
                    fs::write(root.join("locks/store.lock"), b"").unwrap();
                }
            }
            _ => {
                initialize_fixture_store(&root);
                match scenario {
                    "unsupported" => {
                        fs::write(root.join("store.json"), br#"{"version":99}"#).unwrap()
                    }
                    "damaged" => fs::remove_dir(root.join("raw_images")).unwrap(),
                    "missing-ref" => (),
                    _ => {
                        fs::rename(root.join("store.json"), root.join(".store-init.json")).unwrap();
                        if scenario == "interrupted-no-lock" {
                            fs::remove_file(root.join("locks/store.lock")).unwrap();
                        }
                    }
                }
            }
        }
        let before = snapshots::snapshot(directory.path());
        for json in [false, true] {
            let mut command = cli();
            command.arg("--store").arg(&root).args([
                "--podman",
                "/unavailable/podman",
                "--skopeo",
                "/unavailable/skopeo",
            ]);
            if json {
                command.arg("--json");
            }
            let output = command.args(["remove", "missing:t"]).output().unwrap();
            assert_eq!(output.status.code(), Some(1), "{scenario}");
            assert!(output.stdout.is_empty());
            assert!(!output.stderr.is_empty());
            if scenario == "missing-ref" {
                assert!(String::from_utf8_lossy(&output.stderr).contains("missing:t"));
            }
            assert_eq!(snapshots::snapshot(directory.path()), before, "{scenario}");
        }
    }
}

#[test]
fn cli_remove_reclaims_content_preserves_longer_names_and_ignores_metadata_damage() {
    use std::fs;
    for json in [false, true] {
        let fixture = InventoryFixture::new();
        let short = format!("{}:t", "a".repeat(58));
        let long = format!("{}:tag", "a".repeat(58));
        fixture.import(&short);
        fixture.import(&long);
        let root = fixture.root();
        let pointer = |name: &str| {
            root.join("refs")
                .join(hpc_image_store::LocalRef::new(name).unwrap().encoded_path())
                .join("ref")
        };
        let raw = fs::canonicalize(pointer(&short)).unwrap();
        let keeper = fs::canonicalize(pointer(&long)).unwrap();
        fs::write(raw.join("metadata.json"), b"{").unwrap();
        let kept = snapshots::snapshot(&keeper);
        let blobs = snapshots::snapshot(&root.join("blobs/sha256"));
        for tool in ["podman", "skopeo"] {
            fs::remove_file(fixture.directory.path().join(tool)).unwrap();
        }
        let mut command = fixture.command();
        if json {
            command.arg("--json");
        }
        let output = command.args(["remove", &short]).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        if json {
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
                serde_json::json!({"local_ref": short})
            );
        } else {
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!("Removed {short}\n")
            );
        }
        assert!(!raw.exists());
        assert!(fs::symlink_metadata(pointer(&short)).is_err());
        assert!(pointer(&short).parent().unwrap().is_dir());
        assert_eq!(snapshots::snapshot(&keeper), kept);
        assert_eq!(snapshots::snapshot(&root.join("blobs/sha256")), blobs);
        assert_cleanup_complete(&root);
        for name in [&long, "tool:t"] {
            let output = fixture.command().args(["remove", name]).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert_cleanup_complete(&root);
        for relative in ["refs", "raw_images", "blobs/sha256"] {
            assert_eq!(fs::read_dir(root.join(relative)).unwrap().count(), 0);
        }
    }
}

fn assert_exists_output(output: &std::process::Output, status: i32) {
    assert_eq!(
        output.status.code(),
        Some(status),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "{:?}", output.stdout);
    if status == 0 || status == 1 {
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    } else {
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn exists_is_silent_and_json_preserves_all_exit_statuses() {
    use std::{fs, os::unix::fs::symlink};
    let fixture = InventoryFixture::new();
    fixture.import("other:t");
    let report = fixture.report();
    let root = report["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["local_ref"] == "tool:t")
        .unwrap()["stored_root"]
        .as_str()
        .unwrap();
    let pointer = fixture
        .root()
        .join("refs")
        .join(
            hpc_image_store::LocalRef::new("tool:t")
                .unwrap()
                .encoded_path(),
        )
        .join("ref");
    let raw = fs::canonicalize(&pointer).unwrap();
    let other = fixture
        .root()
        .join("refs")
        .join(
            hpc_image_store::LocalRef::new("other:t")
                .unwrap()
                .encoded_path(),
        )
        .join("ref");
    fs::write(fs::canonicalize(other).unwrap().join("index.json"), b"{").unwrap();
    fs::write(raw.join("metadata.json"), b"{").unwrap();
    for tool in ["podman", "skopeo"] {
        fs::remove_file(fixture.directory.path().join(tool)).unwrap();
    }
    for json in [false, true] {
        for (selector, status) in [
            ("tool:t".to_owned(), 0),
            ("tool:missing".to_owned(), 1),
            (format!("tool:ignored@{root}"), 0),
            (format!("tool@sha256:{}", "0".repeat(64)), 1),
            (format!("absent@{root}"), 1),
            (format!("other@{root}"), 125),
        ] {
            let mut command = fixture.command();
            if json {
                command.arg("--json");
            }
            assert_exists_output(
                &command.args(["exists", &selector]).output().unwrap(),
                status,
            );
        }
    }
    fs::write(raw.join("index.json"), b"{").unwrap();
    for json in [false, true] {
        let mut command = fixture.command();
        if json {
            command.arg("--json");
        }
        assert_exists_output(
            &command
                .args(["exists", &format!("tool@{root}")])
                .output()
                .unwrap(),
            125,
        );
    }
    fs::remove_file(&pointer).unwrap();
    symlink("/unavailable/dangling", &pointer).unwrap();
    for json in [false, true] {
        let mut command = fixture.command();
        if json {
            command.arg("--json");
        }
        assert_exists_output(&command.args(["exists", "tool:t"]).output().unwrap(), 0);
    }
    let checked = fixture
        .command()
        .args(["check", "tool:t"])
        .output()
        .unwrap();
    assert_eq!(checked.status.code(), Some(1));
}

#[test]
fn exists_distinguishes_prefix_directories_from_names() {
    let fixture = InventoryFixture::new();
    let short = format!("{}:t", "a".repeat(58));
    let long = format!("{}:tag", "a".repeat(58));
    fixture.import(&long);
    for json in [false, true] {
        for (name, status) in [(&short, 1), (&long, 0)] {
            let mut command = fixture.command();
            if json {
                command.arg("--json");
            }
            assert_exists_output(&command.args(["exists", name]).output().unwrap(), status);
        }
    }
    fixture.import(&short);
    assert_exists_output(
        &fixture.command().args(["exists", &short]).output().unwrap(),
        0,
    );
    assert!(
        fixture
            .command()
            .args(["remove", &short])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_exists_output(
        &fixture.command().args(["exists", &short]).output().unwrap(),
        1,
    );
    assert_exists_output(
        &fixture.command().args(["exists", &long]).output().unwrap(),
        0,
    );
}

#[test]
fn exists_usage_setup_failures_and_help_preserve_parser_contract() {
    use std::fs;
    for json in [false, true] {
        for case in [
            "missing",
            "empty",
            "interrupted",
            "format",
            "lock",
            "recovery",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("store");
            if case == "empty" {
                fs::create_dir(&path).unwrap();
            }
            if matches!(case, "interrupted" | "format" | "lock" | "recovery") {
                let fixture = InventoryFixture::new();
                // Move a complete disposable store under the case's prepared parent.
                fs::rename(fixture.root(), &path).unwrap();
                match case {
                    "interrupted" => {
                        fs::rename(path.join("store.json"), path.join(".store-init.json")).unwrap()
                    }
                    "format" => fs::write(path.join("store.json"), br#"{"version":1}"#).unwrap(),
                    "lock" => {
                        fs::remove_file(path.join("locks/store.lock")).unwrap();
                        fs::create_dir(path.join("locks/store.lock")).unwrap();
                    }
                    _ => fs::write(path.join("pending-mutation.json"), b"{").unwrap(),
                }
            }
            let before = snapshot(directory.path());
            let mut command = cli();
            command.arg("--store").arg(&path);
            if json {
                command.arg("--json");
            }
            assert_exists_output(&command.args(["exists", "tool:t"]).output().unwrap(), 125);
            assert_eq!(snapshot(directory.path()), before, "{case}");
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("absent");
        for args in [
            vec!["exists"],
            vec!["exists", "tool"],
            vec!["exists", "tool:t", "extra"],
            vec!["exists", "tool:t", "--unknown"],
            vec!["exists", "tool@sha256:bad"],
        ] {
            let mut command = cli();
            command.arg("--store").arg(&path);
            if json {
                command.arg("--json");
            }
            assert_exists_output(&command.args(args).output().unwrap(), 2);
            assert!(!path.exists());
        }
        let bare = format!("sha256:{}", "a".repeat(64));
        assert_exists_output(
            &cli()
                .arg("--store")
                .arg(&path)
                .args(["exists", &bare])
                .output()
                .unwrap(),
            2,
        );
    }
    let output = cli().args(["exists", "--help"]).output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("SELECTOR"));
    assert!(output.stderr.is_empty());
}

fn exercise_exists_permissions() {
    use std::{fs, os::unix::fs::PermissionsExt};
    for json in [false, true] {
        for target in ["store.json", "locks/store.lock"] {
            let fixture = InventoryFixture::new();
            let path = fixture.root().join(target);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o0)).unwrap();
            let mut command = fixture.command();
            if json {
                command.arg("--json");
            }
            let output = command.args(["exists", "tool:t"]).output().unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            assert_exists_output(&output, 125);
        }
    }
}
