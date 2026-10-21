use std::process::Command;

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
        blocked = layout / 'cleanup-blocked'
        blocked.mkdir()
        (blocked / 'sentinel').write_text('leave for prune')
        blocked.chmod(0o500)
    elif os.getenv('HPC_TEST_DENY_CLEANUP') == 'sync':
        layout.parent.parent.chmod(0o300)
    print('subprocess output must not leak into result JSON')
"#;

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
    assert!(
        !help
            .lines()
            .any(|line| line.split_whitespace().next() == Some("prune"))
    );
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
    // Explicit-name removal also works after both metadata and graph damage.
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
fn damaged_inventory_prints_partial_results_and_explicit_remove_does_not_follow_it() {
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
    assert!(removed.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&removed.stdout).unwrap()["local_ref"],
        "tool:t"
    );
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
        assert_eq!(report["cleanup_warnings"], serde_json::json!([]));
        assert_eq!(fs::read_dir(root.join("tmp")).unwrap().count(), 0);
        assert!(imported.stderr.is_empty());
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
fn cleanup_warning_keeps_cli_success_and_json_separate_from_diagnostics() {
    // Permission failures must be exercised without root's DAC override. Match
    // the existing initialization permission tests' subprocess approach.
    if unsafe { libc::geteuid() } == 0 {
        use std::os::unix::process::CommandExt;
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "cleanup_warning_worker",
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
        exercise_cleanup_warning();
    }
}

#[test]
#[ignore = "unprivileged subprocess helper"]
fn cleanup_warning_worker() {
    exercise_cleanup_warning();
}

fn exercise_cleanup_warning() {
    use std::{fs, os::unix::fs::PermissionsExt};
    assert_ne!(unsafe { libc::geteuid() }, 0);
    for (json, failure) in [
        (false, "remove"),
        (true, "remove"),
        (false, "sync"),
        (true, "sync"),
    ] {
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
        // Restore the deliberately blocked private directory for test teardown,
        // even if an assertion below fails. The application must not do this.
        if root.join("tmp").is_dir() {
            fs::set_permissions(root.join("tmp"), fs::Permissions::from_mode(0o700)).unwrap();
        }
        if let Ok(entries) = fs::read_dir(root.join("tmp")) {
            for entry in entries {
                let blocked = entry.unwrap().path().join("layout/cleanup-blocked");
                if blocked.is_dir() {
                    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
                }
            }
        }
        assert_eq!(
            imported.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&imported.stderr)
        );
        let diagnostic = String::from_utf8(imported.stderr).unwrap();
        assert!(
            diagnostic.contains("warning:")
                && diagnostic.contains("image is published")
                && diagnostic.contains(if failure == "remove" {
                    "remove published staging"
                } else {
                    "sync staging parent after cleanup"
                }),
            "{diagnostic}"
        );
        if json {
            let report: serde_json::Value = serde_json::from_slice(&imported.stdout).unwrap();
            assert_eq!(report["local_ref"], "tool:tag");
            assert_eq!(report["cleanup_warnings"].as_array().unwrap().len(), 1);
            let path = std::path::Path::new(
                report["cleanup_warnings"][0]["staging_path"]
                    .as_str()
                    .unwrap(),
            );
            assert_eq!(path.parent(), Some(root.join("tmp").as_path()));
            assert_eq!(path.is_dir(), failure == "remove");
        } else {
            assert!(
                String::from_utf8(imported.stdout)
                    .unwrap()
                    .starts_with("Imported tool:tag")
            );
        }
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
    }
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
