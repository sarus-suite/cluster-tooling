use raster::EDF;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{ContainerCtx, DriverError, PodmanCtx, Result};

pub(crate) fn base(podman_ctx: Option<&PodmanCtx>) -> Command {
    let Some(ctx) = podman_ctx else {
        return Command::new("podman");
    };

    let mut command = Command::new(&ctx.podman_path);
    if let Some(environment) = &ctx.podman_env {
        command.envs(environment);
    }
    cli_opt(
        &mut command,
        "--root",
        ctx.graphroot.as_deref().map(Path::as_os_str),
    );
    cli_opt(
        &mut command,
        "--runroot",
        ctx.runroot.as_deref().map(Path::as_os_str),
    );
    command
}

pub(crate) fn run(podman_ctx: Option<&PodmanCtx>) -> Command {
    let mut command = base(podman_ctx);
    if let Some(ctx) = podman_ctx {
        cli_opt(
            &mut command,
            "--module",
            ctx.module.as_deref().map(OsStr::new),
        );
        cli_storage_opt(
            &mut command,
            "additionalimagestore",
            ctx.ro_store.as_deref().map(Path::as_os_str),
        );
        cli_storage_opt(
            &mut command,
            "mount_program",
            ctx.parallax_mount_program.as_deref().map(Path::as_os_str),
        );
    }
    command.arg("run");
    command
}

pub(crate) fn run_from_edf<I, S>(
    edf: &EDF,
    podman_ctx: Option<&PodmanCtx>,
    container_ctx: &ContainerCtx,
    container_command: I,
) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = run(podman_ctx);
    cli_flag(&mut command, container_ctx.auto_remove, "--rm");
    cli_flag(&mut command, container_ctx.detach, "--detach");
    cli_flag(
        &mut command,
        container_ctx.interactive,
        "--interactive",
    );
    cli_flag(&mut command, container_ctx.tty, "--tty");
    cli_flag(&mut command, !edf.writable, "--read-only");
    cli_opt(
        &mut command,
        "--name",
        Some(OsStr::new(&container_ctx.name)),
    );
    cli_opt(
        &mut command,
        "--user",
        container_ctx.user.as_deref().map(OsStr::new),
    );
    cli_opt(
        &mut command,
        "--pidfile",
        container_ctx.pidfile.as_deref().map(Path::as_os_str),
    );
    cli_flag(&mut command, !edf.entrypoint, "--entrypoint=");

    if !edf.workdir.is_empty() {
        cli_opt(&mut command, "--workdir", Some(OsStr::new(&edf.workdir)));
    }
    for mount in &edf.mounts {
        cli_opt(
            &mut command,
            "--volume",
            Some(OsStr::new(&mount.to_volume_string())),
        );
    }
    for device in &edf.devices {
        cli_opt(&mut command, "--device", Some(OsStr::new(device)));
    }
    if container_ctx.set_env {
        for (key, value) in &edf.env {
            cli_kv(&mut command, "--env", OsStr::new(key), OsStr::new(value));
        }
    }
    for (key, value) in &edf.annotations {
        cli_kv(
            &mut command,
            "--annotation",
            OsStr::new(key),
            OsStr::new(value),
        );
    }
    command.arg(&edf.image).args(container_command);
    command
}

pub(crate) fn exec<I, S>(
    container: &str,
    podman_ctx: Option<&PodmanCtx>,
    interactive: bool,
    container_command: I,
) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = base(podman_ctx);
    command.arg("exec");
    cli_flag(&mut command, interactive, "-it");
    command.arg(container).args(container_command);
    command
}

pub(crate) fn pull(image: &str, podman_ctx: Option<&PodmanCtx>) -> Command {
    let mut command = base(podman_ctx);
    command.args(["pull", image]);
    command
}

pub(crate) fn rmi(image: &str, podman_ctx: Option<&PodmanCtx>) -> Command {
    let mut command = base_with_read_only_store(podman_ctx);
    command.args(["rmi", image]);
    command
}

pub(crate) fn rm(name: &str, podman_ctx: Option<&PodmanCtx>) -> Command {
    let mut command = base_with_read_only_store(podman_ctx);
    command.args(["rm", name]);
    command
}

pub(crate) fn container_exists(name: &str, podman_ctx: Option<&PodmanCtx>) -> Command {
    let mut command = base(podman_ctx);
    command.args(["container", "exists", name]);
    command
}

pub(crate) fn container_cleanup(name: &str, podman_ctx: Option<&PodmanCtx>) -> Command {
    let mut command = base_with_read_only_store(podman_ctx);
    command.args(["container", "cleanup", "--rm", name]);
    command
}

pub(crate) fn stop(name: &str, podman_ctx: Option<&PodmanCtx>) -> Command {
    let mut command = base_with_read_only_store(podman_ctx);
    command.args(["stop", name]);
    command
}

pub(crate) fn image_exists(image: &str, podman_ctx: Option<&PodmanCtx>) -> Command {
    let mut command = base_with_read_only_store(podman_ctx);
    command.args(["image", "exists", image]);
    command
}

pub(crate) fn images(podman_ctx: Option<&PodmanCtx>) -> Command {
    let mut command = base_with_read_only_store(podman_ctx);
    command.arg("images");
    command
}

pub(crate) fn inspect(
    target: &str,
    format: Option<&str>,
    podman_ctx: Option<&PodmanCtx>,
) -> Command {
    let mut command = base_with_read_only_store(podman_ctx);
    command.args(["--log-level=error", "inspect"]);
    if let Some(format) = format {
        command.args(["-f", format]);
    }
    command.arg(target);
    command
}

pub(crate) fn info(format: Option<&str>, podman_ctx: Option<&PodmanCtx>) -> Command {
    let mut command = base(podman_ctx);
    command.arg("info");
    if let Some(format) = format {
        command.args(["-f", format]);
    }
    command
}

pub(crate) fn system_reset(podman_ctx: Option<&PodmanCtx>) -> Command {
    let mut command = base(podman_ctx);
    if let Some(ctx) = podman_ctx {
        cli_opt(
            &mut command,
            "--module",
            ctx.module.as_deref().map(OsStr::new),
        );
    }
    command.args(["system", "reset", "--force"]);
    command
}

fn kube(podman_ctx: Option<&PodmanCtx>) -> Command {
    let mut command = base(podman_ctx);
    if let Some(ctx) = podman_ctx {
        cli_storage_opt(
            &mut command,
            "additionalimagestore",
            ctx.ro_store.as_deref().map(Path::as_os_str),
        );
        cli_storage_opt(
            &mut command,
            "mount_program",
            ctx.parallax_mount_program.as_deref().map(Path::as_os_str),
        );
    }
    command.arg("kube");
    command
}

pub(crate) fn kube_play(filepath: &str, podman_ctx: Option<&PodmanCtx>) -> Command {
    let mut command = kube(podman_ctx);
    command.args(["play", filepath]);
    command
}

pub(crate) fn kube_down(filepath: &str, force: bool, podman_ctx: Option<&PodmanCtx>) -> Command {
    let mut command = kube(podman_ctx);
    command.arg("down");
    cli_flag(&mut command, force, "--force");
    command.arg(filepath);
    command
}

pub(crate) fn version(module: Option<&str>) -> Command {
    let mut command = base(None);
    cli_opt(&mut command, "--module", module.map(OsStr::new));
    command.arg("version");
    command
}

pub(crate) fn parallax(
    parallax_path: &PathBuf,
    podman_ctx: &PodmanCtx,
    image: &str,
    action: &'static str,
) -> Result<Command> {
    let read_only_store = podman_ctx
        .ro_store
        .as_ref()
        .ok_or(DriverError::MissingContext {
            operation: parallax_operation(action),
            field: "read-only store path",
        })?;
    let mut command = Command::new(parallax_path);
    command.arg("--roStoragePath").arg(read_only_store);
    cli_opt(
        &mut command,
        "--podmanRoot",
        podman_ctx.graphroot.as_deref().map(Path::as_os_str),
    );
    command.arg(format!("--{action}")).arg("--image").arg(image);
    Ok(command)
}

fn parallax_operation(action: &str) -> &'static str {
    match action {
        "exist" => "parallax exist",
        "migrate" => "parallax migrate",
        "rmi" => "parallax rmi",
        _ => "parallax",
    }
}

fn base_with_read_only_store(podman_ctx: Option<&PodmanCtx>) -> Command {
    let mut command = base(podman_ctx);
    if let Some(ctx) = podman_ctx {
        cli_storage_opt(
            &mut command,
            "additionalimagestore",
            ctx.ro_store.as_deref().map(Path::as_os_str),
        );
    }
    command
}

fn cli_flag(command: &mut Command, enabled: bool, name: &str) {
    if enabled {
        command.arg(name);
    }
}

fn cli_opt(command: &mut Command, name: &str, value: Option<&OsStr>) {
    if let Some(value) = value {
        command.arg(name).arg(value);
    }
}

fn cli_storage_opt(command: &mut Command, name: &str, value: Option<&OsStr>) {
    if let Some(value) = value {
        cli_kv(command, "--storage-opt", OsStr::new(name), value);
    }
}

fn cli_kv(command: &mut Command, name: &str, key: &OsStr, value: &OsStr) {
    command.arg(name).arg(os_string_key_value(key, value));
}

fn os_string_key_value(key: &OsStr, value: &OsStr) -> OsString {
    let mut buffer = OsString::with_capacity(key.len() + 1 + value.len());
    buffer.push(key);
    buffer.push("=");
    buffer.push(value);
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> PodmanCtx {
        PodmanCtx {
            podman_path: PathBuf::from("podman"),
            module: None,
            graphroot: None,
            runroot: None,
            parallax_mount_program: None,
            ro_store: None,
            podman_env: None,
        }
    }

    #[test]
    fn parallax_requires_read_only_store() {
        let error = parallax(&PathBuf::from("parallax"), &context(), "image", "exist").unwrap_err();

        assert!(matches!(
            error,
            DriverError::MissingContext {
                operation: "parallax exist",
                field: "read-only store path"
            }
        ));
    }

    #[test]
    fn container_cleanup_uses_read_only_store() {
        let mut podman_ctx = context();
        podman_ctx.graphroot = Some(PathBuf::from("/tmp/graphroot"));
        podman_ctx.runroot = Some(PathBuf::from("/tmp/runroot"));
        podman_ctx.ro_store = Some(PathBuf::from("/shared/imagestore"));

        let command = container_cleanup("test-container", Some(&podman_ctx));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![
                OsStr::new("--root"),
                OsStr::new("/tmp/graphroot"),
                OsStr::new("--runroot"),
                OsStr::new("/tmp/runroot"),
                OsStr::new("--storage-opt"),
                OsStr::new("additionalimagestore=/shared/imagestore"),
                OsStr::new("container"),
                OsStr::new("cleanup"),
                OsStr::new("--rm"),
                OsStr::new("test-container"),
            ]
        );
    }
}
