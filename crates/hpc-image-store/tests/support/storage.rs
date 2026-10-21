//! Test callers prepare disposable storage; the library only inherits it.
use std::{fs, path::Path, process::Command};

pub fn run(command: &mut Command) -> Vec<u8> {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Return true after running acceptance in a child with test-owned storage.
/// An explicitly supplied disposable context is validated and used directly.
pub fn run_if_needed(test: &str, ignored: bool) -> bool {
    if let Some(disposable) = std::env::var_os("HPC_IMAGE_STORE_TEST_DISPOSABLE") {
        assert_eq!(
            disposable, "1",
            "explicit disposable-storage opt-in must be 1"
        );
        assert!(
            std::env::var_os("CONTAINERS_STORAGE_CONF").is_some(),
            "caller must supply disposable storage configuration; see README"
        );
        assert!(
            run(Command::new("podman").args(["images", "--quiet"])).is_empty(),
            "acceptance needs empty disposable storage; cached layers can mask failed exports"
        );
        return false;
    }

    let temporary = tempfile::Builder::new()
        .prefix("hpc-rootless-")
        .tempdir()
        .unwrap();
    let directory = temporary.path();
    let graphroot = directory.join("storage");
    let configuration = directory.join("storage.conf");
    fs::write(
        &configuration,
        format!(
            "[storage]\ndriver = \"overlay\"\nrunroot = {}\ngraphroot = {}\nrootless_storage_path = {}\n",
            serde_json::to_string(&directory.join("run")).unwrap(),
            serde_json::to_string(&graphroot).unwrap(),
            serde_json::to_string(&graphroot).unwrap(),
        ),
    )
    .unwrap();
    // Verify the effective root before any test or destructive reset. Older
    // rootless Podman also needs rootless_storage_path to select this store.
    let selected = run(Command::new("podman")
        .env("CONTAINERS_STORAGE_CONF", &configuration)
        .args(["info", "--format", "{{.Store.GraphRoot}}"]));
    assert_eq!(
        Path::new(String::from_utf8_lossy(&selected).trim()),
        graphroot
    );

    // Never mutate the Cargo runner's environment while tests run in parallel.
    let mut child = Command::new(std::env::current_exe().unwrap());
    child.args(["--exact", test, "--nocapture"]);
    if ignored {
        child.arg("--ignored");
    }
    let status = child
        .env("CONTAINERS_STORAGE_CONF", &configuration)
        .env("HPC_IMAGE_STORE_TEST_DISPOSABLE", "1")
        .status();
    // Always release test-owned overlay mounts, including after test failure
    // or a failure to launch the child. Caller-supplied storage is never reset.
    let cleanup = Command::new("podman")
        .env("CONTAINERS_STORAGE_CONF", &configuration)
        .args(["system", "reset", "--force"])
        .output();
    let cleanup = cleanup.expect("reset test-owned containers-storage");
    assert!(
        cleanup.status.success(),
        "{}",
        String::from_utf8_lossy(&cleanup.stderr)
    );
    let status = status.expect("launch real-tool acceptance");
    assert!(status.success(), "real-tool acceptance failed: {status}");
    true
}
