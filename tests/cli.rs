use assert_cmd::Command;
use predicates::prelude::*;

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn help_lists_subcommands() {
    let mut cmd = Command::cargo_bin("sarusctl").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--verbose"))
        .stdout(predicate::str::contains("validate"))
        .stdout(predicate::str::contains("render"))
        .stdout(predicate::str::contains("run"));
    // TODO check existence of all subcommands instead of just a few
}

#[test]
fn version_prints_package_version() {
    let mut cmd = Command::cargo_bin("sarusctl").unwrap();
    cmd.arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn help_accepts_top_level_verbose_flag() {
    let mut cmd = Command::cargo_bin("sarusctl").unwrap();
    cmd.args(["--verbose", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--verbose"));
}

#[test]
fn validate_accepts_top_level_verbose_flag() {
    let mut cmd = Command::cargo_bin("sarusctl").unwrap();
    cmd.args(["--verbose", "validate", &fixture("valid.toml")])
        .assert()
        .success()
        .stdout(predicate::str::contains("is a valid EDF file"));
}

#[test]
fn validate_valid_edf_text_output() {
    let mut cmd = Command::cargo_bin("sarusctl").unwrap();
    cmd.args(["validate", &fixture("valid.toml")])
        .assert()
        .success()
        .stdout(predicate::str::contains("is a valid EDF file"));
}

#[test]
fn validate_invalid_edf_text_output() {
    let mut cmd = Command::cargo_bin("sarusctl").unwrap();
    cmd.args(["validate", &fixture("invalid.toml")])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("is an INVALID EDF file"))
        .stderr(predicate::str::contains(
            "not valid under any of the schemas",
        ));
}

#[test]
fn validate_valid_edf_json_output() {
    let mut cmd = Command::cargo_bin("sarusctl").unwrap();
    cmd.args(["validate", "--output", "json", &fixture("valid.toml")])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"return_code\": 0"))
        .stdout(predicate::str::contains("\"stderr\": \"\""));
}

#[test]
fn validate_invalid_edf_json_output() {
    let mut cmd = Command::cargo_bin("sarusctl").unwrap();
    cmd.args(["validate", "--output", "json", &fixture("invalid.toml")])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("\"return_code\": 1"))
        .stdout(predicate::str::contains(
            "not valid under any of the schemas",
        ));
}

#[test]
fn render_valid_edf_output() {
    let mut cmd = Command::cargo_bin("sarusctl").unwrap();
    cmd.args(["render", &fixture("valid.toml")])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"image\": \"alpine:3.22\""));
}

#[test]
fn render_invalid_edf_output() {
    let mut cmd = Command::cargo_bin("sarusctl").unwrap();
    cmd.args(["render", &fixture("invalid.toml")])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "not valid under any of the schemas",
        ));
}

#[test]
fn run_invalid_input_returns_readable_error() {
    let mut cmd = Command::cargo_bin("sarusctl").unwrap();
    cmd.args(["run", &fixture("not-edf-not-yaml.txt")])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("is not valid EDF nor YAML"));
}
