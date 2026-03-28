use assert_cmd::Command;

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

// TODO Make these tests conditional on detecting a file in /etc/sarus-suite/ and working Podman+Parallax installations (maybe just check if they can be reached by something like `which` or if they are in PATH). Remove the ignore attribute after this is done.
#[test]
#[ignore = "requires Podman/Parallax runtime and host configuration"]
fn smoke_pull_and_migrate() {
    let mut cmd = Command::cargo_bin("sarusctl").unwrap();
    cmd.args(["pull", "alpine:3.22"]).assert().success();
}

#[test]
#[ignore = "requires Podman/Parallax runtime and host configuration"]
fn smoke_run_edf() {
    let mut cmd = Command::cargo_bin("sarusctl").unwrap();
    cmd.args(["run", &fixture("valid.toml"), "true"])
        .assert()
        .success();
}

#[test]
#[ignore = "requires Podman/Parallax runtime and host configuration"]
fn smoke_run_yaml() {
    let mut cmd = Command::cargo_bin("sarusctl").unwrap();
    cmd.args(["run", &fixture("pod-single.yaml")])
        .assert()
        .success();
}
