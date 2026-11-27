use assert_cmd::Command;
use hpc_image_store::{ImageSelector, ImportOptions, Store, StoreConfig, TransferContext};
use predicates::prelude::*;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tempfile::{TempDir, tempdir};

const IMAGE: &str = "registry.example:5000/analysis/tool:Release_1";

// Seed real stores through the public API without registry or containers-storage access.
const PRODUCER: &str = r#"#!/usr/bin/python3
import hashlib, json, pathlib, sys
a = sys.argv[1:]
if '--help' in a:
    print('--dest-shared-blob-dir --preserve-digests')
elif 'inspect' in a:
    print(hashlib.sha256(a[-1].encode()).hexdigest())
else:
    layout = pathlib.Path(a[-1][4:])
    blobs = pathlib.Path(a[a.index('--dest-shared-blob-dir') + 1]) / 'sha256'
    def blob(content, media):
        data = json.dumps(content).encode()
        digest = hashlib.sha256(data).hexdigest()
        (blobs / digest).write_bytes(data)
        return {'digest': 'sha256:' + digest, 'size': len(data), 'mediaType': media}
    config = blob({'os': 'linux', 'architecture': 'amd64', 'source': a[-2],
                   'rootfs': {'type': 'layers', 'diff_ids': []}},
                  'application/vnd.oci.image.config.v1+json')
    manifest = blob({'schemaVersion': 2, 'config': config, 'layers': []},
                    'application/vnd.oci.image.manifest.v1+json')
    (layout / 'oci-layout').write_text('{"imageLayoutVersion":"1.0.0"}')
    (layout / 'index.json').write_text(json.dumps({'schemaVersion': 2, 'manifests': [manifest]}))
"#;

const LEGACY_TOOL: &str = r#"#!/usr/bin/python3
import json, pathlib, sys
tool = pathlib.Path(__file__).name
a = sys.argv[1:]
root = pathlib.Path(__file__).parent
with (root / 'calls').open('a') as log:
    log.write(json.dumps({'tool': tool, 'args': a}) + '\n')
if tool == 'podman':
    assert 'info' in a
    print(root / 'graphroot')
elif tool == 'parallax':
    assert '--rmi' in a
    print('removal progress')
    sys.stderr.write('removal diagnostic\n')
else:
    assert tool == 'hook'
    store = pathlib.Path(a[0])
    store.mkdir(parents=True, exist_ok=True)
    store.chmod(0o750)
"#;

struct Fixture {
    directory: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            directory: tempdir().unwrap(),
        };
        for (name, script) in [
            ("producer", PRODUCER),
            ("podman", LEGACY_TOOL),
            ("parallax", LEGACY_TOOL),
            ("hook", LEGACY_TOOL),
        ] {
            let path = fixture.root().join(name);
            fs::write(&path, script).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let config = fixture.root().join("config/sarus-suite");
        fs::create_dir_all(&config).unwrap();
        fs::write(
            config.join("rmi.conf"),
            format!(
                "hpc_image_store = \"hpc-image-store\"\nparallax_imagestore = {:?}\npodman_path = {:?}\nparallax_path = {:?}\n[hooks]\nparallax_imagestore_create = {:?}\n",
                fixture.root().join("parallax-store"),
                fixture.root().join("podman"),
                fixture.root().join("parallax"),
                fixture.root().join("hook"),
            ),
        ).unwrap();
        fixture
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn store(&self, path: impl AsRef<Path>) -> Store {
        Store::new(StoreConfig::new(self.root().join(path)).unwrap())
    }

    fn seed(&self, store: &Store, source: &str, destination: &str) {
        store
            .import(
                &ImageSelector::new(source).unwrap(),
                Some(&ImageSelector::new(destination).unwrap()),
                &TransferContext {
                    podman: self.root().join("producer"),
                    skopeo: self.root().join("producer"),
                    platform: None,
                    storage: None,
                },
                &ImportOptions::default(),
            )
            .unwrap();
    }

    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("sarusctl").unwrap();
        command
            .current_dir(self.root())
            .env("XDG_CONFIG_HOME", self.root().join("config"))
            .env("XDG_CONFIG_DIRS", self.root().join("system-config"))
            .env("PATH", "");
        command
    }

    fn calls(&self) -> Vec<Value> {
        fs::read_to_string(self.root().join("calls"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn assert_no_tool_calls(&self) {
        assert!(self.calls().is_empty());
        assert!(!self.root().join("parallax-store").exists());
    }
}

#[test]
fn rmi_smoke_uses_the_effective_hpc_and_parallax_configuration() {
    let fixture = Fixture::new();
    let default = fixture.store("hpc-image-store");
    let selected = fixture.store("selected");
    fixture.seed(&default, "source:tag", IMAGE);
    fixture.seed(&selected, "source:tag", IMAGE);
    fixture
        .command()
        .arg("--hpc-image-store")
        .arg(fixture.root().join("selected"))
        .args(["--parallax-imagestore", "other/parallax", "rmi", IMAGE])
        .assert()
        .success()
        .stdout("")
        .stderr("");
    let selector = ImageSelector::new(IMAGE).unwrap();
    assert!(default.exists(&selector).unwrap());
    assert!(!selected.exists(&selector).unwrap());
    assert!(!fixture.root().join("other/parallax").exists());
    let calls = fixture.calls();
    assert_eq!(calls.len(), 2);
    let args = calls[1]["args"].as_array().unwrap();
    let store_index = args
        .iter()
        .position(|arg| arg == "--roStoragePath")
        .unwrap()
        + 1;
    assert_eq!(args[store_index], "other/parallax");
}

#[test]
fn rmi_reports_invalid_input_without_store_or_tool_side_effects() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["rmi", "alpine"])
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains("Invalid image argument"));
    fixture
        .command()
        .args(["--hpc-image-store", "", "rmi", IMAGE])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "a value is required for '--hpc-image-store <PATH>'",
        ));
    assert!(!fixture.root().join("hpc-image-store").exists());
    fixture.assert_no_tool_calls();
}

#[test]
fn plx_commands_are_not_available() {
    let fixture = Fixture::new();
    for args in [vec!["-h"], vec!["--help"], vec!["help"], vec![]] {
        let mut command = fixture.command();
        command
            .args(&args)
            .env("XDG_CONFIG_HOME", "/nonexistent/sarusctl-rmi-config");
        let output = command.output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(if args.is_empty() { 2 } else { 0 })
        );
        for stream in [&output.stdout, &output.stderr] {
            assert!(!String::from_utf8_lossy(stream).contains("plx"));
        }
    }
    for args in [
        vec!["plx", "--help"],
        vec!["plx", "rmi", "--help"],
        vec!["plx", "migrate", "--help"],
        vec!["help", "plx"],
    ] {
        fixture
            .command()
            .args(args)
            .env("XDG_CONFIG_HOME", "/nonexistent/sarusctl-rmi-config")
            .assert()
            .code(2)
            .stderr(predicate::str::contains("unrecognized subcommand"));
    }
    fixture
        .command()
        .args(["rmi", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("HPC and Parallax stores"))
        .stdout(predicate::str::contains("name:tag"));
    fixture.assert_no_tool_calls();
}

#[test]
fn rmi_cli_requires_commands_images_and_top_level_options_in_order() {
    let fixture = Fixture::new();
    for args in [
        vec!["plx"],
        vec!["plx", "rmi"],
        vec!["rmi"],
        vec!["plx", "--verbose", "rmi", IMAGE],
        vec!["plx", "rmi", IMAGE, "--parallax-imagestore", "other"],
        vec!["plx", "--hpc-image-store", "other", "rmi", IMAGE],
        vec!["rmi", IMAGE, "--hpc-image-store", "other"],
    ] {
        fixture.command().args(args).assert().code(2);
    }
    fixture.assert_no_tool_calls();
}
