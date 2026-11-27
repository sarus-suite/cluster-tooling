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
        let calls = fs::read_to_string(self.root().join("hook-calls")).unwrap_or_default();
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
        .stdout(predicate::str::contains("--digests"));
    Command::cargo_bin("sarusctl")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--hpc-image-store"));
    for args in [
        vec!["images", "--all"],
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
fn digests_controls_hpc_column() {
    for digests in [false, true] {
        let fixture = Fixture::new();
        let mut command = fixture.command();
        command.arg("images");
        if digests {
            command.arg("--digests");
        }
        let result = command.assert().success();
        let stdout = String::from_utf8_lossy(&result.get_output().stdout);
        assert_eq!(stdout.contains("DIGEST"), digests);
        assert!(!stdout.contains("LEGACY IMAGE TABLE"));
        assert!(!fixture.root().join("calls").exists());
        result.stderr("");
        fixture.assert_hook_paths(&[]);
    }
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
    fixture.assert_hook_paths(&[]);
}

#[test]
fn relative_hpc_override_fails_before_listing_or_running_tools() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["--hpc-image-store", "selected-hpc", "images"])
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
    assert!(!fixture.root().join("other-parent/parallax").exists());
    fixture.assert_hook_paths(&[]);
}

#[test]
fn images_requires_the_hpc_parent_and_does_not_adopt_occupied_store() {
    let fixture = Fixture::new();
    fixture
        .command()
        .arg("--hpc-image-store")
        .arg(fixture.root().join("missing-parent/store"))
        .arg("images")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("No such file or directory"));
    assert!(!fixture.root().join("missing-parent").exists());

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
