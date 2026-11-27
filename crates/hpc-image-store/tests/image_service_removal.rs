use hpc_image_store::{
    CheckScope, Error, ImageSelector, ImageService, ImportOptions, LocalRef, ProvisionError, Store,
    StoreConfig, TransferContext,
};
use raster::config::Config;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tempfile::{TempDir, tempdir};

const IMAGE: &str = "registry.example:5000/analysis/tool:Release_1";
const KEEPER: &str = "analysis/keeper:tag";

const PRODUCER: &str = r#"#!/usr/bin/python3
import hashlib, json, pathlib, sys
a = sys.argv[1:]
if '--help' in a:
    print('--dest-shared-blob-dir --preserve-digests')
elif 'inspect' in a:
    print(hashlib.sha256(a[-1].encode()).hexdigest())
else:
    layout = pathlib.Path(a[-1][4:])
    blobs = pathlib.Path(a[a.index('--dest-shared-blob-dir') + 1]) / 'sha256'
    def blob(content, media):
        data = json.dumps(content).encode()
        digest = hashlib.sha256(data).hexdigest()
        (blobs / digest).write_bytes(data)
        return {'digest': 'sha256:' + digest, 'size': len(data), 'mediaType': media}
    config = blob({'os': 'linux', 'architecture': 'amd64', 'source': a[-2],
                   'rootfs': {'type': 'layers', 'diff_ids': []}},
                  'application/vnd.oci.image.config.v1+json')
    manifest = blob({'schemaVersion': 2, 'config': config, 'layers': []},
                    'application/vnd.oci.image.manifest.v1+json')
    (layout / 'oci-layout').write_text('{"imageLayoutVersion":"1.0.0"}')
    (layout / 'index.json').write_text(json.dumps({'schemaVersion': 2, 'manifests': [manifest]}))
"#;

const RUNTIME_TOOL: &str = r#"#!/usr/bin/python3
import json, pathlib, sys
tool = pathlib.Path(__file__).name
a = sys.argv[1:]
root = pathlib.Path(__file__).parent
with (root / 'calls').open('a') as log:
    log.write(json.dumps({'tool': tool, 'args': a}) + '\n')
failure = root / 'fail-stage'
if failure.exists() and failure.read_text() == tool:
    sys.stderr.write(tool + ' failure diagnostic\n')
    sys.exit(19)
if tool == 'podman':
    assert 'info' in a
    print(root / 'graphroot')
else:
    assert tool == 'parallax'
    assert '--rmi' in a
"#;

struct Fixture {
    directory: TempDir,
    config: Config,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempdir().unwrap();
        for (name, script) in [
            ("producer", PRODUCER),
            ("podman", RUNTIME_TOOL),
            ("parallax", RUNTIME_TOOL),
        ] {
            let path = directory.path().join(name);
            fs::write(&path, script).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let config = Config {
            hpc_image_store: directory.path().join("store").display().to_string(),
            parallax_imagestore: directory
                .path()
                .join("parallax-store")
                .display()
                .to_string(),
            podman_path: directory.path().join("podman").display().to_string(),
            parallax_path: directory.path().join("parallax").display().to_string(),
            ..Config::default()
        };
        Self { directory, config }
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn store(&self) -> Store {
        Store::new(StoreConfig::new(&self.config.hpc_image_store).unwrap())
    }

    fn service(&self) -> ImageService<'_> {
        ImageService::new(&self.config).unwrap()
    }

    fn seed(&self, source: &str, destination: &str) {
        self.store()
            .import(
                &ImageSelector::new(source).unwrap(),
                Some(&ImageSelector::new(destination).unwrap()),
                &TransferContext {
                    podman: self.root().join("producer"),
                    skopeo: self.root().join("producer"),
                    platform: None,
                    storage: None,
                },
                &ImportOptions::default(),
            )
            .unwrap();
    }

    fn calls(&self) -> Vec<Value> {
        fs::read_to_string(self.root().join("calls"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

#[test]
fn removal_uses_the_resolved_runtime_identity_and_preserves_shared_content() {
    let fixture = Fixture::new();
    fixture.seed("source:tag", IMAGE);
    fixture.seed("source:tag", KEEPER);
    let store = fixture.store();
    let image = ImageSelector::new(IMAGE).unwrap();
    let keeper = ImageSelector::new(KEEPER).unwrap();
    let target_resolved = store.resolve(&image).unwrap().unwrap();
    let keeper_resolved = store.resolve(&keeper).unwrap();

    let report = fixture.service().remove_image(IMAGE).unwrap();

    assert_eq!(report.local_ref, LocalRef::new(IMAGE).unwrap());
    assert!(!store.exists(&image).unwrap());
    assert_eq!(store.resolve(&keeper).unwrap(), keeper_resolved);
    assert!(!store.check(&CheckScope::All).unwrap().has_errors());

    let calls = fixture.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["tool"], "podman");
    assert_eq!(calls[1]["tool"], "parallax");
    let args = calls[1]["args"].as_array().unwrap();
    let image_index = args.iter().position(|arg| arg == "--image").unwrap() + 1;
    assert_eq!(args[image_index], target_resolved);

    let error = fixture.service().remove_image(IMAGE).unwrap_err();
    assert!(matches!(error, ProvisionError::Store(Error::RefMissing(_))));
    assert_eq!(fixture.calls().len(), 2);
}

#[test]
fn invalid_references_have_no_store_or_runtime_side_effects() {
    let fixture = Fixture::new();
    for image in [
        "alpine",
        "",
        "../escape:tag",
        "bad image:tag",
        "alpine@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "alpine:tag@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ] {
        let error = fixture.service().remove_image(image).unwrap_err();
        assert!(matches!(
            error,
            ProvisionError::Store(Error::InvalidRef { .. })
        ));
    }
    assert!(!Path::new(&fixture.config.hpc_image_store).exists());
    assert!(fixture.calls().is_empty());
}

#[test]
fn removal_does_not_create_or_initialize_the_authoritative_store() {
    for empty in [false, true] {
        let fixture = Fixture::new();
        let root = Path::new(&fixture.config.hpc_image_store);
        if empty {
            fs::create_dir(root).unwrap();
        }

        assert!(fixture.service().remove_image(IMAGE).is_err());
        assert_eq!(root.exists(), empty);
        if empty {
            assert_eq!(fs::read_dir(root).unwrap().count(), 0);
        }
        assert!(fixture.calls().is_empty());
    }
}

#[test]
fn runtime_cleanup_failure_leaves_the_authoritative_reference_retryable() {
    for (stage, call_count, diagnostic) in [
        ("podman", 1, "podman failure diagnostic"),
        ("parallax", 2, "parallax failure diagnostic"),
    ] {
        let fixture = Fixture::new();
        fixture.seed("source:tag", IMAGE);
        let store = fixture.store();
        let selector = ImageSelector::new(IMAGE).unwrap();
        let before = store.resolve(&selector).unwrap();

        fs::write(fixture.root().join("fail-stage"), stage).unwrap();
        let error = fixture.service().remove_image(IMAGE).unwrap_err();

        assert!(error.to_string().contains(diagnostic));
        assert_eq!(fixture.calls().len(), call_count);
        assert_eq!(store.resolve(&selector).unwrap(), before);
    }
}

#[test]
fn authoritative_recovery_failure_is_reported_after_runtime_cleanup() {
    let fixture = Fixture::new();
    fixture.seed("source:tag", IMAGE);
    fixture.seed("other:tag", KEEPER);
    let store = fixture.store();
    let keeper = store
        .resolve(&ImageSelector::new(KEEPER).unwrap())
        .unwrap()
        .unwrap();
    let digest = keeper.split_once("@sha256:").unwrap().1;
    let declaration = store.config().root().join("blobs/sha256").join(digest);
    fs::remove_file(&declaration).unwrap();

    let error = fixture.service().remove_image(IMAGE).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("RefRemoved"));
    assert!(message.contains("mutation recovery"));
    assert!(message.contains(declaration.to_str().unwrap()));

    let terminal = store
        .config()
        .root()
        .join("refs")
        .join(LocalRef::new(IMAGE).unwrap().encoded_path())
        .join("ref");
    assert_eq!(
        terminal.symlink_metadata().unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    assert_eq!(fixture.calls().len(), 2);
}
