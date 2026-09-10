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
    let mut command = base_with_hpc_options(podman_ctx);
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
    add_container_args_to_command(
        run(podman_ctx),
        ContainerCommandKind::Run,
        edf,
        container_ctx,
        container_command,
    )
}

pub(crate) fn create(podman_ctx: Option<&PodmanCtx>) -> Command {
    let mut command = base_with_hpc_options(podman_ctx);
    command.arg("create");
    command
}

pub(crate) fn create_from_edf<I, S>(
    edf: &EDF,
    podman_ctx: Option<&PodmanCtx>,
    container_ctx: &ContainerCtx,
    container_command: I,
) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    add_container_args_to_command(
        create(podman_ctx),
        ContainerCommandKind::Create,
        edf,
        container_ctx,
        container_command,
    )
}

pub(crate) fn start(container: &str, podman_ctx: Option<&PodmanCtx>, attach: bool) -> Command {
    let mut command = base_with_hpc_options(podman_ctx);
    command.arg("start");
    cli_flag(&mut command, attach, "--attach");
    command.arg("--").arg(container);
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

fn base_with_hpc_options(podman_ctx: Option<&PodmanCtx>) -> Command {
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
    command
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContainerCommandKind {
    Run,
    Create,
}

fn add_container_args_to_command<I, S>(
    mut command: Command,
    kind: ContainerCommandKind,
    edf: &EDF,
    container_ctx: &ContainerCtx,
    container_command: I,
) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    cli_flag(&mut command, container_ctx.auto_remove, "--rm");
    cli_flag(
        &mut command,
        kind == ContainerCommandKind::Run && container_ctx.detach,
        "--detach",
    );
    cli_flag(&mut command, container_ctx.interactive, "--interactive");
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

    // TODO: support entrypoint redefinition as well
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

fn cli_flag(command: &mut Command, enabled: bool, name: &str) {
    if enabled {
        command.arg(name);
    }
}

// TODO: Consider using AsRef<OsStr> or Into<OsStr> to streamline passing of value
fn cli_opt(command: &mut Command, name: &str, value: Option<&OsStr>) {
    if let Some(value) = value {
        command.arg(name).arg(value);
    }
}

// TODO: Consider using AsRef<OsStr> or Into<OsStr> to streamline passing of value
fn cli_storage_opt(command: &mut Command, name: &str, value: Option<&OsStr>) {
    if let Some(value) = value {
        cli_kv(command, "--storage-opt", OsStr::new(name), value);
    }
}

// TODO: Consider using AsRef<OsStr> or Into<OsStr> to streamline passing of value
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
    use std::collections::HashMap;

    fn fixture_edf() -> EDF {
        let edf_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/edf/run_from_edf_test.toml");
        raster::render(edf_path.to_string_lossy().into_owned()).expect("Failed rendering EDF")
    }

    fn podman_context() -> PodmanCtx {
        PodmanCtx {
            podman_path: PathBuf::from("/usr/bin/podman"),
            module: Some(String::from("hpc")),
            graphroot: Some(PathBuf::from("/dev/shm/sarus-test/graphroot")),
            runroot: Some(PathBuf::from("/dev/shm/sarus-test/runroot")),
            parallax_mount_program: Some(PathBuf::from(
                "/usr/local/sarus-test/parallax_mount_program",
            )),
            ro_store: Some(PathBuf::from("/scratch/user/parallax/store")),
            podman_env: None,
        }
    }

    #[test]
    fn run_from_edf_cli_syntax() {
        let p_ctx = podman_context();

        let c_ctx = ContainerCtx {
            name: String::from("edf_test"),
            interactive: true,
            tty: true,
            detach: true,
            auto_remove: true,
            set_env: true,
            pidfile: Some(PathBuf::from("/tmp/test/pidfile")),
            user: Some(String::from("1234:4321")),
        };

        let edf = fixture_edf();

        let cmd = run_from_edf(&edf, Some(&p_ctx), &c_ctx, ["bash"]);
        assert_eq!(cmd.get_program(), OsStr::new("/usr/bin/podman"));

        let args: Vec<&OsStr> = cmd.get_args().collect();
        assert_eq!(args.len(), 43);

        let args_head: Vec<&OsStr> = vec![
            OsStr::new("--root"),
            OsStr::new("/dev/shm/sarus-test/graphroot"),
            OsStr::new("--runroot"),
            OsStr::new("/dev/shm/sarus-test/runroot"),
            OsStr::new("--module"),
            OsStr::new("hpc"),
            OsStr::new("--storage-opt"),
            OsStr::new("additionalimagestore=/scratch/user/parallax/store"),
            OsStr::new("--storage-opt"),
            OsStr::new("mount_program=/usr/local/sarus-test/parallax_mount_program"),
            OsStr::new("run"),
            OsStr::new("--rm"),
            OsStr::new("--detach"),
            OsStr::new("--interactive"),
            OsStr::new("--tty"),
            OsStr::new("--read-only"),
            OsStr::new("--name"),
            OsStr::new("edf_test"),
            OsStr::new("--user"),
            OsStr::new("1234:4321"),
            OsStr::new("--pidfile"),
            OsStr::new("/tmp/test/pidfile"),
            OsStr::new("--entrypoint="),
        ];
        assert_eq!(args[..23], args_head);

        // Use any() and iterator windows to be flexible w.r.t HashMap ordering and
        // at the same time check that option/value pairs are respected
        assert!(
            args.windows(2)
                .any(|w| w == [OsStr::new("--workdir"), OsStr::new("/develop")])
        );
        assert!(args.windows(2).any(|w| w
            == [
                OsStr::new("--volume"),
                OsStr::new("/home/user/test:/develop")
            ]));
        assert!(
            args.windows(2)
                .any(|w| w == [OsStr::new("--volume"), OsStr::new("/src2:/dst2")])
        );
        assert!(
            args.windows(2)
                .any(|w| w == [OsStr::new("--device"), OsStr::new("/dev/fuse")])
        );
        assert!(
            args.windows(2)
                .any(|w| w == [OsStr::new("--device"), OsStr::new("nvidia.com/gpu=all")])
        );
        assert!(
            args.windows(2)
                .any(|w| w == [OsStr::new("--env"), OsStr::new("TEST_1=EDF!")])
        );
        assert!(
            args.windows(2)
                .any(|w| w == [OsStr::new("--env"), OsStr::new("TEST_2=foobar")])
        );
        assert!(args.windows(2).any(|w| w
            == [
                OsStr::new("--annotation"),
                OsStr::new("com.hooks.test1.enabled=true")
            ]));
        assert!(args.windows(2).any(|w| w
            == [
                OsStr::new("--annotation"),
                OsStr::new("com.hooks.test2.enabled=false")
            ]));

        // Image and container command must be positionally at the end of args
        assert_eq!(
            args[args.len() - 2..],
            [OsStr::new("ubuntu:24.04"), OsStr::new("bash")]
        );
    }

    #[test]
    fn run_from_edf_handles_interactive_and_tty_independently() {
        let edf = fixture_edf();

        for (interactive, tty) in [(false, false), (true, false), (false, true), (true, true)] {
            let container_ctx = ContainerCtx {
                name: String::from("edf_test"),
                interactive,
                tty,
                detach: false,
                auto_remove: true,
                set_env: false,
                pidfile: None,
                user: None,
            };

            let command = run_from_edf(&edf, None, &container_ctx, std::iter::empty::<&str>());
            let arguments: Vec<&OsStr> = command.get_args().collect();

            assert_eq!(
                arguments.contains(&OsStr::new("--interactive")),
                interactive
            );
            assert_eq!(arguments.contains(&OsStr::new("--tty")), tty);
            assert!(!arguments.contains(&OsStr::new("-it")));
        }

        let retained_container_ctx = ContainerCtx {
            name: String::from("retained"),
            interactive: false,
            tty: false,
            detach: false,
            auto_remove: false,
            set_env: false,
            pidfile: None,
            user: None,
        };
        let command = run_from_edf(
            &edf,
            None,
            &retained_container_ctx,
            std::iter::empty::<&str>(),
        );
        assert!(!command.get_args().any(|argument| argument == "--rm"));
    }

    #[test]
    fn create_from_edf_uses_create_syntax_and_ignores_detach() {
        let p_ctx = podman_context();
        let c_ctx = ContainerCtx {
            name: String::from("edf_test"),
            interactive: true,
            tty: true,
            detach: true,
            auto_remove: true,
            set_env: true,
            pidfile: Some(PathBuf::from("/tmp/test/pidfile")),
            user: Some(String::from("1234:4321")),
        };
        let edf = fixture_edf();

        let command = create_from_edf(&edf, Some(&p_ctx), &c_ctx, ["bash"]);
        let args: Vec<&OsStr> = command.get_args().collect();

        assert_eq!(
            &args[..22],
            [
                OsStr::new("--root"),
                OsStr::new("/dev/shm/sarus-test/graphroot"),
                OsStr::new("--runroot"),
                OsStr::new("/dev/shm/sarus-test/runroot"),
                OsStr::new("--module"),
                OsStr::new("hpc"),
                OsStr::new("--storage-opt"),
                OsStr::new("additionalimagestore=/scratch/user/parallax/store"),
                OsStr::new("--storage-opt"),
                OsStr::new("mount_program=/usr/local/sarus-test/parallax_mount_program"),
                OsStr::new("create"),
                OsStr::new("--rm"),
                OsStr::new("--interactive"),
                OsStr::new("--tty"),
                OsStr::new("--read-only"),
                OsStr::new("--name"),
                OsStr::new("edf_test"),
                OsStr::new("--user"),
                OsStr::new("1234:4321"),
                OsStr::new("--pidfile"),
                OsStr::new("/tmp/test/pidfile"),
                OsStr::new("--entrypoint="),
            ]
        );
        assert!(!args.contains(&OsStr::new("--detach")));
        assert!(args.contains(&OsStr::new("--rm")));
        assert_eq!(
            args[args.len() - 2..],
            [OsStr::new("ubuntu:24.04"), OsStr::new("bash")]
        );
        assert!(args.windows(2).any(|w| {
            w == [
                OsStr::new("--storage-opt"),
                OsStr::new("mount_program=/usr/local/sarus-test/parallax_mount_program"),
            ]
        }));
        assert!(
            args.windows(2)
                .any(|w| { w == [OsStr::new("--workdir"), OsStr::new("/develop")] })
        );
        assert!(args.windows(2).any(|w| {
            w == [
                OsStr::new("--volume"),
                OsStr::new("/home/user/test:/develop"),
            ]
        }));
        assert!(
            args.windows(2)
                .any(|w| w == [OsStr::new("--volume"), OsStr::new("/src2:/dst2")])
        );
        assert!(
            args.windows(2)
                .any(|w| w == [OsStr::new("--device"), OsStr::new("/dev/fuse")])
        );
        assert!(
            args.windows(2)
                .any(|w| w == [OsStr::new("--device"), OsStr::new("nvidia.com/gpu=all")])
        );
        assert!(
            args.windows(2)
                .any(|w| w == [OsStr::new("--env"), OsStr::new("TEST_1=EDF!")])
        );
        assert!(
            args.windows(2)
                .any(|w| w == [OsStr::new("--env"), OsStr::new("TEST_2=foobar")])
        );
        assert!(args.windows(2).any(|w| {
            w == [
                OsStr::new("--annotation"),
                OsStr::new("com.hooks.test1.enabled=true"),
            ]
        }));
        assert!(args.windows(2).any(|w| {
            w == [
                OsStr::new("--annotation"),
                OsStr::new("com.hooks.test2.enabled=false"),
            ]
        }));
    }

    #[test]
    fn start_cli_syntax_propagates_hpc_context_and_delimits_target() {
        let mut p_ctx = podman_context();
        p_ctx.podman_env = Some(HashMap::from([(
            OsString::from("PODMAN_ONLY"),
            OsString::from("start-value"),
        )]));

        let attached = start("--looks-like-an-option", Some(&p_ctx), true);
        assert_eq!(
            attached.get_args().collect::<Vec<_>>(),
            vec![
                OsStr::new("--root"),
                OsStr::new("/dev/shm/sarus-test/graphroot"),
                OsStr::new("--runroot"),
                OsStr::new("/dev/shm/sarus-test/runroot"),
                OsStr::new("--module"),
                OsStr::new("hpc"),
                OsStr::new("--storage-opt"),
                OsStr::new("additionalimagestore=/scratch/user/parallax/store"),
                OsStr::new("--storage-opt"),
                OsStr::new("mount_program=/usr/local/sarus-test/parallax_mount_program"),
                OsStr::new("start"),
                OsStr::new("--attach"),
                OsStr::new("--"),
                OsStr::new("--looks-like-an-option"),
            ]
        );
        assert!(attached.get_envs().any(|(key, value)| {
            key == OsStr::new("PODMAN_ONLY") && value == Some(OsStr::new("start-value"))
        }));

        let detached = start("container", None, false);
        assert_eq!(
            detached.get_args().collect::<Vec<_>>(),
            vec![
                OsStr::new("start"),
                OsStr::new("--"),
                OsStr::new("container")
            ]
        );
    }

    #[test]
    fn partial_context_only_adds_values_that_are_present() {
        let context = PodmanCtx {
            podman_path: PathBuf::from("/usr/bin/podman"),
            module: None,
            graphroot: Some(PathBuf::from("/tmp/graphroot")),
            runroot: None,
            parallax_mount_program: None,
            ro_store: None,
            podman_env: None,
        };

        assert_eq!(
            run(Some(&context)).get_args().collect::<Vec<_>>(),
            vec![
                OsStr::new("--root"),
                OsStr::new("/tmp/graphroot"),
                OsStr::new("run")
            ]
        );
        assert_eq!(
            create(Some(&context)).get_args().collect::<Vec<_>>(),
            vec![
                OsStr::new("--root"),
                OsStr::new("/tmp/graphroot"),
                OsStr::new("create")
            ]
        );
        assert_eq!(
            start("container", Some(&context), false)
                .get_args()
                .collect::<Vec<_>>(),
            vec![
                OsStr::new("--root"),
                OsStr::new("/tmp/graphroot"),
                OsStr::new("start"),
                OsStr::new("--"),
                OsStr::new("container")
            ]
        );

        assert_eq!(
            run(None).get_args().collect::<Vec<_>>(),
            vec![OsStr::new("run")]
        );
        assert_eq!(
            create(None).get_args().collect::<Vec<_>>(),
            vec![OsStr::new("create")]
        );
    }

    #[test]
    fn create_and_run_preserve_interactive_tty_and_auto_remove_mappings() {
        let edf = fixture_edf();

        for (interactive, tty) in [(false, false), (true, false), (false, true), (true, true)] {
            for auto_remove in [false, true] {
                for detach in [false, true] {
                    let ctx = ContainerCtx {
                        name: String::from("container"),
                        interactive,
                        tty,
                        detach,
                        auto_remove,
                        set_env: false,
                        pidfile: None,
                        user: None,
                    };

                    for (kind, command) in [
                        (
                            ContainerCommandKind::Run,
                            run_from_edf(&edf, None, &ctx, ["true"]),
                        ),
                        (
                            ContainerCommandKind::Create,
                            create_from_edf(&edf, None, &ctx, ["true"]),
                        ),
                    ] {
                        let args: Vec<&OsStr> = command.get_args().collect();
                        assert_eq!(args.contains(&OsStr::new("--interactive")), interactive);
                        assert_eq!(args.contains(&OsStr::new("--tty")), tty);
                        assert_eq!(args.contains(&OsStr::new("--rm")), auto_remove);
                        assert_eq!(
                            args.contains(&OsStr::new("--detach")),
                            kind == ContainerCommandKind::Run && detach
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn optional_edf_and_container_values_are_omitted() {
        let mut edf = fixture_edf();
        edf.workdir.clear();
        edf.entrypoint = true;
        edf.writable = true;
        edf.mounts.clear();
        edf.devices.clear();
        edf.annotations.clear();

        let ctx = ContainerCtx {
            name: String::from("container"),
            interactive: false,
            tty: false,
            detach: false,
            auto_remove: false,
            set_env: false,
            pidfile: None,
            user: None,
        };
        let command = create_from_edf(&edf, None, &ctx, std::iter::empty::<&str>());
        let args: Vec<&OsStr> = command.get_args().collect();

        for omitted in [
            "--workdir",
            "--volume",
            "--device",
            "--env",
            "--annotation",
            "--user",
            "--pidfile",
            "--entrypoint=",
            "--read-only",
            "--rm",
            "--detach",
        ] {
            assert!(!args.contains(&OsStr::new(omitted)), "unexpected {omitted}");
        }
    }

    #[test]
    fn command_arguments_remain_single_values() {
        let edf = fixture_edf();
        let ctx = ContainerCtx {
            name: String::from("name with spaces"),
            interactive: false,
            tty: false,
            detach: false,
            auto_remove: false,
            set_env: true,
            pidfile: None,
            user: Some(String::from("user=with=value")),
        };
        let command = run_from_edf(&edf, None, &ctx, ["", "two words", "a;b", "key=value"]);
        let args: Vec<&OsStr> = command.get_args().collect();
        assert_eq!(
            &args[args.len() - 5..],
            [
                OsStr::new("ubuntu:24.04"),
                OsStr::new(""),
                OsStr::new("two words"),
                OsStr::new("a;b"),
                OsStr::new("key=value"),
            ]
        );
        assert!(
            args.windows(2)
                .any(|w| { w == [OsStr::new("--user"), OsStr::new("user=with=value")] })
        );
    }

    #[test]
    fn podman_environment_is_separate_from_edf_environment() {
        let mut p_ctx = podman_context();
        p_ctx.podman_env = Some(HashMap::from([(
            OsString::from("DRIVER_TEST"),
            OsString::from("yes"),
        )]));
        let command = run(Some(&p_ctx));
        assert!(command.get_envs().any(
            |(key, value)| key == OsStr::new("DRIVER_TEST") && value == Some(OsStr::new("yes"))
        ));

        let edf = fixture_edf();
        let ctx = ContainerCtx {
            name: String::from("container"),
            interactive: false,
            tty: false,
            detach: false,
            auto_remove: false,
            set_env: true,
            pidfile: None,
            user: None,
        };
        let command = run_from_edf(&edf, Some(&p_ctx), &ctx, ["true"]);
        let args: Vec<&OsStr> = command.get_args().collect();
        assert!(
            args.windows(2)
                .any(|w| w == [OsStr::new("--env"), OsStr::new("TEST_1=EDF!")])
        );
        assert!(!args.contains(&OsStr::new("DRIVER_TEST=yes")));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_and_arguments_are_preserved() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let mut p_ctx = podman_context();
        p_ctx.graphroot = Some(PathBuf::from(OsString::from_vec(vec![
            b'/', 0xff, b'/', b'g',
        ])));
        p_ctx.runroot = Some(PathBuf::from(OsString::from_vec(vec![
            b'/', 0xfe, b'/', b'r',
        ])));
        p_ctx.ro_store = Some(PathBuf::from(OsString::from_vec(vec![
            b'/', 0xfd, b'/', b's',
        ])));
        p_ctx.parallax_mount_program = Some(PathBuf::from(OsString::from_vec(vec![
            b'/', 0xfc, b'/', b'm',
        ])));
        let edf = fixture_edf();
        let ctx = ContainerCtx {
            name: String::from("container"),
            interactive: false,
            tty: false,
            detach: false,
            auto_remove: false,
            set_env: false,
            pidfile: None,
            user: None,
        };
        let raw = OsString::from_vec(vec![b'a', 0xfe, b'b']);
        let command = run_from_edf(&edf, Some(&p_ctx), &ctx, [raw]);
        let args: Vec<&OsStr> = command.get_args().collect();
        assert!(args.iter().any(|arg| arg.as_bytes() == [b'a', 0xfe, b'b']));
        assert_eq!(
            command
                .get_args()
                .nth(1)
                .expect("missing graphroot")
                .as_bytes(),
            [b'/', 0xff, b'/', b'g']
        );
        assert_eq!(
            command
                .get_args()
                .nth(3)
                .expect("missing runroot")
                .as_bytes(),
            [b'/', 0xfe, b'/', b'r']
        );
        assert!(command.get_args().any(|argument| {
            argument.as_bytes()
                == [
                    b'a', b'd', b'd', b'i', b't', b'i', b'o', b'n', b'a', b'l', b'i', b'm', b'a',
                    b'g', b'e', b's', b't', b'o', b'r', b'e', b'=', b'/', 0xfd, b'/', b's',
                ]
        }));
        assert!(command.get_args().any(|argument| {
            argument.as_bytes()
                == [
                    b'm', b'o', b'u', b'n', b't', b'_', b'p', b'r', b'o', b'g', b'r', b'a', b'm',
                    b'=', b'/', 0xfc, b'/', b'm',
                ]
        }));
    }

    #[test]
    fn parallax_cli_syntax() {
        let p_ctx = podman_context();

        let parallax_path = PathBuf::from("/usr/local/sarus-test/parallax");
        let image = String::from("ubuntu:24.04");

        let cmd = parallax(&parallax_path, &p_ctx, &image, "migrate").unwrap();

        assert_eq!(cmd.get_program(), parallax_path);

        let args: Vec<&OsStr> = cmd.get_args().collect();
        assert_eq!(args.len(), 7);
        assert!(args.windows(2).any(|w| w
            == [
                OsStr::new("--podmanRoot"),
                OsStr::new(p_ctx.graphroot.as_deref().unwrap())
            ]));
        assert!(args.windows(2).any(|w| w
            == [
                OsStr::new("--roStoragePath"),
                OsStr::new(p_ctx.ro_store.as_deref().unwrap())
            ]));
        assert_eq!(
            args[args.len() - 3..],
            [
                OsStr::new("--migrate"),
                OsStr::new("--image"),
                OsStr::new(&image)
            ]
        );
    }

    #[test]
    fn parallax_requires_read_only_store() {
        let p_ctx = PodmanCtx {
            podman_path: PathBuf::from("podman"),
            module: None,
            graphroot: None,
            runroot: None,
            parallax_mount_program: None,
            ro_store: None,
            podman_env: None,
        };

        let error = parallax(&PathBuf::from("parallax"), &p_ctx, "image", "exist").unwrap_err();

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
        let mut podman_ctx = podman_context();
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
