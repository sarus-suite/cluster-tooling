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
printf '%s\n' "$*" > "$HOOK_FILE"
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
                "parallax_imagestore = {parallax:?}\npodman_path = {podman:?}\n[hooks]\nparallax_imagestore_create = {hook:?}\n",
            ),
        ).unwrap();
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
        .stdout(predicate::str::contains("--all"));
    Command::cargo_bin("sarusctl")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--hpc-imagestore"));
    for args in [
        vec!["--all", "images"],
        vec!["validate", "--all"],
        vec!["images", "--hpc-imagestore", "/tmp/hpc"],
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
fn default_images_initializes_sibling_without_external_tools_or_hook() {
    let fixture = Fixture::new();
    fixture.configure(
        &fixture.root().join("parallax"),
        &fixture.root().join("missing-podman"),
        &fixture.root().join("missing-hook"),
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
        .stderr(predicate::str::is_empty());
    assert!(fixture.root().join("hpc-imagestore/store.json").is_file());
    assert!(!fixture.root().join("parallax").exists());
    assert!(!fixture.root().join("calls").exists());
    assert!(!fixture.root().join("hook-calls").exists());
}

#[test]
fn hpc_override_works_with_empty_parallax_and_resolves_relative_path() {
    let fixture = Fixture::new();
    fixture.configure(Path::new(""), &fixture.podman, &fixture.hook);
    fixture
        .command()
        .args(["--hpc-imagestore", "selected-hpc", "images"])
        .assert()
        .success();
    assert!(fixture.root().join("selected-hpc/store.json").is_file());
    assert!(!fixture.root().join("hpc-imagestore").exists());
}

#[test]
fn sibling_uses_effective_parallax_override() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.root().join("other-parent")).unwrap();
    fixture
        .command()
        .args(["--parallax-imagestore", "other-parent/parallax", "images"])
        .assert()
        .success();
    assert!(
        fixture
            .root()
            .join("other-parent/hpc-imagestore/store.json")
            .is_file()
    );
    assert!(!fixture.root().join("hpc-imagestore").exists());
    assert!(!fixture.root().join("other-parent/parallax").exists());
}

#[test]
fn invalid_paths_fail_without_creating_ancestors_or_adopting_contents() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["--hpc-imagestore", "missing-parent/store", "images"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("HPC store:"));
    assert!(!fixture.root().join("missing-parent").exists());

    let occupied = fixture.root().join("occupied");
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("keep"), "original").unwrap();
    fixture
        .command()
        .args(["--hpc-imagestore", "occupied", "images"])
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
        .args(["--hpc-imagestore", "", "images"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--hpc-imagestore <PATH>"));
    fixture.configure(Path::new(""), &fixture.podman, &fixture.hook);
    fixture
        .command()
        .arg("images")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("--hpc-imagestore PATH"));
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
    result.stderr("hook stderr\nhook stdout\nlegacy warning\n");
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
    assert_eq!(
        fs::read_to_string(fixture.root().join("hook-calls"))
            .unwrap()
            .trim(),
        fixture.root().join("parallax").to_str().unwrap()
    );
    assert!(fixture.root().join("parallax").is_dir());
}

#[test]
fn all_continues_after_hpc_path_failure_and_retains_both_errors() {
    let fixture = Fixture::new();
    fixture
        .command()
        .env("IMAGES_EXIT", "7")
        .args(["--hpc-imagestore", "missing-parent/hpc", "images", "--all"])
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
fn all_preserves_hpc_table_when_podman_or_hook_fails() {
    let fixture = Fixture::new();
    fixture
        .command()
        .env("IMAGES_EXIT", "7")
        .args(["images", "--all"])
        .assert()
        .code(1)
        .stdout(predicate::str::starts_with("HPC store:\nREPOSITORY"))
        .stdout(predicate::str::contains("LEGACY IMAGE TABLE"));
    fixture
        .command()
        .env("HOOK_EXIT", "9")
        .args(["images", "--all"])
        .assert()
        .code(1)
        .stdout(predicate::str::starts_with("HPC store:\nREPOSITORY"))
        .stdout(predicate::str::contains("LEGACY IMAGE TABLE").not())
        .stderr(predicate::str::contains("hook stdout"))
        .stderr(predicate::str::contains(
            "parallax_imagestore_create hook exit code: 9",
        ));
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
fn all_continues_after_hpc_failure_when_podman_succeeds() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["--hpc-imagestore", "missing-parent/hpc", "images", "--all"])
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
        "#!/bin/sh\n/bin/chmod 600 \"$PODMAN_STUB\"\n",
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
    assert!(!fixture.root().join("hook-calls").exists());
}
