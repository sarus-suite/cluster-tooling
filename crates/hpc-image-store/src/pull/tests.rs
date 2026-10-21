use super::*;
use crate::{Acquisition, CheckScope, NamespaceChange, fsutil};
use serde_json::{Value, json};
use std::{
    fs,
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};

#[allow(dead_code)]
#[path = "../../tests/support/registry.rs"]
mod support;
use support::{DESTINATION, Fixture, SOURCE, descriptor, selector};

fn platform(architecture: &str, variant: Option<&str>) -> Platform {
    Platform {
        os: "linux".into(),
        architecture: architecture.into(),
        variant: variant.map(str::to_owned),
    }
}
fn candidate(fixture: &Fixture, architecture: &str, variant: Option<&str>) -> Value {
    let mut descriptor = descriptor(&fixture.manifest, graph::MANIFEST);
    descriptor["platform"] = serde_json::to_value(platform(architecture, variant)).unwrap();
    descriptor
}

#[test]
fn raw_hashing_keeps_whitespace_and_requires_manifest_digest_sources() {
    let fixture = Fixture::new();
    let mut bytes = fixture.manifest.clone();
    bytes.push(b'\n');
    let source = selector(format!("registry/image:ignored@{}", raw_digest(&bytes)));
    assert_ne!(raw_digest(&bytes), fixture.root);
    assert_eq!(
        resolve_raw(&fixture.context, &source, &bytes).unwrap(),
        raw_digest(&bytes)
    );
    assert!(
        resolve_raw(&fixture.context, &source, &fixture.manifest)
            .unwrap_err()
            .contains("does not match")
    );
    for media_type in [graph::INDEX, DOCKER_LIST] {
        let index = serde_json::to_vec(&json!({"schemaVersion":2,"mediaType":media_type,"manifests":[candidate(&fixture,"amd64",None)]})).unwrap();
        for source in [
            format!("registry/image@{}", raw_digest(&index)),
            format!("registry/image:tag@{}", raw_digest(&index)),
        ] {
            assert!(
                resolve_raw(&fixture.context, &selector(source), &index)
                    .unwrap_err()
                    .contains("digest sources must identify")
            );
        }
    }
    assert!(fixture.calls().is_empty());
}

#[test]
fn malformed_and_unsupported_registry_objects_fail() {
    let fixture = Fixture::new();
    let original: Value = serde_json::from_slice(&fixture.manifest).unwrap();
    for bytes in [
        b"invalid".to_vec(),
        b"[]".to_vec(),
        b"{}".to_vec(),
        b"null".to_vec(),
    ] {
        assert!(parse_raw(&bytes).is_err());
    }
    for (field, value) in [
        ("schemaVersion", json!(1)),
        (
            "mediaType",
            json!("application/vnd.docker.distribution.manifest.v2+json"),
        ),
        ("mediaType", Value::Null),
        ("artifactType", json!("artifact")),
        ("subject", json!({})),
        ("config", json!({})),
        ("layers", json!({})),
    ] {
        let mut object = original.clone();
        object[field] = value;
        assert!(
            parse_raw(&serde_json::to_vec(&object).unwrap()).is_err(),
            "accepted {field}"
        );
    }
    for mutate in [
        "mediaType",
        "digest",
        "size",
        "urls",
        "annotations",
        "artifactType",
        "platform",
    ] {
        let mut object = original.clone();
        object["layers"][0][mutate] = match mutate {
            "mediaType" => json!("application/vnd.docker.image.rootfs.diff.tar.gzip"),
            "digest" => json!("sha512:bad"),
            "size" => json!(-1),
            "urls" => json!([17]),
            "annotations" => json!({"key":17}),
            "platform" => json!({"os":"linux"}),
            _ => json!("artifact"),
        };
        assert!(
            parse_raw(&serde_json::to_vec(&object).unwrap()).is_err(),
            "accepted {mutate}"
        );
    }
    let mut object = original;
    object["layers"][1]["size"] = json!(1);
    assert!(parse_raw(&serde_json::to_vec(&object).unwrap()).is_err());
}

#[test]
fn index_platform_policy_is_bounded_and_order_independent() {
    assert_eq!(
        default_index_platform("x86_64").unwrap(),
        platform("amd64", None)
    );
    assert_eq!(
        default_index_platform("aarch64").unwrap(),
        platform("arm64", None)
    );
    assert!(
        default_index_platform("riscv64")
            .unwrap_err()
            .contains("explicit")
    );
    let fixture = Fixture::new();
    let a = candidate(&fixture, "arm64", Some("v8"));
    let mut b = a.clone();
    b["digest"] = json!(format!("sha256:{}", "a".repeat(64)));
    b["platform"]["variant"] = json!("v9");
    let descriptors: Vec<Descriptor> =
        serde_json::from_value(json!([a.clone(), b.clone(), a.clone()])).unwrap();
    assert_eq!(
        select_manifest(&descriptors, &platform("arm64", Some("v8")))
            .unwrap()
            .digest,
        fixture.root
    );
    assert!(
        select_manifest(&descriptors, &platform("arm64", None))
            .unwrap_err()
            .contains("2 distinct")
    );
    assert!(
        select_manifest(&descriptors, &platform("amd64", None))
            .unwrap_err()
            .contains("0 distinct")
    );
    let consistent: Vec<Descriptor> =
        serde_json::from_value(json!([a.clone(), a.clone()])).unwrap();
    assert!(select_manifest(&consistent, &platform("arm64", None)).is_ok());
    for field in ["size", "mediaType", "platform"] {
        let mut conflicting = a.clone();
        conflicting[field] = match field {
            "size" => json!(999),
            "mediaType" => json!(graph::INDEX),
            _ => json!({"os":"linux","architecture":"arm64","variant":"v9"}),
        };
        let descriptors: Vec<Descriptor> =
            serde_json::from_value(json!([a.clone(), conflicting])).unwrap();
        assert!(
            select_manifest(&descriptors, &platform("arm64", None))
                .unwrap_err()
                .contains("conflicting duplicate")
        );
    }
    for media_type in [
        graph::INDEX,
        DOCKER_LIST,
        "application/vnd.docker.distribution.manifest.v2+json",
    ] {
        let mut entry = candidate(&fixture, "amd64", None);
        entry["mediaType"] = json!(media_type);
        let descriptors: Vec<Descriptor> = serde_json::from_value(json!([entry])).unwrap();
        assert!(select_manifest(&descriptors, &platform("amd64", None)).is_err());
    }
    let mut missing = a;
    missing.as_object_mut().unwrap().remove("platform");
    let descriptors: Vec<Descriptor> = serde_json::from_value(json!([missing])).unwrap();
    assert!(select_manifest(&descriptors, &platform("arm64", None)).is_err());
}

#[test]
fn pull_pins_tag_once_builds_provenance_and_checks_locally() {
    for envelope in [None, Some(graph::INDEX), Some(DOCKER_LIST)] {
        let mut fixture = Fixture::new();
        if let Some(media_type) = envelope {
            fixture.set_raw(&serde_json::to_vec(&json!({"schemaVersion":2,"mediaType":media_type,"manifests":[candidate(&fixture,"amd64",None)]})).unwrap());
            fixture.context.platform = Some(platform("amd64", None));
        }
        fixture.config["change_tag_after_inspect"] = json!(true);
        fixture.save();
        let report = fixture.pull(false).unwrap();
        assert_eq!(report.stored_root, fixture.root);
        assert_eq!(report.resolved_digest, fixture.root);
        assert_eq!(report.requested_reference, SOURCE);
        assert!(report.cleanup_warnings.is_empty());
        let metadata = fixture.metadata();
        assert_eq!(metadata.version(), 3);
        assert_eq!(metadata.notes.as_deref(), Some("retained notes"));
        assert!(
            matches!(metadata.acquisition,Acquisition::Pull{requested_reference,resolved_digest} if requested_reference==SOURCE && resolved_digest==fixture.root)
        );
        let blobs = fixture.store.config().root().join("blobs/sha256");
        let size: u64 = fs::read_dir(&blobs)
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum();
        assert_eq!(report.content.size_bytes, size);
        assert_eq!(fs::read_dir(blobs).unwrap().count(), 3);
        assert_eq!(
            fs::read_dir(fixture.store.config().root().join("tmp"))
                .unwrap()
                .count(),
            0
        );
        assert!(!fixture.store.list().unwrap().has_errors());
        for selector in [
            selector(DESTINATION),
            selector(format!("cached/image:ignored@{}", fixture.root)),
        ] {
            assert!(
                !fixture
                    .store
                    .check(&CheckScope::Image(selector))
                    .unwrap()
                    .has_errors()
            );
        }
        let calls = fixture.calls();
        let network: Vec<_> = calls
            .iter()
            .filter(|entry| !entry["args"].as_array().unwrap().contains(&json!("--help")))
            .collect();
        assert_eq!(network.len(), if envelope.is_some() { 3 } else { 2 });
        assert_eq!(
            network[0]["args"].as_array().unwrap().last().unwrap(),
            &json!(format!("docker://{SOURCE}"))
        );
        let copy = network.last().unwrap()["args"].as_array().unwrap();
        assert!(copy.contains(&json!(format!(
            "docker://registry.example/image@{}",
            fixture.root
        ))));
        assert!(copy.contains(&json!("--preserve-digests")));
        assert!(!copy.contains(&json!("--override-arch")));
    }
}

#[test]
fn child_verification_rejects_digest_size_type_and_nested_indexes_before_copy() {
    for failure in ["digest", "size", "type", "nested", "malformed"] {
        let mut fixture = Fixture::new();
        let mut entry = candidate(&fixture, "amd64", None);
        match failure {
            "digest" => {
                entry["digest"] = json!(format!("sha256:{}", "a".repeat(64)));
            }
            "size" => {
                entry["size"] = json!(1);
            }
            "type" | "nested" | "malformed" => {
                let child = if failure == "malformed" {
                    b"{".to_vec()
                } else {
                    serde_json::to_vec(&json!({"schemaVersion":2,"mediaType":if failure=="nested"{graph::INDEX}else{DOCKER_LIST},"manifests":[]})).unwrap()
                };
                entry["digest"] = json!(raw_digest(&child));
                entry["size"] = json!(child.len());
                let path = fixture.temporary.path().join("child");
                fs::write(&path, child).unwrap();
                fixture.config["responses"][format!(
                    "registry.example/image@{}",
                    entry["digest"].as_str().unwrap()
                )] = json!(path);
            }
            _ => unreachable!(),
        }
        fixture.set_index(vec![entry]);
        fixture.context.platform = Some(platform("amd64", None));
        fixture.save();
        assert!(fixture.pull(false).is_err(), "accepted {failure}");
        assert!(!fixture.calls().iter().any(|entry| {
            entry["args"].as_array().unwrap().contains(&json!("copy"))
                && !entry["args"].as_array().unwrap().contains(&json!("--help"))
        }));
        assert!(fixture.store.list().unwrap().entries.is_empty());
    }
}

#[test]
fn direct_manifest_platform_and_explicit_config_validation() {
    let fixture = Fixture::with_platform("otheros", "otherarch", None);
    assert_eq!(
        fixture.pull(false).unwrap().content.platform.architecture,
        "otherarch"
    );
    for (os, arch, variant) in [
        ("windows", "amd64", None),
        ("linux", "arm64", None),
        ("linux", "amd64", Some("v3")),
    ] {
        let mut fixture = Fixture::new();
        fixture.context.platform = Some(Platform {
            os: os.into(),
            architecture: arch.into(),
            variant: variant.map(str::to_owned),
        });
        assert!(
            fixture
                .pull(false)
                .unwrap_err()
                .to_string()
                .contains("completed configuration platform")
        );
        assert!(fixture.store.list().unwrap().entries.is_empty());
    }
    let mut fixture = Fixture::with_platform("linux", "arm64", Some("v8"));
    fixture.context.platform = Some(platform("arm64", Some("v8")));
    fixture.set_index(vec![candidate(&fixture, "arm64", Some("v8"))]);
    fixture.pull(false).unwrap();
    let mut fixture = Fixture::new();
    fixture.context.platform = Some(platform("arm64", None));
    fixture.set_index(vec![candidate(&fixture, "arm64", None)]);
    assert!(
        fixture
            .pull(false)
            .unwrap_err()
            .to_string()
            .contains("completed configuration platform")
    );
}

#[test]
fn naming_combined_selectors_and_ignored_destination_digests() {
    let fixture = Fixture::new();
    let source_digest = format!("registry.example/image@{}", fixture.root);
    let combined = format!("registry.example/image:old@{}", fixture.root);
    let dest_digest = format!("renamed@{}", fixture.layer);
    for (source, destination, expected) in [
        (SOURCE, None, SOURCE.to_owned()),
        (
            source_digest.as_str(),
            None,
            format!("registry.example/image:{}", &fixture.root.hex()[..12]),
        ),
        (
            combined.as_str(),
            None,
            "registry.example/image:old".to_owned(),
        ),
        (
            combined.as_str(),
            Some(dest_digest.as_str()),
            format!("renamed:{}", &fixture.root.hex()[..12]),
        ),
    ] {
        let source = selector(source);
        let destination = destination.map(selector);
        let report = fixture
            .store
            .pull(
                &source,
                destination.as_ref(),
                &fixture.context,
                &PullOptions::default(),
            )
            .unwrap();
        assert_eq!(report.local_ref.as_str(), expected);
        assert_eq!(
            report.ignored_destination_digest,
            destination
                .as_ref()
                .and_then(ImageSelector::digest)
                .cloned()
        );
    }
    for call in fixture.calls().iter().filter(|call| {
        call["args"].as_array().unwrap().contains(&json!("inspect"))
            && !call["args"].as_array().unwrap().contains(&json!("--help"))
    }) {
        assert!(
            !call["args"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()
                .as_str()
                .unwrap()
                .contains(":old@")
        );
    }
}

#[test]
fn invalid_naming_configuration_and_capabilities_do_not_acquire() {
    let fixture = Fixture::new();
    assert!(matches!(
        fixture.store.pull(
            &selector(SOURCE),
            Some(&selector(format!("renamed@{}", fixture.root))),
            &fixture.context,
            &PullOptions::default()
        ),
        Err(Error::InvalidPullDestination { .. })
    ));
    assert!(!fixture.store.config().root().exists());
    assert!(fixture.calls().is_empty());
    let mut context = fixture.context.clone();
    context.platform = Some(platform("", None));
    assert!(
        fixture
            .store
            .pull(&selector(SOURCE), None, &context, &PullOptions::default())
            .is_err()
    );
    context = fixture.context.clone();
    context.auth_file = Some(PathBuf::new());
    assert!(
        fixture
            .store
            .pull(&selector(SOURCE), None, &context, &PullOptions::default())
            .is_err()
    );
    assert!(!fixture.store.config().root().exists());
    let mut fixture = Fixture::new();
    fixture.config["help"] = json!("--raw --retry-times --dest-shared-blob-dir");
    fixture.save();
    assert!(matches!(
        fixture.pull(false),
        Err(Error::ToolCapability {
            capability: "--preserve-digests",
            ..
        })
    ));
    assert!(
        fixture
            .calls()
            .iter()
            .all(|entry| entry["args"].as_array().unwrap().contains(&json!("--help")))
    );
}

#[test]
fn registry_configuration_is_consistent_and_not_saved_in_metadata() {
    let mut fixture = Fixture::new();
    fixture.context.auth_file = Some("/private/auth.json".into());
    fixture.context.cert_dir = Some("/private/certs".into());
    fixture.context.policy = Some("/private/policy.json".into());
    fixture.context.tls_verify = Some(false);
    fixture.context.platform = Some(platform("amd64", None));
    fixture.set_index(vec![candidate(&fixture, "amd64", None)]);
    fixture.pull(false).unwrap();
    for entry in fixture
        .calls()
        .iter()
        .filter(|entry| !entry["args"].as_array().unwrap().contains(&json!("--help")))
    {
        let args = entry["args"].as_array().unwrap();
        for argument in [
            "--policy",
            "/private/policy.json",
            "--authfile",
            "/private/auth.json",
            "/private/certs",
            "--retry-times",
            "0",
        ] {
            assert!(args.contains(&json!(argument)));
        }
        let copying = args.contains(&json!("copy"));
        assert!(args.contains(&json!(if copying {
            "--src-cert-dir"
        } else {
            "--cert-dir"
        })));
        assert!(args.contains(&json!(if copying {
            "--src-tls-verify=false"
        } else {
            "--tls-verify=false"
        })));
    }
    assert!(
        !serde_json::to_string(&fixture.metadata())
            .unwrap()
            .contains("/private")
    );
}

#[test]
fn failures_preserve_previous_ref_and_successful_replacement_reuses_content() {
    let mut fixture = Fixture::new();
    fixture.pull(false).unwrap();
    let pointer = fs::read_link(fixture.pointer()).unwrap();
    let calls = fixture.calls().len();
    assert!(matches!(fixture.pull(false), Err(Error::RefExists(_))));
    assert_eq!(fixture.calls().len(), calls);
    for (setting, message) in [
        ("inspect_failure", "authentication denied"),
        ("copy_failure", "cannot preserve digest"),
        ("copy_failure", "incompatible representation"),
    ] {
        fixture.config[setting] = json!(message);
        fixture.save();
        assert!(
            fixture
                .pull(true)
                .unwrap_err()
                .to_string()
                .contains(message)
        );
        assert_eq!(fs::read_link(fixture.pointer()).unwrap(), pointer);
        fixture.config.as_object_mut().unwrap().remove(setting);
    }
    fixture.save();
    let index = fixture.fixture().join("index.json");
    let original = fs::read(&index).unwrap();
    let mut changed: Value = serde_json::from_slice(&original).unwrap();
    let mut alternate: Value = serde_json::from_slice(&fixture.manifest).unwrap();
    alternate["annotations"] = json!({"fixture":"different complete manifest"});
    let alternate = serde_json::to_vec(&alternate).unwrap();
    let alternate_digest = raw_digest(&alternate);
    fs::write(
        fixture
            .fixture()
            .join("blobs/sha256")
            .join(alternate_digest.hex()),
        &alternate,
    )
    .unwrap();
    changed["manifests"][0] = descriptor(&alternate, graph::MANIFEST);
    fs::write(&index, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(
        fixture
            .pull(true)
            .unwrap_err()
            .to_string()
            .contains("differs from resolved registry manifest")
    );
    assert_eq!(fs::read_link(fixture.pointer()).unwrap(), pointer);
    fs::write(&index, original).unwrap();
    let blob = fixture
        .store
        .config()
        .root()
        .join("blobs/sha256")
        .join(fixture.layer.hex());
    let before = fs::metadata(&blob).unwrap();
    fixture.pull(true).unwrap();
    let after = fs::metadata(blob).unwrap();
    assert_eq!(before.ino(), after.ino());
    assert_eq!(before.mtime(), after.mtime());
    assert_ne!(fs::read_link(fixture.pointer()).unwrap(), pointer);
}

#[test]
fn availability_observations_do_not_reserve_pull_destinations_or_contact_registry() {
    use crate::{ImageAvailability, locks::Operation, test_snapshot::snapshot};
    let fixture = Fixture::new();
    drop(fixture.store.prepare(Operation::Pull).unwrap());
    let selected = selector(DESTINATION);
    assert!(matches!(
        fixture.store.availability(&selected),
        Ok(ImageAvailability::Missing)
    ));
    fixture.pull(false).unwrap();
    let before = snapshot(fixture.store.config().root());
    let calls = fixture.calls().len();
    for selected in [
        selected.clone(),
        selector(format!("{}@{}", selected.name(), fixture.root)),
    ] {
        assert!(matches!(
            fixture.store.availability(&selected),
            Ok(ImageAvailability::Ready)
        ));
    }
    assert!(matches!(fixture.pull(false), Err(Error::RefExists(_))));
    assert_eq!(fixture.calls().len(), calls);
    assert_eq!(snapshot(fixture.store.config().root()), before);
    fixture.pull(true).unwrap();
    let calls = fixture.calls().len();
    fs::remove_file(&fixture.context.skopeo).unwrap();
    assert!(matches!(
        fixture.store.availability(&selected),
        Ok(ImageAvailability::Ready)
    ));
    assert_eq!(fixture.calls().len(), calls);
}

#[test]
fn pull_publication_and_cleanup_keep_existing_failure_semantics() {
    let fixture = Fixture::new();
    fixture.pull(false).unwrap();
    let pointer = fs::read_link(fixture.pointer()).unwrap();
    fsutil::fail_once("ref.rename", libc::EIO);
    assert!(fixture.pull(true).is_err());
    assert_eq!(fs::read_link(fixture.pointer()).unwrap(), pointer);
    fsutil::fail_once("ref.destination-sync", libc::EIO);
    assert!(matches!(
        fixture.pull(true),
        Err(Error::Durability {
            change: NamespaceChange::RefPublished,
            ..
        })
    ));
    assert_ne!(fs::read_link(fixture.pointer()).unwrap(), pointer);
    for checkpoint in ["staging.cleanup-remove", "staging.cleanup-parent-sync"] {
        fsutil::fail_once(checkpoint, libc::EIO);
        let report = fixture.pull(true).unwrap();
        assert_eq!(report.cleanup_warnings.len(), 1);
        assert!(!fixture.store.check(&CheckScope::All).unwrap().has_errors());
    }
}

#[test]
fn concurrent_pulls_share_immutable_objects() {
    let fixture = Fixture::new();
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let workers: Vec<_> = ["cached/one:tag", "cached/two:tag"]
            .into_iter()
            .map(|destination| {
                let fixture = &fixture;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    fixture
                        .store
                        .pull(
                            &selector(SOURCE),
                            Some(&selector(destination)),
                            &fixture.context,
                            &PullOptions::default(),
                        )
                        .unwrap()
                })
            })
            .collect();
        for worker in workers {
            assert_eq!(worker.join().unwrap().stored_root, fixture.root);
        }
    });
    assert_eq!(
        fs::read_dir(fixture.store.config().root().join("blobs/sha256"))
            .unwrap()
            .count(),
        3
    );
    assert_eq!(fixture.store.list().unwrap().entries.len(), 2);
    assert!(!fixture.store.check(&CheckScope::All).unwrap().has_errors());
}

#[test]
#[ignore = "subprocess helper"]
fn pull_worker() {
    let directory = PathBuf::from(std::env::var_os("HPC_PULL_TEST_DIRECTORY").unwrap());
    let store = Store::new(crate::StoreConfig::new(directory.join("store")).unwrap());
    let context = PullContext {
        skopeo: directory.join("skopeo"),
        ..PullContext::default()
    };
    store
        .pull(
            &selector(SOURCE),
            Some(&selector(DESTINATION)),
            &context,
            &PullOptions {
                replace: true,
                notes: None,
            },
        )
        .unwrap();
}
fn try_exclusive(path: &Path) -> bool {
    let file = fs::File::open(path).unwrap();
    // SAFETY: the file owns the descriptor and this lock attempt never blocks.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

#[test]
fn terminated_pull_keeps_prior_ref_and_orphan_copy_locks() {
    let mut fixture = Fixture::new();
    fixture.pull(false).unwrap();
    let unused = crate::LocalRef::new("unused:tag").unwrap();
    fixture
        .store
        .availability(&selector(unused.as_str()))
        .unwrap();
    let unused_lock = fixture.store.layout.ref_lock(&unused);
    let pointer = fs::read_link(fixture.pointer()).unwrap();
    let directory = fixture.temporary.path();
    let release = directory.join("release");
    let ready = directory.join("ready");
    let c_path = std::ffi::CString::new(release.as_os_str().as_bytes()).unwrap();
    // SAFETY: the path is NUL terminated and permissions are a valid mode.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    fixture.config["pause"] = json!(true);
    fixture.save();
    let mut wrapper = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "pull::tests::pull_worker",
            "--nocapture",
        ])
        .env("HPC_PULL_TEST_DIRECTORY", directory)
        .env("HPC_PULL_TEST_ENV", "inherited unchanged")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        assert!(
            wrapper.try_wait().unwrap().is_none(),
            "wrapper exited before readiness"
        );
        assert!(Instant::now() < deadline, "copy did not become ready");
        std::thread::sleep(Duration::from_millis(5));
    }
    wrapper.kill().unwrap();
    wrapper.wait().unwrap();
    let store_lock = fixture.store.config().root().join("locks/store.lock");
    let ref_lock = fixture
        .store
        .config()
        .root()
        .join("locks/refs")
        .join(LocalRef::new(DESTINATION).unwrap().encoded_path())
        .join("ref.lock");
    assert!(!try_exclusive(&store_lock));
    assert!(!try_exclusive(&ref_lock));
    {
        let _short = crate::locks::short_test_waits();
        assert!(matches!(
            fixture.store.prune(&crate::PruneOptions::default()),
            Err(Error::LockTimeout { .. })
        ));
    }
    assert!(unused_lock.exists());
    assert_eq!(fs::read_link(fixture.pointer()).unwrap(), pointer);
    assert!(
        fixture
            .calls()
            .iter()
            .any(|entry| entry["environment"] == "inherited unchanged")
    );
    fs::write(release, b"continue").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !try_exclusive(&store_lock) || !try_exclusive(&ref_lock) {
        assert!(Instant::now() < deadline, "orphan failed to release locks");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(fs::read_link(fixture.pointer()).unwrap(), pointer);
    assert_eq!(
        fs::read_dir(fixture.store.config().root().join("tmp"))
            .unwrap()
            .count(),
        1
    );
    let inode = fs::metadata(&ref_lock).unwrap().ino();
    let report = fixture
        .store
        .prune(&crate::PruneOptions::default())
        .unwrap();
    assert!(!report.has_errors(), "{report:?}");
    assert!(!unused_lock.exists());
    assert_eq!(fs::metadata(&ref_lock).unwrap().ino(), inode);
}

#[test]
fn prune_reclaims_failed_pull_and_cleanup_warning_remnants_without_tools() {
    for case in ["copy-failure", "ref.rename", "staging.cleanup-remove"] {
        let mut fixture = Fixture::new();
        fixture.pull(false).unwrap();
        if case == "copy-failure" {
            fixture.config["copy_failure"] = json!("controlled failure");
            fixture.save();
        } else {
            fsutil::fail_once(
                if case == "ref.rename" {
                    "ref.rename"
                } else {
                    "staging.cleanup-remove"
                },
                libc::EIO,
            );
        }
        let result = fixture.pull(true);
        if case == "staging.cleanup-remove" {
            assert_eq!(result.unwrap().cleanup_warnings.len(), 1);
        } else {
            assert!(result.is_err());
        }
        let calls = fixture.calls().len();
        let report = fixture
            .store
            .prune(&crate::PruneOptions {
                dry_run: false,
                grace_period: Duration::ZERO,
            })
            .unwrap();
        assert!(!report.has_errors(), "{report:?}");
        assert!(report.summaries[&crate::PruneKind::Staging].removed_count > 0);
        assert_eq!(
            fs::read_dir(fixture.store.config().root().join("tmp"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            fs::read_dir(fixture.store.config().root().join("raw_images"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(fixture.calls().len(), calls);
        assert!(!fixture.store.check(&CheckScope::All).unwrap().has_errors());
    }
}
