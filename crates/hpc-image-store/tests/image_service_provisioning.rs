use hpc_image_store::{ImageSelector, ImageService, ProvisionError, Store, StoreConfig};
use raster::config::Config;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tempfile::{TempDir, tempdir};

const SOURCE: &str = "registry.example/image:release";
const TOOL: &str = include_str!("fixtures/image-service-tools.py");

struct Fixture {
    directory: TempDir,
    config: Config,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempdir().unwrap();
        let bin = directory.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let root = serde_json::to_string(directory.path().to_str().unwrap()).unwrap();
        for name in ["skopeo", "podman", "parallax", "hook"] {
            let script = TOOL
                .replace(
                    "directory = pathlib.Path(os.environ[\"PULL_FIXTURE\"])",
                    &format!("directory = pathlib.Path({root})"),
                )
                .replace(
                    "tool = pathlib.Path(sys.argv[0]).name",
                    &format!("tool = {name:?}"),
                )
                .replace(
                    "failure = os.getenv(\"FAIL_STAGE\")",
                    "failure_file = directory / \"fail-stage\"\nfailure = failure_file.read_text() if failure_file.exists() else None",
                )
                .replace(
                    "else:\n        assert \"image\" in arguments and \"exists\" in arguments",
                    "elif \"rmi\" in arguments:\n        sys.exit(0)\n    else:\n        assert \"image\" in arguments and \"exists\" in arguments",
                );
            let path = bin.join(name);
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
            podman_path: bin.join("podman").display().to_string(),
            parallax_path: bin.join("parallax").display().to_string(),
            skopeo_path: bin.join("skopeo").display().to_string(),
            mksquashfs_path: directory.path().join("mksquashfs").display().to_string(),
            ..Config::default()
        };
        let mut config = config;
        config.hooks.parallax_imagestore_create = bin.join("hook").display().to_string();
        let fixture = Self { directory, config };
        fixture.select_image("first");
        fixture
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn service(&self) -> ImageService<'_> {
        ImageService::new(&self.config).unwrap()
    }

    fn store(&self) -> Store {
        Store::new(StoreConfig::new(&self.config.hpc_image_store).unwrap())
    }

    fn workspace(&self) -> PathBuf {
        self.root().join("job")
    }

    fn select_image(&self, revision: &str) {
        let output = std::process::Command::new(&self.config.skopeo_path)
            .args(["--prepare", revision])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn resolved(&self) -> String {
        format!(
            "registry.example/image@{}",
            fs::read_to_string(self.root().join("root")).unwrap()
        )
    }

    fn calls(&self) -> Vec<Value> {
        fs::read_to_string(self.root().join("calls"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn reset_calls(&self) {
        fs::write(self.root().join("calls"), "").unwrap();
    }

    fn fail_at(&self, stage: &str) {
        fs::write(self.root().join("fail-stage"), stage).unwrap();
    }
}

fn args(entry: &Value) -> Vec<&str> {
    entry["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect()
}

#[test]
fn listing_initializes_only_the_authoritative_store() {
    let fixture = Fixture::new();

    let report = fixture.service().list_images().unwrap();

    assert!(report.entries.is_empty());
    assert!(
        Path::new(&fixture.config.hpc_image_store)
            .join("store.json")
            .is_file()
    );
    assert!(!Path::new(&fixture.config.parallax_imagestore).exists());
    assert!(fixture.calls().is_empty());
}

#[test]
fn pull_refreshes_the_authoritative_store_before_migrating_the_resolved_digest() {
    let fixture = Fixture::new();

    let selected = fixture
        .service()
        .pull_image(SOURCE, &fixture.workspace())
        .unwrap();
    let resolved = fixture.resolved();

    assert_eq!(selected.as_str(), resolved);
    assert_eq!(
        fixture
            .store()
            .resolve(&ImageSelector::new(SOURCE).unwrap())
            .unwrap(),
        Some(resolved.clone())
    );
    assert_eq!(
        fs::read_to_string(fixture.root().join("migrated")).unwrap(),
        resolved
    );
    let calls = fixture.calls();
    let migration = calls
        .iter()
        .find(|entry| entry["tool"] == "parallax" && args(entry).contains(&"--migrate"))
        .unwrap();
    assert_eq!(args(migration).last(), Some(&selected.as_str()));
}

#[test]
fn an_existing_runtime_digest_skips_export_and_migration() {
    let fixture = Fixture::new();
    fixture
        .service()
        .pull_image(SOURCE, &fixture.workspace())
        .unwrap();
    fs::remove_file(fixture.root().join("exported")).unwrap();
    fixture.reset_calls();

    let selected = fixture
        .service()
        .pull_image(SOURCE, &fixture.workspace())
        .unwrap();

    assert_eq!(selected.as_str(), fixture.resolved());
    assert!(!fixture.root().join("exported").exists());
    let calls = fixture.calls();
    assert!(!calls.iter().any(|entry| args(entry).contains(&"--migrate")));
    assert!(calls.iter().any(|entry| args(entry).contains(&"--exist")));
}

#[test]
fn failures_after_acquisition_preserve_the_authoritative_image() {
    for stage in ["export", "migrate", "exist"] {
        let fixture = Fixture::new();
        fixture.fail_at(stage);

        let error = fixture
            .service()
            .pull_image(SOURCE, &fixture.workspace())
            .unwrap_err();

        let message = error.to_string();
        let expected = if stage == "exist" {
            "is unavailable in Parallax"
        } else {
            "diagnostic"
        };
        assert!(message.contains(expected), "{stage}: {error}");
        assert_eq!(
            fixture
                .store()
                .resolve(&ImageSelector::new(SOURCE).unwrap())
                .unwrap(),
            Some(fixture.resolved()),
            "{stage}"
        );
    }
}

#[test]
fn invalid_input_and_store_setup_fail_before_acquisition() {
    let fixture = Fixture::new();
    let error = fixture
        .service()
        .pull_image("alpine", &fixture.workspace())
        .unwrap_err();
    assert!(matches!(error, ProvisionError::Store(_)));
    assert!(fixture.calls().is_empty());
    assert!(!Path::new(&fixture.config.hpc_image_store).exists());
    assert!(!Path::new(&fixture.config.parallax_imagestore).exists());

    fixture.fail_at("hook");
    let error = fixture
        .service()
        .pull_image(SOURCE, &fixture.workspace())
        .unwrap_err();
    assert!(error.to_string().contains("hook diagnostic"));
    assert_eq!(
        fixture
            .store()
            .resolve(&ImageSelector::new(SOURCE).unwrap())
            .unwrap(),
        None
    );
    assert!(
        !fixture
            .calls()
            .iter()
            .any(|entry| entry["tool"] == "skopeo")
    );
}
