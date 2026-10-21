use crate::{
    Error, LocalRef, RefMetadata, Sha256Digest, Store, StoreConfig, StoreMetadata,
    layout::{DIRECTORIES, INIT_TEMP},
    locks::{Mode, Operation},
};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

fn store(path: &Path) -> Store {
    Store::new(StoreConfig::new(path).unwrap())
}
fn digest() -> Sha256Digest {
    Sha256Digest::new(format!("sha256:{}", "a".repeat(64))).unwrap()
}
fn initialize(path: &Path) {
    drop(store(path).prepare(Operation::Import).unwrap());
}

#[test]
fn local_name_grammar_and_boundaries() {
    for value in [
        "analysis/tool:release",
        "a.b/c__d/e---f:Release_1.2",
        "sha256:latest",
        "a:_",
        "host:5000/a:t",
        "Registry.EXAMPLE:05000/team/tool:Release_1",
    ] {
        let reference = LocalRef::new(value).unwrap();
        assert_eq!(reference.to_string(), value);
        assert_eq!(format!("{}:{}", reference.name(), reference.tag()), value);
    }
    for value in [
        "",
        "a",
        "a:",
        ":tag",
        "/a:tag",
        "a//b:tag",
        "../a:tag",
        "a/../b:tag",
        "a/./b:tag",
        "a\\b:tag",
        "a:..",
        "a:.",
        "A:tag",
        "a___b:tag",
        "a..b:tag",
        "a-:tag",
        "a: white",
        "a:é",
        "a:t@sha256:abc",
        "docker://a:t",
        "a:t\n",
    ] {
        assert!(LocalRef::new(value).is_err(), "accepted {value:?}");
    }
    assert!(LocalRef::new(format!("{}:{}", "a".repeat(255), "T".repeat(128))).is_ok());
    assert!(LocalRef::new(format!("{}:t", "a".repeat(256))).is_err());
    assert!(LocalRef::new(format!("a:{}", "T".repeat(129))).is_err());
    for hex in ["a".repeat(64), "A".repeat(64)] {
        assert!(LocalRef::new(format!("sha256:{hex}")).is_err());
    }
}

#[test]
fn encoding_is_canonical_reversible_and_bounded() {
    for value in [
        "a:t".to_string(),
        "a:tag".to_string(),
        "a/tag:t".to_string(),
        format!("{}:{}", "a".repeat(255), "Z".repeat(128)),
    ] {
        let reference = LocalRef::new(value).unwrap();
        let encoded = reference.encoded_path();
        assert!(
            encoded
                .components()
                .all(|part| part.as_os_str().len() <= 120)
        );
        assert_eq!(LocalRef::from_encoded_path(&encoded).unwrap(), reference);
    }
    for value in [
        "",
        "/613a74",
        "../613a74",
        "./613a74",
        "613a74/",
        "613A74",
        "613a7",
        "61/3a74",
        "ff",
        "613a74//",
    ] {
        assert!(
            LocalRef::from_encoded_path(Path::new(value)).is_err(),
            "accepted {value:?}"
        );
    }
    let short = LocalRef::new(format!("{}:t", "a".repeat(58))).unwrap();
    let long = LocalRef::new(format!("{}:tag", "a".repeat(58))).unwrap();
    assert!(long.encoded_path().starts_with(short.encoded_path()));
    let temporary = tempfile::tempdir().unwrap();
    let handle = store(temporary.path());
    let guard = handle.prepare(Operation::Import).unwrap();
    let _short_lock = guard.lock_ref(&short).unwrap();
    let _long_lock = guard.lock_ref(&long).unwrap();
    assert_ne!(
        handle.layout.ref_entry(&short),
        handle.layout.ref_entry(&long)
    );
}

#[test]
fn digests_and_deserialization_validate_inputs() {
    let valid = digest();
    assert_eq!(valid.hex(), "a".repeat(64));
    assert_eq!(
        serde_json::from_str::<Sha256Digest>(&serde_json::to_string(&valid).unwrap()).unwrap(),
        valid
    );
    for value in [
        "sha256:a".to_string(),
        format!("sha512:{}", "a".repeat(64)),
        format!("sha256:{}", "A".repeat(64)),
        format!("sha256:{}", "g".repeat(64)),
    ] {
        assert!(Sha256Digest::new(&value).is_err());
        assert!(
            serde_json::from_str::<Sha256Digest>(&serde_json::to_string(&value).unwrap()).is_err()
        );
    }
    assert!(serde_json::from_str::<LocalRef>("\"../a:tag\"").is_err());
}

#[test]
fn metadata_round_trips_and_checks_versions() {
    let reference = RefMetadata::new(LocalRef::new("a:t").unwrap(), digest());
    assert_eq!(
        RefMetadata::from_json(&serde_json::to_vec(&reference).unwrap()).unwrap(),
        reference
    );
    assert_eq!(
        StoreMetadata::from_json(b"{\"version\":1}")
            .unwrap()
            .version(),
        1
    );
    for bytes in [b"{}".as_slice(), b"null", b"{", b"{\"version\":\"1\"}"] {
        assert!(StoreMetadata::from_json(bytes).is_err());
    }
    assert!(matches!(
        StoreMetadata::from_json(b"{\"version\":2}"),
        Err(Error::UnsupportedVersion { .. })
    ));
    assert!(matches!(
        RefMetadata::from_json(b"{\"version\":1}"),
        Err(Error::UnsupportedVersion { .. })
    ));
    assert!(matches!(
        RefMetadata::from_json(b"{\"version\":2}"),
        Err(Error::UnsupportedVersion { version: 2, .. })
    ));
    let mut value = serde_json::to_value(&reference).unwrap();
    value["content"]
        .as_object_mut()
        .unwrap()
        .remove("config_digest");
    assert!(RefMetadata::from_json(&serde_json::to_vec(&value).unwrap()).is_err());
    assert!(
        RefMetadata::from_json(
            b"{\"version\":1,\"local_ref\":\"../a:t\",\"stored_root\":\"sha256:no\"}"
        )
        .is_err()
    );
}

#[test]
fn image_selectors_preserve_components_and_validate_named_references() {
    use crate::ImageSelector;
    let root = digest();
    for (reference, name, tag) in [
        ("tool:Release_1", "tool", Some("Release_1")),
        ("tool", "tool", None),
        (
            "registry.example:5000/team/tool:old",
            "registry.example:5000/team/tool",
            Some("old"),
        ),
        (
            "Registry.EXAMPLE:05000/team/tool",
            "Registry.EXAMPLE:05000/team/tool",
            None,
        ),
        ("a_b/c__d/e---f:tag", "a_b/c__d/e---f", Some("tag")),
    ] {
        let value = format!("{reference}@{root}");
        let selected = ImageSelector::new(&value).unwrap();
        assert_eq!(selected.as_str(), value);
        assert_eq!(selected.to_string(), value);
        assert_eq!(selected.reference(), reference);
        assert_eq!(selected.name(), name);
        assert_eq!(selected.tag(), tag);
        assert_eq!(selected.digest(), Some(&root));
        assert_eq!(selected, value.parse().unwrap());
        if tag.is_some() {
            let local = LocalRef::new(reference).unwrap();
            let tagged = ImageSelector::from(local.clone());
            assert_eq!(tagged, ImageSelector::new(reference).unwrap());
            assert_eq!(tagged.name(), local.name());
            assert_eq!(tagged.tag(), Some(local.tag()));
            assert_eq!(tagged.digest(), None);
            assert_eq!(
                LocalRef::from_encoded_path(&local.encoded_path()).unwrap(),
                local
            );
        }
        assert!(LocalRef::new(value).is_err());
    }
    for invalid in [
        "".to_owned(),
        "tool".into(),
        "a".repeat(64),
        root.to_string(),
        format!("sha256:{}", "A".repeat(64)),
        format!("@{root}"),
        format!("tool:@{root}"),
        format!("tool@@{root}"),
        format!("tool@{root}@{root}"),
        "tool@sha256:abc".into(),
        format!("tool@sha512:{}", "a".repeat(128)),
        format!("tool@sha256:{}", "A".repeat(64)),
        format!("tool:tag@{root}suffix"),
        "docker://tool:tag".into(),
        "[vfs@/tmp]tool:tag".into(),
        "host:port/tool:tag".into(),
        "host:/tool:tag".into(),
        "host..example/tool:tag".into(),
        "Upper/Upper:tag".into(),
        "host:5000/../tool:tag".into(),
        "host:5000/tool".into(),
        "host:5000//tool:tag".into(),
        "a:tag\n".into(),
    ] {
        assert!(
            ImageSelector::new(&invalid).is_err(),
            "accepted {invalid:?}"
        );
    }
    assert!(ImageSelector::new(format!("{}@{root}", "a".repeat(255))).is_ok());
    assert!(ImageSelector::new(format!("{}@{root}", "a".repeat(256))).is_err());
    assert!(ImageSelector::new(format!("a:{}@{root}", "t".repeat(128))).is_ok());
    assert!(ImageSelector::new(format!("a:{}@{root}", "t".repeat(129))).is_err());
}

#[test]
fn selector_digest_reservation_preserves_valid_sha256_repository_tags() {
    use crate::ImageSelector;
    for tag in [
        "latest".to_owned(),
        "g".repeat(64),
        "a".repeat(63),
        "a".repeat(65),
    ] {
        let value = format!("sha256:{tag}");
        let reference = LocalRef::new(&value).unwrap();
        assert_eq!(
            ImageSelector::new(value).unwrap(),
            ImageSelector::from(reference)
        );
    }
    for tag in ["a".repeat(64), "A".repeat(64), "aB".repeat(32)] {
        let value = format!("sha256:{tag}");
        assert!(LocalRef::new(&value).is_err());
        assert!(ImageSelector::new(&value).is_err());
    }
}

#[test]
fn handle_and_reads_do_not_initialize() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("missing");
    let handle = store(&path);
    assert_eq!(handle.config().root(), path);
    for operation in [
        Operation::Export,
        Operation::List,
        Operation::Check,
        Operation::Availability,
    ] {
        assert!(matches!(
            handle.prepare(operation),
            Err(Error::MissingStore(_))
        ));
        assert!(!path.exists());
    }
    fs::create_dir(&path).unwrap();
    assert!(handle.prepare(Operation::List).is_err());
    assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
    assert!(StoreConfig::new("").is_err());
}

#[test]
fn initialization_is_repeatable_and_preserves_permissions_and_lock_identity() {
    let temporary = tempfile::tempdir().unwrap();
    fs::create_dir(temporary.path().join("parents")).unwrap();
    let path = temporary.path().join("parents/store");
    initialize(&path);
    for directory in DIRECTORIES {
        assert_eq!(
            fs::metadata(path.join(directory))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    for file in ["store.json", "locks/store.lock"] {
        assert_eq!(
            fs::metadata(path.join(file)).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let lock = path.join("locks/store.lock");
    let inode = fs::metadata(&lock).unwrap().ino();
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o640)).unwrap();
    initialize(&path);
    assert_eq!(fs::metadata(&lock).unwrap().ino(), inode);
    assert_eq!(
        fs::metadata(&lock).unwrap().permissions().mode() & 0o777,
        0o640
    );
}

#[test]
fn interrupted_scaffolding_can_resume() {
    for count in 0..=DIRECTORIES.len() {
        let temporary = tempfile::tempdir().unwrap();
        for directory in &DIRECTORIES[..count] {
            fs::create_dir_all(temporary.path().join(directory)).unwrap();
        }
        fs::write(temporary.path().join(INIT_TEMP), b"partial JSON").unwrap();
        initialize(temporary.path());
        assert!(!temporary.path().join(INIT_TEMP).exists());
    }
}

#[test]
fn foreign_unsupported_and_incompatible_stores_are_rejected() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("foreign"), b"keep").unwrap();
    assert!(store(temporary.path()).prepare(Operation::Import).is_err());
    assert!(!temporary.path().join("locks").exists());
    assert_eq!(fs::read(temporary.path().join("foreign")).unwrap(), b"keep");
    for entry in [
        "refs/image",
        "blobs/sha256/object",
        "raw_images/data",
        "tmp/acquisition",
        "locks/refs/ref.lock",
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(entry);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"keep").unwrap();
        assert!(
            store(temporary.path()).prepare(Operation::Import).is_err(),
            "adopted {entry}"
        );
        assert_eq!(fs::read(&path).unwrap(), b"keep");
    }
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("store.json"), b"{\"version\":99}").unwrap();
    assert!(matches!(
        store(temporary.path()).prepare(Operation::Import),
        Err(Error::UnsupportedVersion { .. })
    ));
    assert!(!temporary.path().join("locks").exists());
    for entry in ["blobs", "locks", INIT_TEMP] {
        let temporary = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), temporary.path().join(entry)).unwrap();
        assert!(store(temporary.path()).prepare(Operation::Import).is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}

#[test]
fn reads_reject_missing_markers_and_symlinked_locks() {
    let temporary = tempfile::tempdir().unwrap();
    initialize(temporary.path());
    fs::remove_file(temporary.path().join("store.json")).unwrap();
    assert!(matches!(
        store(temporary.path()).prepare(Operation::List),
        Err(Error::MissingStore(_))
    ));
    let temporary = tempfile::tempdir().unwrap();
    initialize(temporary.path());
    let path = temporary.path().join("locks/store.lock");
    fs::remove_file(&path).unwrap();
    symlink(temporary.path().join("store.json"), &path).unwrap();
    assert!(store(temporary.path()).prepare(Operation::List).is_err());
}

#[test]
fn initialized_stores_are_not_repaired_and_ref_lock_paths_reject_symlinks() {
    let temporary = tempfile::tempdir().unwrap();
    initialize(temporary.path());
    fs::remove_dir(temporary.path().join("raw_images")).unwrap();
    assert!(store(temporary.path()).prepare(Operation::Import).is_err());
    assert!(!temporary.path().join("raw_images").exists());

    let temporary = tempfile::tempdir().unwrap();
    initialize(temporary.path());
    let handle = store(temporary.path());
    let reference = LocalRef::new("a:t").unwrap();
    let guard = handle.prepare(Operation::List).unwrap();
    let outside = tempfile::tempdir().unwrap();
    symlink(
        outside.path(),
        temporary
            .path()
            .join("locks/refs")
            .join(reference.encoded_path()),
    )
    .unwrap();
    assert!(guard.lock_ref(&reference).is_err());
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);

    let temporary = tempfile::tempdir().unwrap();
    initialize(temporary.path());
    let handle = store(temporary.path());
    let guard = handle.prepare(Operation::List).unwrap();
    drop(guard.lock_ref(&reference).unwrap());
    let lock = handle.layout.ref_lock(&reference);
    fs::remove_file(&lock).unwrap();
    let target = outside.path().join("keep");
    fs::write(&target, b"preserve").unwrap();
    symlink(&target, &lock).unwrap();
    assert!(guard.lock_ref(&reference).is_err());
    assert_eq!(fs::read(target).unwrap(), b"preserve");
}

#[test]
fn operation_matrix_is_explicit() {
    for operation in [Operation::Import, Operation::Pull, Operation::Remove] {
        assert_eq!(operation.store_mode(), Mode::Shared);
        assert_eq!(operation.ref_mode(), Some(Mode::Exclusive));
    }
    for operation in [Operation::Export, Operation::List, Operation::Check] {
        assert_eq!(operation.store_mode(), Mode::Shared);
        assert_eq!(operation.ref_mode(), Some(Mode::Shared));
        assert!(!operation.initializes());
    }
    assert_eq!(Operation::Prune.store_mode(), Mode::Exclusive);
    assert_eq!(Operation::Prune.ref_mode(), None);
}

// Exercise the actual internal API in a fresh process, without exposing a
// testing command in the development executable.
#[test]
#[ignore = "subprocess helper"]
fn lock_worker() {
    let handle = store(Path::new(&std::env::var("HPC_STORE_TEST_ROOT").unwrap()));
    let action = std::env::var("HPC_STORE_TEST_OPERATION").unwrap();
    println!("WORKER_ATTEMPT");
    std::io::stdout().flush().unwrap();
    wait_for_instruction();
    if action == "initialize" {
        drop(handle.prepare(Operation::Import).unwrap());
        worker_ready();
        wait_for_instruction();
        return;
    }
    if action == "permissions" {
        permission_cases();
        worker_ready();
        wait_for_instruction();
        return;
    }
    let pause = match action.as_str() {
        "bootstrap" => Some("initialization.scaffolding"),
        "pause-root" => Some("initialization.root-created"),
        "pause-lock" => Some("initialization.lock-created"),
        "pause-parent" => Some("initialization.parent-synced"),
        "pause-marker" => Some("initialization.marker-renamed"),
        _ => None,
    };
    if let Some(point) = pause {
        crate::fsutil::pause_once(point);
        drop(handle.prepare(Operation::Import).unwrap());
        return;
    }
    let operation = match action.as_str() {
        "import" => Operation::Import,
        "pull" => Operation::Pull,
        "remove" => Operation::Remove,
        "export" => Operation::Export,
        "list" => Operation::List,
        "check" => Operation::Check,
        "prune" => Operation::Prune,
        _ => panic!("unknown worker operation"),
    };
    let guard = handle.prepare(operation).unwrap();
    let reference = std::env::var("HPC_STORE_TEST_REF")
        .ok()
        .map(|value| LocalRef::new(value).unwrap());
    let ref_guard = reference
        .as_ref()
        .map(|reference| guard.lock_ref(reference).unwrap());
    worker_ready();
    wait_for_instruction();
    drop(ref_guard);
    drop(guard);
}

fn worker_ready() {
    println!("WORKER_READY");
    std::io::stdout().flush().unwrap();
}

fn wait_for_instruction() {
    let mut instruction = String::new();
    assert!(std::io::stdin().read_line(&mut instruction).unwrap() > 0);
}

struct Worker {
    child: Child,
    events: Receiver<String>,
}
impl Worker {
    fn start(root: &Path, operation: &str, reference: Option<&str>) -> Self {
        let mut worker = Self::start_paused(root, operation, reference);
        worker.begin();
        worker
    }

    fn start_paused(root: &Path, operation: &str, reference: Option<&str>) -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--ignored", "--exact", "tests::lock_worker", "--nocapture"])
            .env("HPC_STORE_TEST_ROOT", root)
            .env("HPC_STORE_TEST_OPERATION", operation)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        if let Some(reference) = reference {
            command.env("HPC_STORE_TEST_REF", reference);
        }
        if operation == "permissions" && unsafe { libc::geteuid() } == 0 {
            use std::os::unix::process::CommandExt;
            command.gid(65534).uid(65534);
        }
        let mut child = command.spawn().unwrap();
        let output = child.stdout.take().unwrap();
        let (sender, events) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let worker = Self { child, events };
        worker.wait_for("WORKER_ATTEMPT");
        worker
    }
    fn begin(&mut self) {
        writeln!(self.child.stdin.as_mut().unwrap()).unwrap();
    }
    fn wait_for(&self, event: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let line = self
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("worker did not reach expected state");
            if line.contains(event) {
                break;
            }
        }
    }
    fn assert_blocked(&self) {
        assert!(
            matches!(
                self.events.recv_timeout(Duration::from_millis(150)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "worker unexpectedly progressed"
        );
    }
    fn release(&mut self) {
        writeln!(self.child.stdin.as_mut().unwrap()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline, "worker did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn separate_processes_obey_the_lock_matrix() {
    let cases = [
        ("export", "check", Some("a:t"), false),
        ("list", "export", Some("a:t"), false),
        ("import", "export", Some("a:t"), true),
        ("pull", "remove", Some("a:t"), true),
        ("check", "import", Some("a:t"), true),
        ("import", "pull", Some("b:t"), false),
        ("remove", "prune", None, true),
        ("export", "prune", None, true),
        ("prune", "check", Some("a:t"), true),
        ("prune", "import", Some("a:t"), true),
    ];
    for (first, second, second_ref, blocked) in cases {
        let temporary = tempfile::tempdir().unwrap();
        initialize(temporary.path());
        let mut holder = Worker::start(
            temporary.path(),
            first,
            if first == "prune" { None } else { Some("a:t") },
        );
        holder.wait_for("WORKER_READY");
        let mut contender = Worker::start(temporary.path(), second, second_ref);
        if blocked {
            contender.assert_blocked();
        } else {
            contender.wait_for("WORKER_READY");
        }
        holder.release();
        if blocked {
            contender.wait_for("WORKER_READY");
        }
        contender.release();
    }
}

#[test]
fn concurrent_initialization_and_termination_release_locks() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("store");
    let mut workers: Vec<_> = (0..6)
        .map(|_| Worker::start_paused(&root, "initialize", None))
        .collect();
    for worker in &mut workers {
        worker.begin();
    }
    for worker in &workers {
        worker.wait_for("WORKER_READY");
    }
    for worker in &mut workers {
        worker.release();
    }
    let mut holder = Worker::start(&root, "import", Some("a:t"));
    holder.wait_for("WORKER_READY");
    let mut contender = Worker::start(&root, "remove", Some("a:t"));
    contender.assert_blocked();
    holder.child.kill().unwrap();
    holder.child.wait().unwrap();
    contender.wait_for("WORKER_READY");
    contender.release();
    let handle = store(&root);
    let reference = LocalRef::new("a:t").unwrap();
    let lock = handle.layout.ref_lock(&reference);
    let inode = fs::metadata(&lock).unwrap().ino();
    let guard = handle.prepare(Operation::Remove).unwrap();
    drop(guard.lock_ref(&reference).unwrap());
    assert_eq!(fs::metadata(lock).unwrap().ino(), inode);
}

#[test]
fn killed_bootstrap_can_be_resumed_without_replacing_its_lock() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("store");
    let mut bootstrap = Worker::start(&root, "bootstrap", None);
    bootstrap.wait_for("PUBLICATION_PAUSED");
    let lock = root.join("locks/store.lock");
    let inode = fs::metadata(&lock).unwrap().ino();
    let mut initializer = Worker::start(&root, "initialize", None);
    initializer.assert_blocked();
    bootstrap.child.kill().unwrap();
    bootstrap.child.wait().unwrap();
    initializer.wait_for("WORKER_READY");
    initializer.release();
    assert_eq!(fs::metadata(lock).unwrap().ino(), inode);
    assert!(root.join("store.json").is_file());
    assert!(!root.join(INIT_TEMP).exists());
}

#[test]
fn missing_parent_is_rejected_without_creating_ancestors() {
    let temporary = tempfile::tempdir().unwrap();
    let parent = temporary.path().join("missing/deeper");
    assert!(
        store(&parent.join("store"))
            .prepare(Operation::Import)
            .is_err()
    );
    assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 0);
}

#[test]
fn obsolete_setup_artifacts_are_rejected_without_changes() {
    for name in [
        ".store-bootstrap.json",
        ".store-bootstrap-abcdefghijkl",
        ".store-chain-abcdefghijkl",
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(name);
        fs::write(&path, b"obsolete data").unwrap();
        assert!(matches!(
            store(temporary.path()).prepare(Operation::Import),
            Err(Error::InvalidStore { .. })
        ));
        assert_eq!(fs::read(path).unwrap(), b"obsolete data");
        assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 1);
    }
}

#[test]
fn initialization_syncs_only_the_root_and_its_immediate_parent_in_order() {
    use crate::initialization::take_events;
    for existing in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().join("prepared");
        fs::create_dir(&parent).unwrap();
        let root = parent.join("store");
        if existing {
            fs::create_dir(&root).unwrap();
        }
        take_events();
        initialize(&root);
        let events = take_events();
        let mut expected = vec![
            ("temporary-marker-sync", root.join(INIT_TEMP)),
            ("lock-sync", root.join("locks/store.lock")),
        ];
        expected.extend(
            DIRECTORIES
                .iter()
                .rev()
                .map(|name| ("directory-sync", root.join(name))),
        );
        expected.extend([
            ("directory-sync", root.clone()),
            ("directory-sync", parent),
            ("marker-renamed", root.join("store.json")),
            ("directory-sync", root.clone()),
        ]);
        assert_eq!(events, expected);
        for name in ["blobs/sha256", "refs", "raw_images", "tmp", "locks/refs"] {
            assert_eq!(fs::read_dir(root.join(name)).unwrap().count(), 0);
        }
        // A supported version-1 store has no private recovery record.
        assert!(!root.join(".store-bootstrap.json").exists());
        initialize(&root);
        assert_eq!(
            take_events(),
            vec![
                ("marker-sync", root.join("store.json")),
                ("directory-sync", root.clone()),
            ]
        );
        drop(store(&root).prepare(Operation::List).unwrap());
        assert!(take_events().is_empty());
    }
}

#[test]
fn final_sync_failure_remains_a_failure_until_the_fault_clears() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("store");
    crate::fsutil::fail_persistently("initialization.final-root-sync", libc::EIO);
    for _ in 0..3 {
        assert!(matches!(store(&root).prepare(Operation::Import),
            Err(Error::InitializationDurability { path, source })
                if path == root && source.raw_os_error() == Some(libc::EIO)));
        assert!(root.join("store.json").is_file());
    }
    let inode = fs::metadata(root.join("locks/store.lock")).unwrap().ino();
    // Reads do not run the mutation durability preparation.
    drop(store(&root).prepare(Operation::List).unwrap());
    crate::fsutil::clear_failure();
    initialize(&root);
    assert_eq!(
        fs::metadata(root.join("locks/store.lock")).unwrap().ino(),
        inode
    );

    crate::fsutil::fail_persistently("initialization.marker-sync", libc::EIO);
    for _ in 0..2 {
        assert!(matches!(store(&root).prepare(Operation::Import),
            Err(Error::InitializationDurability { path, .. }) if path == root.join("store.json")));
    }
    crate::fsutil::clear_failure();
    initialize(&root);
}

#[test]
fn parent_sync_failure_preserves_retryable_setup_and_never_publishes_a_marker() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("store");
    crate::fsutil::fail_persistently("initialization.parent-sync", libc::EIO);
    for _ in 0..2 {
        assert!(
            matches!(store(&root).prepare(Operation::Import), Err(Error::Io { path, .. }) if path == temporary.path())
        );
        assert!(!root.join("store.json").exists());
        assert!(root.join(INIT_TEMP).is_file());
    }
    let root_inode = fs::metadata(&root).unwrap().ino();
    let lock_inode = fs::metadata(root.join("locks/store.lock")).unwrap().ino();
    crate::fsutil::clear_failure();
    crate::initialization::take_events();
    initialize(&root);
    let events = crate::initialization::take_events();
    let parent_sync = events
        .iter()
        .position(|(event, path)| *event == "directory-sync" && path == temporary.path())
        .unwrap();
    let publication = events
        .iter()
        .position(|(event, _)| *event == "marker-renamed")
        .unwrap();
    assert!(parent_sync < publication);
    assert_eq!(fs::metadata(&root).unwrap().ino(), root_inode);
    assert_eq!(
        fs::metadata(root.join("locks/store.lock")).unwrap().ino(),
        lock_inode
    );
}

#[test]
fn interrupted_initialization_preserves_root_and_lock_identities() {
    for action in [
        "pause-root",
        "pause-lock",
        "bootstrap",
        "pause-parent",
        "pause-marker",
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("store");
        let mut interrupted = Worker::start(&root, action, None);
        interrupted.wait_for("PUBLICATION_PAUSED");
        let root_inode = fs::metadata(&root).unwrap().ino();
        let lock = root.join("locks/store.lock");
        let lock_inode = fs::metadata(&lock).ok().map(|metadata| metadata.ino());
        interrupted.child.kill().unwrap();
        interrupted.child.wait().unwrap();
        crate::initialization::take_events();
        initialize(&root);
        let events = crate::initialization::take_events();
        if action == "pause-marker" {
            assert_eq!(
                events,
                vec![
                    ("marker-sync", root.join("store.json")),
                    ("directory-sync", root.clone())
                ]
            );
        } else {
            assert!(
                events
                    .iter()
                    .any(|(event, path)| *event == "directory-sync" && path == temporary.path())
            );
        }
        assert_eq!(fs::metadata(&root).unwrap().ino(), root_inode);
        if let Some(inode) = lock_inode {
            assert_eq!(fs::metadata(&lock).unwrap().ino(), inode);
        }
        assert!(root.join("store.json").is_file());
    }
}

#[test]
fn initialization_permission_contract_is_verified_without_root_privileges() {
    let temporary = tempfile::tempdir().unwrap();
    let mut worker = Worker::start(temporary.path(), "permissions", None);
    worker.wait_for("WORKER_READY");
    worker.release();
}

fn permission_cases() {
    assert_ne!(
        unsafe { libc::geteuid() },
        0,
        "permission checks require an unprivileged process"
    );
    let temporary = tempfile::tempdir().unwrap();
    let parent = temporary.path().join("parent");
    fs::create_dir(&parent).unwrap();
    let marked = parent.join("marked");
    initialize(&marked);
    let unmarked = parent.join("unmarked");
    fs::create_dir(&unmarked).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o100)).unwrap();
    initialize(&marked);
    initialize(&marked);
    assert!(matches!(store(&unmarked).prepare(Operation::Import),
        Err(Error::Io { path, source, .. }) if path == parent && source.kind() == std::io::ErrorKind::PermissionDenied));
    assert!(!unmarked.join("store.json").exists());
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
    initialize(&unmarked);
    let prepared = parent.join("prepared");
    fs::create_dir(&prepared).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o100)).unwrap();
    initialize(&prepared.join("store"));
    initialize(&prepared.join("store"));
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
}
