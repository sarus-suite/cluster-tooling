//! Controlled Skopeo producer: no network, alternate storage, or production hooks.
use hpc_image_store::{
    ImageSelector, PullContext, PullOptions, PullReport, RefMetadata, Result, Sha256Digest, Store,
    StoreConfig,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

pub const MANIFEST_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
pub const INDEX_TYPE: &str = "application/vnd.oci.image.index.v1+json";
pub const SOURCE: &str = "registry.example/image:mutable";
pub const DESTINATION: &str = "cached/image:tag";

pub fn selector(value: impl AsRef<str>) -> ImageSelector {
    ImageSelector::new(value.as_ref()).unwrap()
}
pub fn digest(bytes: &[u8]) -> Sha256Digest {
    Sha256Digest::new(format!("sha256:{:x}", Sha256::digest(bytes))).unwrap()
}
pub fn descriptor(bytes: &[u8], media_type: &str) -> Value {
    json!({"mediaType":media_type,"digest":digest(bytes),"size":bytes.len()})
}

const TOOL: &str = r#"#!/usr/bin/python3
import json, os, pathlib, sys
p = pathlib.Path(__file__)
c = json.loads(p.with_suffix('.json').read_text())
a = sys.argv[1:]
with open(c['log'], 'a') as f:
    f.write(json.dumps({'args':a, 'environment':os.getenv('HPC_PULL_TEST_ENV')}) + '\n')
if '--help' in a:
    print(c.get('help', '--raw --retry-times --dest-shared-blob-dir --preserve-digests --authfile --cert-dir --src-cert-dir --tls-verify --src-tls-verify --policy'))
    sys.exit(0)
if 'inspect' in a:
    if c.get('inspect_failure'):
        sys.stderr.write(c['inspect_failure'])
        sys.exit(19)
    reference = a[-1][len('docker://'):]
    response = c.get('responses', {}).get(reference)
    if response is None and '@' in reference:
        candidate = pathlib.Path(c['fixture']) / 'blobs' / 'sha256' / reference.rsplit(':', 1)[-1]
        if candidate.exists(): response = str(candidate)
    data = pathlib.Path(response or c['raw']).read_bytes()
    if c.get('change_tag_after_inspect') and '@' not in reference:
        pathlib.Path(c['raw']).write_bytes(b'changed tag must never be read again')
    sys.stdout.buffer.write(data)
    sys.exit(0)
assert 'copy' in a
assert '--preserve-digests' in a
assert '--all' not in a and '--format' not in a
if c.get('pause'):
    pathlib.Path(c['ready']).write_text(str(os.getpid()))
    with open(c['release'], 'r') as f: f.read()
if c.get('copy_failure'):
    sys.stderr.write(c['copy_failure'])
    sys.exit(17)
fixture = pathlib.Path(c['fixture'])
layout = pathlib.Path(a[-1][4:])
blobs = pathlib.Path(a[a.index('--dest-shared-blob-dir') + 1]) / 'sha256'
for name in ['oci-layout', 'index.json']:
    (layout / name).write_bytes((fixture / name).read_bytes())
for blob in (fixture / 'blobs' / 'sha256').iterdir():
    (blobs / blob.name).write_bytes(blob.read_bytes())
print('tool progress stays out of result JSON')
"#;

pub struct Fixture {
    pub temporary: tempfile::TempDir,
    pub store: Store,
    pub context: PullContext,
    pub config: Value,
    pub manifest: Vec<u8>,
    pub root: Sha256Digest,
    pub layer: Sha256Digest,
}
impl Fixture {
    pub fn new() -> Self {
        Self::with_platform("linux", "amd64", None)
    }
    pub fn with_platform(os: &str, architecture: &str, variant: Option<&str>) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path();
        let fixture = directory.join("fixture");
        fs::create_dir_all(fixture.join("blobs/sha256")).unwrap();
        let layer_bytes = b"synthetic layer bytes; checks do not unpack tar";
        let layer = descriptor(layer_bytes, "application/vnd.oci.image.layer.v1.tar+gzip");
        let config_bytes = serde_json::to_vec(&json!({"os":os,"architecture":architecture,"variant":variant,"created":"2020-01-01T00:00:00Z","rootfs":{"type":"layers","diff_ids":[]}})).unwrap();
        let config = descriptor(&config_bytes, "application/vnd.oci.image.config.v1+json");
        let manifest = serde_json::to_vec_pretty(&json!({"schemaVersion":2,"mediaType":MANIFEST_TYPE,"config":config,"layers":[layer.clone(),layer]})).unwrap();
        for bytes in [&manifest[..], &config_bytes[..], &layer_bytes[..]] {
            fs::write(
                fixture.join("blobs/sha256").join(digest(bytes).hex()),
                bytes,
            )
            .unwrap();
        }
        fs::write(
            fixture.join("oci-layout"),
            br#"{"imageLayoutVersion":"1.0.0"}"#,
        )
        .unwrap();
        fs::write(
            fixture.join("index.json"),
            serde_json::to_vec(
                &json!({"schemaVersion":2,"manifests":[descriptor(&manifest,MANIFEST_TYPE)]}),
            )
            .unwrap(),
        )
        .unwrap();
        let raw = directory.join("raw");
        fs::write(&raw, &manifest).unwrap();
        let skopeo = directory.join("skopeo");
        fs::write(&skopeo, TOOL).unwrap();
        fs::set_permissions(&skopeo, fs::Permissions::from_mode(0o700)).unwrap();
        let config = json!({"fixture":fixture,"raw":raw,"log":directory.join("log"),"ready":directory.join("ready"),"release":directory.join("release")});
        let store = Store::new(StoreConfig::new(directory.join("store")).unwrap());
        let context = PullContext {
            skopeo,
            ..PullContext::default()
        };
        let result = Self {
            temporary,
            store,
            context,
            config,
            root: digest(&manifest),
            layer: digest(layer_bytes),
            manifest,
        };
        result.save();
        result
    }
    pub fn save(&self) {
        fs::write(
            self.context.skopeo.with_extension("json"),
            serde_json::to_vec(&self.config).unwrap(),
        )
        .unwrap();
    }
    pub fn pull(&self, replace: bool) -> Result<PullReport> {
        self.store.pull(
            &selector(SOURCE),
            Some(&selector(DESTINATION)),
            &self.context,
            &PullOptions {
                replace,
                notes: Some("retained notes".into()),
            },
        )
    }
    pub fn metadata(&self) -> RefMetadata {
        self.store.list().unwrap().entries[0]
            .metadata
            .clone()
            .unwrap()
    }
    pub fn calls(&self) -> Vec<Value> {
        fs::read_to_string(self.temporary.path().join("log"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    pub fn set_raw(&self, bytes: &[u8]) {
        fs::write(self.config["raw"].as_str().unwrap(), bytes).unwrap();
    }
    pub fn set_index(&self, descriptors: Vec<Value>) {
        self.set_raw(
            &serde_json::to_vec(
                &json!({"schemaVersion":2,"mediaType":INDEX_TYPE,"manifests":descriptors}),
            )
            .unwrap(),
        );
    }
    pub fn cli(&self, executable: &str) -> Command {
        // The binary path is supplied by the integration-test caller.
        let mut command = Command::new(executable);
        command
            .arg("--store")
            .arg(self.store.config().root())
            .arg("--skopeo")
            .arg(&self.context.skopeo);
        command
    }
    pub fn pointer(&self) -> std::path::PathBuf {
        self.store
            .config()
            .root()
            .join("refs")
            .join(
                hpc_image_store::LocalRef::new(DESTINATION)
                    .unwrap()
                    .encoded_path(),
            )
            .join("ref")
    }
    pub fn fixture(&self) -> &Path {
        Path::new(self.config["fixture"].as_str().unwrap())
    }
}
