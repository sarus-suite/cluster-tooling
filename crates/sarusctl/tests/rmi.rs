use assert_cmd::Command;
use hpc_image_store::{ImageSelector, ImportOptions, Store, StoreConfig, TransferContext};
use predicates::prelude::*;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tempfile::{TempDir, tempdir};

const IMAGE: &str = "registry.example:5000/analysis/tool:Release_1";
const KEEPER: &str = "analysis/keeper:tag";

// The producer seeds real stores through the public API, without registry or
// containers-storage access. Both aliases of a source share the same content.
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
import json, os, pathlib, sys
tool = pathlib.Path(__file__).name
a = sys.argv[1:]
root = pathlib.Path(__file__).parent
with (root / 'calls').open('a') as log:
    log.write(json.dumps({'tool': tool, 'args': a}) + '\n')
if os.getenv('FAIL_STAGE') == tool:
    sys.stderr.write(tool + ' failure diagnostic\n')
    sys.exit(19)
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
            .env("PATH", "")
            .env_remove("FAIL_STAGE");
        command
    }

    fn calls(&self) -> Vec<Value> {
        fs::read_to_string(self.root().join("calls"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn assert_store_hooks(&self, parallax: &Path, hpc: &Path, runs: usize) {
        let calls = self.calls();
        assert_eq!(calls.len(), runs * 2);
        for pair in calls.chunks_exact(2) {
            for (call, path) in pair.iter().zip([parallax, hpc]) {
                assert_eq!(call["tool"], "hook");
                assert_eq!(call["args"], serde_json::json!([path]));
                assert_eq!(
                    fs::metadata(self.root().join(path))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o750
                );
            }
        }
    }

    fn assert_no_tool_calls(&self) {
        assert!(self.calls().is_empty());
        assert!(!self.root().join("parallax-store").exists());
    }
}

#[test]
fn rmi_removes_only_the_exact_hpc_ref_and_preserves_shared_content() {
    let fixture = Fixture::new();
    let store = fixture.store("hpc-image-store");
    fixture.seed(&store, "source:tag", IMAGE);
    fixture.seed(&store, "source:tag", KEEPER);
    let keeper = ImageSelector::new(KEEPER).unwrap();
    let resolved = store.resolve(&keeper).unwrap();
    fixture
        .command()
        .args(["rmi", IMAGE])
        .assert()
        .success()
        .stdout("")
        .stderr("");
    assert!(!store.exists(&ImageSelector::new(IMAGE).unwrap()).unwrap());
    assert_eq!(store.resolve(&keeper).unwrap(), resolved);
    assert!(
        !store
            .check(&hpc_image_store::CheckScope::All)
            .unwrap()
            .has_errors()
    );
    fixture
        .command()
        .args(["rmi", IMAGE])
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains("reference does not exist"));
    fixture.assert_store_hooks(
        &fixture.root().join("parallax-store"),
        Path::new("hpc-image-store"),
        2,
    );
}

#[test]
fn rmi_honors_hpc_override_over_the_effective_parallax_sibling() {
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
    assert!(fixture.root().join("other/parallax").is_dir());
    fixture.assert_store_hooks(
        Path::new("other/parallax"),
        &fixture.root().join("selected"),
        1,
    );
}

#[test]
fn rmi_preserves_configured_hpc_store_with_parallax_override() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.root().join("other")).unwrap();
    let store = fixture.store("hpc-image-store");
    fixture.seed(&store, "source:tag", IMAGE);
    fixture
        .command()
        .args(["--parallax-imagestore", "other/parallax", "rmi", IMAGE])
        .assert()
        .success()
        .stdout("")
        .stderr("");
    assert!(!store.exists(&ImageSelector::new(IMAGE).unwrap()).unwrap());
    assert!(fixture.root().join("other/parallax").is_dir());
    assert!(!fixture.root().join("other/hpc-image-store").exists());
    fixture.assert_store_hooks(Path::new("other/parallax"), Path::new("hpc-image-store"), 1);
}

#[test]
fn rmi_rejects_invalid_inputs_without_store_or_tool_side_effects() {
    let fixture = Fixture::new();
    for image in [
        "alpine",
        "",
        "../escape:tag",
        "bad image:tag",
        "alpine@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "alpine:tag@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ] {
        fixture
            .command()
            .args(["rmi", image])
            .assert()
            .code(1)
            .stdout("")
            .stderr(predicate::str::contains("Invalid image argument"));
    }
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
fn rmi_creates_missing_directories_but_leaves_uninitialized_stores_empty() {
    for empty in [false, true] {
        let fixture = Fixture::new();
        let root = fixture.root().join("hpc-image-store");
        if empty {
            fs::create_dir(&root).unwrap();
        }
        fixture
            .command()
            .args(["rmi", IMAGE])
            .assert()
            .code(1)
            .stdout("")
            .stderr(predicate::str::contains("HPC store remove failed"))
            .stderr(predicate::str::contains(root.to_str().unwrap()))
            .stderr(predicate::str::contains(IMAGE));
        assert!(root.is_dir());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        fixture.assert_store_hooks(
            &fixture.root().join("parallax-store"),
            Path::new("hpc-image-store"),
            1,
        );
    }
}

#[test]
fn rmi_reports_recovery_failure_even_after_the_reference_is_removed() {
    let fixture = Fixture::new();
    let store = fixture.store("hpc-image-store");
    fixture.seed(&store, "source:tag", IMAGE);
    fixture.seed(&store, "other:tag", KEEPER);
    let keeper = store
        .resolve(&ImageSelector::new(KEEPER).unwrap())
        .unwrap()
        .unwrap();
    let digest = keeper.split_once("@sha256:").unwrap().1;
    let declaration = store.config().root().join("blobs/sha256").join(digest);
    fs::remove_file(&declaration).unwrap();
    fixture
        .command()
        .args(["rmi", IMAGE])
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains("HPC store remove failed"))
        .stderr(predicate::str::contains("RefRemoved"))
        .stderr(predicate::str::contains("mutation recovery"))
        .stderr(predicate::str::contains(declaration.to_str().unwrap()));
    // Inspect only the namespace: normal queries are blocked by pending recovery.
    let terminal = store
        .config()
        .root()
        .join("refs")
        .join(
            hpc_image_store::LocalRef::new(IMAGE)
                .unwrap()
                .encoded_path(),
        )
        .join("ref");
    assert_eq!(
        terminal.symlink_metadata().unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    fixture.assert_store_hooks(
        &fixture.root().join("parallax-store"),
        Path::new("hpc-image-store"),
        1,
    );
}

#[test]
fn plx_rmi_passes_legacy_context_and_preserves_hpc_contents() {
    for verbose in [false, true] {
        let fixture = Fixture::new();
        let store = fixture.store("hpc-image-store");
        fixture.seed(&store, "source:tag", IMAGE);
        let before = store.resolve(&ImageSelector::new(IMAGE).unwrap()).unwrap();
        let selected = fixture.root().join("selected-parallax");
        let mut command = fixture.command();
        if verbose {
            command.arg("-v");
        }
        command
            .arg("--hpc-image-store")
            .arg(fixture.root().join("selected-hpc"))
            .args([
                "--parallax-imagestore",
                selected.to_str().unwrap(),
                "plx",
                "rmi",
                IMAGE,
            ]);
        let output = command.assert().success();
        if verbose {
            output
                .stdout(predicate::str::contains("removal progress"))
                .stderr(predicate::str::contains("removal diagnostic"));
        } else {
            output.stdout("").stderr("");
        }
        let calls = fixture.calls();
        assert_eq!(
            calls
                .iter()
                .map(|call| call["tool"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["podman", "hook", "hook", "parallax"]
        );
        assert_eq!(calls[1]["args"], serde_json::json!([selected]));
        assert_eq!(
            calls[2]["args"],
            serde_json::json!([fixture.root().join("selected-hpc")])
        );
        let args: Vec<_> = calls[3]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap())
            .collect();
        for (flag, value) in [
            ("--roStoragePath", selected.to_str().unwrap()),
            (
                "--podmanRoot",
                fixture.root().join("graphroot").to_str().unwrap(),
            ),
            ("--image", IMAGE),
        ] {
            assert_eq!(
                args[args.iter().position(|arg| *arg == flag).unwrap() + 1],
                value
            );
        }
        assert!(args.contains(&"--rmi"));
        assert!(selected.is_dir());
        assert!(!fixture.root().join("parallax-store").exists());
        assert!(fixture.root().join("selected-hpc").is_dir());
        assert_eq!(
            fs::read_dir(fixture.root().join("selected-hpc"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            store.resolve(&ImageSelector::new(IMAGE).unwrap()).unwrap(),
            before
        );
    }
}

#[test]
fn plx_rmi_stops_at_each_legacy_failure() {
    for (stage, count, diagnostic) in [
        ("podman", 1, "podman failure diagnostic"),
        ("parallax", 4, "parallax failure diagnostic"),
    ] {
        let fixture = Fixture::new();
        fixture
            .command()
            .env("FAIL_STAGE", stage)
            .args(["plx", "rmi", IMAGE])
            .assert()
            .code(1)
            .stderr(predicate::str::contains(diagnostic));
        assert_eq!(fixture.calls().len(), count);
        assert_eq!(
            fixture.root().join("hpc-image-store").is_dir(),
            stage == "parallax"
        );
    }
}

#[test]
fn plx_rmi_continues_when_creation_hooks_fail() {
    let fixture = Fixture::new();
    // Store setup currently treats hook failures as best effort.
    fixture
        .command()
        .env("FAIL_STAGE", "hook")
        .args(["plx", "rmi", IMAGE])
        .assert()
        .success()
        .stderr("hook failure diagnostic\nhook failure diagnostic\n");
    let calls = fixture.calls();
    assert_eq!(
        calls
            .iter()
            .map(|call| call["tool"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["podman", "hook", "hook", "parallax"]
    );
    assert!(!fixture.root().join("parallax-store").exists());
    assert!(!fixture.root().join("hpc-image-store").exists());
}

#[test]
fn plx_is_hidden_until_explicitly_requested() {
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
        vec!["help", "plx"],
    ] {
        fixture
            .command()
            .args(args)
            .env("XDG_CONFIG_HOME", "/nonexistent/sarusctl-rmi-config")
            .assert()
            .success()
            .stdout(predicate::str::contains("rmi"));
    }
    fixture
        .command()
        .args(["plx", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Advanced, low-level"));
    fixture
        .command()
        .args(["rmi", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("HPC store"))
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
