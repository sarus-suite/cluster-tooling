use assert_cmd::Command;
use hpc_image_store::{ImageSelector, Store, StoreConfig};
use predicates::prelude::*;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tempfile::{TempDir, tempdir};

const SOURCE: &str = "registry.example/image:release";

struct Fixture {
    directory: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            directory: tempdir().unwrap(),
        };
        fs::create_dir(fixture.root().join("bin")).unwrap();
        for tool in ["skopeo", "podman", "parallax", "hook"] {
            let path = fixture.root().join("bin").join(tool);
            fs::write(&path, include_str!("fixtures/pull-tools.py")).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let config = fixture.root().join("config/sarus-suite");
        fs::create_dir_all(&config).unwrap();
        fs::write(config.join("pull.conf"), format!(
            "parallax_imagestore = {:?}\npodman_path = {:?}\nparallax_path = {:?}\nmksquashfs_path = {:?}\n[hooks]\nparallax_imagestore_create = {:?}\n",
            fixture.root().join("parallax-store"),
            fixture.root().join("bin/podman"),
            fixture.root().join("bin/parallax"),
            fixture.root().join("mksquashfs"),
            fixture.root().join("bin/hook"),
        )).unwrap();
        fixture.select_image("first");
        fixture
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn select_image(&self, revision: &str) {
        let output = std::process::Command::new("/usr/bin/python3")
            .arg(self.root().join("bin/skopeo"))
            .env("PULL_FIXTURE", self.root())
            .args(["--prepare", revision])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn resolved(&self) -> String {
        format!(
            "registry.example/image@{}",
            fs::read_to_string(self.root().join("root")).unwrap()
        )
    }

    fn command(&self) -> Command {
        let mut paths = vec![self.root().join("bin")];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let mut command = Command::cargo_bin("sarusctl").unwrap();
        command
            .current_dir(self.root())
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("PULL_FIXTURE", self.root())
            .env("XDG_CONFIG_HOME", self.root().join("config"))
            .env("XDG_CONFIG_DIRS", self.root().join("system-config"))
            .env("REGISTRY_AUTH_FILE", self.root().join("auth.json"))
            .env("CONTAINERS_STORAGE_CONF", self.root().join("storage.conf"))
            .env_remove("FAIL_STAGE");
        command
    }

    fn calls(&self) -> Vec<Value> {
        let contents = fs::read_to_string(self.root().join("calls")).unwrap_or_default();
        contents
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn store(&self, path: impl AsRef<Path>) -> Store {
        Store::new(StoreConfig::new(self.root().join(path)).unwrap())
    }
}

#[test]
fn pull_refreshes_store_and_exports_resolved_digest_before_migration() {
    let fixture = Fixture::new();
    let first = fixture.resolved();
    for revision in ["first", "second"] {
        fixture.select_image(revision);
        fs::write(fixture.root().join("calls"), "").unwrap();
        fixture
            .command()
            .args(["pull", SOURCE])
            .assert()
            .success()
            .stdout(predicate::str::contains("captured transfer progress").not());
        let resolved = fixture.resolved();
        let store = fixture.store("hpc-imagestore");
        assert_eq!(
            store.resolve(&ImageSelector::new(SOURCE).unwrap()).unwrap(),
            Some(resolved.clone())
        );
        assert_eq!(
            fs::read_to_string(fixture.root().join("migrated")).unwrap(),
            resolved
        );

        let calls = fixture.calls();
        let actions: Vec<_> = calls
            .iter()
            .filter(|entry| !args(entry).contains(&"--help"))
            .map(|entry| (entry["tool"].as_str().unwrap(), args(entry)))
            .collect();
        assert_eq!(
            actions.iter().map(|(tool, _)| *tool).collect::<Vec<_>>(),
            [
                "skopeo", "skopeo", "skopeo", "podman", "podman", "hook", "parallax", "parallax"
            ]
        );
        assert!(
            actions[5]
                .1
                .contains(&fixture.root().join("parallax-store").to_str().unwrap())
        );
        assert!(
            actions[0]
                .1
                .contains(&format!("docker://{SOURCE}").as_str())
        );
        assert!(
            actions[1]
                .1
                .contains(&format!("docker://{resolved}").as_str())
        );
        assert!(actions[1].1.contains(&"--dest-shared-blob-dir"));
        assert!(actions[1].1.contains(&"--preserve-digests"));
        assert!(
            actions[2]
                .1
                .contains(&format!("containers-storage:{resolved}").as_str())
        );
        assert!(actions[2].1.contains(&"--src-shared-blob-dir"));
        assert!(actions[2].1.contains(&"--preserve-digests"));
        assert_eq!(actions[3].1.last(), Some(&resolved.as_str()));
        assert!(actions[4].1.contains(&"info"));
        for (_, arguments) in &actions[6..] {
            assert_eq!(arguments.last(), Some(&resolved.as_str()));
            assert!(arguments.contains(&fixture.root().join("graphroot").to_str().unwrap()));
            assert!(arguments.contains(&fixture.root().join("parallax-store").to_str().unwrap()));
            assert!(arguments.contains(&fixture.root().join("mksquashfs").to_str().unwrap()));
        }
        assert!(actions[6].1.contains(&"--migrate"));
        assert!(actions[7].1.contains(&"--exist"));
        for entry in &calls {
            assert_eq!(
                entry["auth"],
                fixture.root().join("auth.json").to_str().unwrap()
            );
            assert_eq!(
                entry["storage"],
                fixture.root().join("storage.conf").to_str().unwrap()
            );
        }
    }
    assert_ne!(fixture.resolved(), first);
    assert!(
        !fixture
            .store("hpc-imagestore")
            .exists(&ImageSelector::new(first).unwrap())
            .unwrap()
    );
}

fn args(entry: &Value) -> Vec<&str> {
    entry["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect()
}

#[test]
fn pull_overrides_and_digest_inputs_use_the_selected_store() {
    for tagged in [false, true] {
        let fixture = Fixture::new();
        let resolved = fixture.resolved();
        let source = if tagged {
            resolved.replace('@', ":release@")
        } else {
            resolved.clone()
        };
        fixture
            .command()
            .args([
                "--hpc-imagestore",
                "selected-hpc",
                "--parallax-imagestore",
                "selected-parallax",
                "--verbose",
                "pull",
                &source,
            ])
            .assert()
            .success();
        assert_eq!(
            fixture
                .store("selected-hpc")
                .resolve(&ImageSelector::new(&source).unwrap())
                .unwrap(),
            Some(resolved)
        );
        assert!(!fixture.root().join("hpc-imagestore").exists());
        assert!(fixture.root().join("selected-parallax").is_dir());
    }
}

#[test]
fn pull_failures_stop_pipeline_and_preserve_completed_acquisitions() {
    for (stage, operations) in [
        ("inspect", 1),
        ("pull", 2),
        ("export", 3),
        ("podman", 4),
        ("graphroot", 5),
        ("hook", 6),
        ("migrate", 7),
        ("exist", 8),
    ] {
        let fixture = Fixture::new();
        let result = fixture
            .command()
            .env("FAIL_STAGE", stage)
            .args(["pull", SOURCE])
            .assert()
            .code(1);
        if ["inspect", "pull", "export"].contains(&stage) {
            result
                .stderr(predicate::str::contains(format!("{stage} diagnostic")))
                .stderr(predicate::str::contains("registry.example/image"))
                .stderr(predicate::str::contains(
                    fixture.root().join("hpc-imagestore").to_str().unwrap(),
                ));
        }
        let calls = fixture.calls();
        let tools: Vec<_> = calls
            .iter()
            .filter(|entry| !args(entry).contains(&"--help"))
            .map(|entry| entry["tool"].as_str().unwrap())
            .collect();
        let expected = [
            "skopeo", "skopeo", "skopeo", "podman", "podman", "hook", "parallax", "parallax",
        ];
        assert_eq!(tools, expected[..operations], "{stage}");
        if ["export", "podman", "graphroot", "hook", "migrate", "exist"].contains(&stage) {
            assert_eq!(
                fixture
                    .store("hpc-imagestore")
                    .resolve(&ImageSelector::new(SOURCE).unwrap())
                    .unwrap(),
                Some(fixture.resolved())
            );
        }
        assert_eq!(
            fixture.root().join("parallax-store").exists(),
            ["migrate", "exist"].contains(&stage),
            "Parallax setup must only happen after graphroot lookup and a successful hook: {stage}"
        );
    }
}

#[test]
fn pull_validates_syntax_and_store_paths_without_running_tools() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["pull", "alpine"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("Invalid image alpine"));
    assert!(fixture.calls().is_empty());
    assert!(!fixture.root().join("parallax-store").exists());

    fixture
        .command()
        .args(["--hpc-imagestore", "missing-parent/store", "pull", SOURCE])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("HPC store pull failed"));
    assert!(!fixture.root().join("missing-parent").exists());
    assert!(fixture.calls().is_empty());
    assert!(!fixture.root().join("parallax-store").exists());

    let occupied: PathBuf = fixture.root().join("occupied");
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("keep"), "preserved").unwrap();
    fixture
        .command()
        .args(["--hpc-imagestore", "occupied", "pull", SOURCE])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("HPC store pull failed"));
    assert_eq!(
        fs::read_to_string(occupied.join("keep")).unwrap(),
        "preserved"
    );
    assert!(!occupied.join("store.json").exists());
    assert!(fixture.calls().is_empty());
    assert!(!fixture.root().join("parallax-store").exists());
}
