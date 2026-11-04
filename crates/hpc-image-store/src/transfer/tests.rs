use super::*;
use crate::{Acquisition, NamespaceChange, StoreConfig};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    process::Stdio,
    time::{Duration, Instant},
};

fn parse_selector(value: impl AsRef<str>) -> ImageSelector {
    ImageSelector::new(value.as_ref()).unwrap()
}

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
    print(c.get('help', '--raw --retry-times --dest-shared-blob-dir --src-shared-blob-dir --preserve-digests --override-os --override-arch --override-variant'))
    sys.exit(0)
if p.name == 'podman':
    print(c.get('id', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'))
    sys.exit(0)
if 'inspect' in a:
    fixture = pathlib.Path(c['fixture'])
    root = json.loads((fixture / 'index.json').read_text())['manifests'][0]['digest'].split(':')[1]
    sys.stdout.buffer.write((fixture / 'blobs' / 'sha256' / root).read_bytes())
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
else:
    assert '--preserve-digests' in a
    assert not any(flag in a for flag in ['--all', '--format', '--dest-compress', '--dest-decompress', '--dest-compress-format', '--dest-compress-level'])
    source = pathlib.Path(a[-2][4:])
    root = json.loads((source / 'index.json').read_text())['manifests'][0]['digest']
    if c.get('export_failure') or ('@' in a[-1] and a[-1].rsplit('@', 1)[1] != root):
        sys.stderr.write(c.get('export_failure', 'manifest does not match expected digest'))
        sys.exit(18)
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
            &parse_selector("localhost/source:mutable"),
            Some(&parse_selector(self.reference.as_str())),
            &self.context,
            &ImportOptions {
                replace,
                notes: Some("retained notes".into()),
            },
        )
    }
    fn acquire(&self, pulled: bool) {
        if pulled {
            self.store
                .pull(
                    &parse_selector("registry.example/image:tag"),
                    Some(&parse_selector(self.reference.as_str())),
                    &crate::PullContext {
                        skopeo: self.context.skopeo.clone(),
                        ..crate::PullContext::default()
                    },
                    &crate::PullOptions::default(),
                )
                .unwrap();
        } else {
            self.import(false).unwrap();
        }
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
        refs::read(&store_guard, &self.reference).unwrap().metadata
    }
    fn select_image(&mut self, image: &crate::test_support::SyntheticImage) {
        let directory = self.temporary.path().join(image.root.digest.hex());
        if !directory.exists() {
            fs::create_dir_all(directory.join("blobs/sha256")).unwrap();
            oci::write_layout(&directory, &image.root).unwrap();
            for (digest, bytes) in &image.objects {
                fs::write(directory.join("blobs/sha256").join(digest.hex()), bytes).unwrap();
            }
        }
        self.config["fixture"] = json!(directory);
        self.save();
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
fn creation_time_uses_valid_non_epoch_candidates_in_order() {
    const EPOCH: &str = "1970-01-01T00:00:00Z";
    const CONFIG_TIME: &str = "2026-10-21T20:47:08.545668189Z";
    const BUILD_TIME: &str = "2026-10-25T10:26:07.538865+00:00";
    const LABEL_TIME: &str = "2026-10-26T12:00:00Z";
    for (created, annotation, runtime, expected) in [
        (
            Some(CONFIG_TIME),
            Some(BUILD_TIME),
            json!({"Labels":{"org.opencontainers.image.created":LABEL_TIME}}),
            Some(CONFIG_TIME),
        ),
        (
            Some(EPOCH),
            Some(BUILD_TIME),
            json!({"Labels":{"org.opencontainers.image.created":LABEL_TIME}}),
            Some(BUILD_TIME),
        ),
        (
            Some("1970-01-01T01:00:00.000+01:00"),
            Some(BUILD_TIME),
            Value::Null,
            Some(BUILD_TIME),
        ),
        (
            Some("1970-01-01T00:00:00.000000001Z"),
            Some(BUILD_TIME),
            Value::Null,
            Some("1970-01-01T00:00:00.000000001Z"),
        ),
        (
            Some("1969-12-31T23:59:59Z"),
            Some(BUILD_TIME),
            Value::Null,
            Some("1969-12-31T23:59:59Z"),
        ),
        (None, Some(BUILD_TIME), Value::Null, Some(BUILD_TIME)),
        (
            Some("invalid"),
            Some(BUILD_TIME),
            Value::Null,
            Some(BUILD_TIME),
        ),
        (
            None,
            Some("invalid"),
            json!({"Labels":{"org.opencontainers.image.created":LABEL_TIME}}),
            Some(LABEL_TIME),
        ),
        (
            Some(EPOCH),
            Some(EPOCH),
            json!({"Labels":{"org.opencontainers.image.created":LABEL_TIME}}),
            Some(LABEL_TIME),
        ),
        (
            Some(EPOCH),
            None,
            json!({"Labels":{"org.opencontainers.image.created":LABEL_TIME}}),
            Some(LABEL_TIME),
        ),
        (Some(EPOCH), None, Value::Null, Some(EPOCH)),
        (
            Some("invalid"),
            Some("invalid"),
            json!({"Labels":{"org.opencontainers.image.created":EPOCH}}),
            Some("invalid"),
        ),
        (None, None, Value::Null, None),
        (
            None,
            Some("invalid"),
            json!({"Labels":{"org.opencontainers.image.created":"invalid"}}),
            None,
        ),
        (
            None,
            None,
            json!({"Labels":{"org.opencontainers.image.created":EPOCH}}),
            None,
        ),
    ] {
        let config: Config = serde_json::from_value(json!({
            "os":"linux", "architecture":"amd64", "created":created, "config":runtime,
            "history":[{"created":LABEL_TIME}]
        }))
        .unwrap();
        let mut manifest: Manifest = serde_json::from_slice(MANIFEST).unwrap();
        manifest.annotations = annotation.map_or(
            Value::Null,
            |value| json!({"org.opencontainers.image.created":value}),
        );
        assert_eq!(
            image_created(&config, &manifest).as_deref(),
            expected,
            "created={created:?}, annotation={annotation:?}, runtime={}",
            config.runtime
        );
    }
}

fn creation_image(
    created: Value,
    annotations: Value,
    runtime: Value,
) -> crate::test_support::SyntheticImage {
    use sha2::{Digest, Sha256};
    let config = serde_json::to_vec(&json!({
        "os":"linux", "architecture":"amd64", "created":created,
        "config":runtime, "rootfs":{"type":"layers","diff_ids":[]}
    }))
    .unwrap();
    let config_digest = Sha256Digest::new(format!("sha256:{:x}", Sha256::digest(&config))).unwrap();
    let manifest = serde_json::to_vec(&json!({
        "schemaVersion":2, "mediaType":OCI_MANIFEST,
        "config":{"mediaType":OCI_CONFIG, "digest":config_digest,"size":config.len()},
        "layers":[], "annotations":annotations
    }))
    .unwrap();
    let root = Descriptor {
        digest: Sha256Digest::new(format!("sha256:{:x}", Sha256::digest(&manifest))).unwrap(),
        size: manifest.len() as u64,
        media_type: OCI_MANIFEST.into(),
        extra: Default::default(),
    };
    crate::test_support::SyntheticImage {
        objects: [(config_digest, config), (root.digest.clone(), manifest)].into(),
        root,
    }
}

#[test]
fn acquisitions_cache_creation_fallbacks_without_changing_content() {
    for pulled in [false, true] {
        for (created, annotations, runtime, expected) in [
            (
                json!("1970-01-01T00:00:00Z"),
                json!({"org.opencontainers.image.created":"2026-10-25T10:26:07.538865+00:00"}),
                Value::Null,
                Some("2026-10-25T10:26:07.538865+00:00"),
            ),
            (
                Value::Null,
                json!({}),
                json!({"Labels":{"org.opencontainers.image.created":"2026-10-25T10:26:07Z"}}),
                Some("2026-10-25T10:26:07Z"),
            ),
            (
                json!("2026-10-21T20:47:08.545668189Z"),
                json!({"org.opencontainers.image.created":"2026-10-25T10:26:07Z"}),
                json!({"Labels":[]}),
                Some("2026-10-21T20:47:08.545668189Z"),
            ),
        ] {
            let mut fixture = Fixture::new();
            let image = creation_image(created, annotations, runtime);
            fixture.select_image(&image);
            fixture.acquire(pulled);
            let metadata = fixture.read();
            assert_eq!(metadata.content.created.as_deref(), expected);
            assert_eq!(metadata.stored_root, image.root.digest);
            assert_eq!(
                metadata.content.size_bytes,
                image
                    .objects
                    .values()
                    .map(|bytes| bytes.len() as u64)
                    .sum::<u64>()
            );
            for (digest, bytes) in &image.objects {
                assert_eq!(fs::read(fixture.store.layout.blob(digest)).unwrap(), *bytes);
            }
            let report = fixture.store.list().unwrap();
            assert!(report.complete && report.diagnostics.is_empty());
            assert_eq!(
                report.entries[0].metadata.as_ref().unwrap().content,
                metadata.content
            );
            assert!(crate::presentation::format_list_full(&report).contains(expected.unwrap()));
            let table = crate::format_list_podman(&report, false);
            assert!(!table.contains("<none>") && !table.contains("56 years ago"));
        }
    }
}

#[test]
fn unusable_optional_labels_do_not_fail_acquisition() {
    for runtime in [
        json!(false),
        json!([]),
        json!({"Labels":null}),
        json!({"Labels":[]}),
        json!({"Labels":42}),
        json!({"Labels":{"org.opencontainers.image.created":42}}),
    ] {
        let mut fixture = Fixture::new();
        fixture.select_image(&creation_image(Value::Null, json!({}), runtime));
        fixture.acquire(false);
        assert_eq!(fixture.read().content.created, None);
    }
}

#[test]
fn replacement_refreshes_cached_creation_time_and_list_order() {
    const EPOCH: &str = "1970-01-01T00:00:00Z";
    const BUILD_TIME: &str = "2026-10-25T10:26:07.538865+00:00";
    for pulled in [false, true] {
        let mut fixture = Fixture::new();
        let image = creation_image(
            json!(EPOCH),
            json!({"org.opencontainers.image.created":BUILD_TIME}),
            Value::Null,
        );
        fixture.select_image(&image);
        fixture.acquire(pulled);
        // Model a cache written by the older CLI while retaining identical OCI bytes.
        {
            let guard = fixture.store.prepare(Operation::Import).unwrap();
            let metadata_path = refs::raw_image_path(&guard, &fixture.reference)
                .unwrap()
                .join("metadata.json");
            let mut metadata = refs::read(&guard, &fixture.reference).unwrap().metadata;
            metadata.content.created = Some(EPOCH.into());
            fs::write(&metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();
        }
        let report = fixture.store.list().unwrap();
        assert_eq!(
            report.entries[0]
                .metadata
                .as_ref()
                .unwrap()
                .content
                .created
                .as_deref(),
            Some(EPOCH)
        );
        if pulled {
            fixture
                .store
                .pull(
                    &parse_selector("registry.example/image:tag"),
                    Some(&parse_selector(fixture.reference.as_str())),
                    &crate::PullContext {
                        skopeo: fixture.context.skopeo.clone(),
                        ..Default::default()
                    },
                    &crate::PullOptions {
                        replace: true,
                        ..Default::default()
                    },
                )
                .unwrap();
        } else {
            fixture.import(true).unwrap();
        }
        assert_eq!(fixture.read().content.created.as_deref(), Some(BUILD_TIME));
        assert_eq!(fixture.read().stored_root, image.root.digest);
        fixture.reference = LocalRef::new("aaa/older:tag").unwrap();
        fixture.select_image(&creation_image(
            json!("2026-10-21T20:47:08Z"),
            json!({}),
            Value::Null,
        ));
        fixture.acquire(pulled);
        let report = fixture.store.list().unwrap();
        let table = crate::format_list_podman(&report, true);
        let mut lines = table.lines().skip(1);
        assert!(lines.next().unwrap().starts_with("example/tool"));
        assert!(lines.next().unwrap().starts_with("aaa/older"));
        assert!(crate::presentation::format_list_full(&report).contains(BUILD_TIME));
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
        .import(&parse_selector(source), None, &fixture.context, &options)
        .unwrap();
    assert_eq!(imported.local_ref, fixture.reference);
    assert!(
        matches!(fixture.read().acquisition, Acquisition::Import { source: recorded, .. } if recorded == source)
    );
    let calls = fixture.log().len();
    assert!(matches!(
        fixture
            .store
            .import(&parse_selector(source), None, &fixture.context, &options),
        Err(Error::RefExists(_))
    ));
    assert_eq!(fixture.log().len(), calls);
    fixture
        .store
        .import(
            &parse_selector(source),
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
        .export(
            &parse_selector(fixture.reference.as_str()),
            None,
            &fixture.context,
        )
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
fn invalid_import_destination_fails_before_store_preparation_and_tools() {
    let fixture = Fixture::new();
    let source = parse_selector("alpine:latest");
    let destination = parse_selector(format!("renamed@sha256:{ID}"));
    let error = fixture
        .store
        .import(
            &source,
            Some(&destination),
            &fixture.context,
            &ImportOptions::default(),
        )
        .unwrap_err();
    assert!(matches!(error, Error::InvalidImportDestination { .. }));
    assert!(error.to_string().contains("destination digest is ignored"));
    assert!(!fixture.store.config().root().exists());
    assert!(!fixture.temporary.path().join("log").exists());
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
    assert!(
        serde_json::to_value(&report)
            .unwrap()
            .get("cleanup_warnings")
            .is_none()
    );
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
    assert_eq!(metadata.version(), 3);
    assert_eq!(metadata.content.config_digest.hex(), CONFIG_DIGEST);
    assert_ne!(
        metadata.content.config_digest.hex(),
        report.image_id.as_str()
    );
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
            &parse_selector(fixture.reference.as_str()),
            Some(&parse_selector("localhost/output:tag")),
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
        .export(
            &parse_selector(fixture.reference.as_str()),
            None,
            &fixture.context,
        )
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
        1
    );
}

#[test]
fn exists_observations_do_not_reserve_import_destinations_or_invoke_tools() {
    use crate::{locks::Operation, test_snapshot::snapshot};
    let fixture = Fixture::new();
    drop(fixture.store.prepare(Operation::Import).unwrap());
    let selected = parse_selector(fixture.reference.as_str());
    assert!(matches!(fixture.store.exists(&selected), Ok(false)));
    // Another acquisition wins after the observation and before our next call.
    fixture.import(false).unwrap();
    let before = snapshot(fixture.store.config().root());
    let calls = fixture.log().len();
    assert!(matches!(fixture.store.exists(&selected), Ok(true)));
    assert!(matches!(fixture.import(false), Err(Error::RefExists(_))));
    assert_eq!(fixture.log().len(), calls);
    assert_eq!(snapshot(fixture.store.config().root()), before);
    fixture.import(true).unwrap();
    let calls = fixture.log().len();
    for executable in [&fixture.context.podman, &fixture.context.skopeo] {
        fs::remove_file(executable).unwrap();
    }
    let before = snapshot(fixture.store.config().root());
    assert!(matches!(fixture.store.exists(&selected), Ok(true)));
    assert_eq!(snapshot(fixture.store.config().root()), before);
    fs::remove_file(fixture.store.layout.blobs().join(CONFIG_DIGEST)).unwrap();
    assert!(matches!(fixture.store.exists(&selected), Ok(true)));
    assert_eq!(fixture.log().len(), calls);
}

#[test]
fn failed_copy_and_invalid_id_preserve_previous_ref() {
    let mut fixture = Fixture::new();
    fixture.import(false).unwrap();
    let pointer = fs::read_link(fixture.store.layout.ref_entry(&fixture.reference)).unwrap();
    fixture.config["fail"] = json!(true);
    fixture.save();
    assert!(
        matches!(underlying(&fixture.import(true).unwrap_err()), Error::ToolFailed { status, stderr, .. } if status.code() == Some(17) && stderr.contains("controlled transfer failure"))
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
        0
    );
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
                &parse_selector(image),
                Some(&parse_selector(fixture.reference.as_str())),
                &fixture.context,
                &ImportOptions::default(),
            )
            .unwrap();
        fixture
            .store
            .export(
                &parse_selector(fixture.reference.as_str()),
                Some(&parse_selector(image)),
                &fixture.context,
            )
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
        "alpine",
        ID,
    ] {
        assert!(ImageSelector::new(image).is_err());
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
        underlying(&fixture.import(false).unwrap_err()),
        Error::InvalidObject { .. }
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
    let mut fixture = Fixture::new();
    let images = crate::test_support::SharedImages::new();
    fixture.select_image(&images.original);
    let path = fixture
        .temporary
        .path()
        .join(images.original.root.digest.hex());
    let manifest_path = path
        .join("blobs/sha256")
        .join(images.original.root.digest.hex());
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let unique_size = manifest["layers"][1]["size"].as_u64().unwrap();
    manifest["layers"] = json!([manifest["layers"][0].clone(), manifest["layers"][0].clone()]);
    let bytes = serde_json::to_vec(&manifest).unwrap();
    fs::write(&manifest_path, &bytes).unwrap();
    let mut index: Value =
        serde_json::from_slice(&fs::read(path.join("index.json")).unwrap()).unwrap();
    index["manifests"][0]["size"] = json!(bytes.len());
    fs::write(path.join("index.json"), serde_json::to_vec(&index).unwrap()).unwrap();
    let report = fixture.import(false).unwrap();
    let expected: u64 = images
        .original
        .objects
        .values()
        .map(|bytes| bytes.len() as u64)
        .sum();
    assert_eq!(
        report.content.size_bytes,
        expected - images.original.objects[&images.original.root.digest].len() as u64 - unique_size
            + bytes.len() as u64
    );
    let fixture = Fixture::new();
    fixture.alter_manifest(|value| {
        let mut layer = value["config"].clone();
        layer["size"] = json!(79);
        value["layers"] = json!([layer]);
    });
    assert!(
        matches!(underlying(&fixture.import(false).unwrap_err()), Error::InvalidObject { reason, .. } if reason.contains("conflicting"))
    );
}

fn underlying(error: &Error) -> &Error {
    match error {
        Error::Recovery { source, .. } => underlying(source),
        other => other,
    }
}

fn assert_import_idle(store: &Store) {
    assert!(!crate::recovery::pending(&store.layout).unwrap());
    assert_eq!(fs::read_dir(store.layout.staging()).unwrap().count(), 0);
}

#[test]
fn publication_failure_preserves_ref_and_post_commit_failure_reports_visibility() {
    let fixture = Fixture::new();
    fixture.import(false).unwrap();
    let pointer = fs::read_link(fixture.store.layout.ref_entry(&fixture.reference)).unwrap();
    fsutil::fail_once("ref.rename", libc::EIO);
    let error = fixture.import(true).unwrap_err();
    assert!(matches!(error, Error::Recovery { .. }));
    assert_eq!(
        fs::read_link(fixture.store.layout.ref_entry(&fixture.reference)).unwrap(),
        pointer
    );
    assert!(crate::recovery::pending(&fixture.store.layout).unwrap());
    // Preparation recovers the failed replacement before starting the next one.
    fsutil::fail_once("ref.destination-sync", libc::EIO);
    let error = fixture.import(true).unwrap_err();
    assert!(matches!(
        underlying(&error),
        Error::Durability {
            change: NamespaceChange::RefPublished,
            ..
        }
    ));
    assert_ne!(
        fs::read_link(fixture.store.layout.ref_entry(&fixture.reference)).unwrap(),
        pointer
    );
    fixture.read();
    assert_import_idle(&fixture.store);
    assert_eq!(
        fs::read_dir(fixture.store.layout.raw_images())
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn cleanup_failures_are_errors_and_preparation_finishes_recorded_work() {
    use crate::test_snapshot::snapshot;
    for point in [
        "recovery.remove",
        "recovery.cleanup-sync",
        "recovery.record-remove-sync",
    ] {
        let fixture = Fixture::new();
        fixture.import(false).unwrap();
        let old = fs::read_link(fixture.store.layout.ref_entry(&fixture.reference)).unwrap();
        let blob = fixture.store.layout.blobs().join(ROOT);
        let before = snapshot(&blob);
        let untouched = fixture.store.layout.staging().join("unrelated-operation");
        fs::create_dir(&untouched).unwrap();
        fs::write(untouched.join("sentinel"), b"keep").unwrap();
        fsutil::fail_once(point, libc::EIO);
        let error = fixture.import(true).unwrap_err();
        assert!(
            matches!(
                error,
                Error::Recovery {
                    change: Some(NamespaceChange::RefPublished),
                    ..
                }
            ),
            "{error}"
        );
        assert!(error.to_string().contains(point));
        assert_ne!(
            fs::read_link(fixture.store.layout.ref_entry(&fixture.reference)).unwrap(),
            old
        );
        fixture.read();
        assert!(!crate::recovery::pending(&fixture.store.layout).unwrap());
        assert_eq!(snapshot(&blob), before);
        assert_eq!(fs::read(untouched.join("sentinel")).unwrap(), b"keep");
        assert_eq!(
            fs::read_dir(fixture.store.layout.staging())
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            fs::read_dir(fixture.store.layout.raw_images())
                .unwrap()
                .count(),
            1
        );
    }
}

#[test]
fn unknown_staging_contents_block_cleanup_without_following_symlinks() {
    use std::os::unix::fs::symlink;
    let mut fixture = Fixture::new();
    let outside = fixture.temporary.path().join("protected");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("sentinel"), b"keep").unwrap();
    fixture.config["symlink"] = json!(outside);
    fixture.save();
    assert!(fixture.import(false).is_err());
    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"keep");
    assert!(fixture.store.list().is_err());
    assert!(crate::recovery::pending(&fixture.store.layout).unwrap());
    let replaced = fixture.store.layout.staging().join("operation-replaced");
    symlink(&outside, &replaced).unwrap();
    assert!(
        fs::symlink_metadata(&replaced)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn every_export_preserves_the_stored_manifest_and_persistent_state() {
    use crate::test_snapshot::snapshot;
    for pulled in [false, true] {
        let mut fixture = Fixture::new();
        fixture.acquire(pulled);
        fixture.context.podman = "/unavailable/podman".into();
        let root = fixture.read().stored_root;
        let named = parse_selector(fixture.reference.as_str());
        let qualified = parse_selector(format!("example/tool@{root}"));
        let combined = parse_selector(format!("example/tool:ignored@{root}"));
        let plain = parse_selector("localhost/output:tag");
        let matching = parse_selector(format!("localhost/output:tag@{root}"));
        let mismatch = parse_selector(format!("localhost/output:tag@sha256:{}", "f".repeat(64)));
        let before = snapshot(fixture.store.config().root());
        for (source, destination) in [
            (&named, None),
            (&qualified, None),
            (&combined, None),
            (&named, Some(&plain)),
            (&qualified, Some(&matching)),
        ] {
            let calls = fixture.log().len();
            let report = fixture
                .store
                .export(source, destination, &fixture.context)
                .unwrap();
            assert_eq!(report.local_ref, fixture.reference);
            assert_eq!(report.stored_manifest, root);
            assert_eq!(report.destination, destination.unwrap_or(source).as_str());
            let log = fixture.log();
            let export_calls = &log[calls..];
            assert_eq!(export_calls.len(), 2, "one capability check and one copy");
            assert!(export_calls.iter().all(|entry| entry["tool"] == "skopeo"));
            let args = export_calls[1]["args"].as_array().unwrap();
            assert!(args.contains(&json!("--preserve-digests")));
            let shared = args
                .iter()
                .position(|arg| arg == "--src-shared-blob-dir")
                .unwrap();
            assert_eq!(
                args[shared + 1],
                json!(fixture.store.config().root().join("blobs"))
            );
            assert_eq!(
                args.last().unwrap(),
                &json!(format!("containers-storage:{}", report.destination))
            );
            assert_eq!(snapshot(fixture.store.config().root()), before);
        }
        let calls = fixture.log().len();
        assert!(matches!(
            fixture.store.export(&qualified, Some(&mismatch), &fixture.context),
            Err(Error::ToolFailed { operation: "export image", stderr, .. })
                if stderr.contains("does not match expected digest")
        ));
        assert_eq!(fixture.log().len() - calls, 2, "no fallback copy");
        assert_eq!(snapshot(fixture.store.config().root()), before);
    }
}

#[test]
fn every_export_requires_capabilities_and_propagates_preservation_failure() {
    use crate::test_snapshot::snapshot;
    for pulled in [false, true] {
        let mut fixture = Fixture::new();
        fixture.acquire(pulled);
        let source = parse_selector(fixture.reference.as_str());
        let before = snapshot(fixture.store.config().root());
        for (help, missing) in [
            ("--src-shared-blob-dir", "--preserve-digests"),
            ("--preserve-digests", "--src-shared-blob-dir"),
        ] {
            fixture.config["help"] = json!(help);
            fixture.save();
            let calls = fixture.log().len();
            assert!(matches!(
                fixture.store.export(&source, None, &fixture.context),
                Err(Error::ToolCapability { executable, capability })
                    if executable == fixture.context.skopeo && capability == missing
            ));
            let log = fixture.log();
            assert_eq!(
                log.len() - calls,
                1,
                "capability failure must prevent copying"
            );
            assert_eq!(log[calls]["args"], json!(["copy", "--help"]));
            assert_eq!(snapshot(fixture.store.config().root()), before);
        }
        fixture.config.as_object_mut().unwrap().remove("help");
        fixture.config["export_failure"] = json!("cannot preserve stored manifest digest");
        fixture.save();
        let calls = fixture.log().len();
        assert!(matches!(
            fixture.store.export(&source, None, &fixture.context),
            Err(Error::ToolFailed { operation: "export image", executable, status, stderr })
                if executable == fixture.context.skopeo && status.code() == Some(18)
                    && stderr == "cannot preserve stored manifest digest"
        ));
        assert_eq!(
            fixture.log().len() - calls,
            2,
            "no retry without preservation"
        );
        assert_eq!(snapshot(fixture.store.config().root()), before);
    }
}

#[test]
fn import_still_allows_representation_changes_without_preservation_capability() {
    let mut fixture = Fixture::new();
    fixture.config["help"] = json!("--dest-shared-blob-dir");
    fixture.save();
    fixture.import(false).unwrap();
    let log = fixture.log();
    let args = log.last().unwrap()["args"].as_array().unwrap();
    assert!(!args.contains(&json!("--preserve-digests")));
    assert!(args.last().unwrap().as_str().unwrap().starts_with("oci:"));
}

#[test]
fn digest_export_uses_layout_identity_and_a_deterministic_healthy_name() {
    let fixture = Fixture::new();
    let imported = fixture.import(false).unwrap();
    let first = LocalRef::new("example/tool:aaa").unwrap();
    fixture
        .store
        .import(
            &parse_selector("localhost/source:mutable"),
            Some(&parse_selector(first.as_str())),
            &fixture.context,
            &ImportOptions::default(),
        )
        .unwrap();
    let selector = parse_selector(format!("example/tool:ignored@{}", imported.stored_root));
    let report = fixture
        .store
        .export(&selector, None, &fixture.context)
        .unwrap();
    assert_eq!(report.local_ref, first);
    assert_eq!(report.destination, selector.as_str());
    assert_eq!(
        fixture.log().last().unwrap()["args"]
            .as_array()
            .unwrap()
            .last()
            .unwrap(),
        &json!(format!("containers-storage:{selector}"))
    );
    // A damaged earlier name must not hide a usable duplicate root.
    let raw = fs::canonicalize(fixture.store.layout.ref_entry(&first)).unwrap();
    fs::write(raw.join("metadata.json"), b"{").unwrap();
    assert_eq!(
        fixture
            .store
            .export(
                &selector,
                Some(&parse_selector("localhost/output:tag")),
                &fixture.context
            )
            .unwrap()
            .local_ref,
        fixture.reference
    );
    // Healthy resolution also wins over unrelated unknown namespace damage.
    fs::write(fixture.store.layout.root().join("refs/unknown"), b"keep").unwrap();
    assert!(
        fixture
            .store
            .export(
                &selector,
                Some(&parse_selector("localhost/output:tag")),
                &fixture.context
            )
            .is_ok()
    );
    let absent = parse_selector(format!("example/tool@sha256:{}", "f".repeat(64)));
    assert!(
        matches!(fixture.store.export(&absent, None, &fixture.context), Err(Error::ResolutionIncomplete { diagnostics, .. }) if diagnostics.len() == 2)
    );
}

#[test]
fn digest_export_never_uses_config_or_acquisition_identity_as_an_alias() {
    let fixture = Fixture::new();
    fixture.import(false).unwrap();
    for hex in [
        CONFIG_DIGEST,
        ID,
        "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    ] {
        let selector = parse_selector(format!("example/tool@sha256:{hex}"));
        let before = fixture.log().len();
        assert!(matches!(
            fixture.store.export(&selector, None, &fixture.context),
            Err(Error::DigestMissing(_))
        ));
        assert_eq!(fixture.log().len(), before);
    }
    // A missing root is authoritative even if intact metadata claims it.
    let raw = fs::canonicalize(fixture.store.layout.ref_entry(&fixture.reference)).unwrap();
    fs::write(raw.join("index.json"), b"{").unwrap();
    let selector = parse_selector(format!("example/tool@sha256:{ROOT}"));
    assert!(matches!(
        fixture.store.export(&selector, None, &fixture.context),
        Err(Error::ResolutionIncomplete { .. })
    ));
    let report = fixture.store.list().unwrap();
    assert!(report.entries[0].stored_root.is_none());
    assert!(report.entries[0].metadata.is_some());
}

#[test]
fn digest_resolution_reports_incomplete_enumeration_and_skips_removed_names() {
    let fixture = Fixture::new();
    let imported = fixture.import(false).unwrap();
    let selector = parse_selector(format!("example/tool@{}", imported.stored_root));
    fsutil::fail_once("discovery.enumerate", libc::EACCES);
    assert!(matches!(
        fixture.store.export(&selector, None, &fixture.context),
        Err(Error::ResolutionIncomplete { .. })
    ));
    fixture.store.remove(&fixture.reference).unwrap();
    assert!(matches!(
        fixture.store.export(&selector, None, &fixture.context),
        Err(Error::DigestMissing(_))
    ));
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
    if let Ok(point) = std::env::var("HPC_IMPORT_PAUSE") {
        let point = [
            "mutation.intent-durable",
            "ref.destination-sync",
            "recovery.cleaning-durable",
            "recovery.removed",
            "recovery.complete-durable",
        ]
        .into_iter()
        .find(|candidate| *candidate == point)
        .expect("known import checkpoint");
        fsutil::pause_once(point);
    }
    let operation = std::env::var("HPC_TRANSFER_TEST_OPERATION").unwrap();
    if operation == "interrupted-recorded-skopeo" {
        let guard = store.prepare(Operation::Import).unwrap();
        let mutation = Mutation::begin(&guard, &reference, Kind::Import).unwrap();
        let stage = mutation.stage().unwrap();
        let executable = PathBuf::from(std::env::var_os("HPC_TRANSFER_TEST_SKOPEO").unwrap());
        let source = PathBuf::from(std::env::var_os("HPC_TRANSFER_TEST_OCI_SOURCE").unwrap());
        let result = subprocess::run(
            Command::new(&executable)
                .args([
                    "copy",
                    "--quiet",
                    "--preserve-digests",
                    "--dest-shared-blob-dir",
                ])
                .arg(stage.shared_blob_path())
                .arg(oci_argument(&source).unwrap())
                .arg(oci_argument(&stage.layout_path()).unwrap()),
            "interrupted real Skopeo acquisition",
            &executable,
            Some(guard.transfer_fd()),
        );
        use std::os::unix::process::ExitStatusExt;
        assert!(
            matches!(result, Err(Error::ToolFailed { status, .. }) if status.signal() == Some(libc::SIGKILL))
        );
        // No Drop cleanup: the following ordinary query must recover the stage.
    } else if operation == "actual-skopeo" {
        let guard = store.prepare(Operation::Import).unwrap();
        let destination = reference.clone();
        let stage = AcquisitionStage::new(&guard, &destination).unwrap();
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
            Some(guard.transfer_fd()),
        )
        .unwrap();
    } else if operation == "import" {
        store
            .import(
                &parse_selector("localhost/source:mutable"),
                Some(&parse_selector(reference.as_str())),
                &context,
                &ImportOptions {
                    replace: true,
                    notes: None,
                },
            )
            .unwrap();
    } else if operation == "digest-export" {
        store
            .export(
                &parse_selector(format!("example/tool@sha256:{ROOT}")),
                Some(&parse_selector("localhost/output:tag")),
                &context,
            )
            .unwrap();
    } else {
        store
            .export(
                &parse_selector(reference.as_str()),
                Some(&parse_selector("localhost/output:tag")),
                &context,
            )
            .unwrap();
    }
}

#[test]
fn actual_skopeo_retains_lock_after_parent_termination() {
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

    assert!(!try_exclusive(&store_lock), "real Skopeo lost store lock");
    assert!(
        !try_lock(&store_lock, libc::LOCK_SH),
        "orphan writer allowed a reader"
    );

    writer.write_all(CONFIG).unwrap();
    drop(writer);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !try_exclusive(&store_lock) {
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
    try_lock(path, libc::LOCK_EX)
}

fn try_lock(path: &Path, mode: libc::c_int) -> bool {
    use std::os::fd::AsRawFd;
    let file = fs::File::open(path).unwrap();
    // SAFETY: file owns a valid descriptor, and this is a nonblocking attempt.
    unsafe { libc::flock(file.as_raw_fd(), mode | libc::LOCK_NB) == 0 }
}
#[test]
fn orphaned_transfer_retains_store_lock_and_inherits_environment() {
    for operation in ["import", "export", "digest-export"] {
        let mut fixture = Fixture::new();
        fixture.import(false).unwrap();
        let unused_ref = LocalRef::new("unused:tag").unwrap();
        fixture
            .store
            .exists(&parse_selector(unused_ref.as_str()))
            .unwrap();

        let store_inode = fs::metadata(fixture.store.layout.store_lock())
            .unwrap()
            .ino();
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
        assert_eq!(
            try_lock(&fixture.store.layout.store_lock(), libc::LOCK_SH),
            operation != "import"
        );
        {
            let _short = crate::locks::short_test_waits();
            assert!(matches!(
                fixture.store.prepare(Operation::Remove),
                Err(Error::LockTimeout { .. })
            ));
            if operation == "import" {
                let before = crate::test_snapshot::snapshot(fixture.store.config().root());
                assert!(matches!(
                    fixture.store.list(),
                    Err(Error::LockTimeout { .. })
                ));
                assert_eq!(
                    crate::test_snapshot::snapshot(fixture.store.config().root()),
                    before
                );
            }
        }
        assert!(!fixture.store.config().root().join("locks/refs").exists());
        assert!(
            fixture
                .log()
                .iter()
                .any(|entry| entry["environment"] == "inherited unchanged")
        );
        fs::write(release, b"continue").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !try_exclusive(&fixture.store.layout.store_lock()) {
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
        assert!(!fixture.store.list().unwrap().has_errors());
        assert_import_idle(&fixture.store);
        assert!(!fixture.store.config().root().join("locks/refs").exists());
        assert_eq!(
            fs::metadata(fixture.store.layout.store_lock())
                .unwrap()
                .ino(),
            store_inode
        );
    }
}

#[test]
fn digest_aware_import_naming_reports_ignored_digests_and_preserves_source_selection() {
    let digested = format!("alpine@sha256:{ID}");
    let combined = format!("alpine:old@sha256:{ID}");
    let destination_digest = format!("renamed@sha256:{CONFIG_DIGEST}");
    let destination_both = format!("renamed:release@sha256:{CONFIG_DIGEST}");
    for (source, destination, expected) in [
        (digested.as_str(), None, "alpine:aaaaaaaaaaaa"),
        (
            digested.as_str(),
            Some(destination_digest.as_str()),
            "renamed:aaaaaaaaaaaa",
        ),
        (
            digested.as_str(),
            Some(destination_both.as_str()),
            "renamed:release",
        ),
        (digested.as_str(), Some("renamed:tag"), "renamed:tag"),
        (combined.as_str(), None, "alpine:old"),
        (
            "alpine:latest",
            Some(destination_both.as_str()),
            "renamed:release",
        ),
        (
            "registry.example:5000/team/tool:latest",
            None,
            "registry.example:5000/team/tool:latest",
        ),
    ] {
        let fixture = Fixture::new();
        let source = parse_selector(source);
        let destination = destination.map(parse_selector);
        let imported = fixture
            .store
            .import(
                &source,
                destination.as_ref(),
                &fixture.context,
                &ImportOptions::default(),
            )
            .unwrap();
        assert_eq!(imported.local_ref.as_str(), expected);
        assert_eq!(imported.stored_root.hex(), ROOT);
        assert_eq!(imported.image_id.as_str(), ID);
        assert_eq!(
            imported.ignored_destination_digest.as_ref(),
            destination.as_ref().and_then(ImageSelector::digest)
        );
        let guard = fixture.store.prepare(Operation::Export).unwrap();
        let destination = imported.local_ref.clone();
        let metadata = refs::read(&guard, &destination).unwrap().metadata;
        assert!(
            matches!(metadata.acquisition, Acquisition::Import { source: recorded, .. }
                        if recorded == source.as_str())
        );
        drop(guard);
        let calls = fixture.log();
        let inspection = calls.iter().find(|call| call["tool"] == "podman").unwrap();
        let selected = if let Some(digest) = source.digest() {
            format!("{}@{digest}", source.name())
        } else {
            source.to_string()
        };
        assert_eq!(
            inspection["args"].as_array().unwrap().last().unwrap(),
            &selected
        );
        let copy = calls.last().unwrap()["args"].as_array().unwrap();
        let copied_source = if source.digest().is_some() {
            selected.as_str()
        } else {
            ID
        };
        assert!(copy.contains(&json!(format!("containers-storage:{copied_source}"))));
        // The source digest determines naming, never an alternate store identity.
        if let Some(digest) = source.digest() {
            let alias = parse_selector(format!("{}@{digest}", imported.local_ref.name()));
            assert!(matches!(
                fixture.store.export(&alias, None, &fixture.context),
                Err(Error::DigestMissing(_))
            ));
        }
    }
}

#[test]
fn generated_tag_collisions_require_explicit_replacement_without_alternative_tags() {
    let fixture = Fixture::new();
    let one = parse_selector(format!("alpine@sha256:{ID}"));
    let two = parse_selector(format!("alpine@sha256:{}b", &ID[..63]));
    let first = fixture
        .store
        .import(&one, None, &fixture.context, &ImportOptions::default())
        .unwrap();
    let calls = fixture.log().len();
    assert!(
        matches!(fixture.store.import(&two, None, &fixture.context, &ImportOptions::default()),
                     Err(Error::RefExists(reference)) if reference == first.local_ref)
    );
    assert_eq!(fixture.log().len(), calls);
    let replaced = fixture
        .store
        .import(
            &two,
            None,
            &fixture.context,
            &ImportOptions {
                replace: true,
                notes: None,
            },
        )
        .unwrap();
    assert_eq!(replaced.local_ref, first.local_ref);
    assert_eq!(fixture.store.list().unwrap().entries.len(), 1);
}

#[test]
fn qualified_resolution_excludes_other_repositories_and_their_damage() {
    let fixture = Fixture::new();
    let imported = fixture.import(false).unwrap();
    let other = parse_selector("docker.io/library/example/tool:tag");
    fixture
        .store
        .import(
            &parse_selector("source:tag"),
            Some(&other),
            &fixture.context,
            &ImportOptions::default(),
        )
        .unwrap();
    let selected = parse_selector(format!("example/tool:nonexistent@{}", imported.stored_root));
    assert_eq!(
        fixture
            .store
            .export(&selected, None, &fixture.context)
            .unwrap()
            .local_ref,
        fixture.reference
    );
    let other_ref = LocalRef::new(other.reference()).unwrap();
    let raw = fs::canonicalize(fixture.store.layout.ref_entry(&other_ref)).unwrap();
    fs::write(raw.join("metadata.json"), b"{").unwrap();
    let absent = parse_selector(format!("example/tool@sha256:{}", "f".repeat(64)));
    let error = fixture
        .store
        .export(&absent, None, &fixture.context)
        .unwrap_err();
    assert!(matches!(&error, Error::DigestMissing(selector) if selector == &absent));
    assert!(error.to_string().contains(absent.as_str()));
    fixture.store.remove(&fixture.reference).unwrap();
    assert!(matches!(
        fixture.store.export(&selected, None, &fixture.context),
        Err(Error::DigestMissing(_))
    ));
    let damaged = parse_selector(format!("{}@{}", other.name(), imported.stored_root));
    assert!(
        matches!(fixture.store.export(&damaged, None, &fixture.context),
                     Err(Error::ResolutionIncomplete { selector, diagnostics })
                     if selector == damaged && diagnostics.len() == 1)
    );
}

#[test]
fn export_forwards_combined_destination_and_propagates_tool_failure() {
    let mut fixture = Fixture::new();
    fixture.import(false).unwrap();
    fixture.config["fail"] = json!(true);
    fixture.save();
    let destination = parse_selector(format!("output:release@sha256:{ID}"));
    assert!(matches!(
        fixture.store.export(
            &parse_selector(fixture.reference.as_str()),
            Some(&destination),
            &fixture.context
        ),
        Err(Error::ToolFailed {
            operation: "export image",
            ..
        })
    ));
    let calls = fixture.log();
    assert_eq!(
        calls.last().unwrap()["args"]
            .as_array()
            .unwrap()
            .last()
            .unwrap(),
        &format!("containers-storage:{destination}")
    );
    assert!(!fixture.store.list().unwrap().has_errors());
}

#[test]
fn ordinary_query_recovers_import_without_tools() {
    for point in ["ref.rename", "recovery.remove"] {
        let fixture = Fixture::new();
        fixture.import(false).unwrap();
        fsutil::fail_once(point, libc::EIO);
        assert!(fixture.import(true).is_err());
        let calls = fixture.log().len();
        assert!(!fixture.store.list().unwrap().has_errors());
        assert_import_idle(&fixture.store);
        assert_eq!(
            fs::read_dir(fixture.store.layout.raw_images())
                .unwrap()
                .count(),
            1
        );
        assert_eq!(fixture.log().len(), calls);
    }
}

#[path = "import_lifecycle_tests.rs"]
mod import_lifecycle_tests;

#[path = "skopeo_interruption_tests.rs"]
mod skopeo_interruption_tests;
