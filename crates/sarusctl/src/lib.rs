use clap::ValueEnum;
use raster::{self, EDF, config::Config};
use sarus_suite_podman_driver::{self as pmd, ContainerCtx, PodmanCtx};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str;
use uuid::Uuid;

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
    pub username: String,
}

pub trait UserContext {
    fn current_user(&self) -> Result<CurrentUser, AppError>;
}

pub trait RasterOps {
    fn load_config(&self) -> Result<Config, AppError>;
    fn validate(&self, path: &str) -> Result<(), String>;
    fn render(&self, path: &str) -> Result<EDF, String>;
}

pub trait ContainerRuntime {
    fn default_graphroot(&self, ctx: &PodmanCtx) -> Result<PathBuf, AppError>;
    fn images(&self, ctx: &PodmanCtx) -> Result<(), AppError>;
    fn pull(&self, image: &str, ctx: &PodmanCtx) -> Result<(), AppError>;
    fn image_exists(&self, image: &str, ctx: &PodmanCtx) -> Result<bool, AppError>;
    fn parallax_migrate(
        &self,
        parallax_path: &Path,
        ctx: &PodmanCtx,
        image: &str,
    ) -> Result<(), AppError>;
    fn parallax_rmi(
        &self,
        parallax_path: &Path,
        ctx: &PodmanCtx,
        image: &str,
    ) -> Result<(), AppError>;
    fn run_from_edf(
        &self,
        edf: &EDF,
        run_ctx: &PodmanCtx,
        container_ctx: &ContainerCtx,
        container_cmd: &[String],
    ) -> Result<i32, AppError>;
    fn kube_play(&self, filepath: &str, run_ctx: &PodmanCtx) -> Result<(), AppError>;
    // TODO add kube_down for pod teardown
}

pub struct AppDeps<'a> {
    pub raster: &'a dyn RasterOps,
    pub runtime: &'a dyn ContainerRuntime,
    pub user: &'a dyn UserContext,
}

pub struct RealRasterOps;

impl RasterOps for RealRasterOps {
    fn load_config(&self) -> Result<Config, AppError> {
        raster::load_config().map_err(|e| AppError::ConfigLoad(e.to_string()))
    }

    fn validate(&self, path: &str) -> Result<(), String> {
        raster::validate(path.to_string()).map_err(|e| e.to_string())
    }

    fn render(&self, path: &str) -> Result<EDF, String> {
        raster::render(path.to_string()).map_err(|e| e.to_string())
    }
}

pub struct RealContainerRuntime;

impl ContainerRuntime for RealContainerRuntime {
    fn default_graphroot(&self, ctx: &PodmanCtx) -> Result<PathBuf, AppError> {
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

    fn pull(&self, image: &str, ctx: &PodmanCtx) -> Result<(), AppError> {
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

    fn image_exists(&self, image: &str, ctx: &PodmanCtx) -> Result<bool, AppError> {
        Ok(pmd::image_exists(image, Some(ctx)))
    }

    fn parallax_migrate(
        &self,
        parallax_path: &Path,
        ctx: &PodmanCtx,
        image: &str,
    ) -> Result<(), AppError> {
        pmd::parallax_migrate(&parallax_path.to_path_buf(), ctx, image)
            .map_err(|e| AppError::Runtime(format!("Failed migrating parallax: {e}")))
    }

    fn parallax_rmi(
        &self,
        parallax_path: &Path,
        ctx: &PodmanCtx,
        image: &str,
    ) -> Result<(), AppError> {
        pmd::parallax_rmi(&parallax_path.to_path_buf(), ctx, image)
            .map_err(|e| AppError::Runtime(format!("Failed removing image from parallax: {e}")))
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

    fn kube_play(&self, filepath: &str, run_ctx: &PodmanCtx) -> Result<(), AppError> {
        pmd::kube_play(filepath, Some(run_ctx));
        Ok(())
    }
}

pub struct RealUserContext;

impl UserContext for RealUserContext {
    fn current_user(&self) -> Result<CurrentUser, AppError> {
        let uid = users::get_current_uid();
        let gid = users::get_current_gid();
        let username = users::get_user_by_uid(uid)
            .ok_or_else(|| {
                AppError::UserLookup(String::from(
                    "Failed to resolve current user from passwd database",
                ))
            })?
            .name()
            .to_string_lossy()
            .into_owned();

        Ok(CurrentUser { uid, gid, username })
    }
}

/// Context for running containers. Fully custom Sarus Suite parameters (Podman module, Parallax imagestore, etc.).
/// Uses sarusctl-specific graphroot and runroot to not tamper with default podman rootdirs
pub fn build_run_ctx(config: &Config, user: &CurrentUser) -> PodmanCtx {
    let roots_base = PathBuf::from("/dev/shm")
        .join(&user.username)
        .join("sarusctl");

    PodmanCtx {
        podman_path: PathBuf::from(&config.podman_path),
        module: Some(config.podman_module.clone()),
        graphroot: Some(roots_base.join("graphroot")),
        runroot: Some(roots_base.join("runroot")),
        parallax_mount_program: Some(PathBuf::from(&config.parallax_mount_program)),
        ro_store: Some(PathBuf::from(&config.parallax_imagestore)),
        podman_env: None,
    }
    .with_env("PARALLAX_MP_UID", user.uid.to_string())
    .with_env("PARALLAX_MP_GID", user.gid.to_string())
    .with_env(
        "PARALLAX_MP_LOGFILE",
        format!("/tmp/parallax-{}/mount_program.log", user.uid),
    )
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

pub fn extract_images_from_yaml_str(contents: &str) -> Result<Vec<String>, AppError> {
    let manifest = yaml_serde::from_str::<yaml_serde::Value>(contents)
        .map_err(|e| AppError::Yaml(e.to_string()))?;
    let mut images = BTreeSet::new();
    collect_yaml_images(&manifest, &mut images);
    Ok(images.into_iter().collect())
}

pub fn extract_images_from_yaml_manifest(filepath: &Path) -> Result<Vec<String>, AppError> {
    let contents = fs::read_to_string(filepath)
        .map_err(|e| AppError::Io(format!("failed to read {}: {e}", filepath.display())))?;
    extract_images_from_yaml_str(&contents)
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
    match command {
        CommandSpec::Validate { filepath, .. } => validate_command(&filepath, deps),
        CommandSpec::Render { filepath, .. } => render_command(&filepath, deps),
        CommandSpec::Images => images_command(deps),
        CommandSpec::Pull { image } => {
            // pull_command() and migrate_command() are also called internally by other functions,
            // so they receive the config from the outside to avoid loading it multiple times.
            let config = deps.raster.load_config()?;
            pull_command(&image, &config, deps)
        }
        CommandSpec::Migrate { image } => {
            let config = deps.raster.load_config()?;
            migrate_command(&image, &config, deps)
        }
        CommandSpec::Rmi { image } => rmi_command(&image, deps),
        CommandSpec::Run {
            filepath,
            container_cmd,
        } => run_command(&filepath, &container_cmd, deps),
    }
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

fn images_command(deps: &AppDeps<'_>) -> Result<AppOutput, AppError> {
    let config = deps.raster.load_config()?;
    let seed_ctx = build_parallax_seed_ctx(&config);

    // We need to find and explicitly state the graphroot because it needs to be passed to Parallax under the hood.
    // Not necessary on pull context because that's a plain Podman invocation, and Podman resolves the graphroot location on its own.
    let graphroot = deps.runtime.default_graphroot(&seed_ctx)?;
    let ctx = PodmanCtx {
        graphroot: Some(graphroot),
        ..seed_ctx
    };

    if let Some(ro_store) = ctx.ro_store.as_deref()
        && !fs::exists(ro_store).map_err(|e| {
            AppError::Io(format!(
                "Failed to check for existence of Parallax imagestore directory: {e}"
            ))
        })?
    {
        fs::create_dir_all(ro_store).map_err(|e| {
            AppError::Io(format!(
                "Failed to create Parallax imagestore directory: {e}"
            ))
        })?;
    }

    deps.runtime.images(&ctx)?;
    Ok(AppOutput::success(""))
}

fn pull_command(image: &str, config: &Config, deps: &AppDeps<'_>) -> Result<AppOutput, AppError> {
    let ctx = build_pull_ctx(config);
    deps.runtime.pull(image, &ctx)?;
    if !deps.runtime.image_exists(image, &ctx)? {
        return Err(AppError::Runtime(format!(
            "Image {image} is still missing after pull"
        )));
    }

    let mut output = AppOutput::success(format!("Pulled {image} with Podman"));
    merge_output(&mut output, migrate_command(image, config, deps)?);
    Ok(output)
}

fn migrate_command(
    image: &str,
    config: &Config,
    deps: &AppDeps<'_>,
) -> Result<AppOutput, AppError> {
    let seed_ctx = build_parallax_seed_ctx(config);

    // We need to find and explicitly state the graphroot because it needs to be passed to Parallax under the hood.
    // Not necessary on pull context because that's a plain Podman invocation, and Podman resolves the graphroot location on its own.
    let graphroot = deps.runtime.default_graphroot(&seed_ctx)?;
    let ctx = PodmanCtx {
        graphroot: Some(graphroot),
        ..seed_ctx
    };
    let parallax_path = PathBuf::from(&config.parallax_path);

    deps.runtime.parallax_migrate(&parallax_path, &ctx, image)?;
    if !deps.runtime.image_exists(image, &ctx)? {
        return Err(AppError::Runtime(format!(
            "Image {image} is still missing after migration"
        )));
    }

    Ok(AppOutput::success(format!(
        "Migrated {image} with Parallax"
    )))
}

fn rmi_command(image: &str, deps: &AppDeps<'_>) -> Result<AppOutput, AppError> {
    let config = deps.raster.load_config()?;
    let seed_ctx = build_parallax_seed_ctx(&config);

    // We need to find and explicitly state the graphroot because it needs to be passed to Parallax under the hood.
    // Not necessary on pull context because that's a plain Podman invocation, and Podman resolves the graphroot location on its own.
    let graphroot = deps.runtime.default_graphroot(&seed_ctx)?;
    let ctx = PodmanCtx {
        graphroot: Some(graphroot),
        ..seed_ctx
    };
    let parallax_path = PathBuf::from(&config.parallax_path);

    deps.runtime.parallax_rmi(&parallax_path, &ctx, image)?;
    Ok(AppOutput::success(""))
}

fn run_command(
    filepath: &str,
    container_cmd: &[String],
    deps: &AppDeps<'_>,
) -> Result<AppOutput, AppError> {
    match deps.raster.render(filepath) {
        Ok(edf) => {
            // Loading config in each branch is a small duplication,
            // but allows to integration test invalid EDF cases without needing a valid config present
            let config = deps.raster.load_config()?;
            run_edf_command(&edf, container_cmd, &config, deps)
        }
        Err(_) => {
            let contents = fs::read_to_string(filepath)
                .map_err(|e| AppError::Io(format!("failed to read {filepath}: {e}")))?;
            yaml_serde::from_str::<yaml_serde::Value>(&contents).map_err(|_| {
                AppError::UnsupportedInput(format!("{filepath} is not valid EDF nor YAML"))
            })?;

            let config = deps.raster.load_config()?;
            run_yaml_command(filepath, &config, deps)
        }
    }
}

fn run_edf_command(
    edf: &EDF,
    container_cmd: &[String],
    config: &Config,
    deps: &AppDeps<'_>,
) -> Result<AppOutput, AppError> {
    let user = deps.user.current_user()?;
    let run_ctx = build_run_ctx(config, &user);
    let mut output = AppOutput::success("");

    // TODO checking image existence with the run_ctx means that sarusctl custom rootdirs will be created right here. Ensure proper cleanup of graphroot/runroot and parent in case of failure
    if !deps.runtime.image_exists(&edf.image, &run_ctx)? {
        merge_output(&mut output, pull_command(&edf.image, config, deps)?);
    }

    let container_name = format!("sarusctl-{}", &Uuid::new_v4().simple().to_string()[..8]);
    let c_ctx = ContainerCtx {
        name: container_name,
        interactive: true,
        detach: false,
        set_env: true,
        pidfile: None,
    };

    output.return_code = deps
        .runtime
        .run_from_edf(edf, &run_ctx, &c_ctx, container_cmd)?;
    // TODO evaluate whether we can do some cleanup of podman rootdirs when run_from_edf returns so we can have non-persistent graphroot/runroot for runs, removing the need to have human-understandable paths for them. The intent then would be to create them inside something like /dev/shm/sarusctl-{uid}/ (removed afterwards) and skip the resolution of full username from uid, which is problematic when statically compiling with musl libc (does not access LDAP/NSS in system with centralized identity management)
    Ok(output)
}

fn run_yaml_command(
    filepath: &str,
    config: &Config,
    deps: &AppDeps<'_>,
) -> Result<AppOutput, AppError> {
    let user = deps.user.current_user()?;
    let mut run_ctx = build_run_ctx(config, &user);
    run_ctx.module = None;

    let images = extract_images_from_yaml_manifest(Path::new(filepath))?;
    let mut output = AppOutput::success("");

    for image in images {
        // TODO checking image existence with the run_ctx means that sarusctl custom rootdirs will be created right here. Ensure proper cleanup of graphroot/runroot and parent in case of failure
        if !deps.runtime.image_exists(&image, &run_ctx)? {
            merge_output(&mut output, pull_command(&image, config, deps)?);
        }
    }

    deps.runtime.kube_play(filepath, &run_ctx)?;
    // TODO podman exec user command into container marked with specific extension
    // TODO tear down pod with kube_down after user command completes, and report any errors from that as well
    // TODO cleanup sarusct custom rootdirs
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
            parallax_imagestore: String::from("/scratch/user/parallax/store"),
            parallax_mount_program: String::from("/usr/local/bin/parallax_mount_program"),
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
        graphroot: Result<PathBuf, AppError>,
        image_exists: RefCell<HashMap<String, VecDeque<bool>>>,
        pull_results: RefCell<HashMap<String, Result<(), AppError>>>,
        migrate_results: RefCell<HashMap<String, Result<(), AppError>>>,
        rmi_results: RefCell<HashMap<String, Result<(), AppError>>>,
        run_result: Result<i32, AppError>,
        kube_play_result: Result<(), AppError>,
    }

    impl FakeContainerRuntime {
        fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                graphroot: Ok(PathBuf::from("/graphroot")),
                image_exists: RefCell::new(HashMap::new()),
                pull_results: RefCell::new(HashMap::new()),
                migrate_results: RefCell::new(HashMap::new()),
                rmi_results: RefCell::new(HashMap::new()),
                run_result: Ok(0),
                kube_play_result: Ok(()),
            }
        }

        fn push_image_exists(&self, image: &str, values: Vec<bool>) {
            self.image_exists
                .borrow_mut()
                .insert(image.to_string(), VecDeque::from(values));
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
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

        fn pull(&self, image: &str, _ctx: &PodmanCtx) -> Result<(), AppError> {
            self.calls.borrow_mut().push(format!("pull:{image}"));
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

        fn parallax_migrate(
            &self,
            _parallax_path: &Path,
            _ctx: &PodmanCtx,
            image: &str,
        ) -> Result<(), AppError> {
            self.calls.borrow_mut().push(format!("migrate:{image}"));
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
        ) -> Result<(), AppError> {
            self.calls.borrow_mut().push(format!("rmi:{image}"));
            self.rmi_results
                .borrow_mut()
                .remove(image)
                .unwrap_or(Ok(()))
        }

        fn run_from_edf(
            &self,
            edf: &EDF,
            _run_ctx: &PodmanCtx,
            _container_ctx: &ContainerCtx,
            _container_cmd: &[String],
        ) -> Result<i32, AppError> {
            self.calls.borrow_mut().push(format!("run:{}", edf.image));
            self.run_result.clone()
        }

        fn kube_play(&self, filepath: &str, _run_ctx: &PodmanCtx) -> Result<(), AppError> {
            self.calls
                .borrow_mut()
                .push(format!("kube_play:{filepath}"));
            self.kube_play_result.clone()
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

        let images = extract_images_from_yaml_str(yaml).unwrap();
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

        let images = extract_images_from_yaml_str(yaml).unwrap();
        assert_eq!(images, vec![String::from("alpine:3.22")]);
    }

    #[test]
    fn extract_images_from_yaml_str_rejects_invalid_yaml() {
        let err = extract_images_from_yaml_str("apiVersion: [").unwrap_err();
        assert!(matches!(err, AppError::Yaml(_)));
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
        let user = CurrentUser {
            uid: 1234,
            gid: 4321,
            username: String::from("alice"),
        };

        let run = build_run_ctx(&config, &user);
        assert_eq!(run.module, Some(String::from("hpc")));
        assert_eq!(
            run.graphroot,
            Some(PathBuf::from("/dev/shm/alice/sarusctl/graphroot"))
        );
        assert_eq!(
            run.runroot,
            Some(PathBuf::from("/dev/shm/alice/sarusctl/runroot"))
        );
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
            user: CurrentUser {
                uid: 1,
                gid: 1,
                username: String::from("user"),
            },
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
            user: CurrentUser {
                uid: 1,
                gid: 1,
                username: String::from("user"),
            },
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
            user: CurrentUser {
                uid: 1,
                gid: 1,
                username: String::from("user"),
            },
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
            user: CurrentUser {
                uid: 1,
                gid: 1,
                username: String::from("user"),
            },
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
        runtime.push_image_exists("alpine:3.22", vec![true, true]);
        let user = FakeUserContext {
            user: CurrentUser {
                uid: 1,
                gid: 1,
                username: String::from("user"),
            },
        };

        let output = execute_command(
            CommandSpec::Pull {
                image: String::from("alpine:3.22"),
            },
            &mock_deps(&raster, &runtime, &user),
        )
        .unwrap();

        assert_eq!(output.return_code, 0);
        assert_eq!(
            runtime.calls(),
            vec![
                String::from("pull:alpine:3.22"),
                String::from("image_exists:alpine:3.22"),
                String::from("default_graphroot"),
                String::from("migrate:alpine:3.22"),
                String::from("image_exists:alpine:3.22")
            ]
        );
    }

    #[test]
    fn migrate_success_runs_migration_steps() {
        let config = sample_config();
        let raster = FakeRasterOps::new(config);
        let runtime = FakeContainerRuntime::new();
        runtime.push_image_exists("alpine:3.22", vec![true]);
        let user = FakeUserContext {
            user: CurrentUser {
                uid: 1,
                gid: 1,
                username: String::from("user"),
            },
        };

        let output = execute_command(
            CommandSpec::Migrate {
                image: String::from("alpine:3.22"),
            },
            &mock_deps(&raster, &runtime, &user),
        )
        .unwrap();

        assert_eq!(output.return_code, 0);
        assert_eq!(
            runtime.calls(),
            vec![
                String::from("default_graphroot"),
                String::from("migrate:alpine:3.22"),
                String::from("image_exists:alpine:3.22")
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
            user: CurrentUser {
                uid: 1,
                gid: 1,
                username: String::from("user"),
            },
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
            user: CurrentUser {
                uid: 1,
                gid: 1,
                username: String::from("user"),
            },
        };

        let output = execute_command(
            CommandSpec::Rmi {
                image: String::from("alpine:3.22"),
            },
            &mock_deps(&raster, &runtime, &user),
        )
        .unwrap();

        assert_eq!(output, AppOutput::success(""));
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
            user: CurrentUser {
                uid: 1,
                gid: 1,
                username: String::from("user"),
            },
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
        runtime.push_image_exists("alpine:3.22", vec![true]);
        let user = FakeUserContext {
            user: CurrentUser {
                uid: 1,
                gid: 1,
                username: String::from("user"),
            },
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
                String::from("image_exists:alpine:3.22"),
                String::from("run:alpine:3.22")
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
        runtime.push_image_exists("alpine:3.22", vec![false, true, true]);
        let user = FakeUserContext {
            user: CurrentUser {
                uid: 1,
                gid: 1,
                username: String::from("user"),
            },
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
        assert!(output.stdout.contains("Pulled alpine:3.22 with Podman"));
        assert!(output.stdout.contains("Migrated alpine:3.22 with Parallax"));
        assert_eq!(
            runtime.calls(),
            vec![
                String::from("image_exists:alpine:3.22"),
                String::from("pull:alpine:3.22"),
                String::from("image_exists:alpine:3.22"),
                String::from("default_graphroot"),
                String::from("migrate:alpine:3.22"),
                String::from("image_exists:alpine:3.22"),
                String::from("run:alpine:3.22")
            ]
        );
    }

    #[test]
    fn run_edf_fails_before_run_when_pull_fails() {
        let mut raster = FakeRasterOps::new(sample_config());
        raster
            .render_results
            .insert(String::from("job.edf"), Ok(sample_edf("alpine:3.22")));
        let runtime = FakeContainerRuntime::new();
        runtime.push_image_exists("alpine:3.22", vec![false]);
        runtime.pull_results.borrow_mut().insert(
            String::from("alpine:3.22"),
            Err(AppError::Runtime(String::from("registry offline"))),
        );
        let user = FakeUserContext {
            user: CurrentUser {
                uid: 1,
                gid: 1,
                username: String::from("user"),
            },
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
                String::from("image_exists:alpine:3.22"),
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
spec:
  containers:
    - image: alpine:3.22
    - image: ubuntu:24.04
"#,
        )
        .unwrap();

        let mut raster = FakeRasterOps::new(sample_config());
        raster.render_results.insert(
            manifest.to_string_lossy().into_owned(),
            Err(String::from("not an edf")),
        );
        let runtime = FakeContainerRuntime::new();
        runtime.push_image_exists("alpine:3.22", vec![false, true, true]);
        runtime.push_image_exists("ubuntu:24.04", vec![true]);
        let user = FakeUserContext {
            user: CurrentUser {
                uid: 1,
                gid: 1,
                username: String::from("user"),
            },
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
        assert_eq!(
            runtime.calls(),
            vec![
                String::from("image_exists:alpine:3.22"),
                String::from("pull:alpine:3.22"),
                String::from("image_exists:alpine:3.22"),
                String::from("default_graphroot"),
                String::from("migrate:alpine:3.22"),
                String::from("image_exists:alpine:3.22"),
                String::from("image_exists:ubuntu:24.04"),
                format!("kube_play:{}", manifest.to_string_lossy())
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
spec:
  containers:
    - image: alpine:3.22
"#,
        )
        .unwrap();

        let mut raster = FakeRasterOps::new(sample_config());
        raster.render_results.insert(
            manifest.to_string_lossy().into_owned(),
            Err(String::from("not an edf")),
        );
        let runtime = FakeContainerRuntime::new();
        runtime.push_image_exists("alpine:3.22", vec![false, true]);
        runtime.migrate_results.borrow_mut().insert(
            String::from("alpine:3.22"),
            Err(AppError::Runtime(String::from("parallax broke"))),
        );
        let user = FakeUserContext {
            user: CurrentUser {
                uid: 1,
                gid: 1,
                username: String::from("user"),
            },
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
                String::from("image_exists:alpine:3.22"),
                String::from("pull:alpine:3.22"),
                String::from("image_exists:alpine:3.22"),
                String::from("default_graphroot"),
                String::from("migrate:alpine:3.22"),
            ]
        );
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
            user: CurrentUser {
                uid: 1,
                gid: 1,
                username: String::from("user"),
            },
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
