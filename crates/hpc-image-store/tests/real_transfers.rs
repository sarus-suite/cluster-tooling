//! Real-tool acceptance with disposable containers-storage prepared by the test
//! caller. The transfer library inherits that context without changing it.
use hpc_image_store::{
    CheckScope, Error, ImageAvailability, ImageSelector, ImportOptions, LocalRef, Platform,
    PruneKind, PruneOptions, Store, StoreConfig, TransferContext, format_list_podman,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn parse_selector(value: impl AsRef<str>) -> ImageSelector {
    ImageSelector::new(value.as_ref()).unwrap()
}

#[path = "support/storage.rs"]
mod storage;
use storage::run;

#[path = "support/snapshot.rs"]
mod snapshot;

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

// Run within the existing disposable-storage invocation. A second top-level
// test could otherwise compete for explicitly supplied containers-storage.
fn poc_lifecycle(directory: &Path, source: &str, output: &str, context: &TransferContext) {
    let store = Store::new(StoreConfig::new(directory.join("lifecycle-store")).unwrap());
    let keeper = LocalRef::new("poc/keeper:tag").unwrap();
    let target = LocalRef::new("poc/target:tag").unwrap();
    let keeper_selector = parse_selector(keeper.as_str());
    let target_selector = parse_selector(target.as_str());
    let kept = store
        .import(
            &parse_selector(source),
            Some(&keeper_selector),
            context,
            &ImportOptions::default(),
        )
        .unwrap();
    assert!(kept.cleanup_warnings.is_empty());
    let store_lock = store.config().root().join("locks/store.lock");
    let lock_before = fs::metadata(&store_lock).unwrap();
    assert!(matches!(
        store.availability(&target_selector).unwrap(),
        ImageAvailability::Missing
    ));
    let acquired = store
        .import(
            &parse_selector(source),
            Some(&target_selector),
            context,
            &ImportOptions::default(),
        )
        .unwrap();
    assert!(acquired.cleanup_warnings.is_empty());
    assert_eq!(acquired.stored_root, kept.stored_root);
    assert!(matches!(
        store.availability(&target_selector).unwrap(),
        ImageAvailability::Ready
    ));
    let inventory = store.list().unwrap();
    assert!(!inventory.has_errors(), "{inventory:?}");
    assert_eq!(inventory.entries.len(), 2);
    let checked = store.check(&CheckScope::All).unwrap();
    assert!(!checked.has_errors(), "{checked:?}");
    store
        .export(&target_selector, Some(&parse_selector(output)), context)
        .unwrap();
    let restored: Value = serde_json::from_slice(&run(Command::new(&context.skopeo)
        .args(["inspect", "--config"])
        .arg(format!("containers-storage:{output}"))))
    .unwrap();
    assert_eq!(restored["config"]["Env"][0], "MARKER=one");

    store.remove(&target).unwrap();
    assert!(matches!(
        store.availability(&target_selector).unwrap(),
        ImageAvailability::Missing
    ));
    let options = PruneOptions {
        dry_run: true,
        grace_period: Duration::ZERO,
    };
    let before = snapshot::snapshot(store.config().root());
    let preview = store.prune(&options).unwrap();
    assert!(!preview.has_errors(), "{preview:?}");
    assert!(
        preview
            .candidates
            .iter()
            .any(|candidate| candidate.eligible)
    );
    assert_eq!(snapshot::snapshot(store.config().root()), before);
    let options = PruneOptions {
        dry_run: false,
        ..options
    };
    let pruned = store.prune(&options).unwrap();
    assert!(!pruned.has_errors(), "{pruned:?}");
    assert!(pruned.candidates.iter().any(|candidate| candidate.removed));
    assert_eq!(pruned.summaries[&PruneKind::Blob].removed_count, 0);
    assert!(matches!(
        store.availability(&keeper_selector).unwrap(),
        ImageAvailability::Ready
    ));
    store.remove(&keeper).unwrap();
    let pruned = store.prune(&options).unwrap();
    assert!(!pruned.has_errors(), "{pruned:?}");
    assert!(pruned.summaries[&PruneKind::Blob].removed_count > 0);
    for relative in ["blobs/sha256", "raw_images", "tmp", "refs", "locks/refs"] {
        assert_eq!(
            fs::read_dir(store.config().root().join(relative))
                .unwrap()
                .count(),
            0,
            "remaining lifecycle data in {relative}"
        );
    }
    let lock_after = fs::metadata(store_lock).unwrap();
    assert_eq!(lock_after.dev(), lock_before.dev());
    assert_eq!(lock_after.ino(), lock_before.ino());
    // This export shares the source image ID. Remove only its added name so
    // later Podman inventory assertions still observe the original fixture.
    run(Command::new(&context.podman).args(["untag", output, output]));
}

fn try_exclusive(path: &Path) -> bool {
    let file = fs::File::open(path).unwrap();
    // SAFETY: file owns a valid descriptor; this attempt never blocks.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

fn orphaned_export(directory: &Path, config: &Path, destination: &str) {
    let bytes = fs::read(config).unwrap();
    let backup = directory.join("saved-config");
    fs::rename(config, &backup).unwrap();
    let c_path = std::ffi::CString::new(config.as_os_str().as_bytes()).unwrap();
    // SAFETY: the path is NUL-terminated and the permission mask is valid.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    let stderr = directory.join("orphan-stderr");
    // Keep Skopeo's progress pipes open after its parent dies, so this test
    // observes a completed orphan transfer rather than an allowed SIGPIPE exit.
    let skopeo = directory.join("orphan-skopeo");
    fs::write(
        &skopeo,
        "#!/bin/sh\nif [ \"$1\" = copy ] && [ \"$2\" != --help ]; then\nexec skopeo \"$@\" >\"$0.stdout\" 2>\"$0.stderr\"\nfi\nexec skopeo \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&skopeo, fs::Permissions::from_mode(0o700)).unwrap();
    let mut parent = Command::new(env!("CARGO_BIN_EXE_hpc-image-store"))
        .arg("--store")
        .arg(directory.join("store"))
        .arg("--skopeo")
        .arg(skopeo)
        .args(["export", "test/one:tag", destination])
        .stdout(Stdio::null())
        .stderr(fs::File::create(&stderr).unwrap())
        .spawn()
        .unwrap();
    // The real Skopeo process blocks reading the stored config. Opening the
    // other end proves it has reached the transfer before we kill its parent.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut writer = loop {
        match fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(config)
        {
            Ok(writer) => break writer,
            Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {}
            Err(error) => panic!("FIFO writer: {error}"),
        }
        if parent.try_wait().unwrap().is_some() || Instant::now() >= deadline {
            let _ = parent.kill();
            let _ = parent.wait();
            panic!(
                "Skopeo did not open config: {}",
                fs::read_to_string(stderr).unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    parent.kill().unwrap();
    parent.wait().unwrap();
    let store_lock = directory.join("store/locks/store.lock");
    let ref_lock = directory.join("store/locks/refs/746573742f6f6e653a746167/ref.lock");
    assert!(!try_exclusive(&store_lock), "Skopeo lost store lock");
    assert!(!try_exclusive(&ref_lock), "Skopeo lost ref lock");
    writer.write_all(&bytes).unwrap();
    drop(writer);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !try_exclusive(&store_lock) || !try_exclusive(&ref_lock) {
        assert!(Instant::now() < deadline, "Skopeo did not release locks");
        std::thread::sleep(Duration::from_millis(5));
    }
    fs::rename(backup, config).unwrap();
    let restored: Value = serde_json::from_slice(&run(Command::new("skopeo")
        .args(["inspect", "--config"])
        .arg(format!("containers-storage:{destination}"))))
    .unwrap();
    assert_eq!(restored["config"]["Env"][0], "MARKER=one");
}

#[test]
fn real_rootless_round_trip() {
    if storage::run_if_needed("real_rootless_round_trip", false) {
        return;
    }
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
    poc_lifecycle(
        directory,
        &source,
        &format!("{prefix}:poc-output"),
        &context,
    );
    let store = Store::new(StoreConfig::new(directory.join("store")).unwrap());
    let reference = LocalRef::new("test/one:tag").unwrap();
    let report = store
        .import(
            &parse_selector(&source),
            Some(&parse_selector(reference.as_str())),
            &context,
            &ImportOptions::default(),
        )
        .unwrap();
    assert!(report.cleanup_warnings.is_empty());
    let checked = store
        .check(&hpc_image_store::CheckScope::Image(parse_selector(
            format!("test/one@{}", report.stored_root),
        )))
        .unwrap();
    assert!(!checked.has_errors(), "{checked:?}");
    let checked_json = run(Command::new(env!("CARGO_BIN_EXE_hpc-image-store"))
        .arg("--store")
        .arg(directory.join("store"))
        .args([
            "--json",
            "--podman",
            "/unavailable/podman",
            "--skopeo",
            "/unavailable/skopeo",
            "check",
            "--all",
        ]));
    let checked_json: Value = serde_json::from_slice(&checked_json).unwrap();
    assert_eq!(checked_json["complete"], true);
    assert_eq!(checked_json["entries"][0]["status"], "passed");
    let podman_id = String::from_utf8(run(
        Command::new("podman").args(["images", "--format", "{{.ID}}", &source])
    ))
    .unwrap();
    let inventory = store.list().unwrap();
    assert!(!inventory.has_errors());
    assert_eq!(&report.content.config_digest.hex()[..12], podman_id.trim());
    assert!(format_list_podman(&inventory, false).contains(podman_id.trim()));
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
        store.import(
            &parse_selector(&source),
            Some(&parse_selector(reference.as_str())),
            &context,
            &ImportOptions::default()
        ),
        Err(Error::RefExists(_))
    ));
    let blobs = directory.join("store/blobs/sha256");
    let manifest: Value =
        serde_json::from_slice(&fs::read(blobs.join(report.stored_root.hex())).unwrap()).unwrap();
    assert_eq!(
        manifest["config"]["digest"],
        report.content.config_digest.as_str()
    );
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
        .import(
            &parse_selector(&other),
            Some(&parse_selector(second.as_str())),
            &context,
            &ImportOptions::default(),
        )
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
            &parse_selector(format!("test/one@{}", report.stored_root)),
            Some(&parse_selector(&output)),
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
    // Select the actual containers-storage manifest, rather than the fixture's
    // original representation. Import may change this digest through compression.
    let source_digest = String::from_utf8(run(Command::new("skopeo")
        .args(["inspect", "--format", "{{.Digest}}"])
        .arg(format!("containers-storage:{source}"))))
    .unwrap();
    let source_digest = source_digest.trim();
    let digest_source = parse_selector(format!("{prefix}:nonexistent@{source_digest}"));
    let digested = store
        .import(&digest_source, None, &context, &ImportOptions::default())
        .unwrap();
    assert_eq!(digested.local_ref.as_str(), format!("{prefix}:nonexistent"));
    assert_eq!(digested.image_id, report.image_id);
    assert!(digested.ignored_destination_digest.is_none());
    let generated_source = parse_selector(format!("{prefix}@{source_digest}"));
    let generated = store
        .import(&generated_source, None, &context, &ImportOptions::default())
        .unwrap();
    assert_eq!(
        generated.local_ref.as_str(),
        format!("{prefix}:{}", &source_digest[7..19])
    );
    assert_eq!(generated.image_id, report.image_id);
    let qualified = parse_selector(format!("test/one:ignored@{}", report.stored_root));
    let matching = parse_selector(format!("{output}@{}", report.stored_root));
    store.export(&qualified, Some(&matching), &context).unwrap();
    let mismatch = parse_selector(format!(
        "{prefix}:digest-mismatch@sha256:{}",
        "f".repeat(64)
    ));
    let failed = store
        .export(&qualified, Some(&mismatch), &context)
        .unwrap_err();
    assert!(
        matches!(&failed, Error::ToolFailed { operation: "export image", stderr, .. }
                     if stderr.contains("does not match expected digest")
                         || stderr.contains("would not match destination reference")),
        "{failed}"
    );
    // Exercise the executable with real tools as well as the library API.
    let imported: Value =
        serde_json::from_slice(&run(Command::new(env!("CARGO_BIN_EXE_hpc-image-store"))
            .arg("--store")
            .arg(directory.join("store"))
            .args(["--json", "import", &source, "test/cli:tag"])))
        .unwrap();
    assert_eq!(imported["stored_root"], report.stored_root.as_str());
    assert_eq!(imported["cleanup_warnings"], json!([]));
    let exported: Value =
        serde_json::from_slice(&run(Command::new(env!("CARGO_BIN_EXE_hpc-image-store"))
            .arg("--store")
            .arg(directory.join("store"))
            .args(["--json", "export", "test/cli:tag", &output])))
        .unwrap();
    assert_eq!(exported["stored_manifest"], imported["stored_root"]);
    let config_digest = manifest["config"]["digest"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    orphaned_export(
        directory,
        &blobs.join(config_digest),
        &format!("{prefix}:orphan"),
    );
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
            &parse_selector(&source),
            Some(&parse_selector(reference.as_str())),
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
            &parse_selector(&source),
            Some(&parse_selector(reference.as_str())),
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
    store
        .export(
            &parse_selector(reference.as_str()),
            Some(&parse_selector(&output)),
            &context,
        )
        .unwrap();
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
            &parse_selector(&other),
            Some(&parse_selector(reference.as_str())),
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
    store
        .export(
            &parse_selector(reference.as_str()),
            Some(&parse_selector(&output)),
            &context,
        )
        .unwrap();
    // Empty the destination image cache first: otherwise Skopeo may legitimately
    // reuse its existing layer without reading the missing source blob.
    // Digest destinations can retain additional names for the same image.
    // This context is disposable: empty it completely before testing a missing
    // source layer, then prove no cached image can satisfy the export.
    run(Command::new("podman").args(["image", "rm", "--all", "--force"]));
    assert!(run(Command::new("podman").args(["images", "--quiet"])).is_empty());
    // Missing content is detected by Skopeo on export, without a wrapper graph check.
    fs::remove_file(&shared_layer).unwrap();
    let failed_export = store.export(
        &parse_selector(reference.as_str()),
        Some(&parse_selector(format!("{prefix}:broken"))),
        &context,
    );
    assert!(
        matches!(
            failed_export,
            Err(Error::ToolFailed {
                operation: "export image",
                ..
            })
        ),
        "expected missing-content export failure, got {failed_export:?}"
    );
    assert_eq!(fs::read(configuration).unwrap(), configuration_before);
}
