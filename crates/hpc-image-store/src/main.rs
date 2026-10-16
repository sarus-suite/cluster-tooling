use clap::{Parser, Subcommand};
use hpc_image_store::{ImportOptions, LocalRef, Platform, Store, StoreConfig, TransferContext};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    about = "Import/export OCI images using the caller's existing containers-storage context"
)]
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
        local_ref: LocalRef,
        #[arg(long)]
        replace: bool,
        #[arg(long)]
        notes: Option<String>,
    },
    Export {
        local_ref: LocalRef,
        #[arg(value_parser = plain_image)]
        destination: String,
    },
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
fn execute(cli: Cli) -> hpc_image_store::Result<()> {
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
                &local_ref,
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
            local_ref,
            destination,
        } => {
            let report = store.export(&local_ref, &destination, &context)?;
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
    }
    Ok(())
}
fn main() {
    if let Err(error) = execute(Cli::parse()) {
        eprintln!("hpc-image-store: {error}");
        std::process::exit(1);
    }
}
