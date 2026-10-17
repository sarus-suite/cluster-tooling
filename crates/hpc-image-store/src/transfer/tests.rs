use super::*;
use crate::{NamespaceChange, StoreConfig};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    process::Stdio,
    time::{Duration, Instant},
};

const CONFIG: &[u8] =
    br#"{"architecture":"amd64","os":"linux","rootfs":{"type":"layers","diff_ids":[]}}"#;
const CONFIG_DIGEST: &str = "c5b1d63604f273462ef36fadac3182d43ae6a6138731cf594b314835cf1c034f";
const MANIFEST: &[u8] = br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:c5b1d63604f273462ef36fadac3182d43ae6a6138731cf594b314835cf1c034f","size":78},"layers":[],"annotations":{"fixture":"one"}}"#;
const ROOT: &str = "7edeb1b1354d2b2044357c1ecf167324f8523d390f2bcd3496341b97d95c8851";
const ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

// The executable is a controlled producer, not an alternate filesystem layer.
// Tests deliberately alter its outputs and process lifetime.
const TOOL: &str = r#"#!/usr/bin/python3
import json, os, pathlib, sys
p = pathlib.Path(__file__)
c = json.loads(p.with_suffix('.json').read_text())
a = sys.argv[1:]
with open(c['log'], 'a') as f:
    f.write(json.dumps({'tool': p.name, 'args': a, 'environment': os.getenv('HPC_TRANSFER_TEST_ENV')}) + '\n')
if '--help' in a:
    print(c.get('help', '--dest-shared-blob-dir --src-shared-blob-dir --preserve-digests --override-os --override-arch --override-variant'))
    sys.exit(0)
if p.name == 'podman':
    print(c.get('id', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'))
    sys.exit(0)
if c.get('pause'):
    pathlib.Path(c['ready']).write_text(str(os.getpid()))
    with open(c['release'], 'r') as f:
        f.read()
if c.get('fail'):
    sys.stderr.write('controlled transfer failure\n' * 10000)
    sys.exit(17)
if a[-1].startswith('oci:'):
    layout = pathlib.Path(a[-1][4:])
    blobs = pathlib.Path(a[a.index('--dest-shared-blob-dir') + 1]) / 'sha256'
    fixture = pathlib.Path(c['fixture'])
    for name in ['oci-layout', 'index.json']:
        (layout / name).write_bytes((fixture / name).read_bytes())
    for f in (fixture / 'blobs' / 'sha256').iterdir():
        (blobs / f.name).write_bytes(f.read_bytes())
    if c.get('symlink'):
        (layout / 'outside').symlink_to(c['symlink'], target_is_directory=True)
"#;
struct Fixture {
    temporary: tempfile::TempDir,
    context: TransferContext,
    store: Store,
    reference: LocalRef,
    config: Value,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path();
        let fixture = directory.join("fixture");
        fs::create_dir_all(fixture.join("blobs/sha256")).unwrap();
        fs::write(fixture.join("blobs/sha256").join(CONFIG_DIGEST), CONFIG).unwrap();
        fs::write(fixture.join("blobs/sha256").join(ROOT), MANIFEST).unwrap();
        fs::write(
            fixture.join("oci-layout"),
            br#"{"imageLayoutVersion":"1.0.0"}"#,
        )
        .unwrap();
        fs::write(fixture.join("index.json"), serde_json::to_vec(&json!({"schemaVersion":2,"manifests":[{"mediaType":OCI_MANIFEST,"digest":format!("sha256:{ROOT}"),"size":MANIFEST.len()}]})).unwrap()).unwrap();
        let config = json!({"log": directory.join("log"), "fixture":fixture, "ready":directory.join("ready"), "release":directory.join("release")});
        let context = TransferContext {
            podman: directory.join("podman"),
            skopeo: directory.join("skopeo"),
            platform: None,
        };
        for executable in [&context.podman, &context.skopeo] {
            fs::write(executable, TOOL).unwrap();
            fs::set_permissions(executable, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let store = Store::new(StoreConfig::new(directory.join("store")).unwrap());
        let reference = LocalRef::new("example/tool:release").unwrap();
        let fixture = Self {
            temporary,
            context,
            store,
            reference,
            config,
        };
        fixture.save();
        fixture
    }
    fn save(&self) {
        for executable in [&self.context.podman, &self.context.skopeo] {
            fs::write(
                executable.with_extension("json"),
                serde_json::to_vec(&self.config).unwrap(),
            )
            .unwrap();
        }
    }
    fn import(&self, replace: bool) -> Result<ImportReport> {
        self.store.import(
            "localhost/source:mutable",
            Some(&self.reference),
            &self.context,
            &ImportOptions {
                replace,
                notes: Some("retained notes".into()),
            },
        )
    }
    fn log(&self) -> Vec<Value> {
        fs::read_to_string(self.temporary.path().join("log"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn read(&self) -> RefMetadata {
        let store_guard = self.store.prepare(Operation::Export).unwrap();
        let guard = store_guard.lock_ref(&self.reference).unwrap();
        refs::read(&guard).unwrap().metadata
    }
    fn alter_manifest(&self, change: impl FnOnce(&mut Value)) {
        let directory = self.temporary.path().join("fixture");
        let mut value: Value = serde_json::from_slice(MANIFEST).unwrap();
        change(&mut value);
        let bytes = serde_json::to_vec(&value).unwrap();
        fs::write(directory.join("blobs/sha256").join(ROOT), &bytes).unwrap();
        let mut index: Value =
            serde_json::from_slice(&fs::read(directory.join("index.json")).unwrap()).unwrap();
        index["manifests"][0]["size"] = json!(bytes.len());
        fs::write(
            directory.join("index.json"),
            serde_json::to_vec(&index).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn omitted_destinations_use_source_verbatim_and_obey_replacement_policy() {
    let mut fixture = Fixture::new();
    fixture.reference = LocalRef::new("example/tool:Release_1").unwrap();
    let source = fixture.reference.as_str();
    let options = ImportOptions::default();
    let imported = fixture
        .store
        .import(source, None, &fixture.context, &options)
        .unwrap();
    assert_eq!(imported.local_ref, fixture.reference);
    assert!(
        matches!(fixture.read().acquisition, Acquisition::Import { source: recorded, .. } if recorded == source)
    );
    let calls = fixture.log().len();
    assert!(matches!(
        fixture
            .store
            .import(source, None, &fixture.context, &options),
        Err(Error::RefExists(_))
    ));
    assert_eq!(fixture.log().len(), calls);
    fixture
        .store
        .import(
            source,
            None,
            &fixture.context,
            &ImportOptions {
                replace: true,
                notes: None,
            },
        )
        .unwrap();
    let exported = fixture
        .store
        .export(&fixture.reference, None, &fixture.context)
        .unwrap();
    assert_eq!(exported.local_ref, fixture.reference);
    assert_eq!(exported.destination, source);
    assert_eq!(exported.stored_manifest, imported.stored_root);
    let calls = fixture.log();
    let inspection = calls.iter().find(|call| call["tool"] == "podman").unwrap();
    assert_eq!(
        inspection["args"].as_array().unwrap().last().unwrap(),
        source
    );
    assert_eq!(
        calls.last().unwrap()["args"]
            .as_array()
            .unwrap()
            .last()
            .unwrap(),
        &json!(format!("containers-storage:{source}"))
    );
}

#[test]
fn invalid_implicit_import_refs_require_explicit_destinations_before_any_side_effects() {
    let prefixed_id = format!("sha256:{ID}");
    for source in [
        "alpine",
        ID,
        prefixed_id.as_str(),
        "registry.example:5000/team/tool:latest",
    ] {
        let fixture = Fixture::new();
        assert!(matches!(
            fixture
                .store
                .import(source, None, &fixture.context, &ImportOptions::default()),
            Err(Error::InvalidRef { .. })
        ));
        assert!(!fixture.store.config().root().exists());
        assert!(!fixture.temporary.path().join("log").exists());
        fixture
            .store
            .import(
                source,
                Some(&fixture.reference),
                &fixture.context,
                &ImportOptions::default(),
            )
            .unwrap();
        assert!(
            matches!(fixture.read().acquisition, Acquisition::Import { source: recorded, .. } if recorded == source)
        );
    }
}

#[test]
fn import_pins_id_builds_metadata_and_exports_without_podman() {
    let mut fixture = Fixture::new();
    fixture.context.platform = Some(Platform {
        os: "linux".into(),
        architecture: "amd64".into(),
        variant: Some("v1".into()),
    });
    let report = fixture.import(false).unwrap();
    assert!(report.cleanup_warnings.is_empty());
    assert_eq!(
        fs::read_dir(fixture.store.layout.staging())
            .unwrap()
            .count(),
        0
    );
    assert_eq!(report.image_id.as_str(), ID);
    assert_eq!(
        report.content.size_bytes,
        (CONFIG.len() + MANIFEST.len()) as u64
    );
    let metadata = fixture.read();
    assert_eq!(metadata.version(), 2);
    assert_eq!(metadata.notes.as_deref(), Some("retained notes"));
    assert!(metadata.acquired_at_unix_seconds > 0);
    assert!(
        matches!(metadata.acquisition, Acquisition::Import { ref source, .. } if source == "localhost/source:mutable")
    );
    let log = fixture.log();
    assert_eq!(
        log.iter().filter(|entry| entry["tool"] == "podman").count(),
        1
    );
    let copy = log
        .iter()
        .find(|entry| {
            entry["args"].as_array().unwrap().contains(&json!("copy"))
                && !entry["args"].as_array().unwrap().contains(&json!("--help"))
        })
        .unwrap();
    let arguments = copy["args"].as_array().unwrap();
    assert!(arguments.contains(&json!(format!("containers-storage:{ID}"))));
    assert!(!arguments.contains(&json!("localhost/source:mutable")));
    assert!(arguments.contains(&json!("--override-variant")));
    let shared = arguments
        .iter()
        .position(|value| value == "--dest-shared-blob-dir")
        .unwrap();
    assert!(
        Path::new(arguments[shared + 1].as_str().unwrap())
            .starts_with(fixture.store.layout.staging())
    );
    fixture.context.podman = "/does/not/exist".into();
    let exported = fixture
        .store
        .export(
            &fixture.reference,
            Some("localhost/output:tag"),
            &fixture.context,
        )
        .unwrap();
    assert_eq!(exported.stored_manifest, report.stored_root);
    assert_eq!(fixture.read(), metadata);
    assert_eq!(
        fs::read_dir(fixture.store.layout.staging())
            .unwrap()
            .count(),
        0
    );
    assert_eq!(
        fixture
            .log()
            .iter()
            .filter(|entry| entry["tool"] == "podman")
            .count(),
        1
    );
    for entry in fixture.log() {
        let text = entry["args"].to_string();
        for forbidden in [
            "--root",
            "--runroot",
            "--storage-driver",
            "--storage-opt",
            "containers-storage:[",
        ] {
            assert!(!text.contains(forbidden), "{text}");
        }
    }
    let exported = fixture
        .store
        .export(&fixture.reference, None, &fixture.context)
        .unwrap();
    assert_eq!(exported.destination, fixture.reference.as_str());
    assert_ne!(exported.destination, "localhost/source:mutable");
    assert_eq!(
        fixture.log().last().unwrap()["args"]
            .as_array()
            .unwrap()
            .last()
            .unwrap(),
        &json!(format!("containers-storage:{}", fixture.reference))
    );
    assert_eq!(fixture.read(), metadata);
}

#[test]
fn destination_exists_is_checked_before_tools_and_reuse_preserves_blob_age() {
    let fixture = Fixture::new();
    fixture.import(false).unwrap();
    let inode = fs::metadata(fixture.store.layout.blobs().join(ROOT))
        .unwrap()
        .ino();
    let modified = fs::metadata(fixture.store.layout.blobs().join(ROOT))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        fs::metadata(fixture.store.layout.blobs().join(ROOT))
            .unwrap()
            .nlink(),
        1
    );
    let calls = fixture.log().len();
    assert!(matches!(fixture.import(false), Err(Error::RefExists(_))));
    assert_eq!(fixture.log().len(), calls);
    fixture.import(true).unwrap();
    let blob = fs::metadata(fixture.store.layout.blobs().join(ROOT)).unwrap();
    assert_eq!(blob.ino(), inode);
    assert_eq!(blob.modified().unwrap(), modified);
    assert_eq!(blob.nlink(), 1);
    assert_eq!(
        fs::read(fixture.store.layout.blobs().join(ROOT)).unwrap(),
        MANIFEST
    );
    assert_eq!(
        fs::read(fixture.store.layout.blobs().join(CONFIG_DIGEST)).unwrap(),
        CONFIG
    );
    assert_eq!(
        fs::read_dir(fixture.store.layout.staging())
            .unwrap()
            .count(),
        0
    );
    assert_eq!(
        fs::read_dir(fixture.store.layout.raw_images())
            .unwrap()
            .count(),
        2
    );
}

#[test]
fn failed_copy_and_invalid_id_preserve_previous_ref() {
    let mut fixture = Fixture::new();
    fixture.import(false).unwrap();
    let pointer = fs::read_link(fixture.store.layout.ref_entry(&fixture.reference)).unwrap();
    fixture.config["fail"] = json!(true);
    fixture.save();
    assert!(
        matches!(fixture.import(true), Err(Error::ToolFailed { status, stderr, .. }) if status.code() == Some(17) && stderr.contains("controlled transfer failure"))
    );
    assert_eq!(
        fs::read_dir(fixture.store.layout.staging())
            .unwrap()
            .count(),
        1
    );
    fixture.config["id"] = json!("sha256:short");
    fixture.save();
    assert!(matches!(
        fixture.import(true),
        Err(Error::ToolOutput { .. })
    ));
    assert_eq!(
        fs::read_link(fixture.store.layout.ref_entry(&fixture.reference)).unwrap(),
        pointer
    );
    assert_eq!(
        fs::read_dir(fixture.store.layout.staging())
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn image_arguments_accept_transport_named_repositories_and_unqualified_names() {
    let prefixed_id = format!("sha256:{ID}");
    for image in [
        "containers-storage:latest",
        "oci:latest",
        "oci-archive:latest",
        "dir:latest",
        "docker:latest",
        "docker-archive:latest",
        "docker-daemon:latest",
        "ostree:latest",
        "sif:latest",
        "tarball:latest",
        "oci:path",
        "alpine",
        "alpine:latest",
        "localhost/docker:latest",
        "registry.example:5000/team/tool:latest",
        ID,
        prefixed_id.as_str(),
    ] {
        assert!(validate_image_argument(image).is_ok(), "rejected {image}");
    }
}

#[test]
fn transport_named_images_reach_the_tools_without_changing_transport_or_id_pinning() {
    for image in [
        "docker:latest",
        "oci:latest",
        "oci:path",
        "containers-storage:latest",
    ] {
        let fixture = Fixture::new();
        fixture
            .store
            .import(
                image,
                Some(&fixture.reference),
                &fixture.context,
                &ImportOptions::default(),
            )
            .unwrap();
        fixture
            .store
            .export(&fixture.reference, Some(image), &fixture.context)
            .unwrap();
        let calls = fixture.log();
        let inspection = calls.iter().find(|call| call["tool"] == "podman").unwrap();
        assert_eq!(
            inspection["args"].as_array().unwrap().last().unwrap(),
            image
        );
        let copies: Vec<_> = calls
            .iter()
            .filter(|call| {
                let args = call["args"].as_array().unwrap();
                call["tool"] == "skopeo"
                    && args.contains(&json!("copy"))
                    && !args.contains(&json!("--help"))
            })
            .collect();
        assert_eq!(copies.len(), 2);
        let import_args = copies[0]["args"].as_array().unwrap();
        assert!(import_args.contains(&json!(format!("containers-storage:{ID}"))));
        assert_eq!(
            copies[1]["args"].as_array().unwrap().last().unwrap(),
            &json!(format!("containers-storage:{image}"))
        );
        assert!(
            matches!(fixture.read().acquisition, Acquisition::Import { source, .. } if source == image)
        );
    }
}

#[test]
fn unsupported_capabilities_and_invalid_arguments_fail_without_copying() {
    let mut fixture = Fixture::new();
    fixture.config["help"] = json!("--unrelated");
    fixture.save();
    assert!(matches!(
        fixture.import(false),
        Err(Error::ToolCapability { .. })
    ));
    assert!(
        fixture
            .log()
            .iter()
            .all(|entry| entry["tool"] == "skopeo" && entry["args"][1] == "--help")
    );
    let calls_before_invalid_arguments = fixture.log().len();
    for image in [
        "oci://example/layout",
        "[vfs@/tmp]image",
        "docker://example/image",
        "",
        "--help",
        "image with spaces:tag",
        "image:\n",
        "image:\u{7f}",
        "containers-storage:[vfs@/tmp]image",
    ] {
        assert!(matches!(
            fixture.store.import(
                image,
                Some(&fixture.reference),
                &fixture.context,
                &ImportOptions::default()
            ),
            Err(Error::InvalidTransfer(_))
        ));
        assert!(matches!(
            fixture
                .store
                .export(&fixture.reference, Some(image), &fixture.context),
            Err(Error::InvalidTransfer(_))
        ));
    }
    assert_eq!(fixture.log().len(), calls_before_invalid_arguments);
}

#[test]
fn unexpected_index_and_missing_config_are_not_published() {
    let fixture = Fixture::new();
    let directory = fixture.temporary.path().join("fixture");
    let mut index: Value =
        serde_json::from_slice(&fs::read(directory.join("index.json")).unwrap()).unwrap();
    index["manifests"][0]["mediaType"] = json!("application/vnd.oci.image.index.v1+json");
    fs::write(
        directory.join("index.json"),
        serde_json::to_vec(&index).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        fixture.import(false),
        Err(Error::InvalidObject { .. })
    ));
    assert_eq!(
        fs::read_dir(fixture.store.layout.staging())
            .unwrap()
            .count(),
        1
    );
    assert!(!fixture.store.layout.ref_entry(&fixture.reference).exists());
    index["manifests"][0]["mediaType"] = json!(OCI_MANIFEST);
    fs::write(
        directory.join("index.json"),
        serde_json::to_vec(&index).unwrap(),
    )
    .unwrap();
    fs::remove_file(directory.join("blobs/sha256").join(CONFIG_DIGEST)).unwrap();
    assert!(fixture.import(false).is_err());
    assert_eq!(
        fs::read_dir(fixture.store.layout.raw_images())
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn duplicate_descriptors_count_once_and_conflicting_sizes_fail() {
    let fixture = Fixture::new();
    fixture.alter_manifest(|value| {
        value["layers"] = json!([value["config"].clone(), value["config"].clone()]);
    });
    let report = fixture.import(false).unwrap();
    let root_size = fs::metadata(fixture.store.layout.blobs().join(ROOT))
        .unwrap()
        .len();
    assert_eq!(report.content.size_bytes, root_size + CONFIG.len() as u64);
    let fixture = Fixture::new();
    fixture.alter_manifest(|value| {
        let mut layer = value["config"].clone();
        layer["size"] = json!(79);
        value["layers"] = json!([layer]);
    });
    assert!(
        matches!(fixture.import(false), Err(Error::InvalidObject { reason, .. }) if reason.contains("conflicting"))
    );
}

#[test]
fn publication_failure_preserves_ref_and_post_commit_failure_reports_visibility() {
    let fixture = Fixture::new();
    fixture.import(false).unwrap();
    let pointer = fs::read_link(fixture.store.layout.ref_entry(&fixture.reference)).unwrap();
    fsutil::fail_once("ref.rename", libc::EIO);
    assert!(fixture.import(true).is_err());
    assert_eq!(
        fs::read_dir(fixture.store.layout.staging())
            .unwrap()
            .count(),
        1
    );
    assert_eq!(
        fs::read_link(fixture.store.layout.ref_entry(&fixture.reference)).unwrap(),
        pointer
    );
    fsutil::fail_once("ref.destination-sync", libc::EIO);
    assert!(matches!(
        fixture.import(true),
        Err(Error::Durability {
            change: NamespaceChange::RefPublished,
            ..
        })
    ));
    assert_eq!(
        fs::read_dir(fixture.store.layout.staging())
            .unwrap()
            .count(),
        2
    );
    assert_ne!(
        fs::read_link(fixture.store.layout.ref_entry(&fixture.reference)).unwrap(),
        pointer
    );
    fixture.read();
}

#[test]
fn cleanup_failures_warn_after_commit_and_later_imports_leave_remnants_alone() {
    for point in ["staging.cleanup-remove", "staging.cleanup-parent-sync"] {
        let fixture = Fixture::new();
        fixture.import(false).unwrap();
        let previous = fs::read_link(fixture.store.layout.ref_entry(&fixture.reference)).unwrap();
        let blob_path = fixture.store.layout.blobs().join(ROOT);
        let before = fs::metadata(&blob_path).unwrap();
        let untouched = fixture.store.layout.staging().join("unrelated-operation");
        fs::create_dir(&untouched).unwrap();
        fs::write(untouched.join("sentinel"), b"keep").unwrap();

        fsutil::fail_once(point, libc::EIO);
        let report = fixture.import(true).unwrap();
        assert_eq!(report.cleanup_warnings.len(), 1);
        let warning = &report.cleanup_warnings[0];
        assert_eq!(
            warning.staging_path.parent(),
            Some(fixture.store.layout.staging().as_path())
        );
        assert!(warning.message.contains("image is published"));
        assert!(warning.message.contains(point));
        if point == "staging.cleanup-remove" {
            assert!(warning.staging_path.is_dir());
            // Reused CAS blobs are separate from the duplicate staged copies.
            assert_ne!(
                fs::metadata(warning.staging_path.join("blobs/sha256").join(ROOT))
                    .unwrap()
                    .ino(),
                before.ino()
            );
        } else {
            assert!(!warning.staging_path.exists());
            assert!(warning.message.contains("may not survive a crash"));
        }
        assert_ne!(
            fs::read_link(fixture.store.layout.ref_entry(&fixture.reference)).unwrap(),
            previous
        );
        assert_eq!(fixture.read().stored_root, report.stored_root);
        fixture
            .store
            .export(
                &fixture.reference,
                Some("localhost/output:tag"),
                &fixture.context,
            )
            .unwrap();
        assert_eq!(fs::metadata(&blob_path).unwrap().ino(), before.ino());
        assert_eq!(
            fs::metadata(&blob_path).unwrap().modified().unwrap(),
            before.modified().unwrap()
        );
        assert_eq!(fs::read(&blob_path).unwrap(), MANIFEST);
        let serialized = serde_json::to_value(&report).unwrap();
        assert_eq!(
            serialized["cleanup_warnings"][0]["staging_path"],
            json!(warning.staging_path)
        );

        assert!(fixture.import(true).unwrap().cleanup_warnings.is_empty());
        assert_eq!(fs::read(untouched.join("sentinel")).unwrap(), b"keep");
        assert_eq!(
            warning.staging_path.exists(),
            point == "staging.cleanup-remove"
        );
        assert_eq!(
            fs::read_dir(fixture.store.layout.raw_images())
                .unwrap()
                .count(),
            3
        );
    }
}

#[test]
fn cleanup_does_not_follow_symlinks_or_accept_paths_outside_staging() {
    use std::os::unix::fs::symlink;
    let mut fixture = Fixture::new();
    let outside = fixture.temporary.path().join("protected");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("sentinel"), b"keep").unwrap();
    fixture.config["symlink"] = json!(outside);
    fixture.save();
    assert!(fixture.import(false).unwrap().cleanup_warnings.is_empty());
    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"keep");
    let guard = fixture.store.prepare(Operation::Import).unwrap();
    let ref_guard = guard.lock_ref(&fixture.reference).unwrap();
    let replaced = fixture.store.layout.staging().join("operation-replaced");
    symlink(&outside, &replaced).unwrap();
    assert!(staging::cleanup_published(&ref_guard, &replaced).is_none());
    assert!(fs::symlink_metadata(&replaced).is_err());
    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"keep");
    let raw_image = refs::read(&ref_guard).unwrap().raw_image;
    assert!(staging::cleanup_published(&ref_guard, &raw_image).is_some());
    assert!(raw_image.join("metadata.json").is_file());
}

#[test]
fn pulled_manifest_export_requests_preservation() {
    let fixture = Fixture::new();
    fixture.import(false).unwrap();
    let mut metadata = fixture.read();
    metadata.acquisition = Acquisition::Pull {
        requested_reference: "example/image:tag".into(),
        resolved_digest: metadata.stored_root.clone(),
    };
    let raw_image = fs::canonicalize(fixture.store.layout.ref_entry(&fixture.reference)).unwrap();
    fs::write(
        raw_image.join("metadata.json"),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();
    fixture
        .store
        .export(
            &fixture.reference,
            Some("localhost/output:tag"),
            &fixture.context,
        )
        .unwrap();
    assert!(
        fixture.log().last().unwrap()["args"]
            .as_array()
            .unwrap()
            .contains(&json!("--preserve-digests"))
    );
}

#[test]
#[ignore = "subprocess helper"]
fn transfer_worker() {
    let root = PathBuf::from(std::env::var_os("HPC_TRANSFER_TEST_DIRECTORY").unwrap());
    let store = Store::new(StoreConfig::new(root.join("store")).unwrap());
    let reference = LocalRef::new("example/tool:release").unwrap();
    let context = TransferContext {
        podman: root.join("podman"),
        skopeo: root.join("skopeo"),
        platform: None,
    };
    let operation = std::env::var("HPC_TRANSFER_TEST_OPERATION").unwrap();
    if operation == "actual-skopeo" {
        let guard = store.prepare(Operation::Import).unwrap();
        let ref_guard = guard.lock_ref(&reference).unwrap();
        let stage = AcquisitionStage::new(&ref_guard).unwrap();
        let executable = PathBuf::from(std::env::var_os("HPC_TRANSFER_TEST_SKOPEO").unwrap());
        let mut command = Command::new(&executable);
        command
            .arg("copy")
            // The parent is deliberately killed. Avoid SIGPIPE from progress
            // output so this test exercises a transfer that continues afterward.
            .arg("--quiet")
            .arg("--dest-shared-blob-dir")
            .arg(stage.shared_blob_path())
            .arg(oci_argument(&root.join("fixture")).unwrap())
            .arg(oci_argument(&stage.layout_path()).unwrap());
        subprocess::run(
            &mut command,
            "actual Skopeo lock test",
            &executable,
            Some(ref_guard.transfer_fds()),
        )
        .unwrap();
    } else if operation == "import" {
        store
            .import(
                "localhost/source:mutable",
                Some(&reference),
                &context,
                &ImportOptions {
                    replace: true,
                    notes: None,
                },
            )
            .unwrap();
    } else {
        store
            .export(&reference, Some("localhost/output:tag"), &context)
            .unwrap();
    }
}

#[test]
fn actual_skopeo_retains_locks_after_parent_termination() {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    let root = fixture.temporary.path();
    let config = root.join("fixture/blobs/sha256").join(CONFIG_DIGEST);
    fs::remove_file(&config).unwrap();
    let c_path = std::ffi::CString::new(config.as_os_str().as_bytes()).unwrap();
    // SAFETY: c_path is NUL-terminated and the permission mask is valid.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    let stderr = fs::File::create(root.join("worker-stderr")).unwrap();
    let mut wrapper = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "transfer::tests::transfer_worker",
            "--nocapture",
        ])
        .env("HPC_TRANSFER_TEST_DIRECTORY", root)
        .env("HPC_TRANSFER_TEST_OPERATION", "actual-skopeo")
        .env(
            "HPC_TRANSFER_TEST_SKOPEO",
            std::env::var_os("HPC_TEST_SKOPEO").unwrap_or_else(|| "skopeo".into()),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr))
        .spawn()
        .unwrap();
    // A successful nonblocking writer open is the handshake: the real Skopeo
    // source has opened this FIFO and cannot finish reading until we close it.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut writer = loop {
        match fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&config)
        {
            Ok(writer) => break writer,
            Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {}
            Err(error) => panic!("FIFO writer: {error}"),
        }
        if wrapper.try_wait().unwrap().is_some() || Instant::now() >= deadline {
            let _ = wrapper.kill();
            let _ = wrapper.wait();
            panic!(
                "Skopeo did not open source: {}",
                fs::read_to_string(root.join("worker-stderr")).unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    wrapper.kill().unwrap();
    wrapper.wait().unwrap();
    let store_lock = fixture.store.layout.store_lock();
    let ref_lock = fixture.store.layout.ref_lock(&fixture.reference);
    assert!(!try_exclusive(&store_lock), "real Skopeo lost store lock");
    assert!(!try_exclusive(&ref_lock), "real Skopeo lost ref lock");
    writer.write_all(CONFIG).unwrap();
    drop(writer);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !try_exclusive(&store_lock) || !try_exclusive(&ref_lock) {
        assert!(
            Instant::now() < deadline,
            "real Skopeo did not release its locks"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    // Make this also prove the transfer completed, rather than merely exiting.
    let staging = fs::read_dir(fixture.store.layout.staging())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(
        staging.join("layout/index.json").is_file(),
        "{}",
        fs::read_to_string(root.join("worker-stderr")).unwrap()
    );
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "readiness timeout: {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn try_exclusive(path: &Path) -> bool {
    use std::os::fd::AsRawFd;
    let file = fs::File::open(path).unwrap();
    // SAFETY: file owns a valid descriptor, and this is a nonblocking attempt.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}
#[test]
fn orphaned_transfer_retains_store_and_ref_locks_and_inherits_environment() {
    for operation in ["import", "export"] {
        let mut fixture = Fixture::new();
        fixture.import(false).unwrap();
        let root = fixture.temporary.path();
        let release = root.join("release");
        let ready = root.join("ready");
        let c_path = std::ffi::CString::new(release.as_os_str().as_bytes()).unwrap();
        // SAFETY: c_path is NUL-terminated and mode is a valid permission mask.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        fixture.config["pause"] = json!(true);
        fixture.save();
        let mut wrapper = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "transfer::tests::transfer_worker",
                "--nocapture",
            ])
            .env("HPC_TRANSFER_TEST_DIRECTORY", root)
            .env("HPC_TRANSFER_TEST_OPERATION", operation)
            .env("HPC_TRANSFER_TEST_ENV", "inherited unchanged")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        wait_for(&ready);
        assert!(!try_exclusive(&fixture.store.layout.store_lock()));
        wrapper.kill().unwrap();
        wrapper.wait().unwrap();
        assert!(!try_exclusive(&fixture.store.layout.store_lock()));
        assert!(!try_exclusive(
            &fixture.store.layout.ref_lock(&fixture.reference)
        ));
        assert!(
            fixture
                .log()
                .iter()
                .any(|entry| entry["environment"] == "inherited unchanged")
        );
        fs::write(release, b"continue").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !try_exclusive(&fixture.store.layout.store_lock())
            || !try_exclusive(&fixture.store.layout.ref_lock(&fixture.reference))
        {
            assert!(
                Instant::now() < deadline,
                "orphaned child did not release locks"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        // A surviving copy cannot publish or clean its private staging once
        // its owning wrapper is gone. Export never creates a staging tree.
        assert_eq!(
            fs::read_dir(fixture.store.layout.staging())
                .unwrap()
                .count(),
            usize::from(operation == "import")
        );
        assert_eq!(
            fs::read_dir(fixture.store.layout.raw_images())
                .unwrap()
                .count(),
            1
        );
    }
}
