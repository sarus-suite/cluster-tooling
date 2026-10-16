//! Opt-in local acceptance. The caller must select disposable containers-storage
//! using its normal configuration, and enter its rootless namespace as needed.
use hpc_image_store::{
    Error, ImportOptions, LocalRef, Platform, Store, StoreConfig, TransferContext,
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};

fn run(command: &mut Command) -> Vec<u8> {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{:?}: {}",
        command,
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn digest(path: &Path) -> String {
    let output = run(Command::new("sha256sum").arg(path));
    String::from_utf8(output)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .into()
}
fn put_blob(layout: &Path, bytes: &[u8], media_type: &str) -> Value {
    let temporary = layout.join("object");
    fs::write(&temporary, bytes).unwrap();
    let hex = digest(&temporary);
    fs::rename(&temporary, layout.join("blobs/sha256").join(&hex)).unwrap();
    json!({"mediaType":media_type,"digest":format!("sha256:{hex}"),"size":bytes.len()})
}
fn fixture(directory: &Path, layer: &[u8], marker: &str, architecture: &str) -> PathBuf {
    let layout = directory.join(marker);
    fs::create_dir_all(layout.join("blobs/sha256")).unwrap();
    let layer = put_blob(&layout, layer, "application/vnd.oci.image.layer.v1.tar");
    let config = put_blob(
        &layout,
        &serde_json::to_vec(&json!({
            "architecture":architecture, "os":"linux", "created":"2020-01-02T03:04:05Z",
            "config":{"Env":[format!("MARKER={marker}")]},
            "rootfs":{"type":"layers", "diff_ids":[layer["digest"]]}
        }))
        .unwrap(),
        "application/vnd.oci.image.config.v1+json",
    );
    let manifest = put_blob(&layout, &serde_json::to_vec(&json!({
        "schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json", "config":config,"layers":[layer]
    })).unwrap(), "application/vnd.oci.image.manifest.v1+json");
    fs::write(
        layout.join("index.json"),
        serde_json::to_vec(&json!({"schemaVersion":2,"manifests":[manifest]})).unwrap(),
    )
    .unwrap();
    fs::write(
        layout.join("oci-layout"),
        br#"{"imageLayoutVersion":"1.0.0"}"#,
    )
    .unwrap();
    layout
}

#[test]
#[ignore = "requires caller-selected disposable rootless Podman storage and Skopeo"]
fn real_rootless_round_trip() {
    assert_eq!(
        std::env::var("HPC_IMAGE_STORE_TEST_DISPOSABLE").as_deref(),
        Ok("1"),
        "select disposable storage and explicitly opt in; see README"
    );
    let configuration = PathBuf::from(
        std::env::var_os("CONTAINERS_STORAGE_CONF")
            .expect("caller must supply disposable storage configuration"),
    );
    let configuration_before = fs::read(&configuration).unwrap();
    let context = TransferContext::default();
    for executable in [&context.podman, &context.skopeo] {
        println!(
            "{}",
            String::from_utf8_lossy(&run(Command::new(executable).arg("--version")))
        );
    }
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path();
    fs::create_dir(directory.join("payload")).unwrap();
    fs::write(directory.join("payload/hello"), b"shared layer contents\n").unwrap();
    run(Command::new("tar")
        .args([
            "--format=ustar",
            "--owner=0",
            "--group=0",
            "--mtime=@0",
            "-cf",
        ])
        .arg(directory.join("layer.tar"))
        .arg("-C")
        .arg(directory.join("payload"))
        .arg("hello"));
    let layer = fs::read(directory.join("layer.tar")).unwrap();
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };
    let one = fixture(directory, &layer, "one", architecture);
    let two = fixture(directory, &layer, "two", architecture);
    let prefix = format!("localhost/hpc-stage3-test-{}", std::process::id());
    let source = format!("{prefix}:source");
    let other = format!("{prefix}:other");
    let output = format!("{prefix}:output");
    for (layout, name) in [(&one, &source), (&two, &other)] {
        run(Command::new("skopeo")
            .arg("copy")
            .arg(format!("oci:{}", layout.display()))
            .arg(format!("containers-storage:{name}")));
    }
    let store = Store::new(StoreConfig::new(directory.join("store")).unwrap());
    let reference = LocalRef::new("test/one:tag").unwrap();
    let report = store
        .import(&source, &reference, &context, &ImportOptions::default())
        .unwrap();
    assert!(report.cleanup_warnings.is_empty());
    assert_eq!(
        fs::read_dir(directory.join("store/tmp")).unwrap().count(),
        0
    );
    assert_eq!(report.content.platform.architecture, architecture);
    assert_eq!(
        report.content.created.as_deref(),
        Some("2020-01-02T03:04:05Z")
    );
    assert!(matches!(
        store.import(&source, &reference, &context, &ImportOptions::default()),
        Err(Error::RefExists(_))
    ));
    let blobs = directory.join("store/blobs/sha256");
    let manifest: Value =
        serde_json::from_slice(&fs::read(blobs.join(report.stored_root.hex())).unwrap()).unwrap();
    let layer_digest = manifest["layers"][0]["digest"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    let shared_layer = blobs.join(layer_digest);
    assert_eq!(
        manifest["layers"][0]["mediaType"],
        "application/vnd.oci.image.layer.v1.tar+gzip"
    );
    assert_eq!(&fs::read(&shared_layer).unwrap()[..2], &[0x1f, 0x8b]);
    let inode = fs::metadata(&shared_layer).unwrap().ino();
    let age = fs::metadata(&shared_layer).unwrap().modified().unwrap();
    let second = LocalRef::new("test/two:tag").unwrap();
    store
        .import(&other, &second, &context, &ImportOptions::default())
        .unwrap();
    assert_eq!(
        fs::read_dir(directory.join("store/tmp")).unwrap().count(),
        0
    );
    assert_eq!(fs::metadata(&shared_layer).unwrap().ino(), inode);
    assert_eq!(
        fs::metadata(&shared_layer).unwrap().modified().unwrap(),
        age
    );
    store
        .export(
            &reference,
            &output,
            &TransferContext {
                podman: "/unavailable/podman".into(),
                ..context.clone()
            },
        )
        .unwrap();
    let config: Value = serde_json::from_slice(&run(Command::new("skopeo")
        .args(["inspect", "--config"])
        .arg(format!("containers-storage:{output}"))))
    .unwrap();
    assert_eq!(config["config"]["Env"][0], "MARKER=one");
    let overridden = TransferContext {
        platform: Some(Platform {
            os: "linux".into(),
            architecture: architecture.into(),
            variant: None,
        }),
        ..context.clone()
    };
    store
        .import(
            &source,
            &reference,
            &overridden,
            &ImportOptions {
                replace: true,
                notes: Some("replacement".into()),
            },
        )
        .unwrap();
    // Retag the mutable source after inspection but before Skopeo starts. The
    // library must copy the old ID, while a name-based copy would select 'two'.
    let wrapper = directory.join("inspect-and-retag");
    fs::write(&wrapper, format!("#!/usr/bin/python3\nimport subprocess,sys\nresult=subprocess.check_output(['podman',*sys.argv[1:]])\nsubprocess.run(['podman','tag',{},{}],check=True)\nsys.stdout.buffer.write(result)\n", serde_json::to_string(&other).unwrap(), serde_json::to_string(&source).unwrap())).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let pinned = store
        .import(
            &source,
            &reference,
            &TransferContext {
                podman: wrapper,
                ..context.clone()
            },
            &ImportOptions {
                replace: true,
                notes: None,
            },
        )
        .unwrap();
    assert_eq!(pinned.image_id, report.image_id);
    assert!(pinned.cleanup_warnings.is_empty());
    assert_eq!(
        fs::read_dir(directory.join("store/tmp")).unwrap().count(),
        0
    );
    store.export(&reference, &output, &context).unwrap();
    let config: Value = serde_json::from_slice(&run(Command::new("skopeo")
        .args(["inspect", "--config"])
        .arg(format!("containers-storage:{output}"))))
    .unwrap();
    assert_eq!(config["config"]["Env"][0], "MARKER=one");
    // Force a real copy failure by removing the selected image after inspect.
    let wrapper = directory.join("inspect-and-remove");
    fs::write(&wrapper, "#!/usr/bin/python3\nimport subprocess,sys\nresult=subprocess.check_output(['podman',*sys.argv[1:]])\nsubprocess.run(['podman','image','rm','--force',result.decode().strip()],check=True,stdout=subprocess.DEVNULL)\nsys.stdout.buffer.write(result)\n").unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let raw_image = fs::read_dir(directory.join("store/raw_images"))
        .unwrap()
        .count();
    assert!(matches!(
        store.import(
            &other,
            &reference,
            &TransferContext {
                podman: wrapper,
                ..context.clone()
            },
            &ImportOptions {
                replace: true,
                notes: None
            }
        ),
        Err(Error::ToolFailed {
            operation: "import image",
            ..
        })
    ));
    assert_eq!(
        fs::read_dir(directory.join("store/raw_images"))
            .unwrap()
            .count(),
        raw_image
    );
    assert_eq!(
        fs::read_dir(directory.join("store/tmp")).unwrap().count(),
        1
    );
    store.export(&reference, &output, &context).unwrap();
    // Empty the destination image cache first: otherwise Skopeo may legitimately
    // reuse its existing layer without reading the missing source blob.
    run(Command::new("podman").args(["image", "rm", "--force", &output]));
    // Missing content is detected by Skopeo on export, without a wrapper graph check.
    fs::remove_file(&shared_layer).unwrap();
    assert!(matches!(
        store.export(&reference, &format!("{prefix}:broken"), &context),
        Err(Error::ToolFailed {
            operation: "export image",
            ..
        })
    ));
    assert_eq!(fs::read(configuration).unwrap(), configuration_before);
}
