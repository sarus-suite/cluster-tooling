use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;

pub struct PodmanCtx {
    pub podman_path: PathBuf,
    pub module: Option<String>,
    pub graphroot: Option<PathBuf>,
    pub runroot: Option<PathBuf>,
    pub parallax_mount_program: Option<PathBuf>,
    pub ro_store: Option<PathBuf>,
    pub podman_env: Option<HashMap<OsString, OsString>>,
}

impl PodmanCtx {
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.podman_env
            .get_or_insert_with(HashMap::new)
            .insert(key.into(), value.into());
        self
    }
}

pub struct ContainerCtx {
    pub name: String,
    pub interactive: bool,
    pub tty: bool,
    pub detach: bool,
    /// Remove the container automatically when its process exits.
    pub auto_remove: bool,
    pub set_env: bool,
    pub pidfile: Option<PathBuf>,
    pub user: Option<String>,
}
