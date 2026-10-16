use crate::{
    Error, LocalRef, Result,
    error::io_error,
    initialization::{create_directory, require_regular_file},
    layout::Layout,
};
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    os::{
        fd::{AsRawFd, RawFd},
        unix::fs::OpenOptionsExt,
    },
    path::Path,
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Mode {
    Shared,
    Exclusive,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Exclusive => "exclusive",
        }
    }
}

/// Internal so callers cannot accidentally change the store's wait policy.
#[derive(Clone, Copy)]
struct LockWaitPolicy {
    retry_interval: Duration,
    warning_interval: Duration,
    timeout: Duration,
}

impl Default for LockWaitPolicy {
    fn default() -> Self {
        Self {
            retry_interval: Duration::from_millis(100),
            warning_interval: Duration::from_secs(30),
            timeout: Duration::from_secs(300),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Operation {
    Import,
    Pull,
    Export,
    List,
    Remove,
    Check,
    Prune,
}
impl Operation {
    pub fn initializes(self) -> bool {
        matches!(self, Self::Import | Self::Pull | Self::Remove | Self::Prune)
    }
    pub fn store_mode(self) -> Mode {
        if matches!(self, Self::Prune) {
            Mode::Exclusive
        } else {
            Mode::Shared
        }
    }
    pub fn ref_mode(self) -> Option<Mode> {
        match self {
            Self::Import | Self::Pull | Self::Remove => Some(Mode::Exclusive),
            Self::Export | Self::List | Self::Check => Some(Mode::Shared),
            Self::Prune => None,
        }
    }
}

// Closing the last descriptor releases flock. Do not explicitly unlock: a future
// transfer child may need to retain this same open-file description.
pub(crate) struct FileLock {
    _file: File,
}
impl FileLock {
    fn acquire(path: &Path, mode: Mode, create: bool) -> Result<Self> {
        Self::acquire_with_policy(path, mode, create, LockWaitPolicy::default())
    }

    fn acquire_with_policy(
        path: &Path,
        mode: Mode,
        create: bool,
        policy: LockWaitPolicy,
    ) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(create)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)
            .map_err(|source| io_error("open lock", path, source))?;
        require_regular_file(path, &file)?;
        let operation = match mode {
            Mode::Shared => libc::LOCK_SH,
            Mode::Exclusive => libc::LOCK_EX,
        };
        wait_for_lock(path, mode, policy, || {
            // SAFETY: file owns a valid descriptor for the duration of this call;
            // operation is a documented lock constant combined with LOCK_NB.
            if unsafe { libc::flock(file.as_raw_fd(), operation | libc::LOCK_NB) } == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        })?;
        Ok(Self { _file: file })
    }
}

// Keeping the system call separate lets tests inject interruptions and permanent
// errors without relying on process-wide signal handlers or invalid descriptors.
fn wait_for_lock(
    path: &Path,
    mode: Mode,
    policy: LockWaitPolicy,
    mut attempt: impl FnMut() -> io::Result<()>,
) -> Result<()> {
    debug_assert!(!policy.retry_interval.is_zero());
    debug_assert!(!policy.warning_interval.is_zero());
    let started = Instant::now();
    let deadline = started + policy.timeout;
    let mut next_warning = started + policy.warning_interval;
    let mut first_attempt = true;
    loop {
        if !first_attempt && Instant::now() >= deadline {
            return Err(Error::LockTimeout {
                path: path.to_owned(),
                timeout: policy.timeout,
            });
        }
        first_attempt = false;
        match attempt() {
            Ok(()) => return Ok(()),
            Err(source)
                if matches!(
                    source.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(source) => return Err(io_error("acquire lock", path, source)),
        }

        let now = Instant::now();
        if now >= deadline {
            return Err(Error::LockTimeout {
                path: path.to_owned(),
                timeout: policy.timeout,
            });
        }
        if now >= next_warning {
            // Diagnostic output failure must not turn contention into a panic.
            let _ = writeln!(
                io::stderr().lock(),
                "hpc-image-store: warning: still waiting for {} lock {} (elapsed {:?}; timeout {:?})",
                mode.label(),
                path.display(),
                now.duration_since(started),
                policy.timeout
            );
            // Start a new interval rather than emitting missed warnings in a burst.
            next_warning = Instant::now() + policy.warning_interval;
        }
        let delay = policy
            .retry_interval
            .min(deadline.saturating_duration_since(Instant::now()))
            .min(next_warning.saturating_duration_since(Instant::now()));
        thread::sleep(delay);
    }
}

pub(crate) struct StoreGuard<'a> {
    _lock: FileLock,
    layout: &'a Layout,
    operation: Operation,
}
impl<'a> StoreGuard<'a> {
    pub fn acquire(layout: &'a Layout, operation: Operation) -> Result<Self> {
        Ok(Self {
            _lock: FileLock::acquire(&layout.store_lock(), operation.store_mode(), false)?,
            layout,
            operation,
        })
    }

    pub fn lock_ref(&self, reference: &LocalRef) -> Result<RefGuard<'_>> {
        let mode = self
            .operation
            .ref_mode()
            .ok_or(crate::Error::InvalidStore {
                path: self.layout.root().to_owned(),
                reason: "prune does not acquire ref locks",
            })?;
        let key = reference.encoded_path();
        let mut parent = self.layout.root().join("locks/refs");
        for component in key.components() {
            parent.push(component);
            create_directory(&parent)?;
        }
        Ok(RefGuard {
            _lock: FileLock::acquire(&self.layout.ref_lock(reference), mode, true)?,
            _store: self,
            reference: reference.clone(),
            mode,
        })
    }
}

pub(crate) struct RefGuard<'a> {
    _lock: FileLock,
    _store: &'a StoreGuard<'a>,
    reference: LocalRef,
    mode: Mode,
}

impl StoreGuard<'_> {
    pub(crate) fn layout(&self) -> &Layout {
        self.layout
    }
    pub(crate) fn require_shared(&self) -> Result<()> {
        if self.operation.store_mode() != Mode::Shared {
            return Err(Error::Coordination(
                "this operation requires a shared store lock",
            ));
        }
        Ok(())
    }
}

impl RefGuard<'_> {
    /// Both descriptors share their flock open-file descriptions with a child.
    /// The guards must stay alive while the child is spawned and waited for.
    pub(crate) fn transfer_fds(&self) -> [RawFd; 2] {
        [
            self._store._lock._file.as_raw_fd(),
            self._lock._file.as_raw_fd(),
        ]
    }
    pub(crate) fn reference(&self) -> &LocalRef {
        &self.reference
    }
    pub(crate) fn layout(&self) -> &Layout {
        self._store.layout
    }
    pub(crate) fn require_exclusive(&self) -> Result<()> {
        self._store.require_shared()?;
        if self.mode != Mode::Exclusive {
            return Err(Error::Coordination(
                "ref mutation requires an exclusive ref lock",
            ));
        }
        Ok(())
    }
}

pub(crate) fn initialization_lock(layout: &Layout) -> Result<FileLock> {
    FileLock::acquire(&layout.store_lock(), Mode::Exclusive, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::MetadataExt,
        process::{Command, Stdio},
        sync::mpsc,
    };

    fn fast_policy() -> LockWaitPolicy {
        LockWaitPolicy {
            retry_interval: Duration::from_millis(5),
            warning_interval: Duration::from_millis(20),
            timeout: Duration::from_millis(100),
        }
    }

    // Only this helper uses the policy's real flock path in a fresh process.
    // The parent captures stderr without changing the process-wide stderr handle.
    #[test]
    #[ignore = "subprocess helper"]
    fn wait_worker() {
        let path = std::env::var_os("HPC_LOCK_WAIT_TEST_PATH").unwrap();
        let path = Path::new(&path);
        let policy = fast_policy();
        let result = FileLock::acquire_with_policy(path, Mode::Exclusive, false, policy);
        if std::env::var("HPC_LOCK_WAIT_TEST_CASE").unwrap() == "timeout" {
            assert!(
                matches!(result, Err(Error::LockTimeout { path: failed_path, timeout })
                if failed_path == path && timeout == policy.timeout)
            );
        } else {
            assert!(result.is_ok());
        }
    }

    fn capture_worker(path: &Path, case: &str) -> String {
        let temporary = tempfile::tempdir().unwrap();
        let stderr_path = temporary.path().join("stderr");
        let stderr = File::create(&stderr_path).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "locks::tests::wait_worker",
                "--nocapture",
            ])
            .env("HPC_LOCK_WAIT_TEST_PATH", path)
            .env("HPC_LOCK_WAIT_TEST_CASE", case)
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr))
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                let output = fs::read_to_string(&stderr_path).unwrap();
                assert!(status.success(), "worker failed: {output}");
                return output;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("lock-wait worker exceeded its test deadline");
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn immediate_acquisition_has_no_warning() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("lock");
        fs::write(&path, b"preserve").unwrap();
        assert!(capture_worker(&path, "success").is_empty());
    }

    #[test]
    fn contention_warns_and_times_out_without_changing_the_lock() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("lock");
        fs::write(&path, b"preserve").unwrap();
        let holder = FileLock::acquire(&path, Mode::Exclusive, false).unwrap();
        let inode = fs::metadata(&path).unwrap().ino();
        let warnings = capture_worker(&path, "timeout");
        let lines: Vec<_> = warnings.lines().collect();
        assert!(!lines.is_empty(), "no wait warning was emitted");
        // Allow scheduling delays, but never a warning on every 5 ms attempt.
        assert!(
            lines.len() <= 4,
            "warnings were emitted too frequently: {warnings}"
        );
        let mut previous_elapsed_ms = 0.0;
        for line in lines {
            assert!(line.contains("warning: still waiting for exclusive lock"));
            assert!(line.contains(path.to_str().unwrap()));
            assert!(line.contains("elapsed "));
            assert!(line.contains("timeout 100ms"));
            let elapsed_ms: f64 = line
                .split("elapsed ")
                .nth(1)
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .strip_suffix("ms")
                .unwrap()
                .parse()
                .unwrap();
            // Check the minimum interval, allowing floating-point formatting
            // tolerance; scheduling delays may make intervals longer.
            assert!(elapsed_ms + 0.000001 >= previous_elapsed_ms + 20.0);
            previous_elapsed_ms = elapsed_ms;
        }
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
        assert_eq!(fs::read(&path).unwrap(), b"preserve");
        // A failed contender must not release the holder's independent lock.
        let result = FileLock::acquire_with_policy(
            &path,
            Mode::Shared,
            false,
            LockWaitPolicy {
                warning_interval: Duration::from_secs(1),
                ..fast_policy()
            },
        );
        assert!(matches!(result, Err(Error::LockTimeout { .. })));
        drop(holder);
        assert!(
            FileLock::acquire_with_policy(&path, Mode::Exclusive, false, fast_policy()).is_ok()
        );
    }

    #[test]
    fn released_lock_is_acquired_by_a_waiting_contender() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("lock");
        let holder = FileLock::acquire(&path, Mode::Exclusive, true).unwrap();
        let (attempting, attempt_received) = mpsc::channel();
        let (finished, result_received) = mpsc::channel();
        let worker = thread::spawn(move || {
            attempting.send(()).unwrap();
            let result = FileLock::acquire_with_policy(
                &path,
                Mode::Exclusive,
                false,
                LockWaitPolicy {
                    warning_interval: Duration::from_secs(1),
                    timeout: Duration::from_secs(1),
                    ..fast_policy()
                },
            );
            finished.send(result.is_ok()).unwrap();
        });
        attempt_received
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(matches!(
            result_received.recv_timeout(Duration::from_millis(20)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        drop(holder);
        assert!(
            result_received
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
        );
        worker.join().unwrap();
    }

    #[test]
    fn interruptions_are_delayed_and_share_the_same_deadline() {
        let policy = LockWaitPolicy {
            warning_interval: Duration::from_secs(1),
            ..fast_policy()
        };
        let mut attempts = 0;
        let started = Instant::now();
        wait_for_lock(
            Path::new("test.lock"),
            Mode::Shared,
            LockWaitPolicy {
                timeout: Duration::from_secs(1),
                ..policy
            },
            || {
                attempts += 1;
                if attempts < 3 {
                    Err(io::Error::from_raw_os_error(libc::EINTR))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap();
        assert_eq!(attempts, 3);
        assert!(started.elapsed() >= policy.retry_interval * 2);

        let result = wait_for_lock(Path::new("test.lock"), Mode::Shared, policy, || {
            Err(io::Error::from_raw_os_error(libc::EINTR))
        });
        assert!(matches!(result, Err(Error::LockTimeout { .. })));
    }

    #[test]
    fn permanent_errors_fail_on_the_first_attempt() {
        let mut attempts = 0;
        let result = wait_for_lock(Path::new("test.lock"), Mode::Shared, fast_policy(), || {
            attempts += 1;
            Err(io::Error::from_raw_os_error(libc::ENOLCK))
        });
        assert_eq!(attempts, 1);
        assert!(
            matches!(result, Err(Error::Io { source, .. }) if source.raw_os_error() == Some(libc::ENOLCK))
        );
    }

    #[test]
    fn no_attempt_is_made_after_the_deadline() {
        let mut attempts = 0;
        let result = wait_for_lock(Path::new("test.lock"), Mode::Shared, fast_policy(), || {
            attempts += 1;
            thread::sleep(Duration::from_millis(120));
            Err(io::Error::from_raw_os_error(libc::EWOULDBLOCK))
        });
        assert_eq!(attempts, 1);
        assert!(matches!(result, Err(Error::LockTimeout { .. })));
    }

    #[test]
    fn retry_sleep_is_capped_at_the_deadline() {
        let mut attempts = 0;
        let policy = LockWaitPolicy {
            retry_interval: Duration::from_millis(200),
            warning_interval: Duration::from_secs(1),
            timeout: Duration::from_millis(20),
        };
        let started = Instant::now();
        let result = wait_for_lock(Path::new("test.lock"), Mode::Shared, policy, || {
            attempts += 1;
            Err(io::Error::from_raw_os_error(libc::EAGAIN))
        });
        assert_eq!(attempts, 1);
        assert!(matches!(result, Err(Error::LockTimeout { .. })));
        // Five times the deadline tolerates scheduling jitter while catching a
        // full, uncapped 200 ms retry sleep.
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn lock_descriptors_are_close_on_exec_and_do_not_truncate() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("lock");
        std::fs::write(&path, b"preserve").unwrap();
        let guard = FileLock::acquire(&path, Mode::Exclusive, true).unwrap();
        // SAFETY: the guard owns a live descriptor; F_GETFD takes no third argument.
        let flags = unsafe { libc::fcntl(guard._file.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0);
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        assert_eq!(std::fs::read(path).unwrap(), b"preserve");
    }
}
