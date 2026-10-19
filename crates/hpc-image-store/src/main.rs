use clap::CommandFactory;
use clap::{Parser, Subcommand, ValueEnum};
use hpc_image_store::{
    ImageSelector, ImportOptions, LocalRef, Platform, Store, StoreConfig, TransferContext,
    presentation,
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Import, export, list and remove images in a filesystem OCI store")]
struct Cli {
    #[arg(long)]
    store: PathBuf,
    #[arg(long, global = true, default_value = "skopeo")]
    skopeo: PathBuf,
    #[arg(long, global = true, default_value = "podman")]
    podman: PathBuf,
    /// Optional OS/ARCH[/VARIANT] selection; otherwise use Skopeo's host default.
    #[arg(long, global = true, value_parser = parse_platform)]
    platform: Option<Platform>,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    Import {
        #[arg(value_parser = plain_image)]
        source: String,
        /// Destination store ref; defaults to the source, which must include a tag.
        local_ref: Option<LocalRef>,
        #[arg(long)]
        replace: bool,
        #[arg(long)]
        notes: Option<String>,
    },
    Export {
        selector: ImageSelector,
        /// Destination containers-storage image; defaults to the source selector.
        #[arg(value_parser = plain_image)]
        destination: Option<String>,
    },
    /// List committed refs without checking their image content.
    List {
        /// Output format (default: podman). JSON includes diagnostics and status.
        #[arg(long, value_enum)]
        format: Option<ListFormat>,
        /// Include full stored-root digests in Podman-style output.
        #[arg(long)]
        digests: bool,
    },
    /// Remove one named ref, leaving its content for future pruning.
    Remove { local_ref: LocalRef },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ListFormat {
    Podman,
    Full,
    Json,
}
fn plain_image(value: &str) -> Result<String, String> {
    if value.is_empty()
        || value.starts_with('-')
        || value.contains(['[', ']'])
        || value.contains("://")
        || value
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return Err(
            "use a plain containers-storage image name or ID without transport or storage settings"
                .into(),
        );
    }
    Ok(value.into())
}
fn parse_platform(value: &str) -> Result<Platform, String> {
    let fields: Vec<_> = value.split('/').collect();
    if !(2..=3).contains(&fields.len())
        || fields.iter().any(|field| {
            field.is_empty()
                || !field
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        })
    {
        return Err("expected OS/ARCH[/VARIANT]".into());
    }
    Ok(Platform {
        os: fields[0].into(),
        architecture: fields[1].into(),
        variant: fields.get(2).map(|field| (*field).into()),
    })
}
fn execute(cli: Cli) -> hpc_image_store::Result<bool> {
    let store = Store::new(StoreConfig::new(cli.store)?);
    let context = TransferContext {
        skopeo: cli.skopeo,
        podman: cli.podman,
        platform: cli.platform,
    };
    match cli.command {
        Action::Import {
            source,
            local_ref,
            replace,
            notes,
        } => {
            let report = store.import(
                &source,
                local_ref.as_ref(),
                &context,
                &ImportOptions { replace, notes },
            )?;
            for warning in &report.cleanup_warnings {
                eprintln!(
                    "hpc-image-store: warning: {}: {}",
                    warning.staging_path.display(),
                    warning.message
                );
            }
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string(&report).expect("report serialization")
                );
            } else {
                println!(
                    "Imported {} as {} ({} bytes)",
                    report.local_ref, report.stored_root, report.content.size_bytes
                );
            }
        }
        Action::Export {
            selector,
            destination,
        } => {
            let report = store.export_selected(&selector, destination.as_deref(), &context)?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string(&report).expect("report serialization")
                );
            } else {
                println!(
                    "Exported {} ({}) to {}",
                    report.local_ref, report.stored_manifest, report.destination
                );
            }
        }
        Action::List { format, digests } => {
            let report = store.list()?;
            for diagnostic in &report.diagnostics {
                eprintln!(
                    "hpc-image-store: {}: {}",
                    diagnostic.path.display(),
                    diagnostic.message
                );
            }
            let format = if cli.json {
                ListFormat::Json
            } else {
                format.unwrap_or(ListFormat::Podman)
            };
            match format {
                ListFormat::Podman => {
                    print!("{}", presentation::format_list_podman(&report, digests))
                }
                ListFormat::Full => print!("{}", presentation::format_list_full(&report)),
                ListFormat::Json => println!(
                    "{}",
                    serde_json::to_string(&report).expect("report serialization")
                ),
            }
            return Ok(!report.has_errors());
        }
        Action::Remove { local_ref } => {
            let report = store.remove(&local_ref)?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string(&report).expect("report serialization")
                );
            } else {
                println!("Removed {}", report.local_ref);
            }
        }
    }
    Ok(true)
}
fn usage_error(message: impl std::fmt::Display) -> ! {
    Cli::command()
        .error(clap::error::ErrorKind::ValueValidation, message)
        .exit()
}
fn main() {
    let cli = Cli::parse();
    if let Action::Import {
        source,
        local_ref: None,
        ..
    } = &cli.command
        && let Err(error) = LocalRef::new(source.as_str())
    {
        usage_error(format!(
            "an explicit destination ref is required for source {source:?}: {error}"
        ));
    }
    if let Action::List { format, digests } = &cli.command {
        if cli.json && format.is_some_and(|format| format != ListFormat::Json) {
            usage_error("--json conflicts with --format podman or full");
        }
        if *digests && (cli.json || format.is_some_and(|format| format != ListFormat::Podman)) {
            usage_error("--digests requires Podman formatting");
        }
    }
    match execute(cli) {
        Ok(true) => (),
        Ok(false) => std::process::exit(1),
        Err(error) => {
            eprintln!("hpc-image-store: {error}");
            if let hpc_image_store::Error::ResolutionIncomplete { diagnostics, .. } = &error {
                for diagnostic in diagnostics {
                    eprintln!(
                        "hpc-image-store: {}: {}",
                        diagnostic.path.display(),
                        diagnostic.message
                    );
                }
            }
            std::process::exit(1);
        }
    }
}
