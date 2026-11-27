//! High-level provisioning from a registry reference into the Parallax runtime store.

use crate::{
    Error, ImageSelector, ListReport, LocalRef, OverlayStorage, PullContext, PullOptions,
    RemoveReport, Store, StoreConfig, TransferContext,
};
use raster::{Config, hook_run};
use sarus_suite_podman_driver::{self as podman_driver, PodmanCtx};
use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
    str,
};

pub type ProvisionResult<T> = std::result::Result<T, ProvisionError>;

/// Failure while making an image available in the Parallax runtime store.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProvisionError {
    #[error(transparent)]
    Store(#[from] Error),
    #[error(transparent)]
    Driver(#[from] podman_driver::DriverError),
    #[error("Sarus Suite configuration operation failed: {source}")]
    Configuration {
        #[from]
        source: raster::error::SarusError,
    },
    #[error("failed to {operation} {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid {kind} store location {path}: {reason}")]
    InvalidStoreLocation {
        kind: &'static str,
        path: PathBuf,
        reason: &'static str,
    },
    #[error("hook `{command}` failed ({status}): {stderr}")]
    HookFailed {
        command: String,
        status: std::process::ExitStatus,
        stderr: String,
    },
    #[error("cannot derive provisioning lock path from store root {store_root}")]
    InvalidLockPath { store_root: PathBuf },
    #[error("image {reference} is unavailable in Parallax after migration")]
    ImageUnavailable { reference: String },
    #[error("Podman returned an invalid default graphroot: {reason}")]
    InvalidGraphroot { reason: String },
}

/// Coordinates the authoritative image store and the Parallax runtime cache.
pub struct ImageService<'a> {
    config: &'a Config,
    store: Store,
    lock_path: PathBuf,
    stream_output: bool,
}

impl<'a> ImageService<'a> {
    pub fn new(config: &'a Config) -> ProvisionResult<Self> {
        let store_config = StoreConfig::new(&config.hpc_image_store)?;
        let lock_path = provision_lock_path(store_config.root())?;
        Ok(Self {
            config,
            store: Store::new(store_config),
            lock_path,
            stream_output: false,
        })
    }

    /// Stream Parallax migration output to the caller's terminal
    pub fn with_streaming_output(mut self, enabled: bool) -> Self {
        self.stream_output = enabled;
        self
    }

    /// Ensure that an image is available to Parallax and return its resolved selector.
    pub fn ensure_image(
        &self,
        reference: &str,
        temporary_directory: &Path,
    ) -> ProvisionResult<ImageSelector> {
        self.provision_image(reference, temporary_directory, Acquisition::CachedOrPull)
    }

    /// Refresh an image, make it available to Parallax, and return its resolved selector.
    pub fn pull_image(
        &self,
        reference: &str,
        temporary_directory: &Path,
    ) -> ProvisionResult<ImageSelector> {
        self.provision_image(reference, temporary_directory, Acquisition::Refresh)
    }

    /// Ensure that the configured Parallax runtime store is ready for use.
    pub fn prepare_runtime_store(&self) -> ProvisionResult<()> {
        if self.inspect_parallax_store()? == StoreState::Ready {
            return Ok(());
        }

        let _mutation_lock = ProvisionLock::acquire(&self.lock_path)?;
        // Another process may have initialized the runtime store while this
        // process waited for the service-wide mutation lock.
        self.ensure_runtime_store()
    }

    /// Return the authoritative image inventory.
    pub fn list_images(&self) -> ProvisionResult<ListReport> {
        self.store.list().map_err(Into::into)
    }

    /// Remove an exact tagged reference from Parallax and the authoritative image store.
    /// Runtime cleanup happens first, so its failure leaves the authoritative ref retryable.
    pub fn remove_image(&self, reference: &str) -> ProvisionResult<RemoveReport> {
        let reference = LocalRef::new(reference)?;
        let _mutation_lock = ProvisionLock::acquire(&self.lock_path)?;

        // Resolve before touching Parallax so a missing or damaged authoritative
        // reference has no runtime-store side effects. Provisioning migrates the
        // digest selector, so use that same identity for removal.
        let selector = ImageSelector::new(reference.to_string())?;
        let resolved = self
            .store
            .resolve(&selector)?
            .ok_or_else(|| Error::RefMissing(reference.clone()))?;

        let context = self.parallax_removal_context()?;
        podman_driver::parallax_rmi(
            &PathBuf::from(&self.config.parallax_path),
            &context,
            &resolved,
        )?;

        // Parallax is a reconstructible cache. Removing it first leaves the
        // authoritative reference available when runtime cleanup fails.
        self.store.remove(&reference).map_err(Into::into)
    }

    fn provision_image(
        &self,
        reference: &str,
        temporary_directory: &Path,
        acquisition: Acquisition,
    ) -> ProvisionResult<ImageSelector> {
        let selector = ImageSelector::new(reference)?;
        // TODO: future optimization to more narrowly scope to only when we need exclusive locks
        let _mutation_lock = ProvisionLock::acquire(&self.lock_path)?;

        self.ensure_store_locations()?;
        let digest_reference = match acquisition {
            Acquisition::CachedOrPull => self.resolve_or_pull(&selector)?,
            Acquisition::Refresh => self.pull(&selector, true)?,
        };
        let parallax_context = self.parallax_context(temporary_directory);
        let parallax_path = PathBuf::from(&self.config.parallax_path);

        // Fast path: the image is already provisioned in Parallax
        if podman_driver::parallax_exist(&parallax_path, &parallax_context, &digest_reference)? {
            return ImageSelector::new(digest_reference).map_err(Into::into);
        }

        let local_context = self.local_context(temporary_directory);
        let _staging_cleanup = StagingImageCleanup::new(digest_reference.clone(), local_context);

        self.export_to_job_storage(&digest_reference, temporary_directory)?;

        let migration_error = if self.stream_output {
            podman_driver::parallax_migrate_streaming(
                &parallax_path,
                &parallax_context,
                &digest_reference,
            )
            .err()
        } else {
            podman_driver::parallax_migrate(&parallax_path, &parallax_context, &digest_reference)
                .err()
        };

        // As we migrated, image should now be present or we return an error
        if !podman_driver::parallax_exist(&parallax_path, &parallax_context, &digest_reference)? {
            return match migration_error {
                Some(error) => Err(error.into()),
                None => Err(ProvisionError::ImageUnavailable {
                    reference: digest_reference,
                }),
            };
        }

        ImageSelector::new(digest_reference).map_err(Into::into)
    }

    fn resolve_or_pull(&self, selector: &ImageSelector) -> ProvisionResult<String> {
        match self.store.resolve(selector)? {
            Some(reference) => Ok(reference),
            None => self.pull(selector, false),
        }
    }

    fn pull(&self, selector: &ImageSelector, replace: bool) -> ProvisionResult<String> {
        let context = PullContext {
            skopeo: PathBuf::from(&self.config.skopeo_path),
            ..PullContext::default()
        };
        match self.store.pull(
            selector,
            None,
            &context,
            &PullOptions {
                replace,
                ..PullOptions::default()
            },
        ) {
            Ok(report) => Ok(format!(
                "{}@{}",
                report.local_ref.name(),
                report.stored_root
            )),
            // A concurrent job pull might have completed as we waited the pull lock.
            // Treat the conflict as a handoff and read the result rather than replacing it.
            Err(Error::RefExists(_)) => self
                .store
                .resolve(selector)?
                .ok_or_else(|| Error::DigestMissing(selector.clone()).into()),
            Err(error) => Err(error.into()),
        }
    }

    fn export_to_job_storage(
        &self,
        digest_reference: &str,
        temporary_directory: &Path,
    ) -> ProvisionResult<()> {
        let graphroot = temporary_directory.join("graphroot");
        let runroot = temporary_directory.join("runroot");
        let context = TransferContext {
            podman: PathBuf::from(&self.config.podman_path),
            skopeo: PathBuf::from(&self.config.skopeo_path),
            platform: None,
            storage: Some(OverlayStorage::new(graphroot, runroot)?),
        };
        let selector = ImageSelector::new(digest_reference)?;
        self.store.export(&selector, None, &context)?;
        Ok(())
    }

    fn ensure_store_locations(&self) -> ProvisionResult<()> {
        // Quickly check both stores without modifying them
        let parallax_state = self.inspect_parallax_store()?;
        let hpc_state = self.inspect_hpc_store()?;

        // Initialize missing stores and resume interrupted initialization.
        match hpc_state {
            StoreState::Ready => {}
            StoreState::NeedsInitialization => {
                self.store.initialize()?;
                self.store.require_initialized()?;
            }
        }

        match parallax_state {
            StoreState::Ready => {}
            StoreState::NeedsInitialization => {
                self.initialize_parallax_store()?;
            }
        }
        Ok(())
    }

    fn ensure_runtime_store(&self) -> ProvisionResult<()> {
        match self.inspect_parallax_store()? {
            StoreState::Ready => Ok(()),
            StoreState::NeedsInitialization => self.initialize_parallax_store(),
        }
    }

    // TODO: this should be a parallax store function
    fn inspect_parallax_store(&self) -> ProvisionResult<StoreState> {
        let path = Path::new(&self.config.parallax_imagestore);
        if path.as_os_str().is_empty() {
            return Err(ProvisionError::InvalidStoreLocation {
                kind: "Parallax image",
                path: path.to_owned(),
                reason: "path is empty",
            });
        }

        match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => Ok(StoreState::Ready),
            Ok(_) => Err(ProvisionError::InvalidStoreLocation {
                kind: "Parallax image",
                path: path.to_owned(),
                reason: "location exists but is not a directory",
            }),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                Ok(StoreState::NeedsInitialization)
            }
            Err(source) => Err(ProvisionError::Io {
                operation: "inspect Parallax image store",
                path: path.to_owned(),
                source,
            }),
        }
    }

    // TODO: This should be a hpc store function in the future
    fn inspect_hpc_store(&self) -> ProvisionResult<StoreState> {
        match self.store.require_initialized() {
            Ok(()) => Ok(StoreState::Ready),
            Err(Error::MissingStore(_) | Error::InitializationIncomplete { .. }) => {
                Ok(StoreState::NeedsInitialization)
            }
            Err(error) => Err(error.into()),
        }
    }

    // TODO: this should be a parallax function in the future
    fn initialize_parallax_store(&self) -> ProvisionResult<()> {
        let location = self.config.parallax_imagestore.as_str();
        let path = Path::new(location);

        if let Some(execution) =
            hook_run(self.config, "parallax_imagestore_create", vec![location])?
            && !execution.output.status.success()
        {
            return Err(ProvisionError::HookFailed {
                command: execution.command,
                status: execution.output.status,
                stderr: String::from_utf8_lossy(&execution.output.stderr).into_owned(),
            });
        }

        std::fs::create_dir_all(path).map_err(|source| ProvisionError::Io {
            operation: "create Parallax image-store directory",
            path: path.to_owned(),
            source,
        })?;

        match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => Ok(()),
            Ok(_) => Err(ProvisionError::InvalidStoreLocation {
                kind: "Parallax image",
                path: path.to_owned(),
                reason: "initialization did not produce a directory",
            }),
            Err(source) => Err(ProvisionError::Io {
                operation: "validate initialized Parallax image store",
                path: path.to_owned(),
                source,
            }),
        }
    }

    fn parallax_context(&self, temporary_directory: &Path) -> PodmanCtx {
        self.context(
            temporary_directory,
            Some(PathBuf::from(&self.config.parallax_imagestore)),
            Some(PathBuf::from(&self.config.mksquashfs_path)),
        )
    }

    fn local_context(&self, temporary_directory: &Path) -> PodmanCtx {
        self.context(temporary_directory, None, None)
    }

    fn parallax_removal_context(&self) -> ProvisionResult<PodmanCtx> {
        let mut context = PodmanCtx {
            podman_path: PathBuf::from(&self.config.podman_path),
            conmon_path: Some(PathBuf::from(&self.config.conmon_path)),
            module: None,
            graphroot: None,
            runroot: None,
            parallax_mount_program: None,
            ro_store: Some(PathBuf::from(&self.config.parallax_imagestore)),
            mksquashfs_path: Some(PathBuf::from(&self.config.mksquashfs_path)),
            podman_env: None,
        };
        let output = podman_driver::info(Some("{{.Store.GraphRoot}}"), Some(&context))?;
        let graphroot =
            str::from_utf8(&output.stdout).map_err(|error| ProvisionError::InvalidGraphroot {
                reason: error.to_string(),
            })?;
        context.graphroot = Some(PathBuf::from(graphroot.trim()));
        Ok(context)
    }

    fn context(
        &self,
        temporary_directory: &Path,
        read_only_store: Option<PathBuf>,
        mksquashfs_path: Option<PathBuf>,
    ) -> PodmanCtx {
        PodmanCtx {
            podman_path: PathBuf::from(&self.config.podman_path),
            conmon_path: Some(PathBuf::from(&self.config.conmon_path)),
            module: None,
            graphroot: Some(temporary_directory.join("graphroot")),
            runroot: Some(temporary_directory.join("runroot")),
            parallax_mount_program: None,
            ro_store: read_only_store,
            mksquashfs_path,
            podman_env: None,
        }
        .with_env("PARALLAX_MP_UID", self.config.parallax_mp_uid.to_string())
        .with_env("PARALLAX_MP_GID", self.config.parallax_mp_gid.to_string())
        .with_env(
            "PARALLAX_MP_SQUASHFUSE_CMD",
            self.config.parallax_mp_squashfuse_path.clone(),
        )
        .with_env(
            "PARALLAX_MP_LOGFILE",
            self.config.parallax_mp_logfile.clone(),
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Acquisition {
    CachedOrPull,
    Refresh,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StoreState {
    Ready,
    NeedsInitialization,
}

/// Removes the image exported to the job's temporary Podman storage when provisioning ends.
///
/// The guard is created before export so cleanup also runs when export only partially completes,
/// or when migration or Parallax verification fails afterwards.
struct StagingImageCleanup {
    image: String,
    context: PodmanCtx,
}

impl StagingImageCleanup {
    fn new(image: String, context: PodmanCtx) -> Self {
        Self { image, context }
    }
}

impl Drop for StagingImageCleanup {
    fn drop(&mut self) {
        if let Err(error) = podman_driver::rmi(&self.image, Some(&self.context)) {
            eprintln!(
                "hpc-image-store: warning: failed to remove provisioned image {} from temporary Podman storage: {error}",
                self.image
            );
        }
    }
}

fn provision_lock_path(store_root: &Path) -> ProvisionResult<PathBuf> {
    let parent = store_root
        .parent()
        .ok_or_else(|| ProvisionError::InvalidLockPath {
            store_root: store_root.to_owned(),
        })?;
    let file_name = store_root
        .file_name()
        .ok_or_else(|| ProvisionError::InvalidLockPath {
            store_root: store_root.to_owned(),
        })?;
    let mut lock_name = OsString::from(file_name);
    lock_name.push(".provision.lock");
    Ok(parent.join(lock_name))
}

#[derive(Debug)]
struct ProvisionLock {
    _lock_file: File,
}

impl ProvisionLock {
    fn acquire(path: &Path) -> ProvisionResult<Self> {
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|source| ProvisionError::Io {
                operation: "open provisioner lock",
                path: path.to_owned(),
                source,
            })?;
        let metadata = lock_file.metadata().map_err(|source| ProvisionError::Io {
            operation: "inspect provisioner lock",
            path: path.to_owned(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(ProvisionError::Io {
                operation: "validate provisioner lock",
                path: path.to_owned(),
                source: io::Error::new(io::ErrorKind::InvalidInput, "lock is not a regular file"),
            });
        }

        loop {
            // `lock_file` owns this descriptor for the lifetime of the lock.
            // Note: the flock call is a blocking one if held by another process as per LOCK_EX
            // Note 2: If provisioner dies, the kernel will free the flock
            if unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                // The descriptor is live and owned by `lock_file`.
                let flags = unsafe { libc::fcntl(lock_file.as_raw_fd(), libc::F_GETFD) };
                if flags == -1 {
                    return Err(ProvisionError::Io {
                        operation: "read provisioner lock descriptor flags",
                        path: path.to_owned(),
                        source: io::Error::last_os_error(),
                    });
                }

                let inheritable_flags = flags & !libc::FD_CLOEXEC;
                // The descriptor is live and `inheritable_flags` was
                // derived from the descriptor flags returned by F_GETFD.
                if unsafe { libc::fcntl(lock_file.as_raw_fd(), libc::F_SETFD, inheritable_flags) }
                    == -1
                {
                    return Err(ProvisionError::Io {
                        operation: "configure provisioner lock inheritance",
                        path: path.to_owned(),
                        source: io::Error::last_os_error(),
                    });
                }
                return Ok(Self {
                    _lock_file: lock_file,
                });
            }
            let source = io::Error::last_os_error();
            if source.kind() != io::ErrorKind::Interrupted {
                return Err(ProvisionError::Io {
                    operation: "acquire provisioner lock",
                    path: path.to_owned(),
                    source,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn provisioner_config(directory: &Path) -> Config {
        Config {
            hpc_image_store: directory.join("store").display().to_string(),
            parallax_imagestore: directory.join("parallax").display().to_string(),
            ..Config::default()
        }
    }

    fn write_hook(path: &Path, body: &str) {
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    #[test]
    fn provision_lock_is_a_sibling_of_the_store() {
        assert_eq!(
            provision_lock_path(Path::new("/scratch/user/images")).unwrap(),
            PathBuf::from("/scratch/user/images.provision.lock")
        );
    }

    #[test]
    fn provision_lock_rejects_symlink_redirection() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        File::create(&target).unwrap();
        let lock = directory.path().join("store.provision.lock");
        symlink(&target, &lock).unwrap();

        let error = ProvisionLock::acquire(&lock).unwrap_err();
        assert!(matches!(error, ProvisionError::Io { .. }));
    }

    #[test]
    fn provision_lock_excludes_other_writers_and_releases_on_drop() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("store.provision.lock");
        let lock = ProvisionLock::acquire(&path).unwrap();
        // SAFETY: the lock owns this live descriptor.
        let flags = unsafe { libc::fcntl(lock._lock_file.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0);
        assert_eq!(flags & libc::FD_CLOEXEC, 0);
        let contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();

        // SAFETY: `contender` owns this descriptor for the duration of the calls.
        let result = unsafe { libc::flock(contender.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(result, -1);
        assert_eq!(io::Error::last_os_error().kind(), io::ErrorKind::WouldBlock);

        drop(lock);

        // SAFETY: `contender` still owns this descriptor.
        assert_eq!(
            unsafe { libc::flock(contender.as_raw_fd(), libc::LOCK_EX) },
            0
        );
    }

    #[test]
    fn ready_store_locations_skip_the_creation_hook() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = provisioner_config(directory.path());
        let hook = directory.path().join("create-parallax");
        write_hook(&hook, "exit 97");
        config.hooks.parallax_imagestore_create = hook.display().to_string();

        std::fs::create_dir(&config.parallax_imagestore).unwrap();
        let store = Store::new(StoreConfig::new(&config.hpc_image_store).unwrap());
        store.initialize().unwrap();

        let provisioner = ImageService::new(&config).unwrap();
        provisioner.ensure_store_locations().unwrap();
    }

    #[test]
    fn missing_store_locations_are_initialized_once() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = provisioner_config(directory.path());
        let hook = directory.path().join("create-parallax");
        write_hook(&hook, "mkdir -p \"$1\"");
        config.hooks.parallax_imagestore_create = hook.display().to_string();

        let provisioner = ImageService::new(&config).unwrap();
        provisioner.ensure_store_locations().unwrap();
        assert!(Path::new(&config.parallax_imagestore).is_dir());
        provisioner.store.require_initialized().unwrap();

        // Once both stores are ready, even a failing hook is not invoked.
        write_hook(&hook, "exit 97");
        provisioner.ensure_store_locations().unwrap();
    }

    #[test]
    fn preparing_runtime_store_does_not_initialize_authoritative_store() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = provisioner_config(directory.path());
        let hook = directory.path().join("create-parallax");
        write_hook(&hook, "mkdir -p \"$1\"");
        config.hooks.parallax_imagestore_create = hook.display().to_string();

        let provisioner = ImageService::new(&config).unwrap();
        provisioner.prepare_runtime_store().unwrap();

        assert!(Path::new(&config.parallax_imagestore).is_dir());
        assert!(matches!(
            provisioner.store.require_initialized(),
            Err(Error::MissingStore(_))
        ));
    }

    #[test]
    fn ready_runtime_store_does_not_require_the_mutation_lock() {
        let directory = tempfile::tempdir().unwrap();
        let config = provisioner_config(directory.path());
        std::fs::create_dir(&config.parallax_imagestore).unwrap();
        let lock_path = provision_lock_path(Path::new(&config.hpc_image_store)).unwrap();
        std::fs::create_dir(&lock_path).unwrap();

        let provisioner = ImageService::new(&config).unwrap();
        provisioner.prepare_runtime_store().unwrap();
    }

    #[test]
    fn interrupted_hpc_store_initialization_is_resumed() {
        let directory = tempfile::tempdir().unwrap();
        let config = provisioner_config(directory.path());
        std::fs::create_dir(&config.parallax_imagestore).unwrap();
        let store = Store::new(StoreConfig::new(&config.hpc_image_store).unwrap());
        store.initialize().unwrap();
        File::create(Path::new(&config.hpc_image_store).join(crate::layout::INIT_PENDING)).unwrap();
        assert!(matches!(
            store.require_initialized(),
            Err(Error::InitializationIncomplete { .. })
        ));

        let provisioner = ImageService::new(&config).unwrap();
        provisioner.ensure_store_locations().unwrap();
        provisioner.store.require_initialized().unwrap();
    }

    #[test]
    fn invalid_parallax_location_is_not_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let config = provisioner_config(directory.path());
        File::create(&config.parallax_imagestore).unwrap();

        let provisioner = ImageService::new(&config).unwrap();
        let error = provisioner.ensure_store_locations().unwrap_err();
        assert!(matches!(
            error,
            ProvisionError::InvalidStoreLocation {
                kind: "Parallax image",
                ..
            }
        ));
        assert!(matches!(
            provisioner.store.require_initialized(),
            Err(Error::MissingStore(_))
        ));
    }

    #[test]
    fn malformed_hpc_location_does_not_initialize_parallax() {
        let directory = tempfile::tempdir().unwrap();
        let config = provisioner_config(directory.path());
        std::fs::create_dir(&config.hpc_image_store).unwrap();
        File::create(Path::new(&config.hpc_image_store).join("unexpected")).unwrap();

        let provisioner = ImageService::new(&config).unwrap();
        let error = provisioner.ensure_store_locations().unwrap_err();
        assert!(matches!(
            error,
            ProvisionError::Store(Error::InvalidStore { .. })
        ));
        assert!(!Path::new(&config.parallax_imagestore).exists());
    }

    #[test]
    fn contexts_are_derived_from_config_and_job_directory() {
        let directory = tempfile::tempdir().unwrap();
        let config = Config {
            hpc_image_store: directory.path().join("store").display().to_string(),
            parallax_imagestore: directory.path().join("parallax").display().to_string(),
            podman_path: "/tools/podman".into(),
            conmon_path: "/tools/conmon".into(),
            mksquashfs_path: "/tools/mksquashfs".into(),
            ..Config::default()
        };
        let provisioner = ImageService::new(&config).unwrap();
        let job = directory.path().join("job");

        let context = provisioner.parallax_context(&job);
        assert_eq!(context.podman_path, Path::new("/tools/podman"));
        assert_eq!(
            context.conmon_path.as_deref(),
            Some(Path::new("/tools/conmon"))
        );
        assert_eq!(
            context.graphroot.as_deref(),
            Some(job.join("graphroot").as_path())
        );
        assert_eq!(
            context.runroot.as_deref(),
            Some(job.join("runroot").as_path())
        );
        assert_eq!(
            context.ro_store.as_deref(),
            Some(Path::new(&config.parallax_imagestore))
        );
    }
}
