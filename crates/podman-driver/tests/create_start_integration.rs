#![cfg(unix)]

use raster::EDF;
use sarus_suite_podman_driver::{
    self as pmd, ContainerCtx, PodmanCtx, create_from_edf_output, start, start_output,
};
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tempfile::tempdir;

const IMAGE: &str = "docker.io/library/alpine:3.22";
const START_STDIN_HELPER_ENV: &str = "PODMAN_DRIVER_START_STDIN_HELPER_CONTAINER";

fn unique_name(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    format!("{prefix}-{}-{nanos}", std::process::id())
}

fn runtime_error(message: impl Into<String>) -> Box<dyn Error> {
    io::Error::other(message.into()).into()
}

fn prerequisites() -> Result<(), Box<dyn Error>> {
    pmd::info(None, None)
        .map_err(|error| runtime_error(format!("Podman is unavailable: {error}")))?;
    if !pmd::image_exists(IMAGE, None)
        .map_err(|error| runtime_error(format!("cannot query required image {IMAGE}: {error}")))?
    {
        return Err(runtime_error(format!(
            "required image {IMAGE} is not present; pull it before running this ignored test"
        )));
    }
    Ok(())
}

fn container_context(name: &str) -> ContainerCtx {
    ContainerCtx {
        name: name.to_owned(),
        interactive: false,
        tty: false,
        detach: false,
        auto_remove: false,
        set_env: true,
        pidfile: None,
        user: None,
    }
}

fn render_edf(
    image: &str,
    mounts: &[String],
    workdir: &str,
    writable: bool,
    env: &[(&str, &str)],
    annotations: &[(&str, &str)],
) -> EDF {
    let directory = tempdir().expect("create EDF fixture directory");
    let path = directory.path().join("test.toml");
    let mut source = format!("image = {image:?}\nworkdir = {workdir:?}\nwritable = {writable}\n");
    if !mounts.is_empty() {
        source.push_str("mounts = [\n");
        for mount in mounts {
            source.push_str(&format!("  {mount:?},\n"));
        }
        source.push_str("]\n");
    }
    if !env.is_empty() {
        source.push_str("\n[env]\n");
        for (key, value) in env {
            source.push_str(&format!("{key} = {value:?}\n"));
        }
    }
    if !annotations.is_empty() {
        source.push_str("\n[annotations]\n");
        for (key, value) in annotations {
            source.push_str(&format!("{key:?} = {value:?}\n"));
        }
    }
    fs::write(&path, source).expect("write EDF fixture");
    raster::render(path.to_string_lossy().into_owned()).expect("render EDF fixture")
}

fn create_container(
    edf: &EDF,
    podman_ctx: Option<&PodmanCtx>,
    container_ctx: &ContainerCtx,
    command: &[&str],
) -> Result<(), Box<dyn Error>> {
    let output = create_from_edf_output(edf, podman_ctx, container_ctx, command)?;
    if !output.status.success() {
        return Err(runtime_error(format!(
            "create returned unexpected status {}",
            output.status
        )));
    }
    Ok(())
}

fn inspect_text(
    name: &str,
    format: &str,
    podman_ctx: Option<&PodmanCtx>,
) -> Result<String, Box<dyn Error>> {
    let output = pmd::inspect(name, Some(format), podman_ctx)?;
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn wait_for<F>(description: &str, mut predicate: F) -> Result<(), Box<dyn Error>>
where
    F: FnMut() -> Result<bool, Box<dyn Error>>,
{
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if predicate()? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(runtime_error(format!(
                "timed out waiting for {description}"
            )));
        }
        sleep(Duration::from_millis(50));
    }
}

fn cleanup_container(name: &str, podman_ctx: Option<&PodmanCtx>) {
    if pmd::container_exists(name, podman_ctx).unwrap_or(false) {
        let _ = pmd::stop(name, podman_ctx);
        let _ = pmd::container_cleanup(name, podman_ctx);
        let _ = pmd::rm(name, podman_ctx);
    }
}

struct CleanupGuard<'a> {
    name: String,
    podman_ctx: Option<&'a PodmanCtx>,
    armed: bool,
}

impl<'a> CleanupGuard<'a> {
    fn new(name: &str, podman_ctx: Option<&'a PodmanCtx>) -> Self {
        Self {
            name: name.to_owned(),
            podman_ctx,
            armed: true,
        }
    }

    fn cleanup(mut self) {
        cleanup_container(&self.name, self.podman_ctx);
        self.armed = false;
    }
}

impl Drop for CleanupGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            cleanup_container(&self.name, self.podman_ctx);
        }
    }
}

#[test]
#[ignore = "helper for the Podman create/start stdin integration test"]
fn start_stdin_helper() {
    let Some(container) = std::env::var_os(START_STDIN_HELPER_ENV) else {
        return;
    };
    let container = container.to_string_lossy();
    let status = start(&container, None, true).expect("attached start should execute");
    assert!(status.success(), "attached start returned {status}");
}

#[test]
#[ignore = "requires Podman and a pre-pulled alpine:3.22 image"]
fn create_does_not_execute_until_start() -> Result<(), Box<dyn Error>> {
    prerequisites()?;
    let directory = tempdir()?;
    let marker = directory.path().join("marker");
    let name = unique_name("pmd-create");
    let mount = format!("{}:/mnt", directory.path().display());
    let edf = render_edf(IMAGE, &[mount], "", true, &[], &[]);
    let context = container_context(&name);
    let cleanup = CleanupGuard::new(&name, None);

    create_container(
        &edf,
        None,
        &context,
        &["/bin/sh", "-c", "echo started > /mnt/marker"],
    )?;
    assert_eq!(inspect_text(&name, "{{.State.Status}}", None)?, "created");
    assert_eq!(pmd::get_container_pid(&name, None)?, 0);
    assert!(!marker.exists(), "create unexpectedly ran the payload");

    let output = start_output(&name, None, true)?;
    assert!(output.status.success());
    assert!(
        marker.exists(),
        "attached start did not execute the payload"
    );
    assert_eq!(inspect_text(&name, "{{.State.Status}}", None)?, "exited");
    cleanup.cleanup();
    Ok(())
}

#[test]
#[ignore = "requires Podman and a pre-pulled alpine:3.22 image"]
fn stored_edf_configuration_survives_create_and_start() -> Result<(), Box<dyn Error>> {
    prerequisites()?;
    let directory = tempdir()?;
    fs::write(directory.path().join("input"), "mounted-input")?;
    let name = unique_name("pmd-edf-config");
    let mount = format!("{}:/mnt", directory.path().display());
    let edf = render_edf(
        IMAGE,
        &[mount],
        "/tmp",
        false,
        &[("TEST_ENV", "from-edf")],
        &[("com.example.pmd.test", "true")],
    );
    let mut context = container_context(&name);
    context.user = Some(
        String::from_utf8(Command::new("id").args(["-u"]).output()?.stdout)?
            .trim()
            .to_owned(),
    );
    let cleanup = CleanupGuard::new(&name, None);

    create_container(
        &edf,
        None,
        &context,
        &[
            "/bin/sh",
            "-c",
            "printf '%s|%s|' \"$TEST_ENV\" \"$PWD\"; cat /mnt/input",
        ],
    )?;
    let output = start_output(&name, None, true)?;
    assert!(output.status.success());
    assert_eq!(output.stdout, b"from-edf|/tmp|mounted-input");
    assert_eq!(inspect_text(&name, "{{.Config.WorkingDir}}", None)?, "/tmp");
    assert_eq!(
        inspect_text(&name, "{{.Config.User}}", None)?,
        context.user.as_deref().unwrap()
    );
    assert_eq!(
        inspect_text(&name, "{{.HostConfig.ReadonlyRootfs}}", None)?,
        "true"
    );
    assert_eq!(
        inspect_text(
            &name,
            "{{index .Config.Annotations \"com.example.pmd.test\"}}",
            None,
        )?,
        "true"
    );
    cleanup.cleanup();
    Ok(())
}

#[test]
#[ignore = "requires Podman and a pre-pulled alpine:3.22 image"]
fn detached_start_writes_and_reports_the_pidfile() -> Result<(), Box<dyn Error>> {
    prerequisites()?;
    let directory = tempdir()?;
    let pidfile = directory.path().join("container.pid");
    let name = unique_name("pmd-detached");
    let edf = render_edf(IMAGE, &[], "", true, &[], &[]);
    let mut context = container_context(&name);
    context.pidfile = Some(pidfile.clone());
    context.detach = true;
    let cleanup = CleanupGuard::new(&name, None);

    create_container(&edf, None, &context, &["sleep", "30"])?;
    let status = start(&name, None, false)?;
    assert!(status.success());
    wait_for("container to enter running state", || {
        Ok(inspect_text(&name, "{{.State.Status}}", None)? == "running")
    })?;
    wait_for("container PID file", || Ok(pidfile.exists()))?;

    let file_pid: u32 = fs::read_to_string(&pidfile)?.trim().parse()?;
    let inspect_pid = pmd::get_container_pid(&name, None)?;
    assert!(file_pid > 0);
    assert_eq!(file_pid, inspect_pid);
    pmd::stop(&name, None)?;
    cleanup.cleanup();
    Ok(())
}

#[test]
#[ignore = "requires Podman and a pre-pulled alpine:3.22 image"]
fn attached_start_propagates_payload_exit_and_diagnostics() -> Result<(), Box<dyn Error>> {
    prerequisites()?;
    let edf = render_edf(IMAGE, &[], "", true, &[], &[]);

    let first_name = unique_name("pmd-exit-passthrough");
    let first_context = container_context(&first_name);
    let first_cleanup = CleanupGuard::new(&first_name, None);
    create_container(
        &edf,
        None,
        &first_context,
        &["/bin/sh", "-c", "printf out; printf err >&2; exit 7"],
    )?;
    let status = start(&first_name, None, true)?;
    assert_eq!(status.code(), Some(7));

    let second_name = unique_name("pmd-exit-captured");
    let second_context = container_context(&second_name);
    let second_cleanup = CleanupGuard::new(&second_name, None);
    create_container(
        &edf,
        None,
        &second_context,
        &["/bin/sh", "-c", "printf out; printf err >&2; exit 7"],
    )?;
    let error = start_output(&second_name, None, true).expect_err("exit 7 must be rejected");
    assert_eq!(
        error.exit_status().and_then(std::process::ExitStatus::code),
        Some(7)
    );
    assert!(error.stdout().unwrap_or_default().contains("out"));
    assert!(error.stderr().unwrap_or_default().contains("err"));
    first_cleanup.cleanup();
    second_cleanup.cleanup();
    Ok(())
}

#[test]
#[ignore = "requires Podman and a pre-pulled alpine:3.22 image"]
fn retained_container_can_be_started_twice() -> Result<(), Box<dyn Error>> {
    prerequisites()?;
    let directory = tempdir()?;
    let name = unique_name("pmd-restart");
    let mount = format!("{}:/mnt", directory.path().display());
    let edf = render_edf(IMAGE, &[mount], "", true, &[], &[]);
    let context = container_context(&name);
    let cleanup = CleanupGuard::new(&name, None);

    create_container(
        &edf,
        None,
        &context,
        &["/bin/sh", "-c", "echo run >> /mnt/count"],
    )?;
    assert!(start_output(&name, None, true)?.status.success());
    assert!(start_output(&name, None, true)?.status.success());
    assert_eq!(
        fs::read_to_string(directory.path().join("count"))?
            .lines()
            .count(),
        2
    );
    cleanup.cleanup();
    Ok(())
}

#[test]
#[ignore = "requires Podman and a pre-pulled alpine:3.22 image"]
fn auto_remove_removes_container_after_attached_start() -> Result<(), Box<dyn Error>> {
    prerequisites()?;
    let name = unique_name("pmd-auto-remove");
    let edf = render_edf(IMAGE, &[], "", true, &[], &[]);
    let mut context = container_context(&name);
    context.auto_remove = true;
    let cleanup = CleanupGuard::new(&name, None);

    create_container(&edf, None, &context, &["true"])?;
    assert!(start_output(&name, None, true)?.status.success());
    wait_for("auto-removed container", || {
        Ok(!pmd::container_exists(&name, None)?)
    })?;
    cleanup.cleanup();
    Ok(())
}

#[test]
#[ignore = "requires Podman and a pre-pulled alpine:3.22 image"]
fn start_and_duplicate_create_fail_without_affecting_existing_container()
-> Result<(), Box<dyn Error>> {
    prerequisites()?;
    let edf = render_edf(IMAGE, &[], "", true, &[], &[]);
    let missing = unique_name("pmd-missing");
    let error = start_output(&missing, None, false).expect_err("unknown container must fail");
    assert!(error.command().is_some());

    let name = unique_name("pmd-duplicate");
    let context = container_context(&name);
    let cleanup = CleanupGuard::new(&name, None);
    create_container(&edf, None, &context, &["true"])?;
    let error = create_from_edf_output(&edf, None, &context, ["false"])
        .expect_err("duplicate name must fail");
    assert!(error.command().is_some());
    assert!(pmd::container_exists(&name, None)?);
    assert_eq!(inspect_text(&name, "{{.Config.Cmd}}", None)?, "[true]");
    cleanup.cleanup();
    Ok(())
}

fn required_environment(name: &str) -> Result<OsString, Box<dyn Error>> {
    std::env::var_os(name)
        .ok_or_else(|| runtime_error(format!("{name} must be set for the HPC integration test")))
}

#[test]
#[ignore = "requires Podman, Parallax, an HPC module, and migrated test image"]
fn hpc_context_is_used_for_create_and_start() -> Result<(), Box<dyn Error>> {
    for variable in [
        "PODMAN_DRIVER_TEST_RO_STORE",
        "PODMAN_DRIVER_TEST_MOUNT_PROGRAM",
        "PODMAN_DRIVER_TEST_MODULE",
        "PODMAN_DRIVER_TEST_IMAGE",
    ] {
        let _ = required_environment(variable)?;
    }
    let image = required_environment("PODMAN_DRIVER_TEST_IMAGE")?;
    let image = image.to_string_lossy().into_owned();
    let directory = tempdir()?;
    let graphroot = directory.path().join("graphroot");
    let runroot = directory.path().join("runroot");
    let run_context = PodmanCtx {
        podman_path: PathBuf::from(
            std::env::var_os("PODMAN_DRIVER_TEST_PODMAN")
                .unwrap_or_else(|| OsString::from("podman")),
        ),
        module: Some(
            required_environment("PODMAN_DRIVER_TEST_MODULE")?
                .to_string_lossy()
                .into_owned(),
        ),
        graphroot: Some(graphroot),
        runroot: Some(runroot),
        parallax_mount_program: Some(PathBuf::from(required_environment(
            "PODMAN_DRIVER_TEST_MOUNT_PROGRAM",
        )?)),
        ro_store: Some(PathBuf::from(required_environment(
            "PODMAN_DRIVER_TEST_RO_STORE",
        )?)),
        podman_env: None,
    };
    let name = unique_name("pmd-hpc");
    let edf = render_edf(&image, &[], "", true, &[], &[]);
    let container_ctx = container_context(&name);
    let cleanup = CleanupGuard::new(&name, Some(&run_context));

    fs::create_dir_all(run_context.graphroot.as_ref().unwrap())?;
    fs::create_dir_all(run_context.runroot.as_ref().unwrap())?;

    pmd::info(None, Some(&run_context))
        .map_err(|error| runtime_error(format!("Podman HPC context is unavailable: {error}")))?;

    if !pmd::image_exists(&image, Some(&run_context))? {
        return Err(runtime_error(format!(
            "HPC test image {image} is not available through the configured read-only store"
        )));
    }

    create_container(&edf, Some(&run_context), &container_ctx, &["echo", "hpc"])?;
    let output = start_output(&name, Some(&run_context), true)?;
    assert!(output.status.success());
    assert!(output.stdout.windows(3).any(|value| value == b"hpc"));
    cleanup.cleanup();
    Ok(())
}
