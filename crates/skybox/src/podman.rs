use std::error::Error;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::Instant;
use sysinfo::{Pid, System};

use slurm_spank::{SpankHandle, spank_log_user};

use sarus_suite_podman_driver::{self as pmd, ContainerCtx, DriverError, PodmanCtx, command_failed};

use crate::{SpankSkyBox, get_job_env, plugin_err, skybox_log_debug};

pub(crate) const PODMAN_PIDFILE_NAME: &str = "pidfile";

fn process_exists(pid: usize) -> bool {
    let p = Pid::from(pid);

    let s = System::new_all();
    let ret = match s.process(p) {
        None => false,
        Some(process) => {
            let state = process.status();
            skybox_log_debug!("process {pid} status is {state}");
            true
        }
    };
    ret
}

pub(crate) fn podman_start(
    ssb: &mut SpankSkyBox,
    spank: &mut SpankHandle,
) -> Result<(), Box<dyn Error>> {
    let orig_edf = match &ssb.edf {
        Some(o) => o,
        None => {
            return plugin_err("couldn't find edf");
        }
    };

    let run = match &ssb.run {
        Some(o) => o,
        None => {
            return plugin_err("couldn't find run");
        }
    };

    let job = match &ssb.job {
        Some(job) => job,
        None => {
            return plugin_err("couldn't find job");
        }
    };

    let config = &ssb.config;

    let image_digest = run.image_digest.clone();
    let graphroot = format!("{}/graphroot", run.podman_tmp_path);
    let runroot = format!("{}/runroot", run.podman_tmp_path);
    let pidfile = format!("{}/{}", run.podman_tmp_path, PODMAN_PIDFILE_NAME);
    //let command = vec!["sleep", "infinity"];
    let command = vec!["sh", "-c", "kill -STOP $$ ; exit 0"];
    //let command = vec!["sh", "-l", "-c", "exec sh -c 'kill -STOP $$ ; exit 0'"];

    let mut edf = orig_edf.clone();
    edf.image = image_digest;

    let c_ctx = ContainerCtx {
        name: run.name.clone(),
        interactive: false,
        tty: false,
        detach: true,
        auto_remove: true,
        set_env: false,
        pidfile: Some(PathBuf::from(pidfile.clone())),
        user: Some(job.uid.to_string()),
    };

    let run_ctx = PodmanCtx {
        podman_path: PathBuf::from(&config.podman_path),
        conmon_path: Some(PathBuf::from(&config.conmon_path)),
        module: Some(String::from(&config.podman_module)),
        graphroot: Some(PathBuf::from(&graphroot)),
        runroot: Some(PathBuf::from(&runroot)),
        parallax_mount_program: Some(PathBuf::from(&config.parallax_mount_program)),
        ro_store: Some(PathBuf::from(&config.parallax_imagestore)),
        mksquashfs_path: None,
        podman_env: None,
    }
    .with_env("PARALLAX_MP_UID", config.parallax_mp_uid.to_string())
    .with_env("PARALLAX_MP_GID", config.parallax_mp_gid.to_string())
    .with_env(
        "PARALLAX_MP_SQUASHFUSE_CMD",
        config.parallax_mp_squashfuse_path.clone(),
    )
    .with_env("PARALLAX_MP_LOGFILE", config.parallax_mp_logfile.clone())
    .with_env_hashmap(get_job_env(spank));

    skybox_log_debug!(
        "mount env: PARALLAX_MP_UID={} PARALLAX_MP_GID={}",
        config.parallax_mp_uid.to_string(),
        config.parallax_mp_gid.to_string()
    );

    return pmd_run(&edf, &config, &run_ctx, &c_ctx, command);
}

// TODO: clarify usefulness of this function since the pid is already acquired in crate::sync::sync_podman_start_wait()
pub(crate) fn podman_get_pid_from_file(ssb: &mut SpankSkyBox) -> Result<usize, Box<dyn Error>> {
    let run = match &ssb.run {
        Some(o) => o,
        None => {
            return Err("couldn't find run data".into());
        }
    };

    //Try to read from pidfile
    let pidfile = format!("{}/{}", run.podman_tmp_path, PODMAN_PIDFILE_NAME);
    if std::path::Path::new(&pidfile).exists() {
        let strpid = match std::fs::read_to_string(&pidfile) {
            Ok(s) => s,
            Err(_) => {
                let err_msg = format!("cannot read pid from {pidfile}");
                return Err(err_msg.into());
            }
        };
        let pid: usize = match strpid.parse() {
            Ok(p) => p,
            Err(_) => {
                let err_msg = format!("cannot convert {strpid} to number");
                return Err(err_msg.into());
            }
        };
        return Ok(pid);
    } else {
        let err_msg = format!("{pidfile} NOT FOUND!");
        Err(err_msg.into())
    }
}

pub(crate) fn podman_stop(
    ssb: &mut SpankSkyBox,
    _spank: &mut SpankHandle,
) -> Result<(), Box<dyn Error>> {
    let run = match &ssb.run {
        Some(o) => o,
        None => {
            return plugin_err("couldn't find run data");
        }
    };

    let pid = run.pid;

    skybox_log_debug!("stopping container, process {pid}");
    let mut kill = std::process::Command::new("kill")
        .args(["-s", "SIGCONT", &pid.to_string()])
        .spawn()?;
    kill.wait()?;

    if process_exists(pid) {
        skybox_log_debug!("process {pid} is still there, waiting one more second.");
        let pause = std::time::Duration::from_secs(1);
        std::thread::sleep(pause);
    }

    if process_exists(pid) {
        skybox_log_debug!("process {pid} is still there, terminating it.");
        let mut kill = std::process::Command::new("kill")
            .args(["-s", "SIGTERM", &pid.to_string()])
            .spawn()?;
        kill.wait()?;
    }

    Ok(())
}

pub(crate) fn pmd_image_exists(image: &str, ctx: &PodmanCtx) -> pmd::Result<bool> {
    // Debug: Workaround - Decompose function call
    //pmd::image_exists(image, Some(ctx))
    let mut command = pmd::base_with_read_only_store(Some(ctx));
    command.args(["image", "exists", image]);
    log_command(&command);
    let result = pmd::execute_captured(command);
    log_command_result(&result);
    Ok(result?.status.success())
}

/*
pub(crate) fn parallax_image_exists(parallax: &PathBuf, image: &str, ctx: &PodmanCtx) -> pmd::Result<bool> {
    pmd::parallax_exist(parallax, ctx, image)
}
*/

/*
pub(crate) fn pmd_pull(image: &str, ctx: &PodmanCtx) -> pmd::Result<()> {
    pmd::pull(image, Some(ctx))
}
*/

pub(crate) fn pmd_parallax_migrate(
    parallax_path: &str,
    ctx: &PodmanCtx,
    image: &str,
) -> Result<(), Box<dyn Error>> {

    // Debug: Workaround - Decompose function call
    //pmd::parallax_migrate(&PathBuf::from(parallax_path), ctx, image)?;
    //Ok(())

    pmd::require_graphroot(ctx)?;
    let command = pmd::command_parallax(&PathBuf::from(parallax_path), ctx, image, "migrate")?;
    let rendered = log_command(&command);
    let result = pmd::execute_captured(command);
    log_command_result(&result);
    let output = result?;

    if output.status.success() {
        Ok(())
    } else {
        Err(Box::new(command_failed(rendered, output)))
    }
}

pub(crate) fn pmd_rmi(image: &str, ctx: &PodmanCtx) -> Result<(), Box<dyn Error>> {
    // Debug: Workaround - Decompose function call
    //pmd::rmi(image, Some(ctx))

    let command = pmd::command_rmi(image, Some(ctx));
    let rendered = log_command(&command);
    let result = pmd::execute_captured(command);
    log_command_result(&result);
    let output = result?;

    if output.status.success() {
        Ok(())
    } else {
        Err(Box::new(command_failed(rendered, output)))
    }
}

pub(crate) fn pmd_run<I, S>(
    edf: &raster::EDF,
    config: &raster::Config,
    p_ctx: &PodmanCtx,
    c_ctx: &ContainerCtx,
    cmd: I,
) -> Result<(), Box<dyn Error>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let t0 = Instant::now();
    
    // Debug: Workaround - Decompose function call
    //let result = pmd::run_from_edf_output(edf, Some(p_ctx), c_ctx, cmd);
    let command = pmd::command_run_from_edf(edf, Some(p_ctx), c_ctx, cmd);
    let rendered = log_command(&command);
    let result = pmd::execute_captured(command);

    let tend = t0.elapsed();

    if config.perfmon {
        spank_log_user!(
            "skybox-perf: Podman run elapsed time: {:.6} sec",
            tend.as_secs_f64()
        );
    }
    log_command_result(&result);
    let output = result?;

    if output.status.success() {
        Ok(())
    } else {
        Err(Box::new(command_failed(rendered, output)))
    }
}

/*
pub(crate) fn pmd_get_graphroot(ctx: &PodmanCtx) -> Result<String, Box<dyn Error>> {
    // Debug: Workaround - Decompose function call
    //let info_out = pmd::info(Some("{{.Store.GraphRoot}}"),Some(ctx))?
    //    .stdout;
    let command = pmd::command_info(Some("{{.Store.GraphRoot}}"),Some(ctx));
    let rendered = log_command(&command);
    let result = pmd::execute_captured(command);
    log_command_result(&result);
    let output = result?;

    /*
    let graphroot = String::from_utf8_lossy(&info_out)
        .trim_end_matches('\n')
        .to_string();

    Ok(graphroot)
    */

    if output.status.success() {
        let graphroot = String::from_utf8_lossy(&output.stdout)
            .trim_end_matches('\n')
            .to_string();
        Ok(graphroot)
    } else {
        Err(Box::new(command_failed(rendered, output)))
    }
}
*/

pub(crate) fn log_command(cmd: &Command) -> String {
    let mut output = String::from("");
    let envs = cmd.get_envs();
    for (orig_key, opt_value) in envs {
        if opt_value.is_some() {
            let key = orig_key.to_string_lossy().trim_matches('"').to_string();
            let value = opt_value.unwrap().to_string_lossy().trim_matches('"').to_string();
            if value.contains(char::is_whitespace) {
                output = format!("{output}{key}=\"{value}\"; ");
            } else {
                output = format!("{output}{key}={value}; ");
            }
        }
    }
    let program = cmd.get_program().to_string_lossy().trim_matches('"').to_string();
    output = format!("{output}{program}");
    let args = cmd.get_args();
    for orig_arg in args {
        let arg = orig_arg.to_string_lossy().trim_matches('"').to_string();
        if arg.contains(char::is_whitespace) {
            output = format!("{output} \"{arg}\"");
        } else {
            output = format!("{output} {arg}");
        }
    }
    skybox_log_debug!("Executing: {output}");
    output
}

pub(crate) fn log_command_result(result: &Result<Output, DriverError>) {
    match result {
        Ok(o) => log_command_output(&o),
        Err(e) => log_command_error(&e),
    }
}

pub(crate) fn log_command_output(output: &Output) {
    let rc = match output.status.code() {
        Some(c) => c.to_string(),
        None => String::from("Terminated by a signal"),
    };

    skybox_log_debug!("RC: {rc}");
    let stdout = String::from_utf8(output.stdout.clone())
        .unwrap_or(String::from("<cannot parse stdout>"))
        .trim_end()
        .to_string();
    if ! stdout.is_empty() {
        skybox_log_debug!("stdout: {stdout}");
    }

    let stderr = String::from_utf8(output.stderr.clone())
        .unwrap_or(String::from("<cannot parse stderr>"))
        .trim_end()
        .to_string();
    if ! stderr.is_empty() {
        skybox_log_debug!("stderr: {stderr}");
    }
}

pub(crate) fn log_command_error(error: &DriverError) {
    let status = match error.exit_status() {
        Some(s) => s,
        None => {
            skybox_log_debug!("RC: cannot find exit status");
            return;
        },
    };

    let rc = match status.code() {
        Some(c) => c.to_string(),
        None => String::from("Terminated by a signal"),
    };
    skybox_log_debug!("RC: {rc}");

    let stdout = match error.stdout() {
        Some(s) => s.trim_end().to_string(),
        None => String::from(""),
    };
    if ! stdout.is_empty() {
        skybox_log_debug!("stdout: {stdout}");
    }
    
    let stderr = match error.stderr() {
        Some(s) => s.trim_end().to_string(),
        None => String::from(""),
    };
    if ! stderr.is_empty() {
        skybox_log_debug!("stderr: {stderr}");
    }
}
