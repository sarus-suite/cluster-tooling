use crate::{Error, Result, error::io_error, layout::Layout};
use std::path::{Path, PathBuf};

/// Explicit store location. Relative paths are anchored when configuration is created.
#[derive(Clone, Debug)]
pub struct StoreConfig {
    root: PathBuf,
}
impl StoreConfig {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        if root.as_os_str().is_empty() {
            return Err(Error::InvalidStore {
                path: root,
                reason: "store path is empty",
            });
        }
        let root = if root.is_absolute() {
            root
        } else {
            std::env::current_dir()
                .map_err(|source| io_error("resolve current directory", &root, source))?
                .join(root)
        };
        Ok(Self { root })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// A handle only; constructing it does not open, create, or lock the store.
#[derive(Debug)]
pub struct Store {
    config: StoreConfig,
    pub(crate) layout: Layout,
}
impl Store {
    pub fn new(config: StoreConfig) -> Self {
        let layout = Layout::new(config.root.clone());
        Self { config, layout }
    }
    pub fn config(&self) -> &StoreConfig {
        &self.config
    }

    // Internal until actual image operations provide their public interfaces.
    #[allow(dead_code)]
    pub(crate) fn prepare(
        &self,
        operation: crate::locks::Operation,
    ) -> Result<crate::locks::StoreGuard<'_>> {
        if operation.initializes() {
            crate::initialization::initialize(&self.layout)?;
        } else {
            crate::initialization::validate_read_prerequisites(&self.layout)?;
        }
        let guard = crate::locks::StoreGuard::acquire(&self.layout, operation)?;
        crate::initialization::validate_initialized(&self.layout)?;
        Ok(guard)
    }
}
