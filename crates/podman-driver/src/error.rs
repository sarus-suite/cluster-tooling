use std::io;
use std::path::PathBuf;
use std::process::ExitStatus;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, DriverError>;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DriverError {
    #[error("cannot construct {operation} command: missing {field}")]
    MissingContext {
        operation: &'static str,
        field: &'static str,
    },

    #[error("failed to execute `{command}`: {source}")]
    Spawn {
        command: String,
        #[source]
        source: io::Error, // TODO: should this be a process-related error type instead of io::Error?
    },

    #[error("`{command}` failed with {status}: {stderr}")]
    CommandFailed {
        command: String,
        status: ExitStatus,
        stdout: String,
        stderr: String,
    },

    #[error("invalid output from {origin}: {message}")]
    InvalidOutput {
        origin: String,
        message: String,
        output: String,
    },

    #[error("failed to {operation} `{path}`: {source}")]
    File {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl DriverError {
    pub fn command(&self) -> Option<&str> {
        match self {
            Self::Spawn { command, .. } | Self::CommandFailed { command, .. } => Some(command),
            _ => None,
        }
    }

    pub fn exit_status(&self) -> Option<&ExitStatus> {
        match self {
            Self::CommandFailed { status, .. } => Some(status),
            _ => None,
        }
    }

    pub fn stdout(&self) -> Option<&str> {
        match self {
            Self::CommandFailed { stdout, .. } => Some(stdout),
            _ => None,
        }
    }

    pub fn stderr(&self) -> Option<&str> {
        match self {
            Self::CommandFailed { stderr, .. } => Some(stderr),
            _ => None,
        }
    }
}
