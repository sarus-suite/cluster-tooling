use crate::{
    Error, Result,
    error::io_error,
    fsutil, initialization,
    layout::Layout,
    locks::{Mode, Operation, StoreGuard},
    recovery,
};
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

    // Image operations keep this guard until their entire operation finishes.
    #[allow(dead_code)]
    pub(crate) fn prepare(&self, operation: Operation) -> Result<StoreGuard<'_>> {
        let mut guard = self.prepare_initialized(operation)?;

        // If an operation already requires an exclusive guard, it can carry out
        // mutation recovery on its own.
        if operation.store_mode() == Mode::Exclusive {
            recovery::recover(&guard)?;
            return Ok(guard);
        }

        // If recovering a pending mutation is needed, operations using shared guards
        // need to acquire an exclusive guard, recover, re-acquire a shared guard
        // and try again.
        loop {
            if !recovery::pending(&self.layout)? {
                return Ok(guard);
            }
            drop(guard);
            fsutil::checkpoint("recovery.reader-released", self.layout.root())?;
            // Never upgrade flock or hold a second descriptor while locked.
            let recovery = StoreGuard::acquire(&self.layout, Operation::Recovery)?;
            initialization::validate_initialized(&self.layout)?;
            recovery::recover(&recovery)?;
            drop(recovery);
            fsutil::checkpoint("recovery.reader-retry", self.layout.root())?;
            guard = StoreGuard::acquire(&self.layout, operation)?;
            initialization::validate_initialized(&self.layout)?;
        }
    }

    fn prepare_initialized(&self, operation: Operation) -> Result<StoreGuard<'_>> {
        match initialization::validate_read_prerequisites(&self.layout) {
            Ok(()) => {
                let guard = StoreGuard::acquire(&self.layout, operation)?;
                let pending_setup = initialization::pending(&self.layout)?;
                let initialized = initialization::validate_initialized(&self.layout);
                if pending_setup && !operation.initializes() {
                    return Err(Error::InitializationIncomplete {
                        path: self.layout.root().to_owned(),
                    });
                }
                let needs_initialization =
                    pending_setup || matches!(initialized, Err(Error::MissingStore(_)));
                if !operation.initializes() || !needs_initialization {
                    initialized?;
                    return Ok(guard);
                }
                drop(guard);
                fsutil::checkpoint("initialization.operation-released", self.layout.root())?;
            }
            Err(Error::MissingStore(_)) if operation.initializes() => (),
            Err(error) => return Err(error),
        }

        initialization::initialize(&self.layout)?;
        fsutil::checkpoint("initialization.operation-retry", self.layout.root())?;
        let guard = StoreGuard::acquire(&self.layout, operation)?;
        initialization::validate_initialized(&self.layout)?;
        Ok(guard)
    }
}
