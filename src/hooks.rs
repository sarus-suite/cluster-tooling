use std::path::Path;
use std::process::{Command, Output};
use is_executable::IsExecutable;

use crate::error::{SarusError, SarusResult};
use crate::Config;

pub struct ExecutedCommand {
    pub command: String,
    pub output: Output,
}

pub fn hook_run(config: &Config, name: &str, args: Vec<&str>) -> SarusResult<Option<ExecutedCommand>> {

    let hook = match name {
        "parallax_imagestore_create" => &config.hooks.parallax_imagestore_create,
        _ => return Err(SarusError {
                code: 26,
                file_path: None,
                msg: format!("unknown hook name: \"{name}\""),
        }),
    };

    if hook == "" {
        return Ok(None);
    }

    let hook_path = Path::new(&hook);

    if ! hook_path.exists() {
        return Err(SarusError {
            code: 27,
            file_path: None,
            msg: format!("config.hooks.{name} file \"{hook}\" doesn't exist"),
        });
    }

    if ! hook_path.is_executable() {
        return Err(SarusError {
            code: 28,
            file_path: None,
            msg: format!("config.hooks.{name} file \"{hook}\" isn't executable"),
        });
    }

    let output = hook_run_path(hook_path, &args)?;
    //let outstr = hook_output_to_string(output);

    let ec = ExecutedCommand {
        command: format!("{hook} {}", args.concat()),
        output: output,
    };

    //Ok(format!("{name} hook executed: {hook} {}\n{name} hook {outstr}", args.concat()))
    Ok(Some(ec))
}

fn hook_run_path(path: &Path, args: &Vec<&str>) -> SarusResult<Output> {

    match Command::new(path)
                .args(args)
                .output() {
        Ok(output) => Ok(output),
        Err(err)    => return Err(SarusError {
            code: 29,
            file_path: None,
            msg: format!("Running command \"{} {}\" error: {err}", path.display(), args.concat()),
        }),
    }
}
/*
fn hook_output_to_string(output: Output) -> String {

    let ret_result = match output.status.success() {
        true => "succeeded",
        false => "failed",
    };

    let ret_code = match output.status.code() {
        Some(s) => s.to_string(),
        None => String::from("UNKNOWN"),
    };

    let mut ret_stdout = match String::from_utf8(output.stdout) {
        Ok(stdout) => String::from(stdout.strip_suffix("\n").unwrap_or(&stdout)),
        Err(err) => format!("translation stdout error: \"{err}\""),
    };

    if ret_stdout != "" {
        ret_stdout = format!("\nstdout:\n{ret_stdout}");
    }

    let mut ret_stderr = match String::from_utf8(output.stderr) {
        Ok(stderr) => String::from(stderr.strip_suffix("\n").unwrap_or(&stderr)),
        Err(err) => format!("translation stderr error: \"{err}\""),
    };

    if ret_stderr != "" {
        ret_stderr = format!("\nstderr:\n{ret_stderr}");
    }

    return format!("{ret_result} with exit code: {ret_code}{ret_stdout}{ret_stderr}");
}
*/
