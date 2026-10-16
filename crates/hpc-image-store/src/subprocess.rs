//! Synchronous tool execution; children inherit only the operation's lock fds.
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
    locks: Option<[RawFd; 2]>,
) -> Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(fds) = locks {
        // SAFETY: the owning guards stay alive through output(). Only
        // async-signal-safe fcntl calls run between fork and exec. FD flags are
        // changed in the child, leaving every parent descriptor close-on-exec.
        unsafe {
            command.pre_exec(move || {
                for fd in fds {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags == -1
                        || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1
                    {
                        return Err(io::Error::last_os_error());
                    }
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
