use clap::ValueEnum;
use hpc_image_store::{
    ExportReport, ImageSelector, ListReport, PullContext, PullOptions, PullReport, Store,
    StoreConfig, TransferContext, format_list_podman,
};
use raster::{self, EDF, ExecutedCommand, config::Config, hook_run};
use sarus_suite_podman_driver::{self as pmd, ContainerCtx, PodmanCtx};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::str;
use std::time::{Duration, Instant};
use tracing::instrument;
use uuid::Uuid;

// TODO review pub status in this file, restrict pub only to entities needed in main.rs and tests

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecOptions {
    pub verbose: bool,
    pub parallax_imagestore: Option<String>,
    pub hpc_imagestore: Option<PathBuf>,
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
    Images {
        all: bool,
    },
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
    fn load_config_xdg(&self) -> Result<Config, AppError> {
        self.load_config()
    }
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

pub trait HpcStoreOps {
    fn list(&self, path: &Path) -> Result<ListReport, AppError>;
    fn pull(
        &self,
        path: &Path,
        image: &ImageSelector,
        context: &PullContext,
    ) -> Result<PullReport, AppError>;
    fn resolve(&self, path: &Path, image: &ImageSelector) -> Result<Option<String>, AppError>;
    fn export(
        &self,
        path: &Path,
        image: &ImageSelector,
        context: &TransferContext,
    ) -> Result<ExportReport, AppError>;
}

pub struct RealHpcStoreOps;

impl HpcStoreOps for RealHpcStoreOps {
    fn list(&self, path: &Path) -> Result<ListReport, AppError> {
        let config =
            StoreConfig::new(path).map_err(|error| AppError::Runtime(error.to_string()))?;
        Store::new(config)
            .list()
            .map_err(|error| AppError::Runtime(error.to_string()))
    }

    fn pull(
        &self,
        path: &Path,
        image: &ImageSelector,
        context: &PullContext,
    ) -> Result<PullReport, AppError> {
        StoreConfig::new(path)
            .map(Store::new)
            .and_then(|store| {
                store.pull(
                    image,
                    None,
                    context,
                    &PullOptions {
                        replace: true,
                        notes: None,
                    },
                )
            })
            .map_err(|error| hpc_operation_error("pull", path, image, error))
    }

    fn resolve(&self, path: &Path, image: &ImageSelector) -> Result<Option<String>, AppError> {
        StoreConfig::new(path)
            .map(Store::new)
            .and_then(|store| store.resolve(image))
            .map_err(|error| hpc_operation_error("resolve", path, image, error))
    }

    fn export(
        &self,
        path: &Path,
        image: &ImageSelector,
        context: &TransferContext,
    ) -> Result<ExportReport, AppError> {
        StoreConfig::new(path)
            .map(Store::new)
            .and_then(|store| store.export(image, None, context))
            .map_err(|error| hpc_operation_error("export", path, image, error))
    }
}

fn hpc_operation_error(
    operation: &str,
    path: &Path,
    image: &ImageSelector,
    error: impl fmt::Display,
) -> AppError {
    AppError::Runtime(format!(
        "HPC store {operation} failed for {image} in {}: {error}",
        path.display()
    ))
}

// TODO: implement fmt::Debug or [derive(Debug)] annotation
pub struct AppDeps<'a> {
    pub raster: &'a dyn RasterOps,
    pub runtime: &'a dyn ContainerRuntime,
    pub user: &'a dyn UserContext,
    pub hpc_store: &'a dyn HpcStoreOps,
}

pub struct RealRasterOps;

impl RasterOps for RealRasterOps {
    fn load_config(&self) -> Result<Config, AppError> {
        raster::load_config().map_err(|e| AppError::ConfigLoad(e.to_string()))
    }

    fn load_config_xdg(&self) -> Result<Config, AppError> {
        raster::load_config_xdg(raster::config::VarExpand::Must, &None)
            .map_err(|e| AppError::ConfigLoad(e.to_string()))
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

#[derive(Debug)]
pub struct RealContainerRuntime;

impl ContainerRuntime for RealContainerRuntime {
    fn default_graphroot(&self, ctx: &PodmanCtx) -> Result<PathBuf, AppError> {
        //TODO Revise with logging: println!("Resolving default Podman graphroot...");
        let output = pmd::info(Some("{{.Store.GraphRoot}}"), Some(ctx))
            .map_err(|e| AppError::PodmanInfo(e.to_string()))?;
        let graphroot = str::from_utf8(&output.stdout)
            .map_err(|e| AppError::PodmanInfo(e.to_string()))?
            .trim();
        Ok(PathBuf::from(graphroot))
    }

    #[instrument(level = "debug")]
    fn images(&self, ctx: &PodmanCtx) -> Result<(), AppError> {
        pmd::images(Some(ctx)).map_err(|error| AppError::Runtime(error.to_string()))
    }

    #[instrument(level = "debug")]
    fn pull(&self, image: &str, ctx: &PodmanCtx, verbose: bool) -> Result<(), AppError> {
        if verbose {
            pmd::pull_streaming(image, Some(ctx))
                .map_err(|_| AppError::Runtime(format!("Failed to pull image {image}")))
        } else {
            pmd::pull(image, Some(ctx))
                .map_err(|e| AppError::Runtime(format!("Failed to pull image {image}: {e}")))
        }
    }

    #[instrument(level = "debug")]
    fn image_exists(&self, image: &str, ctx: &PodmanCtx) -> Result<bool, AppError> {
        //TODO revise with logging:println!("Checking if image {image} exists in Podman...");
        pmd::image_exists(image, Some(ctx)).map_err(|e| AppError::Runtime(e.to_string()))
    }

    #[instrument(level = "debug")]
    fn parallax_exist(
        &self,
        parallax_path: &Path,
        ctx: &PodmanCtx,
        image: &str,
    ) -> Result<bool, AppError> {
        pmd::parallax_exist(&parallax_path.to_path_buf(), ctx, image)
            .map_err(|e| AppError::Runtime(e.to_string()))
    }

    #[instrument(level = "debug")]
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

    #[instrument(level = "debug")]
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

    #[instrument(level = "debug")]
    fn run_from_edf(
        &self,
        edf: &EDF,
        run_ctx: &PodmanCtx,
        container_ctx: &ContainerCtx,
        container_cmd: &[String],
    ) -> Result<i32, AppError> {
        pmd::run_from_edf(edf, Some(run_ctx), container_ctx, container_cmd)
            .map_err(|e| AppError::Runtime(e.to_string()))?
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
            .map_err(|e| AppError::Runtime(e.to_string()))?
            .code()
            .ok_or_else(|| {
                AppError::Runtime(String::from("Container process terminated by signal"))
            })
    }

    #[instrument(level = "debug")]
    fn kube_play(&self, filepath: &str, run_ctx: &PodmanCtx) -> Result<(), AppError> {
        pmd::kube_play(filepath, Some(run_ctx))
            .map_err(|e| AppError::Runtime(format!("Podman kube play failed: {e}")))
    }

    #[instrument(level = "debug")]
    fn kube_down(&self, filepath: &str, force: bool, run_ctx: &PodmanCtx) -> Result<(), AppError> {
        pmd::kube_down(filepath, force, Some(run_ctx))
            .map_err(|e| AppError::Runtime(format!("Podman kube down failed: {e}")))
    }

    #[instrument(level = "debug")]
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
        conmon_path: Some(PathBuf::from(&config.conmon_path)),
        module: None,
        graphroot: None,
        runroot: None,
        parallax_mount_program: None,
        ro_store: None,
        mksquashfs_path: None,
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
        conmon_path: Some(PathBuf::from(&config.conmon_path)),
        module: None,
        graphroot: None,
        runroot: None,
        parallax_mount_program: None,
        ro_store: Some(PathBuf::from(&config.parallax_imagestore)),
        mksquashfs_path: Some(PathBuf::from(&config.mksquashfs_path)),
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
        conmon_path: Some(PathBuf::from(&config.conmon_path)),
        module: None,
        graphroot: Some(PathBuf::from(&config.parallax_imagestore)),
        runroot: None,
        parallax_mount_program: None,
        ro_store: None,
        mksquashfs_path: None,
        podman_env: None,
    }
}

/// Context for running containers. Fully custom Sarus Suite parameters (Podman module, Parallax imagestore, etc.).
/// Uses sarusctl-specific graphroot and runroot to not tamper with default podman rootdirs
pub fn build_run_ctx(
    config: &Config,
    user: &CurrentUser,
    run_id: &Uuid,
) -> Result<(PodmanCtx, PathBuf), AppError> {
    let roots_base = create_runtime_instance(user.uid, run_id)?;

    let ctx = PodmanCtx {
        podman_path: PathBuf::from(&config.podman_path),
        conmon_path: Some(PathBuf::from(&config.conmon_path)),
        module: Some(config.podman_module.clone()),
        graphroot: Some(roots_base.join("graphroot")),
        runroot: Some(roots_base.join("runroot")),
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
    .with_env("PARALLAX_MP_LOGFILE", config.parallax_mp_logfile.clone());

    Ok((ctx, roots_base))
}

fn create_runtime_instance(uid: u32, run_id: &Uuid) -> Result<PathBuf, AppError> {
    let instance_name = format!("sarusctl-{}-{}", uid, &run_id.simple().to_string()[..12]);

    let mut bases = Vec::new();
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        let runtime_dir = PathBuf::from(runtime_dir);
        if runtime_dir.is_absolute() && runtime_dir.is_dir() {
            bases.push(runtime_dir.join("sarus-suite"));
        }
    }
    bases.push(PathBuf::from("/dev/shm").join(format!("sarus-suite-{uid}")));
    bases.push(std::env::temp_dir().join(format!("sarus-suite-{uid}")));

    let mut errors = Vec::new();
    for base in bases {
        if let Err(error) = create_private_directory(&base) {
            errors.push(format!("{}: {error}", base.display()));
            continue;
        }

        let instance = base.join(&instance_name);
        match fs::DirBuilder::new().mode(0o700).create(&instance) {
            Ok(()) => return Ok(instance),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(AppError::Runtime(format!(
                    "Runtime instance directory already exists: {}",
                    instance.display()
                )));
            }
            Err(error) => errors.push(format!("{}: {error}", instance.display())),
        }
    }

    Err(AppError::Runtime(format!(
        "Cannot create sarusctl runtime directory: {}",
        errors.join("; ")
    )))
}

fn create_private_directory(path: &Path) -> io::Result<()> {
    match fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
    {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(error);
            }
        }
        Err(error) => return Err(error),
    }

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn cleanup_podman_rootdirs(roots_base: &Path) -> Option<String> {
    const CLEANUP_RETRY_INTERVAL: Duration = Duration::from_millis(100);
    const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

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

#[instrument(level = "debug", skip(deps))]
pub fn execute_command_with_options(
    command: CommandSpec,
    deps: &AppDeps<'_>,
    options: ExecOptions,
) -> Result<AppOutput, AppError> {
    match command {
        CommandSpec::Validate { filepath, .. } => validate_command(&filepath, deps),
        CommandSpec::Render { filepath, .. } => render_command(&filepath, deps),
        CommandSpec::Images { all } => {
            let config = load_config_with_options(deps.raster, &options)?;
            images_command(&config, deps, &options, all)
        }
        CommandSpec::Pull { image } => {
            let config = load_config_with_options(deps.raster, &options)?;
            pull_command(&image, &config, deps, &options)
        }
        CommandSpec::Migrate { image } => {
            let config = load_config_with_options(deps.raster, &options)?;
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

#[instrument(level = "debug", skip(raster))]
fn load_config_with_options(
    raster: &dyn RasterOps,
    options: &ExecOptions,
) -> Result<Config, AppError> {
    let mut config = raster.load_config_xdg()?;
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

/// FIXME: Temporary configuration boundary until Raster provides the HPC store path.
fn hpc_imagestore_path(config: &Config, options: &ExecOptions) -> Result<PathBuf, AppError> {
    let invalid_path = || {
        AppError::ConfigLoad(String::from(
            "Cannot determine the HPC store path; supply --hpc-imagestore PATH before the subcommand",
        ))
    };
    let absolute = |path: &Path| -> Result<PathBuf, AppError> {
        if path.as_os_str().is_empty() {
            return Err(invalid_path());
        }
        if path.is_absolute() {
            Ok(path.to_path_buf())
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(path))
                .map_err(|error| AppError::Io(format!("Cannot resolve HPC store path: {error}")))
        }
    };
    if let Some(path) = &options.hpc_imagestore {
        return absolute(path);
    }
    let parallax = absolute(Path::new(&config.parallax_imagestore))?;
    let parent = parallax.parent().ok_or_else(invalid_path)?;
    Ok(parent.join("hpc-imagestore"))
}

fn append_listing_diagnostics(output: &mut AppOutput, source: &str, diagnostics: &str) {
    for line in diagnostics.lines() {
        output.stderr.push_str(&format!("{source}: {line}\n"));
    }
}

// FIXME: When raster supports the path for the HPC imagestore it should not be necessary
// to pass an ExecOptions arg: the override would be resolved in the called and contained
// in the Config object, as it is for parallax path
fn images_command(
    config: &Config,
    deps: &AppDeps<'_>,
    options: &ExecOptions,
    all: bool,
) -> Result<AppOutput, AppError> {
    // Print the contents of the HPC image store
    let mut output = AppOutput::success(if all { "HPC store:\n" } else { "" });
    let report = hpc_imagestore_path(config, options).and_then(|path| deps.hpc_store.list(&path));
    match report {
        Ok(report) => {
            output.stdout.push_str(&format_list_podman(&report, false));
            for diagnostic in &report.diagnostics {
                append_listing_diagnostics(
                    &mut output,
                    "HPC store",
                    &format!("{:?}: {}", diagnostic.path, diagnostic.message),
                );
            }
            if report.has_errors() {
                output.return_code = 1;
                if report.diagnostics.is_empty() {
                    append_listing_diagnostics(
                        &mut output,
                        "HPC store",
                        "Inventory is incomplete or contains damaged entries",
                    );
                }
            }
        }
        Err(error) => {
            output.return_code = 1;
            append_listing_diagnostics(&mut output, "HPC store", &error.to_string());
        }
    }

    // Print content of other image stores
    if all {
        if !output.stdout.ends_with('\n') {
            output.stdout.push('\n');
        }
        // Release the stream locks before Podman inherits stdout and stderr.
        (|| -> io::Result<()> {
            let mut stdout = io::stdout().lock();
            let mut stderr = io::stderr().lock();
            stdout.write_all(output.stdout.as_bytes())?;
            stderr.write_all(output.stderr.as_bytes())?;
            stdout.write_all(b"\nPodman+Parallax stores:\n")?;
            stdout.flush()?;
            stderr.flush()
        })()
        .map_err(|error| AppError::Io(format!("Failed to print image listing: {error}")))?;
        output.stdout.clear();
        output.stderr.clear();

        // Print Podman and Parallax store contents using passthrough command
        if let Err(error) = podman_parallax_images(config, deps) {
            output.return_code = 1;
            append_listing_diagnostics(&mut output, "Podman+Parallax stores", &error.to_string());
        }
    }
    Ok(output)
}

fn podman_parallax_images(config: &Config, deps: &AppDeps<'_>) -> Result<(), AppError> {
    let seed_ctx = build_parallax_seed_ctx(config);

    // Use the Parallax seed context to pre-populate the struct with Podman path and additionalimagestore,
    // then add the default graphroot
    let graphroot = deps.runtime.default_graphroot(&seed_ctx)?;
    let ctx = PodmanCtx {
        graphroot: Some(graphroot),
        ..seed_ctx
    };

    setup_imagestore(config)?;
    deps.runtime.images(&ctx)
}

fn pull_command(
    image: &str,
    config: &Config,
    deps: &AppDeps<'_>,
    options: &ExecOptions,
) -> Result<AppOutput, AppError> {
    let source = ImageSelector::new(image)
        .map_err(|error| AppError::UnsupportedInput(format!("Invalid image {image}: {error}")))?;
    let path = hpc_imagestore_path(config, options)?;

    let transfer_context = TransferContext {
        podman: PathBuf::from(&config.podman_path),
        skopeo: PathBuf::from("skopeo"),
        platform: None,
    };
    let pull_context = PullContext {
        skopeo: transfer_context.skopeo.clone(),
        platform: transfer_context.platform.clone(),
        auth_file: None,
        cert_dir: None,
        policy: None,
        tls_verify: None,
    };

    print_progress_message(&format!("Pulling {source} into the HPC image store..."));
    deps.hpc_store.pull(&path, &source, &pull_context)?;

    // Resolve the original selector, observing the store at the time of lookup.
    let resolved = deps.hpc_store.resolve(&path, &source)?.ok_or_else(|| {
        hpc_operation_error("resolve", &path, &source, "image is missing after pull")
    })?;
    let resolved = ImageSelector::new(&resolved)
        .map_err(|error| hpc_operation_error("resolve", &path, &source, error))?;

    print_progress_message(&format!("Exporting {resolved} to Podman storage..."));
    deps.hpc_store.export(&path, &resolved, &transfer_context)?;
    if !deps
        .runtime
        .image_exists(resolved.as_str(), &build_pull_ctx(config))?
    {
        return Err(AppError::Runtime(format!(
            "Image {resolved} is still missing in Podman after HPC store export"
        )));
    }

    migrate_command(resolved.as_str(), config, deps, options)
}

/// Automatic pulls during `run` retain Podman's original reference handling.
fn pull_for_run(
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
    setup_imagestore(config)?;

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

#[instrument(level = "debug", skip(deps))]
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
            let mut config = load_config_with_options(deps.raster, options)?;
            raster::update_config_by_user(&mut config, edf.clone())
                .map_err(|e| AppError::ConfigLoad(e.to_string()))?;
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
    let mut output = AppOutput::success("");

    let parallax_path = PathBuf::from(&config.parallax_path);

    if !deps
        .runtime
        .parallax_exist(&parallax_path, &plx_ctx, &edf.image)?
    {
        merge_output(
            &mut output,
            pull_for_run(&edf.image, config, deps, options)?,
        );
    }

    let (run_ctx, roots_base) = build_run_ctx(config, &user, &run_id)?;

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
    let cleanup_warning = finalize_podman_cleanup(&roots_base, &container_cleanup_result);

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
    let manifest = parse_yaml_value_from_file(Path::new(filepath))?;

    let images = extract_images_from_yaml_value(&manifest)?;
    let mut output = AppOutput::success("");

    let parallax_path = PathBuf::from(&config.parallax_path);

    for image in images {
        if !deps
            .runtime
            .parallax_exist(&parallax_path, &plx_ctx, &image)?
        {
            merge_output(&mut output, pull_for_run(&image, config, deps, options)?);
        }
    }

    let join_container = get_join_container_from_yaml_manifest(&manifest)?;

    let (mut run_ctx, roots_base) = build_run_ctx(config, &user, &Uuid::new_v4())?;
    // Podman kube play doesn't seem to honor all settings from a module
    // so to avoid confusion we disable the module for the time being.
    // The settings from the module have to be specified in the k8s yaml, if desired
    run_ctx.module = None;

    let play_result = deps.runtime.kube_play(filepath, &run_ctx);
    let exec_result = play_result.as_ref().ok().map(|_| {
        deps.runtime
            .exec_interactive(&join_container, &run_ctx, container_cmd)
    });
    let down_result = deps.runtime.kube_down(filepath, true, &run_ctx);
    let cleanup_warning = finalize_podman_cleanup(&roots_base, &down_result);

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
    roots_base: &Path,
    podman_cleanup_result: &Result<(), AppError>,
) -> Option<String> {
    match podman_cleanup_result {
        Ok(()) => cleanup_podman_rootdirs(roots_base),
        Err(err) => Some(format!(
            "Warning: Podman rootdirs retained because workload cleanup failed: {err}"
        )),
    }
}

#[instrument(level = "debug")]
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
    use serial_test::serial;
    use std::cell::RefCell;
    use std::collections::{HashMap, VecDeque};
    use std::ffi::OsStr;
    use std::os::unix::fs::PermissionsExt;
    use std::rc::Rc;
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
        validate_results: HashMap<String, Result<(), String>>,
        render_results: HashMap<String, Result<EDF, String>>,
    }

    impl FakeRasterOps {
        fn new(config: Config) -> Self {
            Self {
                config: Ok(config),
                validate_results: HashMap::new(),
                render_results: HashMap::new(),
            }
        }
    }

    impl RasterOps for FakeRasterOps {
        fn load_config(&self) -> Result<Config, AppError> {
            self.config.clone()
        }

        fn load_config_path(&self, _path: &Path) -> Result<Config, AppError> {
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

    #[test]
    #[serial]
    fn real_raster_ops_load_config_uses_xdg_user_config_before_system_config() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let xdg_config_home = temp.path().join("xdg-config-home");
        let xdg_config_dirs = temp.path().join("xdg-config-dirs");
        let user_config = xdg_config_home.join("sarus-suite");
        let system_config = xdg_config_dirs.join("sarus-suite");

        fs::create_dir_all(&user_config).unwrap();
        fs::create_dir_all(&system_config).unwrap();
        fs::write(
            system_config.join("10-system.conf"),
            "parallax_path = \"system-parallax\"\n",
        )
        .unwrap();

        let previous = [
            ("HOME", std::env::var_os("HOME")),
            ("XDG_CONFIG_HOME", std::env::var_os("XDG_CONFIG_HOME")),
            ("XDG_CONFIG_DIRS", std::env::var_os("XDG_CONFIG_DIRS")),
        ];

        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("XDG_CONFIG_HOME", &xdg_config_home);
            std::env::set_var("XDG_CONFIG_DIRS", &xdg_config_dirs);
        }
        let system_config = RealRasterOps.load_config_xdg();
        let user_config = fs::write(
            user_config.join("10-user.conf"),
            "parallax_path = \"user-parallax\"\n",
        )
        .map(|_| RealRasterOps.load_config_xdg());
        unsafe {
            for (name, value) in previous {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }

        assert_eq!(system_config.unwrap().parallax_path, "system-parallax");
        assert_eq!(user_config.unwrap().unwrap().parallax_path, "user-parallax");
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
        calls: Rc<RefCell<Vec<String>>>,
        create_run_rootdirs: bool,
        created_run_rootdirs: RefCell<Vec<PathBuf>>,
        pull_verbose: RefCell<Vec<bool>>,
        migrate_verbose: RefCell<Vec<bool>>,
        rmi_verbose: RefCell<Vec<bool>>,
        run_logfiles: RefCell<Vec<Option<String>>>,
        graphroot: Result<PathBuf, AppError>,
        images_result: Result<(), AppError>,
        image_exists_error: Option<AppError>,
        parallax_exists_error: Option<AppError>,
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
                calls: Rc::default(),
                create_run_rootdirs: false,
                created_run_rootdirs: RefCell::new(Vec::new()),
                pull_verbose: RefCell::new(Vec::new()),
                migrate_verbose: RefCell::new(Vec::new()),
                rmi_verbose: RefCell::new(Vec::new()),
                run_logfiles: RefCell::new(Vec::new()),
                graphroot: Ok(PathBuf::from("/graphroot")),
                images_result: Ok(()),
                image_exists_error: None,
                parallax_exists_error: None,
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

        fn run_logfiles(&self) -> Vec<Option<String>> {
            self.run_logfiles.borrow().clone()
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
            self.images_result.clone()
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
            if let Some(error) = &self.image_exists_error {
                return Err(error.clone());
            }
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
            if let Some(error) = &self.parallax_exists_error {
                return Err(error.clone());
            }
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
            self.run_logfiles.borrow_mut().push(
                run_ctx
                    .podman_env
                    .as_ref()
                    .and_then(|env| env.get(OsStr::new("PARALLAX_MP_LOGFILE")))
                    .map(|path| path.to_string_lossy().into_owned()),
            );
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
            hpc_store: &RealHpcStoreOps,
        }
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
        assert_eq!(
            seed.mksquashfs_path,
            Some(PathBuf::from(&config.mksquashfs_path))
        );

        let ro = build_readonly_ctx(&config);
        assert_eq!(ro.podman_path, PathBuf::from("/usr/bin/podman"));
        assert_eq!(ro.graphroot, Some(parallax_imagestore.clone()));

        let (run, roots_base) = build_run_ctx(&config, &user, &run_id).unwrap();
        assert_eq!(run.podman_path, PathBuf::from("/usr/bin/podman"));
        assert_eq!(run.module, Some(String::from("hpc")));
        assert_eq!(run.graphroot, Some(roots_base.join("graphroot")));
        assert_eq!(run.runroot, Some(roots_base.join("runroot")));
        assert!(
            roots_base
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("sarusctl-1234-a1a2a3a4b1b2")
        );
        assert_eq!(
            fs::metadata(&roots_base).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::remove_dir_all(roots_base).unwrap();
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
    #[serial]
    fn build_run_ctx_uses_xdg_runtime_dir_and_creates_private_unique_instance() {
        let temp = tempdir().unwrap();
        let runtime_dir = temp.path().join("runtime");
        fs::create_dir(&runtime_dir).unwrap();

        let previous = std::env::var_os("XDG_RUNTIME_DIR");
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir) };

        let config = sample_config();
        let user = CurrentUser {
            uid: 1234,
            gid: 4321,
        };
        let first_id = Uuid::new_v4();
        let second_id = Uuid::new_v4();
        let (first, first_base) = build_run_ctx(&config, &user, &first_id).unwrap();
        let (second, second_base) = build_run_ctx(&config, &user, &second_id).unwrap();

        match previous {
            Some(value) => unsafe { std::env::set_var("XDG_RUNTIME_DIR", value) },
            None => unsafe { std::env::remove_var("XDG_RUNTIME_DIR") },
        }

        assert!(first_base.starts_with(runtime_dir.join("sarus-suite")));
        assert!(second_base.starts_with(runtime_dir.join("sarus-suite")));
        assert_ne!(first_base, second_base);
        assert_eq!(first.graphroot, Some(first_base.join("graphroot")));
        assert_eq!(second.runroot, Some(second_base.join("runroot")));
        assert_eq!(
            fs::metadata(&first_base).unwrap().permissions().mode() & 0o777,
            0o700
        );

        fs::remove_dir_all(first_base).unwrap();
        fs::remove_dir_all(second_base).unwrap();
    }

    #[test]
    #[serial]
    fn build_run_ctx_falls_back_when_xdg_runtime_dir_is_unusable() {
        let temp = tempdir().unwrap();
        let unusable_runtime_dir = temp.path().join("runtime-file");
        fs::write(&unusable_runtime_dir, "not a directory").unwrap();

        let previous = std::env::var_os("XDG_RUNTIME_DIR");
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", &unusable_runtime_dir) };

        let result = build_run_ctx(
            &sample_config(),
            &CurrentUser {
                uid: 1234,
                gid: 4321,
            },
            &Uuid::new_v4(),
        );

        match previous {
            Some(value) => unsafe { std::env::set_var("XDG_RUNTIME_DIR", value) },
            None => unsafe { std::env::remove_var("XDG_RUNTIME_DIR") },
        }

        let (_, roots_base) = result.unwrap();
        assert!(!roots_base.starts_with(temp.path()));
        assert!(
            roots_base
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("sarusctl-1234-")
        );
        cleanup_podman_rootdirs(&roots_base);
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

    fn hpc_store_inventory(status: hpc_image_store::ListStatus) -> ListReport {
        ListReport {
            entries: vec![hpc_image_store::ListEntry {
                path: PathBuf::from("refs/analysis/tool/release"),
                local_ref: Some(hpc_image_store::LocalRef::new("analysis/tool:release").unwrap()),
                stored_root: None,
                metadata: None,
                status,
            }],
            diagnostics: vec![],
            complete: true,
        }
    }

    struct FakeHpcStore {
        list_report: Result<ListReport, AppError>,
        calls: Rc<RefCell<Vec<String>>>,
        paths: RefCell<Vec<PathBuf>>,
        pull_contexts: RefCell<Vec<PullContext>>,
        transfer_contexts: RefCell<Vec<TransferContext>>,
        resolved: Option<String>,
        failure: Option<&'static str>,
    }

    impl FakeHpcStore {
        fn new(runtime: &FakeContainerRuntime) -> Self {
            Self {
                list_report: Ok(ListReport {
                    entries: Vec::new(),
                    diagnostics: Vec::new(),
                    complete: true,
                }),
                calls: Rc::clone(&runtime.calls),
                paths: RefCell::new(Vec::new()),
                pull_contexts: RefCell::new(Vec::new()),
                transfer_contexts: RefCell::new(Vec::new()),
                resolved: None,
                failure: None,
            }
        }

        fn with_image(mut self, image: &str) -> Self {
            let image = ImageSelector::new(image).unwrap();
            self.resolved = Some(format!("{}@sha256:{}", image.name(), "a".repeat(64)));
            self
        }

        fn record(
            &self,
            operation: &str,
            path: &Path,
            image: &ImageSelector,
        ) -> Result<(), AppError> {
            self.calls
                .borrow_mut()
                .push(format!("hpc_{operation}:{image}"));
            self.paths.borrow_mut().push(path.to_path_buf());
            if self.failure == Some(operation) {
                return Err(hpc_operation_error(operation, path, image, "test failure"));
            }
            Ok(())
        }
    }

    impl HpcStoreOps for FakeHpcStore {
        fn list(&self, path: &Path) -> Result<ListReport, AppError> {
            self.paths.borrow_mut().push(path.to_path_buf());
            self.list_report.clone()
        }

        fn pull(
            &self,
            path: &Path,
            image: &ImageSelector,
            context: &PullContext,
        ) -> Result<PullReport, AppError> {
            self.pull_contexts.borrow_mut().push(context.clone());
            self.record("pull", path, image)?;
            let root =
                hpc_image_store::Sha256Digest::new(format!("sha256:{}", "a".repeat(64))).unwrap();
            let tag = image.tag().unwrap_or(&root.hex()[..12]);
            Ok(PullReport {
                local_ref: hpc_image_store::LocalRef::new(format!("{}:{tag}", image.name()))
                    .unwrap(),
                requested_reference: image.to_string(),
                resolved_digest: root.clone(),
                stored_root: root.clone(),
                content: hpc_image_store::ContentMetadata {
                    config_digest: root,
                    platform: hpc_image_store::Platform {
                        os: "linux".into(),
                        architecture: "amd64".into(),
                        variant: None,
                    },
                    created: None,
                    size_bytes: 0,
                },
                ignored_destination_digest: None,
            })
        }

        fn resolve(&self, path: &Path, image: &ImageSelector) -> Result<Option<String>, AppError> {
            self.record("resolve", path, image)?;
            Ok(self.resolved.clone())
        }

        fn export(
            &self,
            path: &Path,
            image: &ImageSelector,
            context: &TransferContext,
        ) -> Result<ExportReport, AppError> {
            self.transfer_contexts.borrow_mut().push(context.clone());
            self.record("export", path, image)?;
            Ok(ExportReport {
                local_ref: hpc_image_store::LocalRef::new(format!("{}:cached", image.name()))
                    .unwrap(),
                stored_manifest: image.digest().unwrap().clone(),
                destination: image.to_string(),
            })
        }
    }

    #[test]
    fn images_resolves_paths_and_override_precedence() {
        let raster = FakeRasterOps::new(sample_config());
        let options = ExecOptions {
            parallax_imagestore: Some(String::from("relative/parallax")),
            ..ExecOptions::default()
        };
        let config = load_config_with_options(&raster, &options).unwrap();
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            hpc_imagestore_path(&config, &options).unwrap(),
            cwd.join("relative/hpc-imagestore")
        );
        let options = ExecOptions {
            hpc_imagestore: Some(PathBuf::from("explicit-hpc")),
            ..options
        };
        assert_eq!(
            hpc_imagestore_path(&config, &options).unwrap(),
            cwd.join("explicit-hpc")
        );
        let mut config = config;
        config.parallax_imagestore.clear();
        assert_eq!(
            hpc_imagestore_path(&config, &options).unwrap(),
            cwd.join("explicit-hpc")
        );
    }

    #[test]
    fn images_rejects_empty_override_and_unavailable_derivation() {
        for parallax in ["", "/"] {
            let mut config = sample_config();
            config.parallax_imagestore = parallax.into();
            assert!(
                hpc_imagestore_path(&config, &ExecOptions::default())
                    .unwrap_err()
                    .to_string()
                    .contains("--hpc-imagestore")
            );
        }
        let options = ExecOptions {
            hpc_imagestore: Some(PathBuf::new()),
            ..ExecOptions::default()
        };
        assert!(hpc_imagestore_path(&sample_config(), &options).is_err());
    }

    #[test]
    fn images_initializes_only_hpc_store_without_podman_or_parallax_hook() {
        let temp = tempdir().unwrap();
        let mut config = sample_config();
        config.parallax_imagestore = temp.path().join("parallax").to_string_lossy().into_owned();
        config.hooks.parallax_imagestore_create = String::from("/missing/hook");
        let raster = FakeRasterOps::new(config);
        let runtime = FakeContainerRuntime::new();
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };
        let output = execute_command(
            CommandSpec::Images { all: false },
            &mock_deps(&raster, &runtime, &user),
        )
        .unwrap();
        assert_eq!(output.return_code, 0);
        assert!(output.stdout.starts_with("REPOSITORY"));
        assert!(output.stderr.is_empty());
        assert!(runtime.calls().is_empty());
        assert!(temp.path().join("hpc-imagestore/store.json").is_file());
        assert!(!temp.path().join("parallax").exists());
    }

    #[test]
    fn images_keeps_inventory_and_appends_legacy_on_report_or_operation_errors() {
        use hpc_image_store::{DiagnosticCategory, ListDiagnostic, ListStatus};
        let healthy = hpc_store_inventory(ListStatus::Unverified);
        let damaged = hpc_store_inventory(ListStatus::Damaged);
        let mut incomplete = healthy.clone();
        incomplete.complete = false;
        incomplete.diagnostics.push(ListDiagnostic {
            category: DiagnosticCategory::Io,
            path: PathBuf::from("refs/unreadable"),
            local_ref: None,
            message: String::from("permission denied"),
        });
        for (report, failed) in [
            (Ok(healthy), false),
            (Ok(damaged), true),
            (Ok(incomplete), true),
            (
                Err(AppError::Runtime(String::from("store unavailable"))),
                true,
            ),
        ] {
            for legacy_failed in [false, true] {
                let temp = tempdir().unwrap();
                let mut config = sample_config();
                config.parallax_imagestore =
                    temp.path().join("parallax").to_string_lossy().into_owned();
                let raster = FakeRasterOps::new(config);
                let mut runtime = FakeContainerRuntime::new();
                if legacy_failed {
                    runtime.images_result = Err(AppError::Runtime(String::from("legacy failure")));
                }
                let user = FakeUserContext {
                    user: CurrentUser { uid: 1, gid: 1 },
                };
                let store = FakeHpcStore {
                    list_report: report.clone(),
                    ..FakeHpcStore::new(&runtime)
                };
                let deps = AppDeps {
                    hpc_store: &store,
                    ..mock_deps(&raster, &runtime, &user)
                };
                let hpc_output =
                    execute_command(CommandSpec::Images { all: false }, &deps).unwrap();
                assert_eq!(hpc_output.return_code, i32::from(failed));
                if report.is_ok() {
                    assert!(hpc_output.stdout.contains("analysis/tool"));
                }
                assert_eq!(hpc_output.stderr.contains("HPC store:"), failed);
                assert!(runtime.calls().is_empty());
                store.paths.borrow_mut().clear();

                let output = execute_command(CommandSpec::Images { all: true }, &deps).unwrap();
                assert_eq!(output.return_code, i32::from(failed || legacy_failed));
                assert!(output.stdout.is_empty());
                assert!(!output.stderr.contains("HPC store:"));
                assert_eq!(
                    output
                        .stderr
                        .contains("Podman+Parallax stores: legacy failure"),
                    legacy_failed
                );
                assert_eq!(runtime.calls(), vec!["default_graphroot", "images"]);
                assert_eq!(
                    *store.paths.borrow(),
                    vec![temp.path().join("hpc-imagestore")]
                );
            }
        }
    }

    #[test]
    fn images_all_does_not_return_printed_output_after_graphroot_or_runtime_error() {
        for graphroot_failure in [false, true] {
            let temp = tempdir().unwrap();
            let mut config = sample_config();
            config.parallax_imagestore =
                temp.path().join("parallax").to_string_lossy().into_owned();
            let raster = FakeRasterOps::new(config);
            let mut runtime = FakeContainerRuntime::new();
            let error = AppError::Runtime(String::from("runtime unavailable"));
            if graphroot_failure {
                runtime.graphroot = Err(error);
            } else {
                runtime.images_result = Err(error);
            }
            let user = FakeUserContext {
                user: CurrentUser { uid: 1, gid: 1 },
            };
            let store = FakeHpcStore {
                list_report: Ok(hpc_store_inventory(hpc_image_store::ListStatus::Unverified)),
                ..FakeHpcStore::new(&runtime)
            };
            let deps = AppDeps {
                hpc_store: &store,
                ..mock_deps(&raster, &runtime, &user)
            };
            let output = execute_command(CommandSpec::Images { all: true }, &deps).unwrap();
            assert_eq!(output.return_code, 1);
            assert!(output.stdout.is_empty());
            assert!(
                output
                    .stderr
                    .contains("Podman+Parallax stores: runtime unavailable")
            );
        }
    }

    #[test]
    fn images_configuration_failure_prevents_both_listings() {
        let mut raster = FakeRasterOps::new(sample_config());
        raster.config = Err(AppError::ConfigLoad(String::from("bad configuration")));
        let runtime = FakeContainerRuntime::new();
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };
        let store = FakeHpcStore {
            list_report: Ok(hpc_store_inventory(hpc_image_store::ListStatus::Unverified)),
            ..FakeHpcStore::new(&runtime)
        };
        let deps = AppDeps {
            hpc_store: &store,
            ..mock_deps(&raster, &runtime, &user)
        };
        assert!(execute_command(CommandSpec::Images { all: true }, &deps).is_err());
        assert!(runtime.calls().is_empty());
        assert!(store.paths.borrow().is_empty());
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

        let output = execute_command(
            CommandSpec::Images { all: true },
            &mock_deps(&raster, &runtime, &user),
        )
        .unwrap();

        assert_eq!(output.return_code, 0);
        assert!(imagestore.exists());
        assert_eq!(
            runtime.calls(),
            vec![String::from("default_graphroot"), String::from("images")]
        );
    }

    #[test]
    fn hpc_adapters_honor_caller_contexts() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("store");
        let image = ImageSelector::new("registry.example/image:release").unwrap();
        let executable = temp.path().join("caller-skopeo");
        let pull_context = PullContext {
            skopeo: executable.clone(),
            platform: None,
            auth_file: None,
            cert_dir: None,
            policy: None,
            tls_verify: None,
        };
        let error = RealHpcStoreOps
            .pull(&path, &image, &pull_context)
            .unwrap_err();
        assert!(error.to_string().contains(executable.to_str().unwrap()));

        // Invalid caller settings must reach library validation, rather than
        // being replaced by defaults and proceeding to image selection.
        let transfer_context = TransferContext {
            podman: temp.path().join("caller-podman"),
            skopeo: PathBuf::new(),
            platform: None,
        };
        let error = RealHpcStoreOps
            .export(&path, &image, &transfer_context)
            .unwrap_err();
        assert!(error.to_string().contains("tool executable path is empty"));
    }

    #[test]
    fn pull_uses_original_selector_then_resolved_digest_in_order() {
        let digest = format!("sha256:{}", "a".repeat(64));
        for image in [
            String::from("registry.example/alpine:3.22"),
            format!("registry.example/alpine@{digest}"),
            format!("registry.example/alpine:3.22@{digest}"),
        ] {
            for override_path in [false, true] {
                let temp = tempdir().unwrap();
                let mut config = sample_config();
                config.parallax_imagestore =
                    temp.path().join("parallax").to_string_lossy().into_owned();
                let configured_podman = temp.path().join("configured-podman");
                config.podman_path = configured_podman.to_string_lossy().into_owned();
                let raster = FakeRasterOps::new(config);
                let runtime = FakeContainerRuntime::new();
                let store = FakeHpcStore::new(&runtime).with_image(&image);
                let resolved = store.resolved.as_ref().unwrap();
                runtime.push_image_exists(resolved, vec![true]);
                runtime.push_parallax_exist(resolved, vec![true]);
                let user = FakeUserContext {
                    user: CurrentUser { uid: 1, gid: 1 },
                };
                let options = ExecOptions {
                    hpc_imagestore: override_path.then(|| temp.path().join("selected")),
                    ..ExecOptions::default()
                };
                let output = execute_command_with_options(
                    CommandSpec::Pull {
                        image: image.clone(),
                    },
                    &AppDeps {
                        hpc_store: &store,
                        ..mock_deps(&raster, &runtime, &user)
                    },
                    options,
                )
                .unwrap();

                assert_eq!(output, AppOutput::success(""));
                assert!(runtime.pull_verbose().is_empty());
                assert_eq!(runtime.migrate_verbose(), vec![false]);
                assert_eq!(
                    runtime.calls(),
                    vec![
                        format!("hpc_pull:{image}"),
                        format!("hpc_resolve:{image}"),
                        format!("hpc_export:{resolved}"),
                        format!("image_exists:{resolved}"),
                        String::from("default_graphroot"),
                        format!("migrate:{resolved}"),
                        format!("parallax_exist:{resolved}"),
                    ]
                );
                let expected_path = temp.path().join(if override_path {
                    "selected"
                } else {
                    "hpc-imagestore"
                });
                assert_eq!(*store.paths.borrow(), vec![expected_path; 3]);
                let pull_contexts = store.pull_contexts.borrow();
                let transfer_contexts = store.transfer_contexts.borrow();
                assert_eq!(pull_contexts.len(), 1);
                assert_eq!(transfer_contexts.len(), 1);
                assert_eq!(transfer_contexts[0].podman, configured_podman);
                assert_eq!(transfer_contexts[0].skopeo, Path::new("skopeo"));
                assert_eq!(pull_contexts[0].skopeo, transfer_contexts[0].skopeo);
                assert_eq!(pull_contexts[0].platform, transfer_contexts[0].platform);
            }
        }
    }

    #[test]
    fn pull_uses_resolution_result_even_when_tag_changed_after_acquisition() {
        let raster = FakeRasterOps::new(sample_config());
        let runtime = FakeContainerRuntime::new();
        let mut store = FakeHpcStore::new(&runtime).with_image("alpine:3.22");
        let newer = format!("alpine@sha256:{}", "b".repeat(64));
        store.resolved = Some(newer.clone());
        runtime.push_image_exists(&newer, vec![true]);
        runtime.push_parallax_exist(&newer, vec![true]);
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };
        execute_command(
            CommandSpec::Pull {
                image: "alpine:3.22".into(),
            },
            &AppDeps {
                hpc_store: &store,
                ..mock_deps(&raster, &runtime, &user)
            },
        )
        .unwrap();
        assert_eq!(
            runtime.calls(),
            vec![
                String::from("hpc_pull:alpine:3.22"),
                String::from("hpc_resolve:alpine:3.22"),
                format!("hpc_export:{newer}"),
                format!("image_exists:{newer}"),
                String::from("default_graphroot"),
                format!("migrate:{newer}"),
                format!("parallax_exist:{newer}"),
            ]
        );
    }

    #[test]
    fn pull_stops_at_each_failed_stage() {
        for (failure, calls) in [
            ("pull", 1),
            ("resolve", 2),
            ("absent", 2),
            ("invalid_resolution", 2),
            ("export", 3),
            ("missing_podman", 4),
            ("podman_error", 4),
            ("graphroot", 5),
            ("migrate", 6),
            ("missing_parallax", 7),
            ("parallax_error", 7),
        ] {
            let raster = FakeRasterOps::new(sample_config());
            let mut runtime = FakeContainerRuntime::new();
            let mut store = FakeHpcStore::new(&runtime).with_image("alpine:3.22");
            let resolved = store.resolved.clone().unwrap();
            let error = AppError::Runtime(format!("{failure} test failure"));
            match failure {
                "pull" | "resolve" | "export" => store.failure = Some(failure),
                "absent" => store.resolved = None,
                "invalid_resolution" => store.resolved = Some("invalid".into()),
                "podman_error" => runtime.image_exists_error = Some(error),
                "graphroot" => runtime.graphroot = Err(error),
                "migrate" => {
                    runtime
                        .migrate_results
                        .borrow_mut()
                        .insert(resolved.clone(), Err(error));
                }
                "parallax_error" => runtime.parallax_exists_error = Some(error),
                _ => {}
            }
            runtime.push_image_exists(&resolved, vec![failure != "missing_podman"]);
            runtime.push_parallax_exist(&resolved, vec![failure != "missing_parallax"]);
            let user = FakeUserContext {
                user: CurrentUser { uid: 1, gid: 1 },
            };
            let result = execute_command(
                CommandSpec::Pull {
                    image: "alpine:3.22".into(),
                },
                &AppDeps {
                    hpc_store: &store,
                    ..mock_deps(&raster, &runtime, &user)
                },
            );
            assert!(result.is_err(), "{failure}");
            let expected = [
                String::from("hpc_pull:alpine:3.22"),
                String::from("hpc_resolve:alpine:3.22"),
                format!("hpc_export:{resolved}"),
                format!("image_exists:{resolved}"),
                String::from("default_graphroot"),
                format!("migrate:{resolved}"),
                format!("parallax_exist:{resolved}"),
            ];
            assert_eq!(runtime.calls(), expected[..calls], "{failure}");
            assert!(runtime.pull_verbose().is_empty());
        }
    }

    #[test]
    fn pull_rejects_invalid_input_before_setup_or_transfers() {
        for image in ["alpine", "docker://alpine:3.22", "alpine@sha256:abc"] {
            let temp = tempdir().unwrap();
            let mut config = sample_config();
            let parallax = temp.path().join("parallax");
            config.parallax_imagestore = parallax.to_string_lossy().into_owned();
            let raster = FakeRasterOps::new(config);
            let runtime = FakeContainerRuntime::new();
            let store = FakeHpcStore::new(&runtime).with_image("alpine:3.22");
            let user = FakeUserContext {
                user: CurrentUser { uid: 1, gid: 1 },
            };
            let result = execute_command(
                CommandSpec::Pull {
                    image: image.into(),
                },
                &AppDeps {
                    hpc_store: &store,
                    ..mock_deps(&raster, &runtime, &user)
                },
            );
            assert!(
                matches!(result, Err(AppError::UnsupportedInput(_))),
                "{image}"
            );
            assert!(runtime.calls().is_empty());
            assert!(!parallax.exists());
        }
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
    fn run_edf_applies_mount_program_annotations() {
        let logfile = String::from("/tmp/edf-annotation.log");
        let mut edf = sample_edf("alpine:3.22");
        edf.annotations.insert(
            String::from("com.sarus.parallax_mp_logfile"),
            logfile.clone(),
        );
        edf.annotations.insert(
            String::from("com.sarus.parallax_mp_squashfuse_path"),
            String::from("/custom/squashfuse_ll"),
        );

        let mut raster = FakeRasterOps::new(sample_config());
        raster
            .render_results
            .insert(String::from("job.edf"), Ok(edf));
        let runtime = FakeContainerRuntime::new();
        runtime.push_parallax_exist("alpine:3.22", vec![true]);
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };

        let output = execute_command(
            CommandSpec::Run {
                filepath: String::from("job.edf"),
                container_cmd: vec![String::from("true")],
            },
            &mock_deps(&raster, &runtime, &user),
        )
        .unwrap();

        assert_eq!(output.return_code, 0);
        assert_eq!(runtime.run_logfiles(), vec![Some(logfile)]);
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
        let store = FakeHpcStore::new(&runtime).with_image("alpine:3.22");
        let resolved = store.resolved.as_ref().unwrap();
        runtime.push_image_exists(resolved, vec![true]);
        runtime.push_parallax_exist(resolved, vec![true]);
        let user = FakeUserContext {
            user: CurrentUser { uid: 1, gid: 1 },
        };

        let output = execute_command_with_options(
            CommandSpec::Pull {
                image: String::from("alpine:3.22"),
            },
            &AppDeps {
                hpc_store: &store,
                ..mock_deps(&raster, &runtime, &user)
            },
            ExecOptions {
                verbose: true,
                ..ExecOptions::default()
            },
        )
        .unwrap();

        assert_eq!(output, AppOutput::success(""));
        assert!(runtime.pull_verbose().is_empty());
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
