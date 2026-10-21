use super::*;
use serde_json::json;
use std::{fs, os::unix::fs::symlink};

struct Fixture {
    directory: tempfile::TempDir,
    next: u64,
}
impl Fixture {
    fn new() -> Self {
        Self {
            directory: tempfile::tempdir().unwrap(),
            next: 1,
        }
    }
    fn path(&self, descriptor: &Descriptor) -> PathBuf {
        self.directory.path().join(descriptor.digest.hex())
    }
    fn bytes(&mut self, media_type: &str, bytes: &[u8]) -> Descriptor {
        let descriptor = Descriptor {
            media_type: media_type.into(),
            digest: Sha256Digest::new(format!("sha256:{:064x}", self.next)).unwrap(),
            size: bytes.len() as u64,
            extra: BTreeMap::new(),
        };
        self.next += 1;
        fs::write(self.path(&descriptor), bytes).unwrap();
        descriptor
    }
    fn object(&mut self, media_type: &str, value: Value) -> Descriptor {
        self.bytes(media_type, &serde_json::to_vec(&value).unwrap())
    }
    fn manifest(&mut self, layers: Vec<Descriptor>) -> Descriptor {
        let config = self.object(CONFIG, json!({"os":"linux","architecture":"amd64"}));
        self.object(
            MANIFEST,
            json!({"schemaVersion":2,"config":config,"layers":layers}),
        )
    }
    fn index(&mut self, children: Vec<Descriptor>) -> Descriptor {
        self.object(INDEX, json!({"schemaVersion":2,"manifests":children}))
    }
    fn scan(&self, root: &Descriptor) -> GraphReport {
        GraphReader::new(self.directory.path()).scan(root)
    }
}
fn messages(report: &GraphReport) -> String {
    report
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.finding.message.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn complete_graphs_include_all_platforms_nested_indexes_and_layer_types() {
    let mut fixture = Fixture::new();
    let layers = LAYERS
        .iter()
        .map(|media_type| fixture.bytes(media_type, b"opaque layer bytes"))
        .collect();
    let manifest = fixture.manifest(layers);
    let non_host_config = fixture.object(CONFIG, json!({"os":"linux","architecture":"arm64"}));
    let mut non_host = fixture.object(
        MANIFEST,
        json!({"schemaVersion":2,"config":non_host_config,"layers":[]}),
    );
    non_host.extra.insert(
        "platform".into(),
        json!({"os":"linux","architecture":"arm64"}),
    );
    let nested = fixture.index(vec![manifest.clone(), non_host]);
    let root = fixture.index(vec![manifest, nested]);
    let report = fixture.scan(&root);
    assert!(report.complete, "{}", messages(&report));
    assert!(report.diagnostics.is_empty());
    assert_eq!(report.reachable.len(), 12);
    let empty = fixture.index(vec![]);
    assert!(fixture.scan(&empty).complete);
    let zero_layer = fixture.manifest(vec![]);
    assert_eq!(fixture.scan(&zero_layer).reachable.len(), 2);
}

#[test]
fn failures_are_cached_and_each_incoming_size_and_media_type_is_checked() {
    let mut fixture = Fixture::new();
    let root = fixture.manifest(vec![]);
    let mut reader = GraphReader::new(fixture.directory.path());
    assert!(reader.scan(&root).complete);
    fs::remove_file(fixture.path(&root)).unwrap();
    // Cached JSON and filesystem observations are reused across checked refs.
    assert!(reader.scan(&root).complete);
    let mut wrong_size = root.clone();
    wrong_size.size += 1;
    let report = reader.scan(&wrong_size);
    assert!(!report.complete);
    assert!(messages(&report).contains("expected"));
    assert!(messages(&report).contains("conflicting descriptors"));
    let mut wrong_type = root.clone();
    wrong_type.media_type = INDEX.into();
    assert!(messages(&reader.scan(&wrong_type)).contains("conflicting descriptors"));

    let mut reader = GraphReader::new(fixture.directory.path());
    let missing = reader.scan(&root);
    assert!(!missing.complete);
    let finding = &missing.diagnostics[0].finding;
    assert_eq!(finding.kind, FailureKind::Damage);
    assert!(
        matches!(finding.cause.as_ref(), crate::Error::Io { source, .. }
        if source.kind() == std::io::ErrorKind::NotFound)
    );
    fs::write(fixture.path(&root), b"{}").unwrap();
    let cached = reader.scan(&root);
    assert!(!cached.complete);
    assert!(std::sync::Arc::ptr_eq(
        &finding.cause,
        &cached.diagnostics[0].finding.cause
    ));
    assert!(messages(&reader.scan(&root)).contains("open regular file"));
}

#[test]
fn missing_layers_require_local_content_even_with_urls_and_embedded_data() {
    let mut fixture = Fixture::new();
    let mut layer = fixture.bytes(LAYERS[0], b"original");
    layer
        .extra
        .insert("urls".into(), json!(["https://unreachable.invalid/layer"]));
    layer.extra.insert("data".into(), json!("b3JpZ2luYWw="));
    fs::remove_file(fixture.path(&layer)).unwrap();
    let root = fixture.manifest(vec![layer.clone()]);
    let report = fixture.scan(&root);
    assert!(!report.complete);
    assert!(
        report
            .diagnostics
            .iter()
            .any(|error| error.digest == layer.digest)
    );
}

#[test]
fn malformed_and_unsupported_objects_are_unresolved() {
    let cases = [
        (
            MANIFEST,
            json!({"schemaVersion":1,"layers":[]}),
            "schemaVersion",
        ),
        (MANIFEST, json!({"schemaVersion":2,"layers":[]}), "config"),
        (
            MANIFEST,
            json!({"schemaVersion":2,"config":{},"layers":null}),
            "layers",
        ),
        (
            INDEX,
            json!({"schemaVersion":2,"manifests":null}),
            "manifests",
        ),
        (
            INDEX,
            json!({"schemaVersion":2,"mediaType":MANIFEST,"manifests":[]}),
            "mediaType",
        ),
        (
            INDEX,
            json!({"schemaVersion":2,"artifactType":"example/artifact","manifests":[]}),
            "artifact",
        ),
        (
            INDEX,
            json!({"schemaVersion":2,"subject":{},"manifests":[]}),
            "subject",
        ),
        (
            "application/vnd.docker.distribution.manifest.v2+json",
            json!({}),
            "unsupported media type",
        ),
    ];
    let mut fixture = Fixture::new();
    for (media_type, value, expected) in cases {
        let root = fixture.object(media_type, value);
        let report = fixture.scan(&root);
        assert!(!report.complete);
        assert!(
            messages(&report).contains(expected),
            "{}",
            messages(&report)
        );
    }
    for bytes in [b"{".as_slice(), b"[]", b"null"] {
        let root = fixture.bytes(MANIFEST, bytes);
        let report = fixture.scan(&root);
        assert!(!report.complete);
        if bytes == b"{" {
            assert!(matches!(report.diagnostics[0].finding.cause.as_ref(),
                crate::Error::ContentJson { source, .. } if source.is_syntax() || source.is_eof()));
        }
    }
}

#[test]
fn malformed_descriptors_do_not_hide_readable_siblings() {
    let mut fixture = Fixture::new();
    let valid = fixture.manifest(vec![]);
    let malformed = [
        json!({"mediaType":MANIFEST,"digest":"sha512:abc","size":1}),
        json!({"mediaType":MANIFEST,"digest":valid.digest,"size":-1}),
        json!({"digest":valid.digest,"size":1}),
    ];
    for invalid in malformed {
        let root = fixture.object(
            INDEX,
            json!({"schemaVersion":2,"manifests":[invalid,valid]}),
        );
        let report = fixture.scan(&root);
        assert!(!report.complete);
        assert!(report.reachable.contains(&valid.digest));
    }
    for (key, value) in [
        ("platform", json!({"os":"linux"})),
        ("urls", json!(3)),
        ("data", json!([])),
        ("annotations", json!({"bad":2})),
        ("artifactType", json!("example/artifact")),
    ] {
        let mut invalid = valid.clone();
        invalid.extra.insert(key.into(), value);
        assert!(!fixture.scan(&invalid).complete, "{key}");
    }
}

#[test]
fn configs_are_read_as_json_but_layer_bytes_are_not_interpreted_or_hashed() {
    let mut fixture = Fixture::new();
    for config in [
        json!({}),
        json!({"os":"","architecture":"amd64"}),
        json!({"os":"linux","architecture":"amd64","variant":1}),
        json!({"os":"linux","architecture":"amd64","created":[]}),
    ] {
        let config = fixture.object(CONFIG, config);
        let root = fixture.object(
            MANIFEST,
            json!({"schemaVersion":2,"config":config,"layers":[]}),
        );
        assert!(!fixture.scan(&root).complete);
    }
    let layer = fixture.bytes(LAYERS[1], b"not gzip or tar");
    let root = fixture.manifest(vec![layer.clone()]);
    assert!(fixture.scan(&root).complete);
    fs::write(fixture.path(&layer), b"same size junk!").unwrap();
    assert!(fixture.scan(&root).complete);
    fs::write(fixture.path(&layer), b"short").unwrap();
    assert!(!fixture.scan(&root).complete);
}

#[test]
fn cycles_and_deep_nested_indexes_do_not_recurse() {
    let mut fixture = Fixture::new();
    let mut root = fixture.index(vec![]);
    for _ in 0..1500 {
        root = fixture.index(vec![root]);
    }
    let report = fixture.scan(&root);
    assert!(report.complete);
    assert_eq!(report.reachable.len(), 1501);
    // Cyclic fixture identities deliberately do not hash their content: the
    // checker must detect cycles even though it does not verify digests.
    let cycle = json!({"schemaVersion":2,"manifests":[root]});
    fs::write(fixture.path(&root), serde_json::to_vec(&cycle).unwrap()).unwrap();
    let report = fixture.scan(&root);
    assert!(!report.complete);
    assert!(messages(&report).contains("cycle"));
}

#[test]
fn nonregular_dependencies_are_rejected_without_blocking() {
    let mut fixture = Fixture::new();
    let layer = fixture.bytes(LAYERS[0], b"layer");
    let root = fixture.manifest(vec![layer.clone()]);
    let outside = fixture.directory.path().join("outside");
    fs::rename(fixture.path(&layer), &outside).unwrap();
    symlink(&outside, fixture.path(&layer)).unwrap();
    assert!(!fixture.scan(&root).complete);
    fs::remove_file(fixture.path(&layer)).unwrap();
    fs::create_dir(fixture.path(&layer)).unwrap();
    assert!(!fixture.scan(&root).complete);
    fs::remove_dir(fixture.path(&layer)).unwrap();
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let fifo = CString::new(fixture.path(&layer).as_os_str().as_bytes()).unwrap();
    // SAFETY: fifo is a valid NUL-terminated path and mkfifo retains no pointer.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(!fixture.scan(&root).complete);
}
