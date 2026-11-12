use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tempfile::{TempDir, tempdir};

struct Fixture {
    directory: TempDir,
    podman: PathBuf,
    hook: PathBuf,
}

fn executable(path: &Path, script: &str) {
    fs::write(path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

impl Fixture {
    fn new() -> Self {
        let directory = tempdir().unwrap();
        let podman = directory.path().join("podman");
        let hook = directory.path().join("hook");
        executable(
            &podman,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$CALLS_FILE"
case " $* " in
    *" info "*) printf '/stub/graphroot\n' ;;
    *" images "*)
        printf 'LEGACY IMAGE TABLE\n'
        printf 'legacy warning\n' >&2
        exit "${IMAGES_EXIT:-0}"
        ;;
    *) exit 99 ;;
esac
"#,
        );
        executable(
            &hook,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$HOOK_FILE"
if [ "${HOOK_EXIT:-0}" -eq 0 ] && [ -n "$1" ]; then
    /bin/mkdir -p "$1" && /bin/chmod 750 "$1"
fi
printf 'hook stdout\n'
printf 'hook stderr\n' >&2
exit "${HOOK_EXIT:-0}"
"#,
        );
        let fixture = Self {
            directory,
            podman,
            hook,
        };
        fixture.configure(
            &fixture.root().join("parallax"),
            &fixture.podman,
            &fixture.hook,
        );
        fixture
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn configure(&self, parallax: &Path, podman: &Path, hook: &Path) {
        let config_dir = self.root().join("config/sarus-suite");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("images.conf"),
            format!(
                "hpc_image_store = \"hpc-image-store\"\nparallax_imagestore = {parallax:?}\npodman_path = {podman:?}\n[hooks]\nparallax_imagestore_create = {hook:?}\n",
            ),
        ).unwrap();
    }

    fn assert_hook_paths(&self, paths: &[&Path]) {
        let calls = fs::read_to_string(self.root().join("hook-calls")).unwrap();
        let expected: Vec<_> = paths.iter().map(|path| path.to_str().unwrap()).collect();
        assert_eq!(calls.lines().collect::<Vec<_>>(), expected);
    }

    fn command(&self) -> Command {
        Command::from_std(self.std_command())
    }

    fn std_command(&self) -> std::process::Command {
        let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin("sarusctl"));
        command
            .current_dir(self.root())
            .env("XDG_CONFIG_HOME", self.root().join("config"))
            .env("XDG_CONFIG_DIRS", self.root().join("system-config"))
            .env("CALLS_FILE", self.root().join("calls"))
            .env("HOOK_FILE", self.root().join("hook-calls"))
            .env_remove("IMAGES_EXIT")
            .env_remove("HOOK_EXIT");
        command
    }
}

#[test]
fn images_option_placement_and_help() {
    Command::cargo_bin("sarusctl")
        .unwrap()
        .args(["images", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--all"))
        .stdout(predicate::str::contains("--digests"));
    Command::cargo_bin("sarusctl")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--hpc-image-store"));
    for args in [
        vec!["--all", "images"],
        vec!["validate", "--all"],
        vec!["--digests", "images"],
        vec!["validate", "--digests"],
        vec!["images", "--hpc-image-store", "/tmp/hpc"],
    ] {
        Command::cargo_bin("sarusctl")
            .unwrap()
            .args(args)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("unexpected argument"));
    }
}

#[test]
fn digests_controls_hpc_column_and_podman_flag() {
    for all in [false, true] {
        for digests in [false, true] {
            let fixture = Fixture::new();
            let mut command = fixture.command();
            command.arg("images");
            if all {
                command.arg("--all");
            }
            if digests {
                command.arg("--digests");
            }
            let result = command.assert().success();
            let stdout = String::from_utf8_lossy(&result.get_output().stdout);
            assert_eq!(stdout.contains("DIGEST"), digests);
            assert_eq!(stdout.contains("LEGACY IMAGE TABLE"), all);
            if all {
                let calls = fs::read_to_string(fixture.root().join("calls")).unwrap();
                let calls: Vec<_> = calls.lines().collect();
                assert_eq!(calls.len(), 2);
                assert!(!calls[0].contains("--digests"));
                assert!(calls[1].ends_with(if digests {
                    "images --digests"
                } else {
                    "images"
                }));
            } else {
                assert!(!fixture.root().join("calls").exists());
                result.stderr("hook stderr\nhook stdout\n".repeat(2));
            }
            let paths = [
                fixture.root().join("parallax"),
                PathBuf::from("hpc-image-store"),
            ];
            let expected: Vec<_> = paths
                .iter()
                .map(PathBuf::as_path)
                .cycle()
                .take(if all { 4 } else { 2 })
                .collect();
            fixture.assert_hook_paths(&expected);
        }
    }
}

#[test]
fn default_images_initializes_both_stores_through_hook_without_podman() {
    let fixture = Fixture::new();
    fixture.configure(
        &fixture.root().join("parallax"),
        &fixture.root().join("missing-podman"),
        &fixture.hook,
    );
    fixture
        .command()
        .env("PATH", "")
        .arg("images")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("REPOSITORY"))
        .stdout(predicate::str::contains("IMAGE ID"))
        .stdout(predicate::str::contains("HPC store:").not())
        .stderr("hook stderr\nhook stdout\n".repeat(2));
    assert!(fixture.root().join("hpc-image-store/store.json").is_file());
    for store in ["hpc-image-store", "parallax"] {
        // The hook applies filesystem tuning before the store is opened.
        let mode = fs::metadata(fixture.root().join(store))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o750);
    }
    assert!(!fixture.root().join("calls").exists());
    fixture.assert_hook_paths(&[
        &fixture.root().join("parallax"),
        Path::new("hpc-image-store"),
    ]);
}

#[test]
fn absolute_hpc_override_works_with_empty_parallax() {
    let fixture = Fixture::new();
    fixture.configure(Path::new(""), &fixture.podman, &fixture.hook);
    fixture
        .command()
        .arg("--hpc-image-store")
        .arg(fixture.root().join("selected-hpc"))
        .arg("images")
        .assert()
        .success();
    assert!(fixture.root().join("selected-hpc/store.json").is_file());
    assert!(!fixture.root().join("hpc-image-store").exists());
    fixture.assert_hook_paths(&[Path::new(""), &fixture.root().join("selected-hpc")]);
}

#[test]
fn relative_hpc_override_fails_before_listing_or_running_tools() {
    for all in [false, true] {
        let fixture = Fixture::new();
        let mut command = fixture.command();
        command.args(["--hpc-image-store", "selected-hpc", "images"]);
        if all {
            command.arg("--all");
        }
        command
            .assert()
            .code(1)
            .stdout(predicate::str::is_empty())
            .stderr(predicate::str::contains(
                "--hpc-image-store value must be an absolute path",
            ));
        assert!(!fixture.root().join("selected-hpc").exists());
        assert!(!fixture.root().join("hpc-image-store").exists());
        assert!(!fixture.root().join("calls").exists());
        assert!(!fixture.root().join("hook-calls").exists());
    }
}

#[test]
fn parallax_override_preserves_configured_hpc_store() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.root().join("other-parent")).unwrap();
    fixture
        .command()
        .args(["--parallax-imagestore", "other-parent/parallax", "images"])
        .assert()
        .success();
    assert!(fixture.root().join("hpc-image-store/store.json").is_file());
    assert!(!fixture.root().join("other-parent/hpc-image-store").exists());
    assert!(fixture.root().join("other-parent/parallax").is_dir());
    fixture.assert_hook_paths(&[
        Path::new("other-parent/parallax"),
        Path::new("hpc-image-store"),
    ]);
}

#[test]
fn setup_creates_ancestors_but_does_not_adopt_occupied_store() {
    let fixture = Fixture::new();
    fixture
        .command()
        .arg("--hpc-image-store")
        .arg(fixture.root().join("missing-parent/store"))
        .arg("images")
        .assert()
        .success();
    assert!(
        fixture
            .root()
            .join("missing-parent/store/store.json")
            .is_file()
    );

    let occupied = fixture.root().join("occupied");
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("keep"), "original").unwrap();
    fixture
        .command()
        .arg("--hpc-image-store")
        .arg(&occupied)
        .arg("images")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("HPC store:"));
    assert_eq!(
        fs::read_to_string(occupied.join("keep")).unwrap(),
        "original"
    );
    assert!(!occupied.join("store.json").exists());

    fixture
        .command()
        .args(["--hpc-image-store", "", "images"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--hpc-image-store <PATH>"));
    fixture.configure(Path::new(""), &fixture.podman, &fixture.hook);
    fixture.command().arg("images").assert().success();
    assert!(fixture.root().join("hpc-image-store/store.json").is_file());
}

#[test]
fn all_orders_sections_and_preserves_legacy_context_and_hook() {
    let fixture = Fixture::new();
    let result = fixture
        .command()
        .args(["images", "--all"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&result.get_output().stdout);
    assert!(stdout.starts_with("HPC store:\nREPOSITORY"));
    assert!(stdout.contains("\n\nPodman+Parallax stores:\nLEGACY IMAGE TABLE\n"));
    assert_eq!(stdout.matches("HPC store:").count(), 1);
    assert_eq!(stdout.matches("REPOSITORY").count(), 1);
    assert_eq!(stdout.matches("Podman+Parallax stores:").count(), 1);
    assert_eq!(stdout.matches("LEGACY IMAGE TABLE").count(), 1);
    result.stderr(format!(
        "{}legacy warning\n",
        "hook stderr\nhook stdout\n".repeat(4)
    ));
    let calls = fs::read_to_string(fixture.root().join("calls")).unwrap();
    let lines: Vec<_> = calls.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("info"));
    assert!(lines[1].contains("--root /stub/graphroot"));
    assert!(lines[1].contains(&format!(
        "additionalimagestore={}",
        fixture.root().join("parallax").display()
    )));
    assert!(lines[1].ends_with("images"));
    fixture.assert_hook_paths(&[
        &fixture.root().join("parallax"),
        Path::new("hpc-image-store"),
        &fixture.root().join("parallax"),
        Path::new("hpc-image-store"),
    ]);
    assert!(fixture.root().join("parallax").is_dir());
}

#[test]
fn all_continues_after_hpc_path_failure_and_retains_both_errors() {
    let fixture = Fixture::new();
    fs::write(fixture.root().join("blocked-parent"), "file").unwrap();
    fixture
        .command()
        .env("IMAGES_EXIT", "7")
        .arg("--hpc-image-store")
        .arg(fixture.root().join("blocked-parent/hpc"))
        .args(["images", "--all"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "Podman+Parallax stores:\nLEGACY IMAGE TABLE",
        ))
        .stderr(predicate::str::contains("HPC store:"))
        .stderr(predicate::str::contains("legacy warning"))
        .stderr(predicate::str::contains("failed with exit status: 7"));
}

#[test]
fn all_preserves_hpc_table_when_podman_fails() {
    let fixture = Fixture::new();
    fixture
        .command()
        .env("IMAGES_EXIT", "7")
        .args(["images", "--all"])
        .assert()
        .code(1)
        .stdout(predicate::str::starts_with("HPC store:\nREPOSITORY"))
        .stdout(predicate::str::contains("LEGACY IMAGE TABLE"));
    fixture.configure(
        &fixture.root().join("parallax"),
        &fixture.root().join("missing-podman"),
        &fixture.hook,
    );
    fixture
        .command()
        .args(["images", "--all"])
        .assert()
        .code(1)
        .stdout(predicate::str::starts_with("HPC store:\nREPOSITORY"))
        .stderr(predicate::str::contains(
            "Podman+Parallax stores: Failed to query Podman info",
        ));
}

#[test]
fn all_continues_listing_existing_stores_when_hook_fails() {
    let fixture = Fixture::new();
    fixture.command().arg("images").assert().success();
    // Store setup currently treats hook failures as best effort.
    fixture
        .command()
        .env("HOOK_EXIT", "9")
        .args(["images", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("HPC store:\nREPOSITORY"))
        .stdout(predicate::str::contains("LEGACY IMAGE TABLE"))
        .stderr(format!(
            "{}legacy warning\n",
            "hook stderr\nhook stdout\n".repeat(4)
        ));
}

#[test]
fn all_continues_after_hpc_failure_when_podman_succeeds() {
    let fixture = Fixture::new();
    fs::write(fixture.root().join("blocked-parent"), "file").unwrap();
    fixture
        .command()
        .arg("--hpc-image-store")
        .arg(fixture.root().join("blocked-parent/hpc"))
        .args(["images", "--all"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "Podman+Parallax stores:\nLEGACY IMAGE TABLE\n",
        ))
        .stderr(predicate::str::contains("HPC store:"))
        .stderr(predicate::str::contains("legacy warning"));
}

#[test]
fn all_reports_images_spawn_failure_after_successful_graphroot_lookup() {
    let fixture = Fixture::new();
    executable(
        &fixture.hook,
        // Disable Podman only after its graphroot lookup has been recorded.
        "#!/bin/sh\n[ ! -f \"$CALLS_FILE\" ] || /bin/chmod 600 \"$PODMAN_STUB\"\n",
    );
    fixture
        .command()
        .env("PODMAN_STUB", &fixture.podman)
        .args(["images", "--all"])
        .assert()
        .code(1)
        .stdout(predicate::str::starts_with("HPC store:\nREPOSITORY"))
        .stdout(predicate::str::contains("Podman+Parallax stores:"))
        .stderr(predicate::str::contains(
            "Podman+Parallax stores: failed to execute",
        ));
    let calls = fs::read_to_string(fixture.root().join("calls")).unwrap();
    assert_eq!(calls.lines().count(), 1);
    assert!(calls.contains("info"));
}

#[test]
fn all_stops_before_podman_when_printing_fails() {
    let fixture = Fixture::new();
    let mut command = fixture.std_command();
    command.args(["images", "--all"]);
    command.stdout(
        fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap(),
    );
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Failed to print image listing:"));
    assert!(!fixture.root().join("calls").exists());
    fixture.assert_hook_paths(&[
        &fixture.root().join("parallax"),
        Path::new("hpc-image-store"),
    ]);
}
