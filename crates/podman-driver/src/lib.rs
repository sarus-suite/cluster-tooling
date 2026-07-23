mod command;
mod context;
mod error;
mod execute;

use raster::EDF;
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;
use std::process::{ExitStatus, Output};
use std::str;

pub use context::{ContainerCtx, PodmanCtx};
pub use error::{DriverError, Result};

pub fn run<I, S>(args: I, podman_ctx: Option<&PodmanCtx>) -> Result<ExitStatus>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = command::run(podman_ctx);
    command.args(args);
    execute::execute_passthrough(command)
}

pub fn run_output<I, S>(args: I, podman_ctx: Option<&PodmanCtx>) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = command::run(podman_ctx);
    command.args(args);
    execute::execute_captured(command)
}

pub fn run_from_edf<I, S>(
    edf: &EDF,
    podman_ctx: Option<&PodmanCtx>,
    container_ctx: &ContainerCtx,
    container_command: I,
) -> Result<ExitStatus>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    execute::execute_passthrough(command::run_from_edf(
        edf,
        podman_ctx,
        container_ctx,
        container_command,
    ))
}

pub fn run_from_edf_output<I, S>(
    edf: &EDF,
    podman_ctx: Option<&PodmanCtx>,
    container_ctx: &ContainerCtx,
    container_command: I,
) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    execute::execute_checked(command::run_from_edf(
        edf,
        podman_ctx,
        container_ctx,
        container_command,
    ))
}

pub fn exec<I, S>(
    container: &str,
    podman_ctx: Option<&PodmanCtx>,
    container_command: I,
) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    execute::execute_captured(command::exec(
        container,
        podman_ctx,
        false,
        container_command,
    ))
}

pub fn exec_interactive<I, S>(
    container: &str,
    podman_ctx: Option<&PodmanCtx>,
    container_command: I,
) -> Result<ExitStatus>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    execute::execute_passthrough(command::exec(
        container,
        podman_ctx,
        true,
        container_command,
    ))
}

pub fn pull(image: &str, podman_ctx: Option<&PodmanCtx>) -> Result<()> {
    execute::execute_checked(command::pull(image, podman_ctx)).map(|_| ())
}

pub fn pull_streaming(image: &str, podman_ctx: Option<&PodmanCtx>) -> Result<()> {
    execute::execute_passthrough_checked(command::pull(image, podman_ctx))
}

pub fn rmi(image: &str, podman_ctx: Option<&PodmanCtx>) -> Result<()> {
    execute::execute_checked(command::rmi(image, podman_ctx)).map(|_| ())
}

pub fn rm(name: &str, podman_ctx: Option<&PodmanCtx>) -> Result<()> {
    execute::execute_checked(command::rm(name, podman_ctx)).map(|_| ())
}

pub fn container_exists(name: &str, podman_ctx: Option<&PodmanCtx>) -> Result<bool> {
    execute::execute_probe_with_false_code(command::container_exists(name, podman_ctx), 1)
}

pub fn container_cleanup(name: &str, podman_ctx: Option<&PodmanCtx>) -> Result<()> {
    execute::execute_checked(command::container_cleanup(name, podman_ctx)).map(|_| ())
}

pub fn stop(name: &str, podman_ctx: Option<&PodmanCtx>) -> Result<()> {
    execute::execute_checked(command::stop(name, podman_ctx)).map(|_| ())
}

pub fn images(podman_ctx: Option<&PodmanCtx>) -> Result<()> {
    execute::execute_passthrough_checked(command::images(podman_ctx))
}

pub fn image_exists(image: &str, podman_ctx: Option<&PodmanCtx>) -> Result<bool> {
    execute::execute_probe(command::image_exists(image, podman_ctx))
}

pub fn inspect(
    target: &str,
    format: Option<&str>,
    podman_ctx: Option<&PodmanCtx>,
) -> Result<Output> {
    execute::execute_checked(command::inspect(target, format, podman_ctx))
}

pub fn info(format: Option<&str>, podman_ctx: Option<&PodmanCtx>) -> Result<Output> {
    execute::execute_checked(command::info(format, podman_ctx))
}

pub fn system_reset(podman_ctx: Option<&PodmanCtx>) -> Result<()> {
    execute::execute_checked(command::system_reset(podman_ctx)).map(|_| ())
}

pub fn kube_play(filepath: &str, podman_ctx: Option<&PodmanCtx>) -> Result<()> {
    execute::execute_passthrough_checked(command::kube_play(filepath, podman_ctx))
}

pub fn kube_down(filepath: &str, force: bool, podman_ctx: Option<&PodmanCtx>) -> Result<()> {
    execute::execute_passthrough_checked(command::kube_down(filepath, force, podman_ctx))
}

pub fn version(module: Option<&str>) -> Result<Output> {
    execute::execute_checked(command::version(module))
}

/// Returns a container PID obtained through `podman inspect`.
/// Note: Podman yields PID `0` for stopped containers.
///
/// This helper is retained for compatibility but is a candidate for future deprecation because
/// current workspace consumers obtain container PIDs through other mechanisms.
pub fn get_container_pid(name: &str, podman_ctx: Option<&PodmanCtx>) -> Result<u32> {
    let output = inspect(name, Some("{{.State.Pid}}"), podman_ctx)?;
    parse_pid(
        format!("podman inspect output for container `{name}`"),
        &output.stdout,
    )
}

/// Retrieves the pid of a running container from the default pidfile for an overlay store driver
/// If the runroot is passed as argument, this function is much faster than get_container_pid(),
/// which uses `podman inspect`.
/// This function does not work if:
///   - the container is stopped
///   - a custom pidfile was specified in `podman run`
///   - storage driver is not overlay
///
/// This helper is retained for compatibility but is a candidate for future deprecation because
/// current workspace consumers obtain container PIDs through other mechanisms. It does not work
/// for stopped containers, custom PID files, or non-overlay storage drivers.
pub fn get_container_pid_from_default_file(
    container_id: &str,
    runroot: Option<&PathBuf>,
) -> Result<u32> {
    let runroot = match runroot {
        Some(runroot) => runroot.clone(),
        None => {
            // If we weren't given a runroot as argument, retrieve it from `podman info`
            // Notice that here we pass None as podman context: if a specific podman context were
            // to be passed to this function just to propagate the runroot, then the caller could
            // have provided the runroot directly by passing the related PodmanCtx field
            let output = info(Some("{{.Store.RunRoot}}"), None)?;
            let value = parse_text("podman info runroot output", &output.stdout)?;
            PathBuf::from(value.trim())
        }
    };
    let path = runroot
        .join("overlay-containers")
        .join(container_id)
        .join("userdata/pidfile");
    let contents = fs::read(&path).map_err(|source| DriverError::File {
        operation: "read container PID file",
        path: path.clone(),
        source,
    })?;
    parse_pid(format!("PID file `{}`", path.display()), &contents)
}

pub fn parallax_exist(
    parallax_path: &PathBuf,
    podman_ctx: &PodmanCtx,
    image: &str,
) -> Result<bool> {
    // TODO: parallax commands maybe don't need to return a Result anymore.
    let command = command::parallax(parallax_path, podman_ctx, image, "exist")?;
    execute::execute_probe(command)
}

pub fn parallax_migrate(
    parallax_path: &PathBuf,
    podman_ctx: &PodmanCtx,
    image: &str,
) -> Result<()> {
    require_graphroot(podman_ctx)?;
    let command = command::parallax(parallax_path, podman_ctx, image, "migrate")?;
    execute::execute_checked(command).map(|_| ())
}

pub fn parallax_rmi(parallax_path: &PathBuf, podman_ctx: &PodmanCtx, image: &str) -> Result<()> {
    let command = command::parallax(parallax_path, podman_ctx, image, "rmi")?;
    execute::execute_checked(command).map(|_| ())
}

pub fn parallax_migrate_streaming(
    parallax_path: &PathBuf,
    podman_ctx: &PodmanCtx,
    image: &str,
) -> Result<()> {
    require_graphroot(podman_ctx)?;
    let command = command::parallax(parallax_path, podman_ctx, image, "migrate")?;
    execute::execute_passthrough_checked(command)
}

pub fn parallax_rmi_streaming(
    parallax_path: &PathBuf,
    podman_ctx: &PodmanCtx,
    image: &str,
) -> Result<()> {
    let command = command::parallax(parallax_path, podman_ctx, image, "rmi")?;
    execute::execute_passthrough_checked(command)
}

fn require_graphroot(podman_ctx: &PodmanCtx) -> Result<()> {
    podman_ctx
        .graphroot
        .as_ref()
        .map(|_| ())
        .ok_or(DriverError::MissingContext {
            operation: "parallax migrate",
            field: "graphroot",
        })
}

fn parse_pid(origin: String, output: &[u8]) -> Result<u32> {
    let value = parse_text(&origin, output)?;
    value.trim().parse().map_err(
        |source: std::num::ParseIntError| DriverError::InvalidOutput {
            origin,
            message: source.to_string(),
            output: String::from_utf8_lossy(output).into_owned(),
        },
    )
}

fn parse_text<'a>(origin: &str, output: &'a [u8]) -> Result<&'a str> {
    str::from_utf8(output).map_err(|source| DriverError::InvalidOutput {
        origin: origin.to_owned(),
        message: source.to_string(),
        output: String::from_utf8_lossy(output).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pid_output() {
        assert_eq!(parse_pid("test output".into(), b"1234\n").unwrap(), 1234);
    }

    #[test]
    fn reports_invalid_pid_output() {
        let error = parse_pid("test output".into(), b"not-a-pid").unwrap_err();
        assert!(matches!(
            error,
            DriverError::InvalidOutput { ref origin, ref output, .. }
                if origin == "test output" && output == "not-a-pid"
        ));
    }

    #[test]
    fn reports_non_utf8_pid_output_lossily() {
        let error = parse_pid("test output".into(), &[0xff]).unwrap_err();
        assert!(matches!(
            error,
            DriverError::InvalidOutput { ref output, .. } if output == "�"
        ));
    }

    #[test]
    fn migrate_requires_graphroot() {
        let context = PodmanCtx {
            podman_path: PathBuf::from("podman"),
            module: None,
            graphroot: None,
            runroot: None,
            parallax_mount_program: None,
            ro_store: Some(PathBuf::from("store")),
            podman_env: None,
        };

        let error = parallax_migrate(&PathBuf::from("parallax"), &context, "image").unwrap_err();
        assert!(matches!(
            error,
            DriverError::MissingContext {
                operation: "parallax migrate",
                field: "graphroot"
            }
        ));
    }

    #[test]
    fn missing_pid_file_reports_its_path() {
        let runroot = std::env::temp_dir().join(format!(
            "podman-driver-missing-pidfile-{}",
            std::process::id()
        ));
        let expected = runroot
            .join("overlay-containers")
            .join("container")
            .join("userdata/pidfile");

        let error = get_container_pid_from_default_file("container", Some(&runroot)).unwrap_err();
        assert!(matches!(
            error,
            DriverError::File { ref path, .. } if path == &expected
        ));
    }
}
