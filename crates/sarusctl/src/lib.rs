use clap::ValueEnum;
use raster::{self, EDF, ExecutedCommand, config::Config, hook_run};
use sarus_suite_podman_driver::{self as pmd, ContainerCtx, PodmanCtx};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::str;
use std::time::{Duration, Instant};
use uuid::Uuid;

// TODO review pub status in this file, restrict pub only to entities needed in main.rs and tests

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecOptions {
    pub verbose: bool,
    pub parallax_imagestore: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum FormatOutput {
    Text,
    Json,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormattedOutput {
    pub stdout: String,
    pub stderr: String,
}

pub fn format_output(format: FormatOutput, output: &AppOutput) -> FormattedOutput {
    match format {
        FormatOutput::Text => FormattedOutput {
            stdout: output.stdout.clone(),
            stderr: output.stderr.clone(),
        },
        FormatOutput::Json => FormattedOutput {
            stdout: serde_json::to_string_pretty(output).unwrap_or_else(|_| String::from("{}")),
            stderr: String::new(),
        },
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandSpec {
    Validate {
        filepath: String,
        output: FormatOutput,
    },
    Render {
        filepath: String,
        output: FormatOutput,
    },
    Images,
    Pull {
        image: String,
    },
    Migrate {
        image: String,
    },
    Rmi {
        image: String,
    },
    Run {
        filepath: String,
        container_cmd: Vec<String>,
    },
}

impl CommandSpec {
    pub fn output_format(&self) -> FormatOutput {
        match self {
            CommandSpec::Validate { output, .. } | CommandSpec::Render { output, .. } => *output,
            _ => FormatOutput::Text,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AppOutput {
    pub stdout: String,
    pub stderr: String,
    pub return_code: i32,
}

impl AppOutput {
    fn success(stdout: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: String::new(),
            return_code: 0,
        }
    }

    fn failure(stdout: impl Into<String>, stderr: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: stderr.into(),
            return_code: 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppError {
    ConfigLoad(String),
    PodmanInfo(String),
    Runtime(String),
    UnsupportedInput(String),
    Io(String),
    Yaml(String),
    UserLookup(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::ConfigLoad(msg) => write!(f, "Failed to load configuration: {msg}"),
            AppError::PodmanInfo(msg) => write!(f, "Failed to query Podman info: {msg}"),
            AppError::Runtime(msg) => write!(f, "{msg}"),
            AppError::UnsupportedInput(msg) => write!(f, "{msg}"),
            AppError::Io(msg) => write!(f, "{msg}"),
            AppError::Yaml(msg) => write!(f, "{msg}"),
            AppError::UserLookup(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for AppError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentUser {
    pub uid: u32,
    pub gid: u32,
}

pub trait UserContext {
    fn current_user(&self) -> Result<CurrentUser, AppError>;
}

pub trait RasterOps {
    fn load_config(&self) -> Result<Config, AppError>;
    fn load_config_path(&self, path: &Path) -> Result<Config, AppError>;
    fn validate(&self, path: &str) -> Result<(), String>;
    fn render(&self, path: &str) -> Result<EDF, String>;
}

pub trait ContainerRuntime {
    fn default_graphroot(&self, ctx: &PodmanCtx) -> Result<PathBuf, AppError>;
    fn images(&self, ctx: &PodmanCtx) -> Result<(), AppError>;
    fn pull(&self, image: &str, ctx: &PodmanCtx, verbose: bool) -> Result<(), AppError>;
    fn image_exists(&self, image: &str, ctx: &PodmanCtx) -> Result<bool, AppError>;
    fn parallax_exist(
        &self,
        parallax_path: &Path,
        ctx: &PodmanCtx,
        image: &str,
    ) -> Result<bool, AppError>;
    fn parallax_migrate(
        &self,
        parallax_path: &Path,
        ctx: &PodmanCtx,
        image: &str,
        verbose: bool,
    ) -> Result<(), AppError>;
    fn parallax_rmi(
        &self,
        parallax_path: &Path,
        ctx: &PodmanCtx,
        image: &str,
        verbose: bool,
    ) -> Result<(), AppError>;
    fn run_from_edf(
        &self,
        edf: &EDF,
        run_ctx: &PodmanCtx,
        container_ctx: &ContainerCtx,
        container_cmd: &[String],
    ) -> Result<i32, AppError>;
    fn exec_interactive(
        &self,
        container_name: &str,
        podman_ctx: &PodmanCtx,
        container_cmd: &[String],
    ) -> Result<i32, AppError>;
    fn kube_play(&self, filepath: &str, run_ctx: &PodmanCtx) -> Result<(), AppError>;
    fn kube_down(&self, filepath: &str, force: bool, run_ctx: &PodmanCtx) -> Result<(), AppError>;
    fn cleanup_container(&self, container_name: &str, run_ctx: &PodmanCtx) -> Result<(), AppError>;
}

pub struct AppDeps<'a> {
    pub raster: &'a dyn RasterOps,
    pub runtime: &'a dyn ContainerRuntime,
    pub user: &'a dyn UserContext,
}

pub struct RealRasterOps;

impl RasterOps for RealRasterOps {
    fn load_config(&self) -> Result<Config, AppError> {
        load_config_with_fallback(self)
    }

    fn load_config_path(&self, path: &Path) -> Result<Config, AppError> {
        raster::load_config_path(
            Some(path.to_path_buf()),
            raster::config::VarExpand::Must,
            &None,
        )
        .map_err(|e| AppError::ConfigLoad(e.to_string()))
    }

    fn validate(&self, path: &str) -> Result<(), String> {
        raster::validate(path.to_string()).map_err(|e| e.to_string())
    }

    fn render(&self, path: &str) -> Result<EDF, String> {
        raster::render(path.to_string()).map_err(|e| e.to_string())
    }
}

pub struct RealContainerRuntime;

const SYSTEM_CONFIG_DIR: &str = "/etc/sarus-suite";
const USER_CONFIG_DIR_NAME: &str = "sarus-suite";

fn load_config_with_fallback(raster: &dyn RasterOps) -> Result<Config, AppError> {
    let mut candidates = vec![PathBuf::from(SYSTEM_CONFIG_DIR)];
    if let Some(user_dir) = user_config_dir() {
        candidates.push(user_dir);
    }

    load_config_from_candidates(raster, &candidates)
}

fn load_config_from_candidates(
    raster: &dyn RasterOps,
    candidates: &[PathBuf],
) -> Result<Config, AppError> {
    let mut searched = Vec::with_capacity(candidates.len());

    for path in candidates {
        searched.push(path.display().to_string());
        if config_dir_exists(path)? {
            return raster.load_config_path(path);
        }
    }

    Err(AppError::ConfigLoad(format!(
        "Cannot find config files in {}",
        searched.join(" or ")
    )))
}

fn config_dir_exists(path: &Path) -> Result<bool, AppError> {
    match fs::metadata(path).map(|metadata| metadata.is_dir()) {
        Ok(true) => Ok(true),
        Ok(false) => Err(AppError::ConfigLoad(format!(
            "Failed to access configuration directory {}: path exists but is not a directory",
            path.display()
        ))),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(AppError::ConfigLoad(format!(
            "Failed to access configuration directory {}: {err}",
            path.display()
        ))),
    }
}

fn user_config_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|path| path.join(".config").join(USER_CONFIG_DIR_NAME))
}

impl ContainerRuntime for RealContainerRuntime {
    fn default_graphroot(&self, ctx: &PodmanCtx) -> Result<PathBuf, AppError> {
        //TODO Revise with logging: println!("Resolving default Podman graphroot...");
        let output = pmd::info(Some("{{.Store.GraphRoot}}"), Some(ctx));
        if !output.status.success() {
            return Err(AppError::PodmanInfo(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        let graphroot = str::from_utf8(&output.stdout)
            .map_err(|e| AppError::PodmanInfo(e.to_string()))?
            .trim();
        Ok(PathBuf::from(graphroot))
    }

    fn images(&self, ctx: &PodmanCtx) -> Result<(), AppError> {
        pmd::images(Some(ctx));
        Ok(())
    }

    fn pull(&self, image: &str, ctx: &PodmanCtx, verbose: bool) -> Result<(), AppError> {
        if verbose {
            let status = pmd::pull_streaming(image, Some(ctx));
            if status.success() {
                Ok(())
            } else {
                Err(AppError::Runtime(format!("Failed to pull image {image}")))
            }
        } else {
            let out = pmd::loggable::pull(image, Some(ctx));
            if out.output.status.success() {
                Ok(())
            } else {
                Err(AppError::Runtime(format!(
                    "Failed to pull image {image}: {}",
                    String::from_utf8_lossy(&out.output.stderr).trim()
                )))
            }
        }
    }

    fn image_exists(&self, image: &str, ctx: &PodmanCtx) -> Result<bool, AppError> {
        //TODO revise with logging:println!("Checking if image {image} exists in Podman...");
        pmd::image_exists(image, Some(ctx)).map_err(|e| AppError::Runtime(e.to_string()))
    }

    fn parallax_exist(
        &self,
        parallax_path: &Path,
        ctx: &PodmanCtx,
        image: &str,
    ) -> Result<bool, AppError> {
        pmd::parallax_exist(&parallax_path.to_path_buf(), ctx, image)
            .map_err(|e| AppError::Runtime(e.to_string()))
    }

    fn parallax_migrate(
        &self,
        parallax_path: &Path,
        ctx: &PodmanCtx,
        image: &str,
        verbose: bool,
    ) -> Result<(), AppError> {
        if verbose {
            pmd::parallax_migrate_streaming(&parallax_path.to_path_buf(), ctx, image)
                .map_err(|_| AppError::Runtime(format!("Failed to migrate image {image}")))
        } else {
            pmd::parallax_migrate(&parallax_path.to_path_buf(), ctx, image)
                .map_err(|e| AppError::Runtime(format!("Failed to migrate image {image}: {e}")))
        }
    }

    fn parallax_rmi(
        &self,
        parallax_path: &Path,
        ctx: &PodmanCtx,
        image: &str,
        verbose: bool,
    ) -> Result<(), AppError> {
        if verbose {
            pmd::parallax_rmi_streaming(&parallax_path.to_path_buf(), ctx, image).map_err(|_| {
                AppError::Runtime(format!(
                    "Failed to remove image {image} from Parallax imagestore"
                ))
            })
        } else {
            pmd::parallax_rmi(&parallax_path.to_path_buf(), ctx, image).map_err(|e| {
                AppError::Runtime(format!(
                    "Failed to remove image {image} from Parallax imagestore: {e}"
                ))
            })
        }
    }

    fn run_from_edf(
        &self,
        edf: &EDF,
        run_ctx: &PodmanCtx,
        container_ctx: &ContainerCtx,
        container_cmd: &[String],
    ) -> Result<i32, AppError> {
        pmd::run_from_edf(edf, Some(run_ctx), container_ctx, container_cmd)
            .code()
            .ok_or_else(|| {
                AppError::Runtime(String::from("Container process terminated by signal"))
            })
    }

    fn exec_interactive(
        &self,
        container_name: &str,
        podman_ctx: &PodmanCtx,
        container_cmd: &[String],
    ) -> Result<i32, AppError> {
        pmd::exec_interactive(container_name, Some(podman_ctx), container_cmd)
            .code()
            .ok_or_else(|| {
                AppError::Runtime(String::from("Container process terminated by signal"))
            })
    }

    fn kube_play(&self, filepath: &str, run_ctx: &PodmanCtx) -> Result<(), AppError> {
        let output = pmd::kube_play_output(filepath, Some(run_ctx));
        podman_output_result(output, "Podman kube play")
    }

    fn kube_down(&self, filepath: &str, force: bool, run_ctx: &PodmanCtx) -> Result<(), AppError> {
        let output = pmd::kube_down_output(filepath, force, Some(run_ctx));
        podman_output_result(output, "Podman kube down")
    }

    fn cleanup_container(&self, container_name: &str, run_ctx: &PodmanCtx) -> Result<(), AppError> {
        let exists = pmd::container_exists(container_name, Some(run_ctx))
            .map_err(|e| AppError::Runtime(e.to_string()))?;
        if !exists {
            return Ok(());
        }

        pmd::container_cleanup(container_name, Some(run_ctx))
            .map_err(|e| AppError::Runtime(e.to_string()))
    }
}

fn podman_output_result(output: std::process::Output, operation: &str) -> Result<(), AppError> {
    if output.status.success() {
        let _ = io::stdout().write_all(&output.stdout);
        let _ = io::stderr().write_all(&output.stderr);
        Ok(())
    } else {
        Err(AppError::Runtime(format!(
            "{operation} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

pub struct RealUserContext;

impl UserContext for RealUserContext {
    fn current_user(&self) -> Result<CurrentUser, AppError> {
        let uid = users::get_current_uid();
        let gid = users::get_current_gid();

        Ok(CurrentUser { uid, gid })
    }
}

/// Context for pulling images with Podman, essentially equivalent to native Podman configuration
fn build_pull_ctx(config: &Config) -> PodmanCtx {
    PodmanCtx {
        podman_path: PathBuf::from(&config.podman_path),
        module: None,
        graphroot: None,
        runroot: None,
        parallax_mount_program: None,
        ro_store: None,
        podman_env: None,
    }
}

/// Seed context for Parallax image-related operations (e.g. ls, migrate, rmi).
/// Intended to have default graphroot and Parallax imagestore as read-only additionalimagestore.
/// Functions using this should complete the context by resolving the default graphroot at runtime and setting it explicitly,
/// because Parallax requires the graphroot to be passed as a CLI argument.
pub fn build_parallax_seed_ctx(config: &Config) -> PodmanCtx {
    PodmanCtx {
        podman_path: PathBuf::from(&config.podman_path),
        module: None,
        graphroot: None,
        runroot: None,
        parallax_mount_program: None,
        ro_store: Some(PathBuf::from(&config.parallax_imagestore)),
        podman_env: None,
    }
}

/// Context which uses the Parallax imagestore (normally a read-only additionalimagestore location) as graphroot.
/// This should be used only for read actions, since the Parallax store is not intended to be manipulated directly by Podman.
/// This context is mostly useful to check if an image exists in the Parallax store (and therefore if it needs pulling or not)
/// without using the run_ctx, which would trigger creation of custom Podman rootdirs and require cleanup in case of errors before run.
pub fn build_readonly_ctx(config: &Config) -> PodmanCtx {
    PodmanCtx {
        podman_path: PathBuf::from(&config.podman_path),
        module: None,
        graphroot: Some(PathBuf::from(&config.parallax_imagestore)),
        runroot: None,
        parallax_mount_program: None,
        ro_store: None,
        podman_env: None,
    }
}

/// Context for running containers. Fully custom Sarus Suite parameters (Podman module, Parallax imagestore, etc.).
/// Uses sarusctl-specific graphroot and runroot to not tamper with default podman rootdirs
pub fn build_run_ctx(config: &Config, user: &CurrentUser, run_id: &Uuid) -> PodmanCtx {
    let roots_base = PathBuf::from("/dev/shm").join(format!(
        "sarusctl-{}-{}",
        user.uid,
        &run_id.simple().to_string()[..12]
    ));

    PodmanCtx {
        podman_path: PathBuf::from(&config.podman_path),
        module: Some(config.podman_module.clone()),
        graphroot: Some(roots_base.join("graphroot")),
        runroot: Some(roots_base.join("runroot")),
        parallax_mount_program: Some(PathBuf::from(&config.parallax_mount_program)),
        ro_store: Some(PathBuf::from(&config.parallax_imagestore)),
        podman_env: None,
    }
    .with_env("PARALLAX_MP_UID", config.parallax_mp_uid.to_string())
    .with_env("PARALLAX_MP_GID", config.parallax_mp_gid.to_string())
    .with_env(
        "PARALLAX_MP_SQUASHFUSE_CMD",
        config.parallax_mp_squashfuse_path.clone(),
    )
    .with_env("PARALLAX_MP_LOGFILE", config.parallax_mp_logfile.clone())
}

fn cleanup_podman_rootdirs(run_ctx: &PodmanCtx) -> Option<String> {
    const CLEANUP_RETRY_INTERVAL: Duration = Duration::from_millis(100);
    const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

    let roots_base = run_ctx
        .graphroot
        .as_ref()
        .and_then(|graphroot| graphroot.parent().map(Path::to_path_buf))
        .or_else(|| {
            run_ctx
                .runroot
                .as_ref()
                .and_then(|runroot| runroot.parent().map(Path::to_path_buf))
        });

    let roots_base = roots_base?;

    // TODO candidate for verbose mode: println!("Cleaning up Podman rootdirs at {}", roots_base.display());
    let start = Instant::now();
    let mut last_remove_error = String::new();

    // Empyrical testing observed that Podman rootdirs can reappear after being removed,
    // likely due to some cleanup process in Podman that runs asynchronously after container termination.
    // To handle this, we attempt to remove the rootdirs and then check for their existence in a loop with a timeout,
    // instead of assuming that a single remove_dir_all will be sufficient.
    loop {
        match fs::remove_dir_all(&roots_base) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
            Err(err) => last_remove_error = err.to_string(),
        }

        std::thread::sleep(CLEANUP_RETRY_INTERVAL);

        if !roots_base.exists() {
            return None;
        }

        let elapsed = start.elapsed();
        if elapsed >= CLEANUP_TIMEOUT {
            let details = describe_dir_entries(&roots_base);
            return Some(if last_remove_error.is_empty() {
                format!(
                    "Warning: Podman rootdirs {} still exist after {} ms of cleanup retries; {details}",
                    roots_base.display(),
                    elapsed.as_millis()
                )
            } else {
                format!(
                    "Warning: Podman rootdirs {} still exist after {} ms of cleanup retries; last remove_dir_all error: {err}; {details}",
                    roots_base.display(),
                    elapsed.as_millis(),
                    err = last_remove_error
                )
            });
        }
    }
}

fn describe_dir_entries(path: &Path) -> String {
    let entries = fs::read_dir(path)
        .ok()
        .map(|iter| {
            iter.filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if entries.is_empty() {
        String::from("directory is empty")
    } else {
        format!("remaining entries: {}", entries.join(", "))
    }
}

pub fn extract_images_from_yaml_value(
    manifest: &yaml_serde::Value,
) -> Result<Vec<String>, AppError> {
    let mut images = BTreeSet::new();
    collect_yaml_images(manifest, &mut images);
    Ok(images.into_iter().collect())
}

pub fn extract_join_container_from_yaml_value(
    manifest: &yaml_serde::Value,
) -> Result<String, AppError> {
    let containers = yaml_mapping_get(manifest, "spec")
        .and_then(|spec| yaml_mapping_get(spec, "containers"))
        .and_then(yaml_serde::Value::as_sequence)
        .ok_or_else(|| AppError::Yaml(String::from("YAML manifest must define spec.containers")))?;

    let mut all_names = Vec::new();
    let mut join_names = Vec::new();

    for container in containers {
        let name = yaml_mapping_get(container, "name")
            .and_then(yaml_serde::Value::as_str)
            .ok_or_else(|| {
                AppError::Yaml(String::from(
                    "Each container in spec.containers must define a string name",
                ))
            })?
            .to_string();

        let join_label = yaml_mapping_get(container, "labels")
            .and_then(|labels| yaml_mapping_get(labels, "run.sarus.join"))
            .and_then(yaml_serde::Value::as_str);

        if join_label == Some("true") {
            join_names.push(name.clone());
        }

        all_names.push(name);
    }

    match join_names.as_slice() {
        [name] => Ok(name.clone()),
        [] => match all_names.as_slice() {
            [name] => Ok(name.clone()),
            [] => Err(AppError::Yaml(String::from(
                "YAML manifest must define at least one container in spec.containers",
            ))),
            _ => Err(AppError::Yaml(String::from(
                "YAML manifest must label exactly one container with run.sarus.join=\"true\" when spec.containers has multiple containers",
            ))),
        },
        _ => Err(AppError::Yaml(String::from(
            "YAML manifest must not label more than one container with run.sarus.join=\"true\"",
        ))),
    }
}

pub fn extract_pod_name_from_yaml_value(manifest: &yaml_serde::Value) -> Result<String, AppError> {
    yaml_mapping_get(manifest, "metadata")
        .and_then(|metadata| yaml_mapping_get(metadata, "name"))
        .and_then(yaml_serde::Value::as_str)
        .map(String::from)
        .ok_or_else(|| {
            AppError::Yaml(String::from(
                "YAML manifest must define metadata.name as a string",
            ))
        })
}

pub fn get_join_container_from_yaml_manifest(
    manifest: &yaml_serde::Value,
) -> Result<String, AppError> {
    let container_name = extract_join_container_from_yaml_value(manifest)?;
    let pod_name = extract_pod_name_from_yaml_value(manifest)?;
    Ok(format!("{pod_name}-{container_name}"))
}

fn parse_yaml_value_from_file(filepath: &Path) -> Result<yaml_serde::Value, AppError> {
    let contents = fs::read_to_string(filepath)
        .map_err(|e| AppError::Io(format!("failed to read {}: {e}", filepath.display())))?;
    parse_yaml_value_from_str(&contents)
}

fn parse_yaml_value_from_str(contents: &str) -> Result<yaml_serde::Value, AppError> {
    yaml_serde::from_str::<yaml_serde::Value>(contents).map_err(|e| AppError::Yaml(e.to_string()))
}

fn yaml_mapping_get<'a>(value: &'a yaml_serde::Value, key: &str) -> Option<&'a yaml_serde::Value> {
    value
        .as_mapping()?
        .get(yaml_serde::Value::String(String::from(key)))
}

/// Recursively traverse the YAML structure to find all values of "image" keys that are strings, and collect them into the provided set.
fn collect_yaml_images(value: &yaml_serde::Value, images: &mut BTreeSet<String>) {
    match value {
        yaml_serde::Value::Mapping(mapping) => {
            for (key, value) in mapping {
                if matches!(key, yaml_serde::Value::String(s) if s == "image")
                    && matches!(value, yaml_serde::Value::String(_))
                    && let yaml_serde::Value::String(image) = value
                {
                    images.insert(image.clone());
                }
                collect_yaml_images(value, images);
            }
        }
        yaml_serde::Value::Sequence(sequence) => {
            for value in sequence {
                collect_yaml_images(value, images);
            }
        }
        _ => {}
    }
}

pub fn execute_command(command: CommandSpec, deps: &AppDeps<'_>) -> Result<AppOutput, AppError> {
    execute_command_with_options(command, deps, ExecOptions::default())
}

pub fn execute_command_with_options(
    command: CommandSpec,
    deps: &AppDeps<'_>,
    options: ExecOptions,
) -> Result<AppOutput, AppError> {
    match command {
        CommandSpec::Validate { filepath, .. } => validate_command(&filepath, deps),
        CommandSpec::Render { filepath, .. } => render_command(&filepath, deps),
        CommandSpec::Images => {
            let config = load_config_with_options(deps.raster, &options)?;
            images_command(&config, deps)
        }
        CommandSpec::Pull { image } => {
            let config = load_config_with_options(deps.raster, &options)?;
            setup_imagestore(&config)?;
            pull_command(&image, &config, deps, &options)
        }
        CommandSpec::Migrate { image } => {
            let config = load_config_with_options(deps.raster, &options)?;
            setup_imagestore(&config)?;
            migrate_command(&image, &config, deps, &options)
        }
        CommandSpec::Rmi { image } => {
            let config = load_config_with_options(deps.raster, &options)?;
            rmi_command(&image, &config, deps, &options)
        }
        CommandSpec::Run {
            // Run is the only command that loads the config on its own to facilitate testing of invalid EDF files without needing a valid config present
            filepath,
            container_cmd,
        } => run_command(&filepath, &container_cmd, deps, &options),
    }
}

fn load_config_with_options(
    raster: &dyn RasterOps,
    options: &ExecOptions,
) -> Result<Config, AppError> {
    let mut config = raster.load_config()?;
    if let Some(parallax_imagestore) = &options.parallax_imagestore {
        config.parallax_imagestore = parallax_imagestore.clone();
    }
    Ok(config)
}

fn validate_command(filepath: &str, deps: &AppDeps<'_>) -> Result<AppOutput, AppError> {
    Ok(match deps.raster.validate(filepath) {
        Ok(_) => AppOutput::success(format!("{filepath} is a valid EDF file")),
        Err(err) => AppOutput::failure(format!("{filepath} is an INVALID EDF file"), err),
    })
}

fn render_command(filepath: &str, deps: &AppDeps<'_>) -> Result<AppOutput, AppError> {
    Ok(match deps.raster.render(filepath) {
        Ok(edf) => AppOutput::success(
            serde_json::to_string_pretty(&edf).unwrap_or_else(|_| String::from("ERROR")),
        ),
        Err(err) => AppOutput::failure("", err),
    })
}

fn images_command(config: &Config, deps: &AppDeps<'_>) -> Result<AppOutput, AppError> {
    let seed_ctx = build_parallax_seed_ctx(config);

    // Use the Parallax seed context to pre-populate the struct with Podman path and additionalimagestore,
    // then add the default graphroot
    let graphroot = deps.runtime.default_graphroot(&seed_ctx)?;
    let ctx = PodmanCtx {
        graphroot: Some(graphroot),
        ..seed_ctx
    };

    setup_imagestore(config)?;

    deps.runtime.images(&ctx)?;
    Ok(AppOutput::success(""))
}

fn pull_command(
    image: &str,
    config: &Config,
    deps: &AppDeps<'_>,
    options: &ExecOptions,
) -> Result<AppOutput, AppError> {
    let ctx = build_pull_ctx(config);
    print_progress_message(&format!("Pulling {image} with Podman..."));
    deps.runtime.pull(image, &ctx, options.verbose)?;
    if !deps.runtime.image_exists(image, &ctx)? {
        return Err(AppError::Runtime(format!(
            "Image {image} is still missing after pull"
        )));
    }

    let mut output = AppOutput::success("");
    merge_output(&mut output, migrate_command(image, config, deps, options)?);
    Ok(output)
}

fn migrate_command(
    image: &str,
    config: &Config,
    deps: &AppDeps<'_>,
    options: &ExecOptions,
) -> Result<AppOutput, AppError> {
    let seed_ctx = build_parallax_seed_ctx(config);

    // We need to find and explicitly state the graphroot because it needs to be passed to Parallax under the hood.
    // Not necessary on Podman commands, because Podman resolves the graphroot location on its own.
    let graphroot = deps.runtime.default_graphroot(&seed_ctx)?;
    let ctx = PodmanCtx {
        graphroot: Some(graphroot),
        ..seed_ctx
    };
    let parallax_path = PathBuf::from(&config.parallax_path);

    print_progress_message(&format!("Migrating {image} with Parallax..."));
    deps.runtime
        .parallax_migrate(&parallax_path, &ctx, image, options.verbose)?;
    if !deps.runtime.parallax_exist(&parallax_path, &ctx, image)? {
        return Err(AppError::Runtime(format!(
            "Image {image} is still missing after migration"
        )));
    }

    Ok(AppOutput::success(""))
}

fn rmi_command(
    image: &str,
    config: &Config,
    deps: &AppDeps<'_>,
    options: &ExecOptions,
) -> Result<AppOutput, AppError> {
    let seed_ctx = build_parallax_seed_ctx(config);

    // We need to find and explicitly state the graphroot because it needs to be passed to Parallax under the hood.
    // Not necessary on Podman commands, because Podman resolves the graphroot location on its own.
    let graphroot = deps.runtime.default_graphroot(&seed_ctx)?;
    let ctx = PodmanCtx {
        graphroot: Some(graphroot),
        ..seed_ctx
    };
    let parallax_path = PathBuf::from(&config.parallax_path);
    setup_imagestore(config)?;

    deps.runtime
        .parallax_rmi(&parallax_path, &ctx, image, options.verbose)?;
    Ok(AppOutput::success(""))
}

fn run_command(
    filepath: &str,
    container_cmd: &[String],
    deps: &AppDeps<'_>,
    options: &ExecOptions,
) -> Result<AppOutput, AppError> {
    match deps.raster.render(filepath) {
        Ok(edf) => {
            // Loading config in each branch is a small duplication,
            // but allows to integration test invalid EDF cases without needing a valid config present
            let config = load_config_with_options(deps.raster, options)?;
            setup_imagestore(&config)?;
            run_edf_command(&edf, container_cmd, &config, deps, options)
        }
        Err(_) => {
            let contents = fs::read_to_string(filepath)
                .map_err(|e| AppError::Io(format!("failed to read {filepath}: {e}")))?;
            yaml_serde::from_str::<yaml_serde::Value>(&contents).map_err(|_| {
                AppError::UnsupportedInput(format!("{filepath} is not valid EDF nor YAML"))
            })?;

            let config = load_config_with_options(deps.raster, options)?;
            setup_imagestore(&config)?;
            run_yaml_command(filepath, container_cmd, &config, deps, options)
        }
    }
}

fn run_edf_command(
    edf: &EDF,
    container_cmd: &[String],
    config: &Config,
    deps: &AppDeps<'_>,
    options: &ExecOptions,
) -> Result<AppOutput, AppError> {
    let run_id = Uuid::new_v4();
    let user = deps.user.current_user()?;
    let plx_ctx = build_parallax_seed_ctx(config);
    let run_ctx = build_run_ctx(config, &user, &run_id);
    let mut output = AppOutput::success("");

    let parallax_path = PathBuf::from(&config.parallax_path);

    if !deps
        .runtime
        .parallax_exist(&parallax_path, &plx_ctx, &edf.image)?
    {
        merge_output(
            &mut output,
            pull_command(&edf.image, config, deps, options)?,
        );
    }

    let container_name = format!("sarusctl-{}", &run_id.simple().to_string()[..12]);
    let c_ctx = ContainerCtx {
        name: container_name,
        interactive: io::stdin().is_terminal(),
        tty: io::stdin().is_terminal() && io::stdout().is_terminal(),
        detach: false,
        auto_remove: false,
        set_env: true,
        pidfile: None,
        user: Some(user.uid.to_string()),
    };

    let run_result = deps
        .runtime
        .run_from_edf(edf, &run_ctx, &c_ctx, container_cmd);
    let container_cleanup_result = deps.runtime.cleanup_container(&c_ctx.name, &run_ctx);
    let cleanup_warning = finalize_podman_cleanup(&run_ctx, &container_cleanup_result);

    // Append warning to error in case of run failure
    output.return_code = match run_result {
        Ok(return_code) => return_code,
        Err(err) => {
            return Err(match cleanup_warning {
                Some(warning) => combine_error_with_warning(err, warning),
                None => err,
            });
        }
    };
    // Append warning to output in case of run success
    if let Some(warning) = cleanup_warning {
        append_warning(&mut output, warning);
    }
    Ok(output)
}

fn run_yaml_command(
    filepath: &str,
    container_cmd: &[String],
    config: &Config,
    deps: &AppDeps<'_>,
    options: &ExecOptions,
) -> Result<AppOutput, AppError> {
    let user = deps.user.current_user()?;
    let plx_ctx = build_parallax_seed_ctx(config);
    let mut run_ctx = build_run_ctx(config, &user, &Uuid::new_v4());
    // Podman kube play doesn't seem to honor all settings from a module
    // so to avoid confusion we disable the module for the time being.
    // The settings from the module have to be specified in the k8s yaml, if desired
    run_ctx.module = None;

    let manifest = parse_yaml_value_from_file(Path::new(filepath))?;

    let images = extract_images_from_yaml_value(&manifest)?;
    let mut output = AppOutput::success("");

    let parallax_path = PathBuf::from(&config.parallax_path);

    for image in images {
        if !deps
            .runtime
            .parallax_exist(&parallax_path, &plx_ctx, &image)?
        {
            merge_output(&mut output, pull_command(&image, config, deps, options)?);
        }
    }

    let join_container = get_join_container_from_yaml_manifest(&manifest)?;

    let play_result = deps.runtime.kube_play(filepath, &run_ctx);
    let exec_result = play_result.as_ref().ok().map(|_| {
        deps.runtime
            .exec_interactive(&join_container, &run_ctx, container_cmd)
    });
    let down_result = deps.runtime.kube_down(filepath, true, &run_ctx);
    let cleanup_warning = finalize_podman_cleanup(&run_ctx, &down_result);

    // Append warning to error in case of run failure
    if let Err(err) = play_result {
        return Err(match cleanup_warning {
            Some(warning) => combine_error_with_warning(err, warning),
            None => err,
        });
    }
    output.return_code = match exec_result.expect("exec result missing after successful kube play")
    {
        Ok(return_code) => return_code,
        Err(err) => {
            return Err(match cleanup_warning {
                Some(warning) => combine_error_with_warning(err, warning),
                None => err,
            });
        }
    };
    if let Err(err) = down_result {
        return Err(match cleanup_warning {
            Some(warning) => combine_error_with_warning(err, warning),
            None => err,
        });
    }
    // Append warning to output in case of run success
    if let Some(warning) = cleanup_warning {
        append_warning(&mut output, warning);
    }
    Ok(output)
}

fn merge_output(base: &mut AppOutput, extra: AppOutput) {
    if !extra.stdout.is_empty() {
        if !base.stdout.is_empty() {
            base.stdout.push('\n');
        }
        base.stdout.push_str(&extra.stdout);
    }

    if !extra.stderr.is_empty() {
        if !base.stderr.is_empty() {
            base.stderr.push('\n');
        }
        base.stderr.push_str(&extra.stderr);
    }

    if extra.return_code != 0 {
        base.return_code = extra.return_code;
    }
}

fn print_progress_message(message: &str) {
    let _ = writeln!(io::stderr(), "{message}");
}

fn append_warning(output: &mut AppOutput, warning: String) {
    if !output.stderr.is_empty() {
        output.stderr.push('\n');
    }
    output.stderr.push_str(&warning);
}

fn combine_error_with_warning(err: AppError, warning: String) -> AppError {
    AppError::Runtime(format!("{err}\n{warning}"))
}

fn finalize_podman_cleanup(
    run_ctx: &PodmanCtx,
    podman_cleanup_result: &Result<(), AppError>,
) -> Option<String> {
    match podman_cleanup_result {
        Ok(()) => cleanup_podman_rootdirs(run_ctx),
        Err(err) => Some(format!(
            "Warning: Podman rootdirs retained because workload cleanup failed: {err}"
        )),
    }
}

fn setup_imagestore(config: &Config) -> Result<(), AppError> {
    let imagestore = &config.parallax_imagestore;
    let imagestore_pb = PathBuf::from(&imagestore);

    let opt_ec =
        hook_run(config, "parallax_imagestore_create", vec![&imagestore]).map_err(|e| {
            AppError::Io(format!(
                "Failed to run parallax_imagestore_create hook: {e}"
            ))
        })?;

    match opt_ec {
        Some(ec) => log_hook_ec(ec, "parallax_imagestore_create hook")?,
        None => {}
    }

    if !fs::exists(&imagestore_pb).map_err(|e| {
        AppError::Io(format!(
            "Failed to check for existence of Parallax imagestore directory: {e}"
        ))
    })? {
        fs::create_dir_all(&imagestore_pb).map_err(|e| {
            AppError::Io(format!(
                "Failed to create Parallax imagestore directory: {e}"
            ))
        })?;
    }
    Ok(())
}

fn log_hook_ec(ec: ExecutedCommand, prefix: &str) -> Result<(), AppError> {
    let rc = match ec.output.status.code() {
        Some(ok) => format!("{ok}"),
        None => String::from("UNKNOWN"),
    };

    let mut stdout = match String::from_utf8(ec.output.stdout) {
        Ok(ok) => ok,
        Err(_) => String::from(""),
    };
    if stdout.ends_with("\n") {
        stdout.pop();
    };

    let mut stderr = match String::from_utf8(ec.output.stderr) {
        Ok(ok) => ok,
        Err(_) => String::from(""),
    };
    if stderr.ends_with("\n") {
        stderr.pop();
    };

    if stderr != "" {
        let lines = stderr.split("\n");
        for line in lines {
            eprintln!("{}", line);
        }
    }

    if stdout != "" {
        let lines = stdout.split("\n");
        for line in lines {
            eprintln!("{}", line);
        }
    }

    if rc != "0" {
        if rc == "UNKNOWN" {
            return Err(AppError::Io(format!("{prefix} exited by signal")));
        } else {
            return Err(AppError::Io(format!("{prefix} exit code: {rc}")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::cell::RefCell;
    use std::collections::{HashMap, VecDeque};
    use std::ffi::OsStr;
    use tempfile::tempdir;

    fn sample_config() -> Config {
        Config {
            parallax_imagestore: std::env::temp_dir()
                .join("sarusctl-test-parallax-store")
                .to_string_lossy()
                .into_owned(),
            parallax_mount_program: String::from("/usr/local/bin/parallax_mount_program"),
            parallax_mp_uid: 1234,
            parallax_mp_gid: 4321,
            parallax_mp_logfile: String::from("/tmp/parallax-1234/mount_program.log"),
            parallax_path: String::from("/usr/local/bin/parallax"),
            podman_module: String::from("hpc"),
            podman_path: String::from("/usr/bin/podman"),
            ..Config::default()
        }
    }

    fn sample_edf(image: &str) -> EDF {
        EDF {
            annotations: HashMap::new(),
            devices: Vec::new(),
            entrypoint: true,
            env: HashMap::new(),
            image: image.to_string(),
            mounts: Vec::new(),
            workdir: String::new(),
            writable: true,
        }
    }

    struct FakeRasterOps {
        config: Result<Config, AppError>,
        config_paths: RefCell<Vec<PathBuf>>,
        validate_results: HashMap<String, Result<(), String>>,
        render_results: HashMap<String, Result<EDF, String>>,
    }

    impl FakeRasterOps {
        fn new(config: Config) -> Self {
            Self {
                config: Ok(config),
                config_paths: RefCell::new(Vec::new()),
                validate_results: HashMap::new(),
                render_results: HashMap::new(),
            }
        }

        fn config_paths(&self) -> Vec<PathBuf> {
            self.config_paths.borrow().clone()
        }
    }

    impl RasterOps for FakeRasterOps {
        fn load_config(&self) -> Result<Config, AppError> {
            self.config.clone()
        }

        fn load_config_path(&self, path: &Path) -> Result<Config, AppError> {
            self.config_paths.borrow_mut().push(path.to_path_buf());
            self.config.clone()
        }

        fn validate(&self, path: &str) -> Result<(), String> {
            self.validate_results.get(path).cloned().unwrap_or(Ok(()))
        }

        fn render(&self, path: &str) -> Result<EDF, String> {
            self.render_results
                .get(path)
                .cloned()
                .unwrap_or_else(|| Err(String::from("missing render result")))
        }
    }

    struct FakeUserContext {
        user: CurrentUser,
    }

    impl UserContext for FakeUserContext {
        fn current_user(&self) -> Result<CurrentUser, AppError> {
            Ok(self.user.clone())
        }
    }

    struct FakeContainerRuntime {
        calls: RefCell<Vec<String>>,
        create_run_rootdirs: bool,
        created_run_rootdirs: RefCell<Vec<PathBuf>>,
        pull_verbose: RefCell<Vec<bool>>,
        migrate_verbose: RefCell<Vec<bool>>,
        rmi_verbose: RefCell<Vec<bool>>,
        graphroot: Result<PathBuf, AppError>,
        image_exists: RefCell<HashMap<String, VecDeque<bool>>>,
        parallax_exist: RefCell<HashMap<String, VecDeque<bool>>>,
        pull_results: RefCell<HashMap<String, Result<(), AppError>>>,
        migrate_results: RefCell<HashMap<String, Result<(), AppError>>>,
        rmi_results: RefCell<HashMap<String, Result<(), AppError>>>,
        run_result: Result<i32, AppError>,
        kube_play_result: Result<(), AppError>,
        kube_down_result: Result<(), AppError>,
        cleanup_container_result: Result<(), AppError>,
    }

    impl FakeContainerRuntime {
        fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                create_run_rootdirs: false,
                created_run_rootdirs: RefCell::new(Vec::new()),
                pull_verbose: RefCell::new(Vec::new()),
                migrate_verbose: RefCell::new(Vec::new()),
                rmi_verbose: RefCell::new(Vec::new()),
                graphroot: Ok(PathBuf::from("/graphroot")),
                image_exists: RefCell::new(HashMap::new()),
                pull_results: RefCell::new(HashMap::new()),
                migrate_results: RefCell::new(HashMap::new()),
                rmi_results: RefCell::new(HashMap::new()),
                parallax_exist: RefCell::new(HashMap::new()),
                run_result: Ok(0),
                kube_play_result: Ok(()),
                kube_down_result: Ok(()),
                cleanup_container_result: Ok(()),
            }
        }

        fn with_run_rootdir_creation(mut self) -> Self {
            self.create_run_rootdirs = true;
            self
        }

        fn create_and_record_run_rootdirs(&self, run_ctx: &PodmanCtx) {
            if !self.create_run_rootdirs {
                return;
            }

            let graphroot = run_ctx.graphroot.as_ref().expect("missing graphroot");
            let runroot = run_ctx.runroot.as_ref().expect("missing runroot");
            let roots_base = graphroot.parent().expect("graphroot has no parent");
            assert_eq!(runroot.parent(), Some(roots_base));

            fs::create_dir_all(graphroot).expect("failed to create test graphroot");
            fs::create_dir_all(runroot).expect("failed to create test runroot");
            self.created_run_rootdirs
                .borrow_mut()
                .push(roots_base.to_path_buf());
        }

        fn created_run_rootdirs(&self) -> Vec<PathBuf> {
            self.created_run_rootdirs.borrow().clone()
        }

        fn push_image_exists(&self, image: &str, values: Vec<bool>) {
            self.image_exists
                .borrow_mut()
                .insert(image.to_string(), VecDeque::from(values));
        }

        fn push_parallax_exist(&self, image: &str, values: Vec<bool>) {
            self.parallax_exist
                .borrow_mut()
                .insert(image.to_string(), VecDeque::from(values));
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }

        fn pull_verbose(&self) -> Vec<bool> {
            self.pull_verbose.borrow().clone()
        }

        fn migrate_verbose(&self) -> Vec<bool> {
            self.migrate_verbose.borrow().clone()
        }

        fn rmi_verbose(&self) -> Vec<bool> {
            self.rmi_verbose.borrow().clone()
        }
    }

    impl ContainerRuntime for FakeContainerRuntime {
        fn default_graphroot(&self, _ctx: &PodmanCtx) -> Result<PathBuf, AppError> {
            self.calls
                .borrow_mut()
                .push(String::from("default_graphroot"));
            self.graphroot.clone()
        }

        fn images(&self, _ctx: &PodmanCtx) -> Result<(), AppError> {
            self.calls.borrow_mut().push(String::from("images"));
            Ok(())
        }

        fn pull(&self, image: &str, _ctx: &PodmanCtx, verbose: bool) -> Result<(), AppError> {
            self.calls.borrow_mut().push(format!("pull:{image}"));
            self.pull_verbose.borrow_mut().push(verbose);
            self.pull_results
                .borrow_mut()
                .remove(image)
                .unwrap_or(Ok(()))
        }

        fn image_exists(&self, image: &str, _ctx: &PodmanCtx) -> Result<bool, AppError> {
            self.calls
                .borrow_mut()
                .push(format!("image_exists:{image}"));
            let mut states = self.image_exists.borrow_mut();
            let queue = states.entry(image.to_string()).or_default();
            Ok(queue.pop_front().unwrap_or(false))
        }

        fn parallax_exist(
            &self,
            _parallax_path: &Path,
            _ctx: &PodmanCtx,
            image: &str,
        ) -> Result<bool, AppError> {
            self.calls
                .borrow_mut()
                .push(format!("parallax_exist:{image}"));
            let mut states = self.parallax_exist.borrow_mut();
            let queue = states.entry(image.to_string()).or_default();
            Ok(queue.pop_front().unwrap_or(false))
        }

        fn parallax_migrate(
            &self,
            _parallax_path: &Path,
            _ctx: &PodmanCtx,
            image: &str,
            verbose: bool,
        ) -> Result<(), AppError> {
            self.calls.borrow_mut().push(format!("migrate:{image}"));
            self.migrate_verbose.borrow_mut().push(verbose);
            self.migrate_results
                .borrow_mut()
                .remove(image)
                .unwrap_or(Ok(()))
        }

        fn parallax_rmi(
            &self,
            _parallax_path: &Path,
            _ctx: &PodmanCtx,
            image: &str,
            verbose: bool,
        ) -> Result<(), AppError> {
            self.calls.borrow_mut().push(format!("rmi:{image}"));
            self.rmi_verbose.borrow_mut().push(verbose);
            self.rmi_results
                .borrow_mut()
                .remove(image)
                .unwrap_or(Ok(()))
        }

        fn run_from_edf(
            &self,
            edf: &EDF,
            run_ctx: &PodmanCtx,
            _container_ctx: &ContainerCtx,
            container_cmd: &[String],
        ) -> Result<i32, AppError> {
            self.create_and_record_run_rootdirs(run_ctx);
            self.calls
                .borrow_mut()
                .push(format!("run:{}:{container_cmd:?}", edf.image));
            self.run_result.clone()
        }

        fn exec_interactive(
            &self,
            container_name: &str,
            _podman_ctx: &PodmanCtx,
            container_cmd: &[String],
        ) -> Result<i32, AppError> {
            self.calls.borrow_mut().push(format!(
                "exec_interactive:{container_name}:{container_cmd:?}"
            ));
            self.run_result.clone()
        }

        fn kube_play(&self, filepath: &str, run_ctx: &PodmanCtx) -> Result<(), AppError> {
            self.create_and_record_run_rootdirs(run_ctx);
            self.calls
                .borrow_mut()
                .push(format!("kube_play:{filepath}"));
            self.kube_play_result.clone()
        }

        fn kube_down(
            &self,
            filepath: &str,
            force: bool,
            _run_ctx: &PodmanCtx,
        ) -> Result<(), AppError> {
            self.calls
                .borrow_mut()
                .push(format!("kube_down:{filepath}?force={force}"));
            self.kube_down_result.clone()
        }

        fn cleanup_container(
            &self,
            _container_name: &str,
            _run_ctx: &PodmanCtx,
        ) -> Result<(), AppError> {
            self.calls
                .borrow_mut()
                .push(String::from("cleanup_container"));
            self.cleanup_container_result.clone()
        }
    }

    fn mock_deps<'a>(
        raster: &'a dyn RasterOps,
        runtime: &'a dyn ContainerRuntime,
        user: &'a dyn UserContext,
    ) -> AppDeps<'a> {
        AppDeps {
            raster,
            runtime,
            user,
        }
    }

    #[test]
    fn load_config_from_candidates_uses_first_existing_directory() {
        let temp = tempdir().unwrap();
        let system_dir = temp.path().join("system");
        let user_dir = temp.path().join("user");
        fs::create_dir_all(&system_dir).unwrap();
        fs::create_dir_all(&user_dir).unwrap();

        let raster = FakeRasterOps::new(sample_config());
        let config = load_config_from_candidates(&raster, &[system_dir.clone(), user_dir]).unwrap();

        assert_eq!(config.parallax_path, sample_config().parallax_path);
        assert_eq!(raster.config_paths(), vec![system_dir]);
    }

    #[test]
    fn load_config_from_candidates_falls_back_when_system_directory_is_missing() {
        let temp = tempdir().unwrap();
        let system_dir = temp.path().join("system");
        let user_dir = temp.path().join("user");
        fs::create_dir_all(&user_dir).unwrap();

        let raster = FakeRasterOps::new(sample_config());
        let config = load_config_from_candidates(&raster, &[system_dir, user_dir.clone()]).unwrap();

        assert_eq!(config.parallax_path, sample_config().parallax_path);
        assert_eq!(raster.config_paths(), vec![user_dir]);
    }

    #[test]
    fn load_config_from_candidates_does_not_fallback_after_existing_directory_errors() {
        let temp = tempdir().unwrap();
        let system_dir = temp.path().join("system");
        let user_dir = temp.path().join("user");
        fs::create_dir_all(&system_dir).unwrap();
        fs::create_dir_all(&user_dir).unwrap();

        let mut raster = FakeRasterOps::new(sample_config());
        raster.config = Err(AppError::ConfigLoad(String::from("invalid config")));

        let err = match load_config_from_candidates(&raster, &[system_dir.clone(), user_dir]) {
            Ok(_) => panic!("expected config load to fail"),
            Err(err) => err,
        };

        assert_eq!(err, AppError::ConfigLoad(String::from("invalid config")));
        assert_eq!(raster.config_paths(), vec![system_dir]);
    }

    #[test]
    fn load_config_from_candidates_reports_both_missing_locations() {
        let temp = tempdir().unwrap();
        let system_dir = temp.path().join("system");
        let user_dir = temp.path().join("user");
        let raster = FakeRasterOps::new(sample_config());

        let err =
            match load_config_from_candidates(&raster, &[system_dir.clone(), user_dir.clone()]) {
                Ok(_) => panic!("expected config load to fail"),
                Err(err) => err,
            };

        assert_eq!(
            err,
            AppError::ConfigLoad(format!(
                "Cannot find config files in {} or {}",
                system_dir.display(),
                user_dir.display()
            ))
        );
        assert!(raster.config_paths().is_empty());
    }

    #[test]
    fn load_config_with_options_overrides_parallax_imagestore() {
        let raster = FakeRasterOps::new(sample_config());
        let override_path = String::from("/overridden/parallax/imagestore");
        let options = ExecOptions {
            parallax_imagestore: Some(override_path.clone()),
            ..ExecOptions::default()
        };

        let config = load_config_with_options(&raster, &options).unwrap();

        assert_eq!(config.parallax_imagestore, override_path);
    }

    #[test]
    fn parse_yaml_value_from_str_rejects_invalid_yaml() {
        let err = parse_yaml_value_from_str("apiVersion: [").unwrap_err();
        assert!(matches!(err, AppError::Yaml(_)));
    }

    #[test]
    fn extract_images_from_yaml_str_deduplicates_nested_images() {
        let yaml = r#"
apiVersion: v1
kind: Pod
spec:
  initContainers:
    - image: alpine:3.22
  containers:
    - image: ubuntu:24.04
    - image: alpine:3.22
  sidecars:
    image: busybox:1.36
"#;

        let manifest = parse_yaml_value_from_str(yaml).unwrap();
        let images = extract_images_from_yaml_value(&manifest).unwrap();
        assert_eq!(
            images,
            vec![
                String::from("alpine:3.22"),
                String::from("busybox:1.36"),
                String::from("ubuntu:24.04")
            ]
        );
    }

    #[test]
    fn extract_images_from_yaml_str_ignores_non_string_values() {
        let yaml = r#"
spec:
  containers:
    - image:
        name: alpine
    - image: alpine:3.22
  images:
    - should-not-count
"#;

        let manifest = parse_yaml_value_from_str(yaml).unwrap();
        let images = extract_images_from_yaml_value(&manifest).unwrap();
        assert_eq!(images, vec![String::from("alpine:3.22")]);
    }

    #[test]
    fn extract_join_container_from_yaml_str_returns_single_unlabeled_container() {
        let yaml = r#"
apiVersion: v1
kind: Pod
metadata:
  name: training-pod
spec:
  containers:
    - name: app
      image: alpine:3.22
"#;

        let manifest = parse_yaml_value_from_str(yaml).unwrap();
        let join_container = extract_join_container_from_yaml_value(&manifest).unwrap();
        assert_eq!(join_container, String::from("app"));
    }

    #[test]
    fn extract_join_container_from_yaml_str_returns_labeled_container() {
        let yaml = r#"
apiVersion: v1
kind: Pod
metadata:
  name: training-pod
spec:
  containers:
    - name: app
      image: alpine:3.22
    - name: sidecar
      image: ubuntu:24.04
      labels:
        run.sarus.join: "true"
"#;

        let manifest = parse_yaml_value_from_str(yaml).unwrap();
        let join_container = extract_join_container_from_yaml_value(&manifest).unwrap();
        assert_eq!(join_container, String::from("sidecar"));
    }

    #[test]
    fn extract_join_container_from_yaml_str_rejects_multiple_labeled_containers() {
        let yaml = r#"
apiVersion: v1
kind: Pod
metadata:
  name: training-pod
spec:
  containers:
    - name: app
      image: alpine:3.22
      labels:
        run.sarus.join: "true"
    - name: sidecar
      image: ubuntu:24.04
      labels:
        run.sarus.join: "true"
"#;

        let manifest = parse_yaml_value_from_str(yaml).unwrap();
        let err = extract_join_container_from_yaml_value(&manifest).unwrap_err();
        assert_eq!(
            err,
            AppError::Yaml(String::from(
                "YAML manifest must not label more than one container with run.sarus.join=\"true\"",
            ))
        );
    }

    #[test]
    fn extract_join_container_from_yaml_str_rejects_unlabeled_multi_container_pod() {
        let yaml = r#"
apiVersion: v1
kind: Pod
spec:
  containers:
    - name: app
      image: alpine:3.22
    - name: sidecar
      image: ubuntu:24.04
"#;

        let manifest = parse_yaml_value_from_str(yaml).unwrap();
        let err = extract_join_container_from_yaml_value(&manifest).unwrap_err();
        assert_eq!(
            err,
            AppError::Yaml(String::from(
                "YAML manifest must label exactly one container with run.sarus.join=\"true\" when spec.containers has multiple containers",
            ))
        );
    }

    #[test]
    fn extract_join_container_from_yaml_str_ignores_init_containers_and_nested_templates() {
        let yaml = r#"
apiVersion: v1
kind: Pod
spec:
  initContainers:
    - name: init
      image: busybox:1.36
      labels:
        run.sarus.join: "true"
  containers:
    - name: app
      image: alpine:3.22
      labels:
        run.sarus.join: "true"
    - name: sidecar
      image: ubuntu:24.04
  template:
    spec:
      containers:
        - name: nested
          image: debian:bookworm
          labels:
            run.sarus.join: "true"
"#;

        let manifest = parse_yaml_value_from_str(yaml).unwrap();
        let join_container = extract_join_container_from_yaml_value(&manifest).unwrap();
        assert_eq!(join_container, String::from("app"));
    }

    #[test]
    fn extract_pod_name_from_yaml_str_returns_metadata_name() {
        let yaml = r#"
apiVersion: v1
kind: Pod
metadata:
  name: training-pod
spec:
  containers:
    - name: app
      image: alpine:3.22
"#;

        let manifest = parse_yaml_value_from_str(yaml).unwrap();
        let pod_name = extract_pod_name_from_yaml_value(&manifest).unwrap();
        assert_eq!(pod_name, String::from("training-pod"));
    }

    #[test]
    fn extract_pod_name_from_yaml_str_requires_metadata_name() {
        let yaml = r#"
apiVersion: v1
kind: Pod
spec:
  containers:
    - name: app
      image: alpine:3.22
"#;

        let manifest = parse_yaml_value_from_str(yaml).unwrap();
        let err = extract_pod_name_from_yaml_value(&manifest).unwrap_err();
        assert_eq!(
            err,
            AppError::Yaml(String::from(
                "YAML manifest must define metadata.name as a string",
            ))
        );
    }

    #[test]
    fn format_output_keeps_text_streams_separate() {
        let out = AppOutput {
            stdout: String::from("ok"),
            stderr: String::from("warn"),
            return_code: 1,
        };

        let formatted = format_output(FormatOutput::Text, &out);
        assert_eq!(
            formatted,
            FormattedOutput {
                stdout: String::from("ok"),
                stderr: String::from("warn"),
            }
        );
    }

    #[test]
    fn format_output_json_serializes_app_output() {
        let out = AppOutput {
            stdout: String::from("ok"),
            stderr: String::from("warn"),
            return_code: 1,
        };

        let formatted = format_output(FormatOutput::Json, &out);
        assert!(formatted.stdout.contains("\"stdout\": \"ok\""));
        assert!(formatted.stdout.contains("\"stderr\": \"warn\""));
        assert!(formatted.stdout.contains("\"return_code\": 1"));
        assert!(formatted.stderr.is_empty());
    }

    #[test]
    fn build_context_helpers_match_expected_fields() {
        let config = sample_config();
        let run_id = Uuid::parse_str("a1a2a3a4b1b2c1c2d1d2d3d4d5d6d7d8").unwrap();
        let user = CurrentUser {
            uid: 1234,
            gid: 4321,
        };
        let parallax_imagestore = std::env::temp_dir().join("sarusctl-test-parallax-store");

        let pull = build_pull_ctx(&config);
        assert_eq!(pull.podman_path, PathBuf::from("/usr/bin/podman"));

        let seed = build_parallax_seed_ctx(&config);
        assert_eq!(seed.podman_path, PathBuf::from("/usr/bin/podman"));
        assert_eq!(seed.ro_store, Some(parallax_imagestore.clone()));

        let ro = build_readonly_ctx(&config);
        assert_eq!(ro.podman_path, PathBuf::from("/usr/bin/podman"));
        assert_eq!(ro.graphroot, Some(parallax_imagestore.clone()));

        let run = build_run_ctx(&config, &user, &run_id);
        assert_eq!(run.podman_path, PathBuf::from("/usr/bin/podman"));
        assert_eq!(run.module, Some(String::from("hpc")));
        assert_eq!(
            run.graphroot,
            Some(PathBuf::from(
                "/dev/shm/sarusctl-1234-a1a2a3a4b1b2/graphroot"
            ))
        );
        assert_eq!(
            run.runroot,
            Some(PathBuf::from("/dev/shm/sarusctl-1234-a1a2a3a4b1b2/runroot"))
        );
        assert_eq!(seed.ro_store, Some(parallax_imagestore));
        let env = run.podman_env.expect("missing env");
        assert_eq!(env.get(OsStr::new("PARALLAX_MP_UID")).unwrap(), "1234");
        assert_eq!(env.get(OsStr::new("PARALLAX_MP_GID")).unwrap(), "4321");
        assert_eq!(
            env.get(OsStr::new("PARALLAX_MP_LOGFILE")).unwrap(),
            "/tmp/parallax-1234/mount_program.log"
        );
    }

    #[test]
    fn validate_success_returns_zero() {
        let raster = FakeRasterOps::new(sample_config());
        let runtime = FakeContainerRuntime::new();
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };

        let output = execute_command(
            CommandSpec::Validate {
                filepath: String::from("valid.edf"),
                output: FormatOutput::Text,
            },
            &mock_deps(&raster, &runtime, &user),
        )
        .unwrap();

        assert_eq!(output.return_code, 0);
        assert_eq!(output.stdout, "valid.edf is a valid EDF file");
    }

    #[test]
    fn validate_failure_returns_invalid_message() {
        let mut raster = FakeRasterOps::new(sample_config());
        raster.validate_results.insert(
            String::from("invalid.edf"),
            Err(String::from("missing image specification")),
        );
        let runtime = FakeContainerRuntime::new();
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };

        let output = execute_command(
            CommandSpec::Validate {
                filepath: String::from("invalid.edf"),
                output: FormatOutput::Text,
            },
            &mock_deps(&raster, &runtime, &user),
        )
        .unwrap();

        assert_eq!(output.return_code, 1);
        assert_eq!(output.stdout, "invalid.edf is an INVALID EDF file");
        assert_eq!(output.stderr, "missing image specification");
    }

    #[test]
    fn render_success_returns_json() {
        let mut raster = FakeRasterOps::new(sample_config());
        raster
            .render_results
            .insert(String::from("valid.edf"), Ok(sample_edf("alpine:3.22")));
        let runtime = FakeContainerRuntime::new();
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };

        let output = execute_command(
            CommandSpec::Render {
                filepath: String::from("valid.edf"),
                output: FormatOutput::Text,
            },
            &mock_deps(&raster, &runtime, &user),
        )
        .unwrap();

        assert_eq!(output.return_code, 0);
        assert!(output.stdout.contains("\"image\": \"alpine:3.22\""));
    }

    #[test]
    fn images_creates_missing_imagestore_and_calls_runtime() {
        let temp = tempdir().unwrap();
        let imagestore = temp.path().join("imagestore");
        let mut config = sample_config();
        config.parallax_imagestore = imagestore.to_string_lossy().into_owned();

        let raster = FakeRasterOps::new(config);
        let runtime = FakeContainerRuntime::new();
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };

        let output =
            execute_command(CommandSpec::Images, &mock_deps(&raster, &runtime, &user)).unwrap();

        assert_eq!(output.return_code, 0);
        assert!(imagestore.exists());
        assert_eq!(
            runtime.calls(),
            vec![String::from("default_graphroot"), String::from("images")]
        );
    }

    #[test]
    fn pull_success_runs_migrate_after_pull() {
        let config = sample_config();
        let raster = FakeRasterOps::new(config);
        let runtime = FakeContainerRuntime::new();
        runtime.push_image_exists("alpine:3.22", vec![true]);
        runtime.push_parallax_exist("alpine:3.22", vec![true]);
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };

        let output = execute_command(
            CommandSpec::Pull {
                image: String::from("alpine:3.22"),
            },
            &mock_deps(&raster, &runtime, &user),
        )
        .unwrap();

        assert_eq!(output, AppOutput::success(""));
        assert_eq!(runtime.pull_verbose(), vec![false]);
        assert_eq!(runtime.migrate_verbose(), vec![false]);
        assert_eq!(
            runtime.calls(),
            vec![
                String::from("pull:alpine:3.22"),
                String::from("image_exists:alpine:3.22"),
                String::from("default_graphroot"),
                String::from("migrate:alpine:3.22"),
                String::from("parallax_exist:alpine:3.22")
            ]
        );
    }

    #[test]
    fn migrate_success_runs_migration_steps() {
        let config = sample_config();
        let raster = FakeRasterOps::new(config);
        let runtime = FakeContainerRuntime::new();
        runtime.push_parallax_exist("alpine:3.22", vec![true]);
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };

        let output = execute_command(
            CommandSpec::Migrate {
                image: String::from("alpine:3.22"),
            },
            &mock_deps(&raster, &runtime, &user),
        )
        .unwrap();

        assert_eq!(output, AppOutput::success(""));
        assert_eq!(runtime.migrate_verbose(), vec![false]);
        assert_eq!(
            runtime.calls(),
            vec![
                String::from("default_graphroot"),
                String::from("migrate:alpine:3.22"),
                String::from("parallax_exist:alpine:3.22")
            ]
        );
    }

    #[test]
    fn migrate_failure_is_returned_as_runtime_error() {
        let config = sample_config();
        let raster = FakeRasterOps::new(config);
        let runtime = FakeContainerRuntime::new();
        runtime.migrate_results.borrow_mut().insert(
            String::from("alpine:3.22"),
            Err(AppError::Runtime(String::from("boom"))),
        );
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };

        let err = execute_command(
            CommandSpec::Migrate {
                image: String::from("alpine:3.22"),
            },
            &mock_deps(&raster, &runtime, &user),
        )
        .unwrap_err();

        assert_eq!(err, AppError::Runtime(String::from("boom")));
    }

    #[test]
    fn rmi_success_runs_removal_steps() {
        let config = sample_config();
        let raster = FakeRasterOps::new(config);
        let runtime = FakeContainerRuntime::new();
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };

        let output = execute_command(
            CommandSpec::Rmi {
                image: String::from("alpine:3.22"),
            },
            &mock_deps(&raster, &runtime, &user),
        )
        .unwrap();

        assert_eq!(output, AppOutput::success(""));
        assert_eq!(runtime.rmi_verbose(), vec![false]);
        assert_eq!(
            runtime.calls(),
            vec![
                String::from("default_graphroot"),
                String::from("rmi:alpine:3.22")
            ]
        );
    }

    #[test]
    fn rmi_failure_is_returned_as_runtime_error() {
        let config = sample_config();
        let raster = FakeRasterOps::new(config);
        let runtime = FakeContainerRuntime::new();
        runtime.rmi_results.borrow_mut().insert(
            String::from("alpine:3.22"),
            Err(AppError::Runtime(String::from("boom"))),
        );
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };

        let err = execute_command(
            CommandSpec::Rmi {
                image: String::from("alpine:3.22"),
            },
            &mock_deps(&raster, &runtime, &user),
        )
        .unwrap_err();

        assert_eq!(err, AppError::Runtime(String::from("boom")));
    }

    #[test]
    fn run_edf_skips_pull_when_image_exists() {
        let mut raster = FakeRasterOps::new(sample_config());
        raster
            .render_results
            .insert(String::from("job.edf"), Ok(sample_edf("alpine:3.22")));
        let runtime = FakeContainerRuntime::new();
        runtime.push_parallax_exist("alpine:3.22", vec![true]);
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };

        let output = execute_command(
            CommandSpec::Run {
                filepath: String::from("job.edf"),
                container_cmd: vec![String::from("sh")],
            },
            &mock_deps(&raster, &runtime, &user),
        )
        .unwrap();

        assert_eq!(output.return_code, 0);
        assert_eq!(
            runtime.calls(),
            vec![
                String::from("parallax_exist:alpine:3.22"),
                String::from("run:alpine:3.22:[\"sh\"]"),
                String::from("cleanup_container")
            ]
        );
    }

    #[test]
    fn run_edf_pulls_missing_image_before_run() {
        let mut raster = FakeRasterOps::new(sample_config());
        raster
            .render_results
            .insert(String::from("job.edf"), Ok(sample_edf("alpine:3.22")));
        let runtime = FakeContainerRuntime::new();
        runtime.push_parallax_exist("alpine:3.22", vec![false, true]);
        runtime.push_image_exists("alpine:3.22", vec![true]);
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };

        let output = execute_command(
            CommandSpec::Run {
                filepath: String::from("job.edf"),
                container_cmd: vec![String::from("sh")],
            },
            &mock_deps(&raster, &runtime, &user),
        )
        .unwrap();

        assert_eq!(output, AppOutput::success(""));
        assert_eq!(runtime.pull_verbose(), vec![false]);
        assert_eq!(runtime.migrate_verbose(), vec![false]);
        assert_eq!(
            runtime.calls(),
            vec![
                String::from("parallax_exist:alpine:3.22"),
                String::from("pull:alpine:3.22"),
                String::from("image_exists:alpine:3.22"),
                String::from("default_graphroot"),
                String::from("migrate:alpine:3.22"),
                String::from("parallax_exist:alpine:3.22"),
                String::from("run:alpine:3.22:[\"sh\"]"),
                String::from("cleanup_container")
            ]
        );
    }

    fn unique_test_user() -> CurrentUser {
        CurrentUser {
            uid: Uuid::new_v4().as_u128() as u32,
            gid: 1,
        }
    }

    fn assert_rootdirs_removed(roots_base: &Path) {
        if roots_base.exists() {
            let cleanup_result = fs::remove_dir_all(roots_base);
            panic!(
                "Podman rootdirs were not removed after run: {}; test cleanup result: {cleanup_result:?}",
                roots_base.display()
            );
        }
    }

    #[test]
    fn run_edf_removes_rootdirs_after_run() {
        let mut raster = FakeRasterOps::new(sample_config());
        raster
            .render_results
            .insert(String::from("job.edf"), Ok(sample_edf("alpine:3.22")));
        let runtime = FakeContainerRuntime::new().with_run_rootdir_creation();
        runtime.push_parallax_exist("alpine:3.22", vec![true]);
        let user = unique_test_user();

        let output = execute_command(
            CommandSpec::Run {
                filepath: String::from("job.edf"),
                container_cmd: vec![String::from("sh")],
            },
            &mock_deps(&raster, &runtime, &FakeUserContext { user: user.clone() }),
        )
        .unwrap();

        assert_eq!(output.return_code, 0);
        let rootdirs = runtime.created_run_rootdirs();
        assert_eq!(rootdirs.len(), 1);
        assert_rootdirs_removed(&rootdirs[0]);
    }

    #[test]
    fn run_edf_preserves_exit_code_after_container_cleanup() {
        let mut raster = FakeRasterOps::new(sample_config());
        raster
            .render_results
            .insert(String::from("job.edf"), Ok(sample_edf("alpine:3.22")));
        let mut runtime = FakeContainerRuntime::new();
        runtime.push_parallax_exist("alpine:3.22", vec![true]);
        runtime.run_result = Ok(127);

        let output = execute_command(
            CommandSpec::Run {
                filepath: String::from("job.edf"),
                container_cmd: vec![String::from("missing-command")],
            },
            &mock_deps(
                &raster,
                &runtime,
                &FakeUserContext {
                    user: unique_test_user(),
                },
            ),
        )
        .unwrap();

        assert_eq!(output.return_code, 127);
        assert_eq!(output.stderr, "");
        assert_eq!(
            runtime.calls().last().map(String::as_str),
            Some("cleanup_container")
        );
    }

    #[test]
    fn run_edf_retains_rootdirs_when_container_cleanup_fails() {
        let mut raster = FakeRasterOps::new(sample_config());
        raster
            .render_results
            .insert(String::from("job.edf"), Ok(sample_edf("alpine:3.22")));
        let mut runtime = FakeContainerRuntime::new().with_run_rootdir_creation();
        runtime.push_parallax_exist("alpine:3.22", vec![true]);
        runtime.cleanup_container_result =
            Err(AppError::Runtime(String::from("cleanup still active")));

        let output = execute_command(
            CommandSpec::Run {
                filepath: String::from("job.edf"),
                container_cmd: vec![String::from("true")],
            },
            &mock_deps(
                &raster,
                &runtime,
                &FakeUserContext {
                    user: unique_test_user(),
                },
            ),
        )
        .unwrap();

        assert_eq!(output.return_code, 0);
        assert!(output.stderr.contains("cleanup still active"));
        assert!(output.stderr.contains("rootdirs retained"));

        let rootdirs = runtime.created_run_rootdirs();
        assert_eq!(rootdirs.len(), 1);
        assert!(rootdirs[0].exists());
        fs::remove_dir_all(&rootdirs[0]).unwrap();
    }

    #[test]
    fn run_edf_fails_before_run_when_pull_fails() {
        let mut raster = FakeRasterOps::new(sample_config());
        raster
            .render_results
            .insert(String::from("job.edf"), Ok(sample_edf("alpine:3.22")));
        let runtime = FakeContainerRuntime::new();
        runtime.push_parallax_exist("alpine:3.22", vec![false]);
        runtime.pull_results.borrow_mut().insert(
            String::from("alpine:3.22"),
            Err(AppError::Runtime(String::from("registry offline"))),
        );
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };

        let err = execute_command(
            CommandSpec::Run {
                filepath: String::from("job.edf"),
                container_cmd: vec![String::from("sh")],
            },
            &mock_deps(&raster, &runtime, &user),
        )
        .unwrap_err();

        assert_eq!(err, AppError::Runtime(String::from("registry offline")));
        assert_eq!(
            runtime.calls(),
            vec![
                String::from("parallax_exist:alpine:3.22"),
                String::from("pull:alpine:3.22")
            ]
        );
    }

    #[test]
    fn run_yaml_pulls_only_missing_images_then_kube_play() {
        let temp = tempdir().unwrap();
        let manifest = temp.path().join("pod.yaml");
        fs::write(
            &manifest,
            r#"
apiVersion: v1
kind: Pod
metadata:
  name: training-pod
spec:
  containers:
    - name: app
      image: alpine:3.22
    - name: sidecar
      image: ubuntu:24.04
      labels:
        run.sarus.join: "true"
"#,
        )
        .unwrap();

        let mut raster = FakeRasterOps::new(sample_config());
        raster.render_results.insert(
            manifest.to_string_lossy().into_owned(),
            Err(String::from("not an edf")),
        );
        let runtime = FakeContainerRuntime::new();
        runtime.push_parallax_exist("alpine:3.22", vec![false, true]);
        runtime.push_image_exists("alpine:3.22", vec![true]);
        runtime.push_parallax_exist("ubuntu:24.04", vec![true]);
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };

        let output = execute_command(
            CommandSpec::Run {
                filepath: manifest.to_string_lossy().into_owned(),
                container_cmd: vec![],
            },
            &mock_deps(&raster, &runtime, &user),
        )
        .unwrap();

        assert_eq!(output.return_code, 0);
        assert_eq!(runtime.pull_verbose(), vec![false]);
        assert_eq!(runtime.migrate_verbose(), vec![false]);
        assert_eq!(
            runtime.calls(),
            vec![
                String::from("parallax_exist:alpine:3.22"),
                String::from("pull:alpine:3.22"),
                String::from("image_exists:alpine:3.22"),
                String::from("default_graphroot"),
                String::from("migrate:alpine:3.22"),
                String::from("parallax_exist:alpine:3.22"),
                String::from("parallax_exist:ubuntu:24.04"),
                format!("kube_play:{}", manifest.to_string_lossy()),
                String::from("exec_interactive:training-pod-sidecar:[]"),
                format!("kube_down:{}?force=true", manifest.to_string_lossy()),
            ]
        );
    }

    #[test]
    fn run_yaml_fails_before_kube_play_when_migration_fails() {
        let temp = tempdir().unwrap();
        let manifest = temp.path().join("pod.yaml");
        fs::write(
            &manifest,
            r#"
apiVersion: v1
kind: Pod
metadata:
  name: training-pod
spec:
  containers:
    - name: app
      image: alpine:3.22
"#,
        )
        .unwrap();

        let mut raster = FakeRasterOps::new(sample_config());
        raster.render_results.insert(
            manifest.to_string_lossy().into_owned(),
            Err(String::from("not an edf")),
        );
        let runtime = FakeContainerRuntime::new();
        runtime.push_parallax_exist("alpine:3.22", vec![false]);
        runtime.push_image_exists("alpine:3.22", vec![true]);
        runtime.migrate_results.borrow_mut().insert(
            String::from("alpine:3.22"),
            Err(AppError::Runtime(String::from("parallax broke"))),
        );
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };

        let err = execute_command(
            CommandSpec::Run {
                filepath: manifest.to_string_lossy().into_owned(),
                container_cmd: vec![],
            },
            &mock_deps(&raster, &runtime, &user),
        )
        .unwrap_err();

        assert_eq!(err, AppError::Runtime(String::from("parallax broke")));
        assert_eq!(
            runtime.calls(),
            vec![
                String::from("parallax_exist:alpine:3.22"),
                String::from("pull:alpine:3.22"),
                String::from("image_exists:alpine:3.22"),
                String::from("default_graphroot"),
                String::from("migrate:alpine:3.22"),
            ]
        );
    }

    #[test]
    fn run_yaml_removes_rootdirs_after_kube_play() {
        let temp = tempdir().unwrap();
        let manifest = temp.path().join("pod.yaml");
        fs::write(
            &manifest,
            r#"
apiVersion: v1
kind: Pod
metadata:
  name: training-pod
spec:
  containers:
    - name: app
      image: alpine:3.22
"#,
        )
        .unwrap();

        let mut raster = FakeRasterOps::new(sample_config());
        raster.render_results.insert(
            manifest.to_string_lossy().into_owned(),
            Err(String::from("not an edf")),
        );
        let runtime = FakeContainerRuntime::new().with_run_rootdir_creation();
        runtime.push_parallax_exist("alpine:3.22", vec![true]);
        let user = unique_test_user();

        let output = execute_command(
            CommandSpec::Run {
                filepath: manifest.to_string_lossy().into_owned(),
                container_cmd: vec![],
            },
            &mock_deps(&raster, &runtime, &FakeUserContext { user: user.clone() }),
        )
        .unwrap();

        assert_eq!(output.return_code, 0);
        let rootdirs = runtime.created_run_rootdirs();
        assert_eq!(rootdirs.len(), 1);
        assert_rootdirs_removed(&rootdirs[0]);
    }

    #[test]
    fn run_yaml_retains_rootdirs_when_kube_down_fails() {
        let temp = tempdir().unwrap();
        let manifest = temp.path().join("pod.yaml");
        fs::write(
            &manifest,
            r#"
apiVersion: v1
kind: Pod
metadata:
  name: training-pod
spec:
  containers:
    - name: app
      image: alpine:3.22
"#,
        )
        .unwrap();

        let mut raster = FakeRasterOps::new(sample_config());
        raster.render_results.insert(
            manifest.to_string_lossy().into_owned(),
            Err(String::from("not an edf")),
        );
        let mut runtime = FakeContainerRuntime::new().with_run_rootdir_creation();
        runtime.push_parallax_exist("alpine:3.22", vec![true]);
        runtime.kube_down_result = Err(AppError::Runtime(String::from("pod still active")));

        let err = execute_command(
            CommandSpec::Run {
                filepath: manifest.to_string_lossy().into_owned(),
                container_cmd: vec![],
            },
            &mock_deps(
                &raster,
                &runtime,
                &FakeUserContext {
                    user: unique_test_user(),
                },
            ),
        )
        .unwrap_err();

        assert!(err.to_string().contains("pod still active"));
        assert!(err.to_string().contains("rootdirs retained"));

        let rootdirs = runtime.created_run_rootdirs();
        assert_eq!(rootdirs.len(), 1);
        assert!(rootdirs[0].exists());
        fs::remove_dir_all(&rootdirs[0]).unwrap();
    }

    #[test]
    fn run_yaml_skips_exec_after_kube_play_failure_but_still_tears_down() {
        let temp = tempdir().unwrap();
        let manifest = temp.path().join("pod.yaml");
        fs::write(
            &manifest,
            r#"
apiVersion: v1
kind: Pod
metadata:
  name: training-pod
spec:
  containers:
    - name: app
      image: alpine:3.22
"#,
        )
        .unwrap();

        let mut raster = FakeRasterOps::new(sample_config());
        raster.render_results.insert(
            manifest.to_string_lossy().into_owned(),
            Err(String::from("not an edf")),
        );
        let mut runtime = FakeContainerRuntime::new().with_run_rootdir_creation();
        runtime.push_parallax_exist("alpine:3.22", vec![true]);
        runtime.kube_play_result = Err(AppError::Runtime(String::from("play failed")));

        let err = execute_command(
            CommandSpec::Run {
                filepath: manifest.to_string_lossy().into_owned(),
                container_cmd: vec![],
            },
            &mock_deps(
                &raster,
                &runtime,
                &FakeUserContext {
                    user: unique_test_user(),
                },
            ),
        )
        .unwrap_err();

        assert_eq!(err, AppError::Runtime(String::from("play failed")));
        assert!(
            !runtime
                .calls()
                .iter()
                .any(|call| call.starts_with("exec_interactive:"))
        );
        assert!(
            runtime
                .calls()
                .iter()
                .any(|call| call.starts_with("kube_down:"))
        );
        let rootdirs = runtime.created_run_rootdirs();
        assert_eq!(rootdirs.len(), 1);
        assert_rootdirs_removed(&rootdirs[0]);
    }

    #[test]
    fn run_yaml_fails_before_kube_play_when_join_container_selection_is_ambiguous() {
        let temp = tempdir().unwrap();
        let manifest = temp.path().join("pod.yaml");
        fs::write(
            &manifest,
            r#"
apiVersion: v1
kind: Pod
metadata:
  name: training-pod
spec:
  containers:
    - name: app
      image: alpine:3.22
    - name: sidecar
      image: ubuntu:24.04
"#,
        )
        .unwrap();

        let mut raster = FakeRasterOps::new(sample_config());
        raster.render_results.insert(
            manifest.to_string_lossy().into_owned(),
            Err(String::from("not an edf")),
        );
        let runtime = FakeContainerRuntime::new();
        runtime.push_parallax_exist("alpine:3.22", vec![true]);
        runtime.push_parallax_exist("ubuntu:24.04", vec![true]);
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };

        let err = execute_command(
            CommandSpec::Run {
                filepath: manifest.to_string_lossy().into_owned(),
                container_cmd: vec![],
            },
            &mock_deps(&raster, &runtime, &user),
        )
        .unwrap_err();

        assert_eq!(
            err,
            AppError::Yaml(String::from(
                "YAML manifest must label exactly one container with run.sarus.join=\"true\" when spec.containers has multiple containers",
            ))
        );
        assert_eq!(
            runtime.calls(),
            vec![
                String::from("parallax_exist:alpine:3.22"),
                String::from("parallax_exist:ubuntu:24.04"),
            ]
        );
    }

    #[test]
    fn pull_propagates_verbose_option() {
        let config = sample_config();
        let raster = FakeRasterOps::new(config);
        let runtime = FakeContainerRuntime::new();
        runtime.push_image_exists("alpine:3.22", vec![true]);
        runtime.push_parallax_exist("alpine:3.22", vec![true]);
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };

        let output = execute_command_with_options(
            CommandSpec::Pull {
                image: String::from("alpine:3.22"),
            },
            &mock_deps(&raster, &runtime, &user),
            ExecOptions {
                verbose: true,
                ..ExecOptions::default()
            },
        )
        .unwrap();

        assert_eq!(output, AppOutput::success(""));
        assert_eq!(runtime.pull_verbose(), vec![true]);
        assert_eq!(runtime.migrate_verbose(), vec![true]);
    }

    #[test]
    fn rmi_propagates_verbose_option() {
        let config = sample_config();
        let raster = FakeRasterOps::new(config);
        let runtime = FakeContainerRuntime::new();
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };

        let output = execute_command_with_options(
            CommandSpec::Rmi {
                image: String::from("alpine:3.22"),
            },
            &mock_deps(&raster, &runtime, &user),
            ExecOptions {
                verbose: true,
                ..ExecOptions::default()
            },
        )
        .unwrap();

        assert_eq!(output, AppOutput::success(""));
        assert_eq!(runtime.rmi_verbose(), vec![true]);
    }

    #[test]
    fn run_propagates_verbose_option_to_internal_pull_and_migrate() {
        let mut raster = FakeRasterOps::new(sample_config());
        raster
            .render_results
            .insert(String::from("job.edf"), Ok(sample_edf("alpine:3.22")));
        let runtime = FakeContainerRuntime::new();
        runtime.push_parallax_exist("alpine:3.22", vec![false, true]);
        runtime.push_image_exists("alpine:3.22", vec![true]);
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };

        let output = execute_command_with_options(
            CommandSpec::Run {
                filepath: String::from("job.edf"),
                container_cmd: vec![String::from("sh")],
            },
            &mock_deps(&raster, &runtime, &user),
            ExecOptions {
                verbose: true,
                ..ExecOptions::default()
            },
        )
        .unwrap();

        assert_eq!(output, AppOutput::success(""));
        assert_eq!(runtime.pull_verbose(), vec![true]);
        assert_eq!(runtime.migrate_verbose(), vec![true]);
    }

    #[test]
    fn run_invalid_input_returns_unsupported_input_error() {
        let temp = tempdir().unwrap();
        let input = temp.path().join("bad.txt");
        fs::write(&input, "foo: [bar").unwrap();

        let mut raster = FakeRasterOps::new(sample_config());
        raster.render_results.insert(
            input.to_string_lossy().into_owned(),
            Err(String::from("not an edf")),
        );
        let runtime = FakeContainerRuntime::new();
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };

        let err = execute_command(
            CommandSpec::Run {
                filepath: input.to_string_lossy().into_owned(),
                container_cmd: vec![],
            },
            &mock_deps(&raster, &runtime, &user),
        )
        .unwrap_err();

        assert_eq!(
            err,
            AppError::UnsupportedInput(format!(
                "{} is not valid EDF nor YAML",
                input.to_string_lossy()
            ))
        );
    }
}
