use clap::{Parser, Subcommand};
use sarusctl::{
    AppDeps, CommandSpec, ExecOptions, FormatOutput, RealContainerRuntime, RealRasterOps,
    RealUserContext, execute_command_with_options, format_output,
};

const SARUSCTL_VERSION: &str = match option_env!("SARUSCTL_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// CLI tool for sarus-suite
#[derive(Parser)]
#[command(version = SARUSCTL_VERSION, about)]
struct Args {
    #[arg(long, short)]
    verbose: bool,

    /// Override the Parallax imagestore path from the configuration
    #[arg(long, value_name = "PATH")]
    parallax_imagestore: Option<String>,

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

fn main() {
    let args = Args::parse();
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
        verbose: args.verbose,
        parallax_imagestore: args.parallax_imagestore,
    };

    match execute_command_with_options(command.clone(), &deps, options) {
        Ok(output) => {
            let formatted = format_output(command.output_format(), &output);
            if !formatted.stdout.is_empty() {
                println!("{}", formatted.stdout);
            }
            if !formatted.stderr.is_empty() {
                eprintln!("{}", formatted.stderr);
            }
            std::process::exit(output.return_code);
        }
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}
