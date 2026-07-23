use std::ffi::OsStr;
use std::process::{Command, ExitStatus, Output};

use crate::{DriverError, Result};

pub(crate) fn execute_captured(mut command: Command) -> Result<Output> {
    let rendered = render_command(&command);
    execute_captured_rendered(&mut command, &rendered)
}

// TODO: Consider if this should be named `execute_captured_checked` for consistency
pub(crate) fn execute_checked(mut command: Command) -> Result<Output> {
    let rendered = render_command(&command);
    let output = execute_captured_rendered(&mut command, &rendered)?;

    if output.status.success() {
        Ok(output)
    } else {
        Err(command_failed(rendered, output))
    }
}

pub(crate) fn execute_passthrough(mut command: Command) -> Result<ExitStatus> {
    let rendered = render_command(&command);
    execute_passthrough_rendered(&mut command, &rendered)
}

pub(crate) fn execute_passthrough_checked(mut command: Command) -> Result<()> {
    let rendered = render_command(&command);
    let status = execute_passthrough_rendered(&mut command, &rendered)?;

    if status.success() {
        Ok(())
    } else {
        Err(DriverError::CommandFailed {
            command: rendered,
            status,
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

pub(crate) fn execute_probe(command: Command) -> Result<bool> {
    execute_captured(command).map(|output| output.status.success())
}

// TODO: Consider to merge this with `execute_probe` and introduce a parameter describing an optional exit policy
pub(crate) fn execute_probe_with_false_code(command: Command, false_code: i32) -> Result<bool> {
    let rendered = render_command(&command);
    let mut command = command;
    let output = execute_captured_rendered(&mut command, &rendered)?;

    match output.status.code() {
        Some(0) => Ok(true),
        Some(code) if code == false_code => Ok(false),
        _ => Err(command_failed(rendered, output)),
    }
}

fn execute_captured_rendered(command: &mut Command, rendered: &str) -> Result<Output> {
    // Future observability hook: record that the command is starting.
    let output = command.output().map_err(|source| DriverError::Spawn {
        command: rendered.to_owned(),
        source,
    })?;
    // Future observability hook: record the command's completion and status.
    Ok(output)
}

fn execute_passthrough_rendered(command: &mut Command, rendered: &str) -> Result<ExitStatus> {
    // Future observability hook: record that the command is starting.
    command.status().map_err(|source| DriverError::Spawn {
        command: rendered.to_owned(),
        source,
    })
}

fn command_failed(command: String, output: Output) -> DriverError {
    DriverError::CommandFailed {
        command,
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    }
}

pub(crate) fn render_command(command: &Command) -> String {
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(quote_os_argument)
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_os_argument(argument: &OsStr) -> String {
    let argument = argument.to_string_lossy();
    let is_safe = !argument.is_empty()
        && argument
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "_@%+=:,./-".contains(ch));

    if is_safe {
        argument.into_owned()
    } else {
        format!("'{}'", argument.replace('\'', "'\"'\"'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn renders_and_quotes_command_arguments() {
        let mut command = Command::new("some program");
        command.args(["plain", "two words", "it's"]);

        assert_eq!(
            render_command(&command),
            "'some program' plain 'two words' 'it'\"'\"'s'"
        );
    }

    #[cfg(unix)]
    #[test]
    fn renders_non_utf8_arguments_lossily() {
        use std::os::unix::ffi::OsStringExt;

        let mut command = Command::new("program");
        command.arg(OsString::from_vec(vec![b'a', 0xff, b'b']));

        assert_eq!(render_command(&command), "program 'a�b'");
    }

    #[test]
    fn reports_spawn_failures_with_the_command() {
        let error =
            execute_captured(Command::new("/definitely/not/a/real/executable")).unwrap_err();

        assert_eq!(error.command(), Some("/definitely/not/a/real/executable"));
        assert!(matches!(error, DriverError::Spawn { .. }));
    }

    #[test]
    fn captured_unchecked_returns_non_zero_output() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf output; printf error >&2; exit 7"]);

        let output = execute_captured(command).unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, b"output");
        assert_eq!(output.stderr, b"error");
    }

    #[test]
    fn captured_checked_reports_non_zero_output() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf output; printf error >&2; exit 7"]);

        let error = execute_checked(command).unwrap_err();
        assert_eq!(error.exit_status().and_then(ExitStatus::code), Some(7));
        assert_eq!(error.stdout(), Some("output"));
        assert_eq!(error.stderr(), Some("error"));
        assert!(error.to_string().contains("error"));
    }

    #[test]
    fn passthrough_unchecked_returns_non_zero_status() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 9"]);

        assert_eq!(execute_passthrough(command).unwrap().code(), Some(9));
    }

    #[test]
    fn passthrough_checked_reports_non_zero_status_without_captured_output() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 9"]);

        let error = execute_passthrough_checked(command).unwrap_err();
        assert_eq!(error.exit_status().and_then(ExitStatus::code), Some(9));
        assert_eq!(error.stderr(), Some(""));
    }

    #[test]
    fn probes_map_success_and_any_non_zero_status_to_booleans() {
        let mut present = Command::new("sh");
        present.args(["-c", "exit 0"]);
        assert!(execute_probe(present).unwrap());

        let mut absent = Command::new("sh");
        absent.args(["-c", "exit 42"]);
        assert!(!execute_probe(absent).unwrap());
    }

    #[test]
    fn probes_with_false_code_reject_unexpected_statuses() {
        let mut present = Command::new("sh");
        present.args(["-c", "exit 0"]);
        assert!(execute_probe_with_false_code(present, 1).unwrap());

        let mut absent = Command::new("sh");
        absent.args(["-c", "exit 1"]);
        assert!(!execute_probe_with_false_code(absent, 1).unwrap());

        let mut failed = Command::new("sh");
        failed.args(["-c", "printf error >&2; exit 125"]);
        let error = execute_probe_with_false_code(failed, 1).unwrap_err();
        assert_eq!(error.exit_status().and_then(ExitStatus::code), Some(125));
        assert_eq!(error.stderr(), Some("error"));
    }

    #[cfg(unix)]
    #[test]
    fn checked_execution_preserves_signal_termination() {
        let mut command = Command::new("sh");
        command.args(["-c", "kill -TERM $$"]);

        let error = execute_checked(command).unwrap_err();
        assert_eq!(error.exit_status().and_then(ExitStatus::code), None);
    }

    #[test]
    fn captured_diagnostics_are_lossy_for_non_utf8_output() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf '\\377' >&2; exit 1"]);

        let error = execute_checked(command).unwrap_err();
        assert_eq!(error.stderr(), Some("�"));
    }
}
