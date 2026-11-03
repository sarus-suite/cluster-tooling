//! Real OCI-to-OCI copying into recorded staging, without Podman or a registry.
use super::*;
use crate::{test_snapshot::snapshot, test_support::SyntheticImage};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Write,
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    process::{Child, ExitStatus},
};

fn streaming_source() -> (SyntheticImage, Sha256Digest) {
    fn object(
        objects: &mut BTreeMap<Sha256Digest, Vec<u8>>,
        bytes: Vec<u8>,
        media: &str,
    ) -> Descriptor {
        let digest = Sha256Digest::new(format!("sha256:{:x}", Sha256::digest(&bytes))).unwrap();
        let descriptor = Descriptor {
            media_type: media.into(),
            digest: digest.clone(),
            size: bytes.len() as u64,
            extra: BTreeMap::new(),
        };
        objects.insert(digest, bytes);
        descriptor
    }
    let mut objects = BTreeMap::new();
    let layer = object(
        &mut objects,
        vec![b'x'; 4096],
        "application/vnd.oci.image.layer.v1.tar",
    );
    let config = object(
        &mut objects,
        serde_json::to_vec(&json!({
            "os":"linux", "architecture":"amd64",
            "rootfs":{"type":"layers", "diff_ids":[layer.digest]}
        }))
        .unwrap(),
        OCI_CONFIG,
    );
    let root = object(
        &mut objects,
        serde_json::to_vec(&json!({
            "schemaVersion":2, "mediaType":OCI_MANIFEST, "config":config, "layers":[layer]
        }))
        .unwrap(),
        OCI_MANIFEST,
    );
    (SyntheticImage { root, objects }, layer.digest)
}

#[test]
fn real_skopeo_subject_output_is_rejected_in_private_recorded_staging() {
    let mut fixture = Fixture::new();
    fixture.import(false).unwrap();
    let refs_before = snapshot(&fixture.store.layout.root().join("refs"));
    let raw_before = snapshot(&fixture.store.layout.raw_images());
    let blobs_before = snapshot(&fixture.store.layout.blobs());
    let (original, _) = streaming_source();
    let mut source = original.with_manifest_change(|manifest| {
        manifest["subject"] = serde_json::to_value(&original.root).unwrap();
    });
    // Keep the accurately hashed subject in the source as well as the root.
    source.objects.insert(
        original.root.digest.clone(),
        original.objects[&original.root.digest].clone(),
    );
    fixture.select_image(&source);
    let source_path = PathBuf::from(fixture.config["fixture"].as_str().unwrap());
    let guard = fixture.store.prepare(Operation::Import).unwrap();
    let mutation = Mutation::begin(&guard, &fixture.reference, Kind::Import).unwrap();
    let stage = mutation.stage().unwrap();
    let executable =
        PathBuf::from(std::env::var_os("HPC_TEST_SKOPEO").unwrap_or_else(|| "skopeo".into()));
    subprocess::run(
        Command::new(&executable)
            .args([
                "copy",
                "--quiet",
                "--preserve-digests",
                "--dest-shared-blob-dir",
            ])
            .arg(stage.shared_blob_path())
            .arg(oci_argument(&source_path).unwrap())
            .arg(oci_argument(&stage.layout_path()).unwrap()),
        "real Skopeo subject-bearing acquisition",
        &executable,
        Some(guard.transfer_fd()),
    )
    .unwrap();
    let root = oci::single_root(&stage.layout_path()).unwrap();
    assert_eq!(root.digest, source.root.digest);
    let path = stage
        .shared_blob_path()
        .join("sha256")
        .join(root.digest.hex());
    let copied: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(
        copied.get("subject").is_some(),
        "regression requires real subject-bearing output"
    );
    let error = mutation.context(acquired_content(&stage).err().unwrap());
    assert!(
        matches!(&error, Error::Recovery { phase: "acquiring", change: None, source, .. }
        if matches!(source.as_ref(), Error::InvalidObject { path: actual, reason }
            if actual == &path && reason.contains("subject"))),
        "{error}"
    );
    assert_eq!(
        snapshot(&fixture.store.layout.root().join("refs")),
        refs_before
    );
    assert_eq!(snapshot(&fixture.store.layout.raw_images()), raw_before);
    assert_eq!(snapshot(&fixture.store.layout.blobs()), blobs_before);
    drop(stage);
    drop(mutation);
    drop(guard);
    assert!(!fixture.store.list().unwrap().has_errors());
    assert!(!fixture.store.list().unwrap().has_errors());
    assert_import_idle(&fixture.store);
    assert_eq!(
        snapshot(&fixture.store.layout.root().join("refs")),
        refs_before
    );
    assert_eq!(snapshot(&fixture.store.layout.raw_images()), raw_before);
    assert_eq!(snapshot(&fixture.store.layout.blobs()), blobs_before);
}

struct SkopeoCopy {
    worker: Child,
    pid_path: PathBuf,
    producer_killed: bool,
}
impl SkopeoCopy {
    fn producer(&self) -> Option<libc::pid_t> {
        fs::read_to_string(&self.pid_path)
            .ok()?
            .trim()
            .parse()
            .ok()
            .filter(|pid| *pid > 0)
    }
    fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.worker.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "Skopeo operation worker did not exit"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for SkopeoCopy {
    fn drop(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if self.worker.try_wait().ok().flatten().is_some() {
                return;
            }
            if !self.producer_killed
                && let Some(pid) = self.producer()
            {
                // SAFETY: this test wrapper recorded its own producer PID. Kill
                // the producer first so its waiting operation can reap it.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
                self.producer_killed = true;
            }
            if Instant::now() >= deadline {
                // Bound teardown even if setup or the operation wrapper failed.
                // The worker creates its own group, inherited by its producer.
                unsafe {
                    libc::kill(-(self.worker.id() as libc::pid_t), libc::SIGKILL);
                }
                let _ = self.worker.wait();
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn killed_real_skopeo_temporary_blob_recovers_on_list() {
    let mut fixture = Fixture::new();
    fixture.import(false).unwrap();
    // Keep another name sharing all objects, and preserve the target identity.
    let target = fixture.reference.clone();
    fixture.reference = LocalRef::new("example/keeper:tag").unwrap();
    fixture.import(false).unwrap();
    fixture.reference = target;
    let refs_before = snapshot(&fixture.store.layout.root().join("refs"));
    let raw_before = snapshot(&fixture.store.layout.raw_images());
    let blobs_before = snapshot(&fixture.store.layout.blobs());
    let lock_before = snapshot(&fixture.store.layout.store_lock());
    let (source, layer) = streaming_source();
    fixture.select_image(&source);
    let source_path = PathBuf::from(fixture.config["fixture"].as_str().unwrap());
    let fifo = source_path.join("blobs/sha256").join(layer.hex());
    fs::remove_file(&fifo).unwrap();
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: name is NUL-terminated and the mode is valid.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);

    let root = fixture.temporary.path();
    let wrapper = root.join("real-skopeo");
    let pid_path = wrapper.with_extension("pid");
    fs::write(
        &wrapper,
        r#"#!/usr/bin/python3
import os, pathlib, sys
pathlib.Path(__file__).with_suffix('.pid').write_text(str(os.getpid()))
executable = os.environ['HPC_REAL_SKOPEO']
os.execvp(executable, [executable, *sys.argv[1:]])
"#,
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let diagnostics = root.join("interrupted-skopeo.stderr");
    let mut copy = SkopeoCopy {
        worker: Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "transfer::tests::transfer_worker",
                "--nocapture",
            ])
            .env("HPC_TRANSFER_TEST_DIRECTORY", root)
            .env("HPC_TRANSFER_TEST_OPERATION", "interrupted-recorded-skopeo")
            .env("HPC_TRANSFER_TEST_SKOPEO", &wrapper)
            .env("HPC_TRANSFER_TEST_OCI_SOURCE", &source_path)
            .env(
                "HPC_REAL_SKOPEO",
                std::env::var_os("HPC_TEST_SKOPEO").unwrap_or_else(|| "skopeo".into()),
            )
            .stdout(Stdio::null())
            .stderr(fs::File::create(&diagnostics).unwrap())
            .process_group(0)
            .spawn()
            .unwrap(),
        pid_path,
        producer_killed: false,
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut writer = loop {
        match fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo)
        {
            Ok(writer) => break writer,
            Err(error) if error.raw_os_error() == Some(libc::ENXIO) => (),
            Err(error) => panic!("open streaming source: {error}"),
        }
        assert!(
            copy.worker.try_wait().unwrap().is_none() && Instant::now() < deadline,
            "Skopeo did not open its source: {}",
            fs::read_to_string(&diagnostics).unwrap()
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    writer.write_all(&source.objects[&layer][..1024]).unwrap();
    // The source remains open and incomplete until the actual destination
    // temporary file appears. No duration assumption authorizes termination.
    let record: crate::mutation::Record =
        serde_json::from_slice(&fs::read(fixture.store.layout.pending_mutation()).unwrap())
            .unwrap();
    assert_eq!(record.phase, crate::mutation::Phase::Acquiring);
    let stage = fixture.store.layout.staging().join(record.staging.unwrap());
    let temporary = loop {
        let found = fs::read_dir(stage.join("layout"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("oci-put-blob")
            });
        if let Some(path) = found {
            break path;
        }
        assert!(
            copy.worker.try_wait().unwrap().is_none() && Instant::now() < deadline,
            "Skopeo did not create a destination temporary: {}",
            fs::read_to_string(&diagnostics).unwrap()
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(fs::symlink_metadata(&temporary).unwrap().is_file());
    assert!(!try_exclusive(&fixture.store.layout.store_lock()));
    {
        let _short = crate::locks::short_test_waits();
        assert!(matches!(
            fixture.store.list(),
            Err(Error::LockTimeout { .. })
        ));
        assert!(temporary.exists());
    }
    let pid = copy.producer().expect("real Skopeo PID");
    copy.producer_killed = true;
    // SAFETY: PID is the live producer, observed holding its source FIFO open.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    assert!(
        copy.wait().success(),
        "{}",
        fs::read_to_string(&diagnostics).unwrap()
    );
    drop(writer);
    assert!(
        temporary.is_file(),
        "SIGKILL must leave the actual producer file"
    );
    assert!(!fixture.store.list().unwrap().has_errors());
    assert!(!fixture.store.list().unwrap().has_errors());
    assert!(!stage.exists());
    assert_import_idle(&fixture.store);
    assert_eq!(
        snapshot(&fixture.store.layout.root().join("refs")),
        refs_before
    );
    assert_eq!(snapshot(&fixture.store.layout.raw_images()), raw_before);
    assert_eq!(snapshot(&fixture.store.layout.blobs()), blobs_before);
    assert_eq!(snapshot(&fixture.store.layout.store_lock()), lock_before);
}
