use clap::{Parser, Subcommand, ValueEnum};
use raster::{self, EDF};
use sarus_suite_podman_driver::{self as pmd, PodmanCtx};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fs, path::PathBuf, str::Utf8Error};
use users;
use uuid::Uuid;

/// CLI tool for sarus-suite
#[derive(Parser)]
#[command(version, about)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(ValueEnum, Clone)]
enum FormatOutput {
    Text,
    Json,
}

#[derive(Subcommand)]
enum Command {
    /// Validate EDF file
    Validate {
        filepath: String,
        #[arg(long, short, value_enum,default_value_t = FormatOutput::Text)]
        output: FormatOutput,
    },
    /// Render EDF file
    Render {
        filepath: String,
        #[arg(long, short, value_enum,default_value_t = FormatOutput::Text)]
        output: FormatOutput,
    },
    /// List images including Parallax storage
    Images {},
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

fn get_podman_default_graphroot(p_ctx: &PodmanCtx) -> Result<PathBuf, Utf8Error> {
    let info_out = sarus_suite_podman_driver::info(Some("{{.Store.GraphRoot}}"), Some(p_ctx));
    let graphroot = str::from_utf8(&info_out.stdout)?;
    let graphroot = graphroot.trim();
    Ok(PathBuf::from(graphroot))
}

fn generate_podman_contexts(
    config: &raster::config::Config,
) -> Result<(PodmanCtx, PodmanCtx, PodmanCtx, PodmanCtx), Box<dyn std::error::Error>> {
    let default_ctx = PodmanCtx {
        podman_path: PathBuf::from(&config.podman_path),
        module: None,
        graphroot: None,
        runroot: None,
        parallax_mount_program: None,
        ro_store: None,
        podman_env: None,
    };

    let default_graphroot = get_podman_default_graphroot(&default_ctx)?;

    let migrate_ctx = PodmanCtx {
        podman_path: PathBuf::from(&config.podman_path),
        module: None,
        graphroot: Some(default_graphroot),
        runroot: None,
        parallax_mount_program: None,
        ro_store: Some(PathBuf::from(&config.parallax_imagestore)),
        podman_env: None,
    };

    let ro_ctx = PodmanCtx {
        podman_path: PathBuf::from(&config.podman_path),
        module: None,
        graphroot: Some(PathBuf::from(&config.parallax_imagestore)),
        runroot: None,
        parallax_mount_program: None,
        ro_store: None,
        podman_env: None,
    };

    let uid = users::get_current_uid();
    let gid = users::get_current_gid();
    let username = users::get_user_by_uid(uid)
        .unwrap_or_else(|| panic!("Failed to resolve current user from passwd database"))
        .name()
        .to_string_lossy()
        .into_owned();
    let roots_base = PathBuf::from("/dev/shm").join(username).join("sarusctl");

    let run_ctx = PodmanCtx {
        podman_path: PathBuf::from(&config.podman_path),
        module: Some(String::from(&config.podman_module)),
        graphroot: Some(roots_base.join("graphroot")),
        runroot: Some(roots_base.join("runroot")),
        parallax_mount_program: Some(PathBuf::from(&config.parallax_mount_program)),
        ro_store: Some(PathBuf::from(&config.parallax_imagestore)),
        podman_env: None,
    }
    .with_env("PARALLAX_MP_UID", uid.to_string())
    .with_env("PARALLAX_MP_GID", gid.to_string())
    .with_env(
        "PARALLAX_MP_LOGFILE",
        format!("/tmp/parallax-{}/mount_program.log", uid),
    );

    Ok((default_ctx, migrate_ctx, ro_ctx, run_ctx))
}

#[derive(Serialize, Deserialize, Clone)]
struct Out {
    stdout: String,
    stderr: String,
    return_code: i32,
}

fn printout(fout: FormatOutput, out: Out) {
    match fout {
        FormatOutput::Text => {
            let o = out.stdout.as_str();
            let e = out.stderr.as_str();

            if o != "" {
                println!("{o}");
            }
            if e != "" {
                eprintln!("{e}");
            }
        }
        FormatOutput::Json => {
            println!(
                "{}",
                (serde_json::to_string_pretty(&out)).unwrap_or(String::from("{}"))
            );
        }
    }
}

fn validate(filepath: String, fout: FormatOutput) -> i32 {
    let mut out = Out {
        stdout: format!(""),
        stderr: format!(""),
        return_code: 0,
    };

    let ret = raster::validate(filepath.clone());

    match ret {
        Ok(_) => {
            out.stdout = format!("{filepath} is a valid EDF file");
        }
        Err(e) => {
            out.stdout = format!("{filepath} is an INVALID EDF file");
            out.stderr = format!("{e}");
            out.return_code = 1;
        }
    }
    printout(fout, out.clone());
    return out.return_code;
}

fn render(filepath: String, fout: FormatOutput) -> i32 {
    let mut out = Out {
        stdout: format!(""),
        stderr: format!(""),
        return_code: 0,
    };

    let ret = raster::render(filepath.clone());

    match ret {
        Ok(o) => {
            out.stdout = format!(
                "{}",
                serde_json::to_string_pretty(&o).unwrap_or(String::from("ERROR"))
            );
        }
        Err(e) => {
            out.stdout = format!("");
            out.stderr = format!("{e}");
            out.return_code = 1;
        }
    }
    printout(fout, out.clone());
    return out.return_code;
}

fn images(config: &raster::config::Config) -> i32 {
    let mut ctx = PodmanCtx {
        podman_path: PathBuf::from(&config.podman_path),
        module: None,
        graphroot: None,
        runroot: None,
        parallax_mount_program: None,
        ro_store: Some(PathBuf::from(&config.parallax_imagestore)),
        podman_env: None,
    };

    ctx.graphroot = match get_podman_default_graphroot(&ctx) {
        Ok(o) => Some(o),
        Err(e) => panic!("Failed to generate Podman contexts: {}", e),
    };

    if !fs::exists(ctx.ro_store.as_deref().unwrap())
        .expect("Failed to check for existence of Parallax imagestore directory")
    {
        fs::create_dir_all(ctx.ro_store.as_deref().unwrap())
            .expect("Failed to create Parallax imagestore directory");
    }

    pmd::images(Some(&ctx));
    return 0;
}

fn migrate(image: String, config: &raster::config::Config) -> i32 {
    let mut ctx = PodmanCtx {
        podman_path: PathBuf::from(&config.podman_path),
        module: None,
        graphroot: None,
        runroot: None,
        parallax_mount_program: None,
        ro_store: Some(PathBuf::from(&config.parallax_imagestore)),
        podman_env: None,
    };

    ctx.graphroot = match get_podman_default_graphroot(&ctx) {
        Ok(o) => Some(o),
        Err(e) => panic!("Failed to generate Podman contexts: {}", e),
    };

    let parallax_path = PathBuf::from(&config.parallax_path);

    pmd::parallax_migrate(&parallax_path, &ctx, &image).unwrap();
    return 0;
}

fn rmi(image: String, config: &raster::config::Config) -> i32 {
    let mut ctx = PodmanCtx {
        podman_path: PathBuf::from(&config.podman_path),
        module: None,
        graphroot: None,
        runroot: None,
        parallax_mount_program: None,
        ro_store: Some(PathBuf::from(&config.parallax_imagestore)),
        podman_env: None,
    };

    ctx.graphroot = match get_podman_default_graphroot(&ctx) {
        Ok(o) => Some(o),
        Err(e) => panic!("Failed to generate Podman contexts: {}", e),
    };

    let parallax_path = PathBuf::from(&config.parallax_path);

    pmd::parallax_rmi(&parallax_path, &ctx, &image).unwrap();
    return 0;
}

fn run_edf(edf: EDF, container_cmd: &Vec<String>, config: &raster::config::Config) -> i32 {
    let (default_ctx, migrate_ctx, ro_ctx, run_ctx) = match generate_podman_contexts(&config) {
        Ok(o) => o,
        Err(e) => panic!("Failed to generate Podman contexts: {}", e),
    };

    let container_name = format!("sarusctl-{}", &Uuid::new_v4().simple().to_string()[..8]);

    let c_ctx = pmd::ContainerCtx {
        name: container_name,
        interactive: true,
        detach: false,
        set_env: true,
        pidfile: None,
    };

    if !pmd::image_exists(&edf.image, Some(&ro_ctx)) {
        println!("Pulling {} with Podman", &edf.image);
        pmd::pull(&edf.image, Some(&default_ctx));
        println!("Migrating {} with Parallax", &edf.image);
        let parallax_path = PathBuf::from(&config.parallax_path);
        match pmd::parallax_migrate(&parallax_path, &migrate_ctx, &edf.image) {
            Ok(_) => (),
            Err(e) => panic!("Failed migrating parallax: {}", e),
        };
        assert!(pmd::image_exists(&edf.image, Some(&run_ctx)));
    }

    pmd::run_from_edf(&edf, Some(&run_ctx), &c_ctx, container_cmd)
        .code()
        .unwrap()
}

fn collect_yaml_images(value: &yaml_serde::Value, images: &mut BTreeSet<String>) {
    match value {
        yaml_serde::Value::Mapping(mapping) => {
            for (key, value) in mapping {
                if matches!(key, yaml_serde::Value::String(s) if s == "image")
                    && matches!(value, yaml_serde::Value::String(_))
                {
                    if let yaml_serde::Value::String(image) = value {
                        images.insert(image.clone());
                    }
                }

                collect_yaml_images(value, images);
            }
        }
        yaml_serde::Value::Sequence(sequence) => {
            for value in sequence {
                collect_yaml_images(value, images);
            }
        }
        _ => (),
    }
}

fn extract_images_from_yaml_manifest(
    filepath: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let contents = fs::read_to_string(filepath)?;
    let manifest = yaml_serde::from_str::<yaml_serde::Value>(&contents)?;
    let mut images = BTreeSet::new();

    collect_yaml_images(&manifest, &mut images);

    Ok(images.into_iter().collect())
}

fn run_yaml(filepath: &str, config: &raster::config::Config) -> i32 {
    let (default_ctx, migrate_ctx, ro_ctx, _run_ctx) = match generate_podman_contexts(&config) {
        Ok(o) => o,
        Err(e) => panic!("Failed to generate Podman contexts: {}", e),
    };
    let run_ctx = PodmanCtx {
        module: None,
        podman_env: None,
        .._run_ctx
    };

    let images = extract_images_from_yaml_manifest(filepath)
        .unwrap_or_else(|e| panic!("failed to extract container images from {}: {e}", filepath));
    let parallax_path = PathBuf::from(&config.parallax_path);

    for image in images {
        if pmd::image_exists(&image, Some(&ro_ctx)) {
            continue;
        }

        println!("Pulling {} with Podman", &image);
        pmd::pull(&image, Some(&default_ctx));
        println!("Migrating {} with Parallax", &image);
        match pmd::parallax_migrate(&parallax_path, &migrate_ctx, &image) {
            Ok(_) => (),
            Err(e) => panic!("Failed migrating parallax: {}", e),
        };
        assert!(pmd::image_exists(&image, Some(&run_ctx)));
    }

    pmd::kube_play(filepath, Some(&run_ctx));
    return 0;
}

fn run(filepath: String, container_cmd: &Vec<String>, config: &raster::config::Config) -> i32 {
    match raster::render(filepath.clone()) {
        Ok(edf) => run_edf(edf, container_cmd, config),
        Err(_) => {
            let contents = std::fs::read_to_string(&filepath)
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", filepath));
            yaml_serde::from_str::<yaml_serde::Value>(&contents)
                .unwrap_or_else(|_| panic!("{} is not valid EDF nor YAML", filepath));

            run_yaml(&filepath, config)
        }
    }
}

fn main() {
    let args = Args::parse();

    let config = raster::load_config();

    let config = match config {
        Ok(o) => o,
        Err(_e) => panic!("Failed to load configuration"),
    };

    let rc = match args.command {
        Command::Validate { filepath, output } => validate(filepath, output),
        Command::Render { filepath, output } => render(filepath, output),
        Command::Images {} => images(&config),
        Command::Migrate { image } => migrate(image, &config),
        Command::Rmi { image } => rmi(image, &config),
        Command::Run {
            filepath,
            container_cmd,
        } => run(filepath, &container_cmd, &config),
    };

    std::process::exit(rc);
}
