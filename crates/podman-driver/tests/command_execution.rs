#![cfg(unix)]

use raster::EDF;
use sarus_suite_podman_driver::{
    ContainerCtx, DriverError, PodmanCtx, create, create_from_edf, create_from_edf_output,
    create_output, start, start_output,
};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tempfile::{TempDir, tempdir};

const FAKE_PODMAN: &str = r#"#!/bin/sh
: > "$PODMAN_DRIVER_TEST_ARGS"
printf x >> "$PODMAN_DRIVER_TEST_COUNT"
for arg do
    printf '%s\0' "$arg" >> "$PODMAN_DRIVER_TEST_ARGS"
done
printf '%s' "${PODMAN_DRIVER_TEST_VALUE-}" > "$PODMAN_DRIVER_TEST_ENV_OUTPUT"
if [ "${PODMAN_DRIVER_TEST_MODE-}" = "read_stdin" ]; then
    cat >/dev/null
    printf 'stdin-eof'
fi
printf 'stdout\377'
printf 'stderr\376' >&2
if [ "${PODMAN_DRIVER_TEST_SIGNAL-}" = "TERM" ]; then
    kill -TERM $$
fi
exit "${PODMAN_DRIVER_TEST_EXIT-0}"
"#;

struct FakePodman {
    _directory: TempDir,
    args_path: PathBuf,
    count_path: PathBuf,
    env_output_path: PathBuf,
    context: PodmanCtx,
}

impl FakePodman {
    fn new() -> Self {
        let directory = tempdir().expect("create fake Podman directory");
        let script_path = directory.path().join("podman");
        let args_path = directory.path().join("args");
        let count_path = directory.path().join("count");
        let env_output_path = directory.path().join("environment");

        fs::write(&script_path, FAKE_PODMAN).expect("write fake Podman");
        let mut permissions = fs::metadata(&script_path)
            .expect("stat fake Podman")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&script_path, permissions).expect("make fake Podman executable");

        let context = PodmanCtx {
            podman_path: script_path,
            module: Some(String::from("test-module")),
            graphroot: Some(directory.path().join("graphroot")),
            runroot: Some(directory.path().join("runroot")),
            parallax_mount_program: Some(PathBuf::from("/test/mount-program")),
            ro_store: Some(PathBuf::from("/test/read-only-store")),
            podman_env: None,
        }
        .with_env(
            "PODMAN_DRIVER_TEST_ARGS",
            args_path.as_os_str().to_os_string(),
        )
        .with_env(
            "PODMAN_DRIVER_TEST_COUNT",
            count_path.as_os_str().to_os_string(),
        )
        .with_env(
            "PODMAN_DRIVER_TEST_ENV_OUTPUT",
            env_output_path.as_os_str().to_os_string(),
        )
        .with_env("PODMAN_DRIVER_TEST_VALUE", "podman-only")
        .with_env("PODMAN_DRIVER_TEST_EXIT", "0");

        Self {
            _directory: directory,
            args_path,
            count_path,
            env_output_path,
            context,
        }
    }

    fn set_env(&mut self, key: &str, value: impl Into<OsString>) {
        self.context
            .podman_env
            .as_mut()
            .expect("fake context environment")
            .insert(OsString::from(key), value.into());
    }

    fn arguments(&self) -> Vec<Vec<u8>> {
        fs::read(&self.args_path)
            .expect("fake Podman did not record arguments")
            .split(|byte| *byte == 0)
            .filter(|argument| !argument.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    fn invocation_count(&self) -> usize {
        fs::read(&self.count_path)
            .expect("fake Podman did not record invocation")
            .len()
    }

    fn recorded_environment(&self) -> Vec<u8> {
        fs::read(&self.env_output_path).expect("fake Podman did not record environment")
    }
}

fn fixture_edf() -> EDF {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/edf/run_from_edf_test.toml");
    raster::render(path.to_string_lossy().into_owned()).expect("render EDF fixture")
}

fn container_context() -> ContainerCtx {
    ContainerCtx {
        name: String::from("execution-test"),
        interactive: false,
        tty: false,
        detach: false,
        auto_remove: false,
        set_env: true,
        pidfile: None,
        user: None,
    }
}

fn configure_exit(fake: &mut FakePodman, code: i32) {
    fake.set_env("PODMAN_DRIVER_TEST_EXIT", code.to_string());
    fake.set_env("PODMAN_DRIVER_TEST_SIGNAL", "");
}

#[test]
fn raw_create_wrappers_use_the_expected_execution_policy() {
    let mut passthrough = FakePodman::new();
    configure_exit(&mut passthrough, 7);
    let status = create(["--name", "raw-create"], Some(&passthrough.context))
        .expect("passthrough create should execute");
    assert_eq!(status.code(), Some(7));
    assert_eq!(passthrough.invocation_count(), 1);
    let arguments = passthrough.arguments();
    assert_eq!(
        arguments[10..],
        [
            b"create".to_vec(),
            b"--name".to_vec(),
            b"raw-create".to_vec()
        ]
    );

    let mut captured = FakePodman::new();
    configure_exit(&mut captured, 7);
    let output = create_output(["--name", "raw-create"], Some(&captured.context))
        .expect("captured create should execute");
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"stdout\xff");
    assert_eq!(output.stderr, b"stderr\xfe");
    assert_eq!(captured.invocation_count(), 1);

    let successful = FakePodman::new();
    let output = create_output(["--name", "raw-create"], Some(&successful.context))
        .expect("successful captured create should execute");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"stdout\xff");
    assert_eq!(output.stderr, b"stderr\xfe");
}

#[test]
fn edf_create_and_start_wrappers_check_only_the_checked_variants() {
    let edf = fixture_edf();
    let context = container_context();

    let mut create_fake = FakePodman::new();
    configure_exit(&mut create_fake, 7);
    let status = create_from_edf(
        &edf,
        Some(&create_fake.context),
        &context,
        ["echo", "created"],
    )
    .expect("passthrough EDF create should execute");
    assert_eq!(status.code(), Some(7));
    assert_eq!(create_fake.invocation_count(), 1);
    assert_eq!(create_fake.recorded_environment(), b"podman-only");

    let mut checked_create_fake = FakePodman::new();
    configure_exit(&mut checked_create_fake, 7);
    let error = create_from_edf_output(
        &edf,
        Some(&checked_create_fake.context),
        &context,
        ["echo", "created"],
    )
    .expect_err("checked EDF create should reject exit 7");
    assert!(matches!(error, DriverError::CommandFailed { .. }));
    assert_eq!(
        error.exit_status().and_then(std::process::ExitStatus::code),
        Some(7)
    );
    assert!(
        error
            .command()
            .is_some_and(|command| command.contains(" create "))
    );
    assert_eq!(error.stdout(), Some("stdout�"));
    assert_eq!(error.stderr(), Some("stderr�"));
    assert_eq!(checked_create_fake.invocation_count(), 1);

    let mut start_fake = FakePodman::new();
    configure_exit(&mut start_fake, 7);
    let status = start("execution-test", Some(&start_fake.context), false)
        .expect("passthrough start should execute");
    assert_eq!(status.code(), Some(7));
    assert_eq!(start_fake.invocation_count(), 1);

    let mut checked_start_fake = FakePodman::new();
    configure_exit(&mut checked_start_fake, 7);
    let error = start_output("execution-test", Some(&checked_start_fake.context), true)
        .expect_err("checked start should reject exit 7");
    assert!(matches!(error, DriverError::CommandFailed { .. }));
    assert_eq!(
        error.exit_status().and_then(std::process::ExitStatus::code),
        Some(7)
    );
    assert!(
        error
            .command()
            .is_some_and(|command| command.contains(" start "))
    );
    assert_eq!(error.stdout(), Some("stdout�"));
    assert_eq!(error.stderr(), Some("stderr�"));
    assert_eq!(checked_start_fake.invocation_count(), 1);

    let arguments = checked_start_fake.arguments();
    assert_eq!(
        arguments[10..],
        [
            b"start".to_vec(),
            b"--attach".to_vec(),
            b"--".to_vec(),
            b"execution-test".to_vec(),
        ]
    );
}

#[test]
fn create_and_start_wrappers_do_not_chain_commands() {
    let edf = fixture_edf();
    let context = container_context();
    let fake = FakePodman::new();

    create_from_edf_output(&edf, Some(&fake.context), &context, ["true"])
        .expect("create should execute once");
    assert_eq!(fake.invocation_count(), 1);

    start_output("execution-test", Some(&fake.context), false).expect("start should execute once");
    assert_eq!(fake.invocation_count(), 2);
}

#[test]
fn checked_execution_preserves_signal_termination() {
    let mut fake = FakePodman::new();
    fake.set_env("PODMAN_DRIVER_TEST_SIGNAL", "TERM");

    let error = start_output("execution-test", Some(&fake.context), false)
        .expect_err("signal termination should be an execution error");
    assert_eq!(
        error.exit_status().and_then(std::process::ExitStatus::code),
        None
    );
}

#[test]
fn captured_execution_closes_child_stdin() {
    let mut fake = FakePodman::new();
    fake.set_env("PODMAN_DRIVER_TEST_MODE", "read_stdin");

    let output = create_output(["--name", "stdin-test"], Some(&fake.context))
        .expect("captured execution should receive EOF");
    assert!(output.stdout.starts_with(b"stdin-eof"));
}

#[test]
fn missing_executable_reports_the_rendered_command() {
    let context = PodmanCtx {
        podman_path: PathBuf::from("/definitely/not/a/podman-executable"),
        module: None,
        graphroot: None,
        runroot: None,
        parallax_mount_program: None,
        ro_store: None,
        podman_env: Some(HashMap::new()),
    };

    let error = create(["--name", "missing"], Some(&context)).expect_err("spawn must fail");
    assert!(matches!(error, DriverError::Spawn { .. }));
    assert_eq!(
        error.command(),
        Some("/definitely/not/a/podman-executable create --name missing")
    );
}
