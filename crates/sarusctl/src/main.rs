mod logging;

use clap::{ArgAction, Parser, Subcommand};
use sarusctl::{
    AppDeps, CommandSpec, ExecOptions, FormatOutput, RealContainerRuntime, RealRasterOps,
    RealUserContext, execute_command_with_options, format_output,
};
use std::process::ExitCode;
use tracing::{self, Level, span};

use crate::logging::init_tracing;

const SARUSCTL_VERSION: &str = match option_env!("SARUSCTL_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// CLI tool for sarus-suite
#[derive(Parser)]
#[command(version = SARUSCTL_VERSION, about)]
struct Args {
    /// Override the Parallax imagestore path from the configuration
    #[arg(long, value_name = "PATH")]
    parallax_imagestore: Option<String>,

    /// Report elapsed times for instrumented operations
    #[arg(long)]
    profile: bool,

    /// Increase logging verbosity (-v, -vv, -vvv)
    #[arg(short, long, action = ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate EDF file
    Validate {
        filepath: String,
        #[arg(long, short, value_enum, default_value_t = FormatOutput::Text)]
        output: FormatOutput,
    },
    /// Render EDF file
    Render {
        filepath: String,
        #[arg(long, short, value_enum, default_value_t = FormatOutput::Text)]
        output: FormatOutput,
    },
    /// List images including Parallax storage
    Images {},
    /// Pull image with Podman and migrate to Parallax storage
    Pull { image: String },
    /// Migrate image to Parallax storage
    Migrate { image: String },
    /// Remove image from Parallax storage
    Rmi { image: String },
    /// Run container from EDF file
    Run {
        filepath: String,
        container_cmd: Vec<String>,
    },
}

impl From<Command> for CommandSpec {
    fn from(value: Command) -> Self {
        match value {
            Command::Validate { filepath, output } => CommandSpec::Validate { filepath, output },
            Command::Render { filepath, output } => CommandSpec::Render { filepath, output },
            Command::Images {} => CommandSpec::Images,
            Command::Pull { image } => CommandSpec::Pull { image },
            Command::Migrate { image } => CommandSpec::Migrate { image },
            Command::Rmi { image } => CommandSpec::Rmi { image },
            Command::Run {
                filepath,
                container_cmd,
            } => CommandSpec::Run {
                filepath,
                container_cmd,
            },
        }
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    init_tracing(args.verbose, args.profile);
    let _main_span = span!(Level::INFO, "main").entered();

    let _init_span = span!(Level::INFO, "init").entered();
    let command: CommandSpec = args.command.into();

    let raster = RealRasterOps;
    let runtime = RealContainerRuntime;
    let user = RealUserContext;
    let deps = AppDeps {
        raster: &raster,
        runtime: &runtime,
        user: &user,
    };
    let options = ExecOptions {
        verbose: args.verbose > 0,
        parallax_imagestore: args.parallax_imagestore,
    };
    drop(_init_span);

    // TODO: Evaluate if it's more elegant to return a Result
    match execute_command_with_options(command.clone(), &deps, options) {
        Ok(output) => {
            let formatted = format_output(command.output_format(), &output);
            if !formatted.stdout.is_empty() {
                println!("{}", formatted.stdout);
            }
            if !formatted.stderr.is_empty() {
                eprintln!("{}", formatted.stderr);
            }
            ExitCode::from(output.return_code as u8)
        }
        Err(err) => {
            eprintln!("{err}");
            ExitCode::from(1)
        }
    }
}
