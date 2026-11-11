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
        fs::create_dir(fixture.root().join("runtime")).unwrap();
        for tool in ["skopeo", "podman", "parallax", "hook"] {
            let path = fixture.root().join("bin").join(tool);
            fs::write(&path, include_str!("fixtures/pull-tools.py")).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let config = fixture.root().join("config/sarus-suite");
        fs::create_dir_all(&config).unwrap();
        fs::write(config.join("pull.conf"), format!(
            "hpc_image_store = {:?}\nparallax_imagestore = {:?}\npodman_path = {:?}\nparallax_path = {:?}\nskopeo_path = {:?}\nmksquashfs_path = {:?}\n[hooks]\nparallax_imagestore_create = {:?}\n",
            fixture.root().join("hpc-imagestore"),
            fixture.root().join("parallax-store"),
            fixture.root().join("bin/podman"),
            fixture.root().join("bin/parallax"),
            fixture.root().join("bin/skopeo"),
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
            .env("XDG_RUNTIME_DIR", self.root().join("runtime"))
            .env("XDG_CONFIG_HOME", self.root().join("config"))
            .env("XDG_CONFIG_DIRS", self.root().join("system-config"))
            .env("REGISTRY_AUTH_FILE", self.root().join("auth.json"))
            .env("CONTAINERS_STORAGE_CONF", self.root().join("storage.conf"))
            .env_remove("FAIL_STAGE")
            .env_remove("FAIL_CLEANUP");
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
                "skopeo", "skopeo", "parallax", "skopeo", "podman", "hook", "parallax", "parallax",
                "podman"
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
        assert!(actions[2].1.contains(&"--exist"));
        assert_eq!(actions[2].1.last(), Some(&resolved.as_str()));
        assert!(!actions[2].1.contains(&"--podmanRoot"));
        assert!(
            actions[2]
                .1
                .contains(&fixture.root().join("parallax-store").to_str().unwrap())
        );
        assert!(
            actions[2]
                .1
                .contains(&fixture.root().join("mksquashfs").to_str().unwrap())
        );
        let storage: Value =
            serde_json::from_slice(&fs::read(fixture.root().join("export-storage")).unwrap())
                .unwrap();
        let graphroot = storage["graphroot"].as_str().unwrap();
        let runroot = storage["runroot"].as_str().unwrap();
        assert!(actions[3].1.contains(
            &format!("containers-storage:[overlay@{graphroot}+{runroot}]{resolved}").as_str()
        ));
        assert!(actions[4].1.contains(&graphroot));
        assert!(actions[4].1.contains(&runroot));
        assert!(!Path::new(graphroot).parent().unwrap().exists());
        assert!(actions[3].1.contains(&"--src-shared-blob-dir"));
        assert!(actions[3].1.contains(&"--preserve-digests"));
        assert_eq!(actions[4].1.last(), Some(&resolved.as_str()));
        for (_, arguments) in &actions[6..8] {
            assert_eq!(arguments.last(), Some(&resolved.as_str()));
            assert!(arguments.contains(&graphroot));
            assert!(arguments.contains(&fixture.root().join("parallax-store").to_str().unwrap()));
            assert!(arguments.contains(&fixture.root().join("mksquashfs").to_str().unwrap()));
        }
        assert!(actions[6].1.contains(&"--migrate"));
        assert!(actions[7].1.contains(&"--exist"));
        assert!(actions[8].1.contains(&"unshare"));
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
fn repeated_pull_skips_export_and_migration_until_digest_changes() {
    let fixture = Fixture::new();
    fixture.command().args(["pull", SOURCE]).assert().success();
    let first = fixture.resolved();

    // The fast path must not depend on the image still being in Podman storage.
    fs::remove_file(fixture.root().join("exported")).unwrap();
    fs::write(fixture.root().join("calls"), "").unwrap();
    fixture
        .command()
        .args(["pull", SOURCE])
        .assert()
        .success()
        .stderr(predicate::str::contains("Exporting").not())
        .stderr(predicate::str::contains("Migrating").not());
    let calls = fixture.calls();
    let actions: Vec<_> = calls
        .iter()
        .filter(|entry| !args(entry).contains(&"--help"))
        .collect();
    assert_eq!(
        actions
            .iter()
            .map(|entry| entry["tool"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["skopeo", "skopeo", "parallax"]
    );
    assert!(args(actions[2]).contains(&"--exist"));
    assert_eq!(args(actions[2]).last(), Some(&first.as_str()));
    assert!(!fixture.root().join("exported").exists());
    assert_eq!(
        fixture
            .store("hpc-imagestore")
            .resolve(&ImageSelector::new(SOURCE).unwrap())
            .unwrap(),
        Some(first.clone())
    );

    fixture.select_image("second");
    fs::write(fixture.root().join("calls"), "").unwrap();
    fixture.command().args(["pull", SOURCE]).assert().success();
    let second = fixture.resolved();
    assert_ne!(first, second);
    assert_eq!(
        fs::read_to_string(fixture.root().join("exported")).unwrap(),
        second
    );
    assert_eq!(
        fs::read_to_string(fixture.root().join("migrated")).unwrap(),
        second
    );
    let calls = fixture.calls();
    assert!(calls.iter().any(|entry| args(entry).contains(&"--migrate")));
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
            .arg("--hpc-imagestore")
            .arg(fixture.root().join("selected-hpc"))
            .args([
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
        ("export", 4),
        ("podman", 5),
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
            .filter(|entry| !args(entry).contains(&"--help") && !args(entry).contains(&"unshare"))
            .map(|entry| entry["tool"].as_str().unwrap())
            .collect();
        let expected = [
            "skopeo", "skopeo", "parallax", "skopeo", "podman", "hook", "parallax", "parallax",
        ];
        assert_eq!(tools, expected[..operations], "{stage}");
        assert_eq!(
            calls
                .iter()
                .filter(|entry| args(entry).contains(&"unshare"))
                .count(),
            usize::from(operations >= 4),
            "{stage}"
        );
        let runtime_base = fixture.root().join("runtime/sarus-suite");
        if runtime_base.exists() {
            assert_eq!(fs::read_dir(runtime_base).unwrap().count(), 0, "{stage}");
        }
        if ["export", "podman", "hook", "migrate", "exist"].contains(&stage) {
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
            "Parallax setup must only happen after a successful hook: {stage}"
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
        .stderr(predicate::str::contains(
            "Error constructing selector for image alpine",
        ));
    assert!(fixture.calls().is_empty());
    assert!(!fixture.root().join("parallax-store").exists());

    fixture
        .command()
        .arg("--hpc-imagestore")
        .arg(fixture.root().join("missing-parent/store"))
        .args(["pull", SOURCE])
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
        .arg("--hpc-imagestore")
        .arg(&occupied)
        .args(["pull", SOURCE])
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

fn run_input(fixture: &Fixture, yaml: bool) -> PathBuf {
    let path = fixture
        .root()
        .join(if yaml { "pod.yaml" } else { "job.toml" });
    let contents = if yaml {
        format!(
            "apiVersion: v1\nkind: Pod\nmetadata:\n  name: training-pod\nspec:\n  containers:\n    - name: app\n      image: {SOURCE}\n"
        )
    } else {
        format!("image = {SOURCE:?}\n")
    };
    fs::write(&path, contents).unwrap();
    path
}

#[test]
fn run_acquires_images_from_missing_empty_and_pending_stores() {
    for state in ["missing", "empty", "pending"] {
        for yaml in [false, true] {
            let fixture = Fixture::new();
            match state {
                "empty" => fs::create_dir(fixture.root().join("hpc-imagestore")).unwrap(),
                "pending" => {
                    fixture.store("hpc-imagestore").list().unwrap();
                    fs::write(
                        fixture.root().join("hpc-imagestore/.store-init-pending"),
                        "",
                    )
                    .unwrap();
                }
                _ => {}
            }
            let input = run_input(&fixture, yaml);
            let original = fs::read(&input).unwrap();
            fixture
                .command()
                .args(["run", input.to_str().unwrap(), "true"])
                .assert()
                .success();
            assert_eq!(fs::read(&input).unwrap(), original);
            assert_eq!(
                fixture
                    .store("hpc-imagestore")
                    .resolve(&ImageSelector::new(SOURCE).unwrap())
                    .unwrap(),
                Some(fixture.resolved())
            );
            let calls = fixture.calls();
            let registry_calls: Vec<_> = calls
                .iter()
                .filter(|entry| {
                    entry["tool"] == "skopeo"
                        && args(entry).iter().any(|arg| arg.starts_with("docker://"))
                })
                .collect();
            assert_eq!(registry_calls.len(), 2, "{state}, yaml={yaml}");
            if yaml {
                assert_eq!(
                    fs::read(fixture.root().join("manifest-play")).unwrap(),
                    fs::read(fixture.root().join("manifest-down")).unwrap()
                );
                let prepared =
                    fs::read_to_string(fixture.root().join("manifest-path-play")).unwrap();
                assert_eq!(
                    prepared,
                    fs::read_to_string(fixture.root().join("manifest-path-down")).unwrap()
                );
                assert!(!Path::new(&prepared).exists());
            } else {
                assert_eq!(
                    fs::read_to_string(fixture.root().join("executed")).unwrap(),
                    fixture.resolved()
                );
            }
            assert_eq!(
                fs::read_dir(fixture.root().join("runtime/sarus-suite"))
                    .unwrap()
                    .count(),
                0
            );
        }
    }
}

#[test]
fn run_exports_cached_images_without_refreshing_the_registry() {
    for yaml in [false, true] {
        let fixture = Fixture::new();
        let input = run_input(&fixture, yaml);
        let resolved = fixture.resolved();
        // Stop after acquisition to leave the image cached only in the HPC store.
        fixture
            .command()
            .env("FAIL_STAGE", "export")
            .args(["pull", SOURCE])
            .assert()
            .code(1);
        for prepared in [false, true] {
            fs::write(fixture.root().join("calls"), "").unwrap();
            fixture
                .command()
                .env("WORKLOAD_EXIT_CODE", "7")
                .args(["run", input.to_str().unwrap(), "true"])
                .assert()
                .code(7);
            let calls = fixture.calls();
            assert!(
                !calls
                    .iter()
                    .any(|entry| args(entry).iter().any(|arg| arg.starts_with("docker://")))
            );
            assert_eq!(
                calls
                    .iter()
                    .filter(|entry| entry["tool"] == "skopeo"
                        && args(entry).contains(&"copy")
                        && !args(entry).contains(&"--help"))
                    .count(),
                usize::from(!prepared)
            );
            assert_eq!(
                calls
                    .iter()
                    .filter(|entry| args(entry).contains(&"--migrate"))
                    .count(),
                usize::from(!prepared)
            );
            assert_eq!(
                fs::read_to_string(fixture.root().join("migrated")).unwrap(),
                resolved
            );
        }
        // A changed registry tag must not replace the locally cached identity during run.
        fixture.select_image("second");
        fs::write(fixture.root().join("calls"), "").unwrap();
        fixture
            .command()
            .env("EXPECTED_REFERENCE", &resolved)
            .args(["run", input.to_str().unwrap(), "true"])
            .assert()
            .success();
        assert!(
            !fixture
                .calls()
                .iter()
                .any(|entry| entry["tool"] == "skopeo")
        );
    }
}

#[test]
fn run_rejects_damaged_and_inaccessible_stores_without_acquisition() {
    for state in ["occupied", "damaged", "inaccessible"] {
        if state == "inaccessible" && users::get_effective_uid() == 0 {
            continue; // Root bypasses these permission restrictions.
        }
        let fixture = Fixture::new();
        let input = run_input(&fixture, false);
        let root = fixture.root().join("hpc-imagestore");
        if state == "occupied" {
            fs::create_dir(&root).unwrap();
            fs::write(root.join("keep"), "preserved").unwrap();
        } else {
            fixture.store("hpc-imagestore").list().unwrap();
            if state == "damaged" {
                fs::write(root.join("store.json"), "invalid marker").unwrap();
            } else {
                fs::set_permissions(
                    root.join("locks/store.lock"),
                    fs::Permissions::from_mode(0o000),
                )
                .unwrap();
            }
        }
        fixture
            .command()
            .args(["run", input.to_str().unwrap()])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("HPC store"));
        if state == "inaccessible" {
            fs::set_permissions(
                root.join("locks/store.lock"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        assert!(
            !fixture
                .calls()
                .iter()
                .any(|entry| entry["tool"] == "skopeo")
        );
        if state == "occupied" {
            assert_eq!(fs::read_to_string(root.join("keep")).unwrap(), "preserved");
        }
    }
}

#[test]
fn run_retains_prepared_yaml_when_teardown_fails() {
    let fixture = Fixture::new();
    let input = run_input(&fixture, true);
    fixture
        .command()
        .env("FAIL_STAGE", "down")
        .args(["run", input.to_str().unwrap()])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("rootdirs retained"));
    let prepared = fs::read_to_string(fixture.root().join("manifest-path-down")).unwrap();
    assert!(Path::new(&prepared).is_file());
    assert_eq!(
        fs::read(&prepared).unwrap(),
        fs::read(fixture.root().join("manifest-play")).unwrap()
    );
}

#[test]
fn run_yaml_preserves_octal_numbers_and_other_scalar_spellings() {
    let fixture = Fixture::new();
    let input = fixture.root().join("pod.yaml");
    let original = format!(
        "# Grüße 🐧\r\napiVersion: v1\r\nkind: Pod\r\nmetadata:\r\n  name: training-pod\r\n  annotations:\r\n    quoted: '01000'\r\nspec:\r\n  securityContext:\r\n    runAsUser: 01000\r\n  containers:\r\n    - name: app\r\n      image: {SOURCE} # preserve comment\r\n      securityContext:\r\n        privileged: yes\r\n"
    );
    fs::write(&input, &original).unwrap();
    fixture
        .command()
        .args(["run", input.to_str().unwrap(), "true"])
        .assert()
        .success();

    let expected = original.replace(SOURCE, &format!("\"{}\"", fixture.resolved()));
    for phase in ["play", "down"] {
        let prepared =
            fs::read_to_string(fixture.root().join(format!("manifest-{phase}"))).unwrap();
        assert_eq!(prepared, expected);
        // Check the source spelling; the inspection tree is not the input passed to Podman.
        // Leaving 01000 unquoted preserves Podman's interpretation as UID 512.
        assert!(prepared.contains("runAsUser: 01000\r\n"));
    }
    assert_eq!(fs::read_to_string(input).unwrap(), original);
}

#[test]
fn run_cli_store_overrides_take_precedence_over_edf_annotations() {
    let fixture = Fixture::new();
    let input = run_input(&fixture, false);
    let annotated_hpc = fixture.root().join("annotated-hpc");
    let annotated_parallax = fixture.root().join("annotated-parallax");
    fs::write(
        &input,
        format!(
            "image = {SOURCE:?}\n[annotations]\n\"com.sarus.hpc_image_store\" = {annotated_hpc:?}\n\"com.sarus.parallax_imagestore\" = {annotated_parallax:?}\n"
        ),
    ).unwrap();
    let selected_parallax = fixture.root().join("selected-parallax");
    fixture
        .command()
        .arg("--hpc-imagestore")
        .arg(fixture.root().join("selected-hpc"))
        .arg("--parallax-imagestore")
        .arg(&selected_parallax)
        .arg("run")
        .arg(&input)
        .arg("true")
        .assert()
        .success();

    let resolved = fixture.resolved();
    assert_eq!(
        fixture
            .store("selected-hpc")
            .resolve(&ImageSelector::new(SOURCE).unwrap())
            .unwrap(),
        Some(resolved.clone())
    );
    let calls = fixture.calls();
    let migration = calls
        .iter()
        .find(|entry| entry["tool"] == "parallax" && args(entry).contains(&"--migrate"))
        .unwrap();
    let arguments = args(migration);
    let store_index = arguments
        .iter()
        .position(|arg| *arg == "--roStoragePath")
        .unwrap()
        + 1;
    assert_eq!(arguments[store_index], selected_parallax.to_str().unwrap());
    assert_eq!(
        fs::read_to_string(fixture.root().join("migrated")).unwrap(),
        resolved
    );
    assert!(selected_parallax.is_dir());
    assert!(!annotated_hpc.exists());
    assert!(!annotated_parallax.exists());
}

#[test]
fn run_digest_inputs_honor_store_overrides() {
    for yaml in [false, true] {
        let fixture = Fixture::new();
        let input = run_input(&fixture, yaml);
        let resolved = fixture.resolved();
        let source = if yaml {
            resolved.replace('@', ":release@")
        } else {
            resolved.clone()
        };
        let contents = fs::read_to_string(&input).unwrap().replace(SOURCE, &source);
        fs::write(&input, &contents).unwrap();
        fixture
            .command()
            .arg("--hpc-imagestore")
            .arg(fixture.root().join("selected-hpc"))
            .args([
                "--parallax-imagestore",
                "selected-parallax",
                "--verbose",
                "run",
                input.to_str().unwrap(),
                "true",
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
        assert!(fixture.root().join("selected-parallax").is_dir());
        assert!(!fixture.root().join("hpc-imagestore").exists());
        assert!(!fixture.root().join("parallax-store").exists());
        assert_eq!(fs::read_to_string(input).unwrap(), contents);
    }
}

#[test]
fn pull_cleans_runtime_directory_when_storage_path_is_invalid() {
    let fixture = Fixture::new();
    let runtime = fixture.root().join("runtime+invalid");
    fs::create_dir(&runtime).unwrap();
    fixture
        .command()
        .env("XDG_RUNTIME_DIR", &runtime)
        .args(["pull", SOURCE])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "cannot be represented in a storage specifier",
        ));
    assert_eq!(
        fs::read_dir(runtime.join("sarus-suite")).unwrap().count(),
        0
    );
    assert!(!fixture.root().join("export-storage").exists());
}

#[test]
fn relative_hpc_override_rejects_pull_and_run_before_running_tools() {
    for operation in ["pull", "run-edf", "run-yaml"] {
        let fixture = Fixture::new();
        let mut command = fixture.command();
        command.args(["--hpc-imagestore", "selected-hpc"]);
        let input = if operation == "pull" {
            command.args(["pull", SOURCE]);
            None
        } else {
            let path = run_input(&fixture, operation == "run-yaml");
            let original = fs::read(&path).unwrap();
            command.arg("run").arg(&path).arg("true");
            Some((path, original))
        };
        command
            .assert()
            .code(1)
            .stdout(predicate::str::is_empty())
            .stderr(predicate::str::contains(
                "--hpc-imagestore value must be an absolute path",
            ));
        assert!(fixture.calls().is_empty(), "{operation}");
        assert!(!fixture.root().join("selected-hpc").exists());
        assert!(!fixture.root().join("hpc-imagestore").exists());
        assert!(!fixture.root().join("parallax-store").exists());
        assert_eq!(
            fs::read_dir(fixture.root().join("runtime"))
                .unwrap()
                .count(),
            0
        );
        if let Some((path, original)) = input {
            assert_eq!(fs::read(path).unwrap(), original);
        }
    }
}

#[test]
fn migration_error_retains_cleanup_failure_diagnostic() {
    let fixture = Fixture::new();
    fixture
        .command()
        .env("FAIL_STAGE", "migrate")
        .env("FAIL_CLEANUP", "1")
        .args(["pull", SOURCE])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("migrate diagnostic"))
        .stderr(predicate::str::contains(
            "failed to clean disposable Podman store",
        ))
        .stderr(predicate::str::contains("cleanup diagnostic"));
}
