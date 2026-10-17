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
        "registry.example:5000/team/tool:latest",
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
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("an explicit destination ref is required")
        );
        assert!(!root.exists());
    }
}

#[test]
fn command_help_describes_optional_destinations() {
    for (action, positional) in [("import", "[LOCAL_REF]"), ("export", "[DESTINATION]")] {
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
        vec!["list"],
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
fn help_is_successful_and_lists_only_stage_three_commands() {
    let output = cli().arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("import") && help.contains("export"));
    assert!(!help.contains("--storage-driver"));
    assert!(output.stderr.is_empty());
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
        "alpine",
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
