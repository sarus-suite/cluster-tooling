//! Synchronous tool execution; children inherit only the operation's lock descriptor.
use crate::{Error, Result};
use std::{
    io,
    os::{fd::RawFd, unix::process::CommandExt},
    path::Path,
    process::{Command, Output, Stdio},
};

pub(crate) fn run(
    command: &mut Command,
    operation: &'static str,
    executable: &Path,
    lock: Option<RawFd>,
) -> Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(fd) = lock {
        // SAFETY: the owning guard stays alive through output(). Only
        // async-signal-safe fcntl calls run between fork and exec. FD flags are
        // changed in the child, leaving every parent descriptor close-on-exec.
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags == -1 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    // std drains both pipes concurrently, including large stderr on failure.
    let output = command.output().map_err(|source| Error::ToolIo {
        operation,
        executable: executable.to_owned(),
        source,
    })?;
    if !output.status.success() {
        return Err(Error::ToolFailed {
            operation,
            executable: executable.to_owned(),
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Store, StoreConfig, locks::Operation};

    #[test]
    fn copy_inherits_only_its_single_store_descriptor_and_parent_stays_close_on_exec() {
        let temporary = tempfile::tempdir().unwrap();
        let stores: Vec<_> = ["selected", "unrelated"]
            .into_iter()
            .map(|name| Store::new(StoreConfig::new(temporary.path().join(name)).unwrap()))
            .collect();
        for store in &stores {
            drop(store.prepare(Operation::Import).unwrap());
        }
        let selected = stores[0].prepare(Operation::Export).unwrap();
        let unrelated = stores[1].prepare(Operation::Export).unwrap();
        let script = r#"
import json, os, sys
identities = {(os.stat(p).st_dev, os.stat(p).st_ino): p for p in sys.argv[1:]}
inherited = []
for fd in os.listdir('/proc/self/fd'):
    try:
        stat = os.fstat(int(fd))
    except OSError:
        continue
    path = identities.get((stat.st_dev, stat.st_ino))
    if path:
        inherited.append(path)
print(json.dumps(inherited))
"#;
        for lock in [Some(selected.transfer_fd()), None] {
            let mut command = Command::new("python3");
            command.args(["-c", script]);
            for store in &stores {
                command.arg(store.layout.store_lock());
            }
            let output = run(
                &mut command,
                "inspect child descriptors",
                Path::new("python3"),
                lock,
            )
            .unwrap();
            let inherited: Vec<std::path::PathBuf> =
                serde_json::from_slice(&output.stdout).unwrap();
            let expected = if lock.is_some() {
                vec![stores[0].layout.store_lock()]
            } else {
                Vec::new()
            };
            assert_eq!(inherited, expected);
            for guard in [&selected, &unrelated] {
                // SAFETY: guard owns a live descriptor; F_GETFD has no third argument.
                let flags = unsafe { libc::fcntl(guard.transfer_fd(), libc::F_GETFD) };
                assert!(flags >= 0);
                assert_ne!(flags & libc::FD_CLOEXEC, 0);
            }
        }
    }
}
