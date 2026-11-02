//! Synthetic content for mutation and recovery acceptance tests.
//!
//! Digests identify the actual fixture bytes. Layers are opaque synthetic bytes;
//! these fixtures exercise storage and structural checking, not runnable images.
use crate::{
    LocalRef, RefMetadata, Sha256Digest, Store, StoreConfig,
    graph::{CONFIG, INDEX, LAYERS, MANIFEST},
    locks::{Operation, RefGuard},
    oci::{self, Descriptor},
    refs,
    staging::{AcquisitionStage, BlobSpec, CompletedStage},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

#[derive(Clone)]
pub(crate) struct SyntheticImage {
    pub root: Descriptor,
    pub objects: BTreeMap<Sha256Digest, Vec<u8>>,
}

impl SyntheticImage {
    pub fn digests(&self) -> BTreeSet<Sha256Digest> {
        self.objects.keys().cloned().collect()
    }
}

fn object(
    objects: &mut BTreeMap<Sha256Digest, Vec<u8>>,
    media_type: &str,
    bytes: Vec<u8>,
) -> Descriptor {
    let digest = Sha256Digest::new(format!("sha256:{:x}", Sha256::digest(&bytes))).unwrap();
    let descriptor = Descriptor {
        media_type: media_type.into(),
        digest: digest.clone(),
        size: bytes.len() as u64,
        extra: BTreeMap::new(),
    };
    objects.insert(digest, bytes);
    descriptor
}

fn layered_image(name: &str) -> (SyntheticImage, Descriptor, Descriptor) {
    let mut objects = BTreeMap::new();
    let shared = object(&mut objects, LAYERS[0], b"shared layer bytes".to_vec());
    let unique = object(
        &mut objects,
        LAYERS[0],
        format!("{name} layer bytes").into_bytes(),
    );
    let config = object(
        &mut objects,
        CONFIG,
        serde_json::to_vec(&json!({"os":"linux","architecture":"amd64","fixture":name})).unwrap(),
    );
    let root = object(
        &mut objects,
        MANIFEST,
        serde_json::to_vec(&json!({"schemaVersion":2,"config":config,"layers":[shared,unique]}))
            .unwrap(),
    );
    (SyntheticImage { root, objects }, shared, unique)
}

pub(crate) struct SharedImages {
    pub original: SyntheticImage,
    pub keeper: SyntheticImage,
    pub replacement: SyntheticImage,
    pub shared: BTreeSet<Sha256Digest>,
    pub original_only: BTreeSet<Sha256Digest>,
    pub keeper_only: BTreeSet<Sha256Digest>,
    pub replacement_only: BTreeSet<Sha256Digest>,
}

impl SharedImages {
    pub fn new() -> Self {
        let (original, common, _) = layered_image("original");
        let (keeper, _, _) = layered_image("keeper");
        let (replacement, _, _) = layered_image("replacement");
        let shared = BTreeSet::from([common.digest]);
        let exclusive =
            |image: &SyntheticImage| image.digests().difference(&shared).cloned().collect();
        Self {
            original_only: exclusive(&original),
            keeper_only: exclusive(&keeper),
            replacement_only: exclusive(&replacement),
            original,
            keeper,
            replacement,
            shared,
        }
    }
}

/// A branching index with a nested first manifest and a direct second manifest.
pub(crate) struct NestedImages {
    pub top: SyntheticImage,
    pub first: SyntheticImage,
    pub second: Descriptor,
    pub nested: Descriptor,
    pub config: Descriptor,
    pub layer: Descriptor,
}

impl NestedImages {
    pub fn new() -> Self {
        let mut objects = BTreeMap::new();
        let config = object(
            &mut objects,
            CONFIG,
            br#"{"architecture":"amd64","os":"linux","rootfs":{"type":"layers","diff_ids":[]}}"#
                .to_vec(),
        );
        let layer = object(&mut objects, LAYERS[0], b"shared layer bytes".to_vec());
        let first = object(&mut objects, MANIFEST, serde_json::to_vec(&json!({"schemaVersion":2,"config":config,"layers":[layer],"annotations":{"name":"one"}})).unwrap());
        let first_image = SyntheticImage {
            root: first.clone(),
            objects: objects.clone(),
        };
        let second = object(&mut objects, MANIFEST, serde_json::to_vec(&json!({"schemaVersion":2,"config":config,"layers":[layer],"annotations":{"name":"two"}})).unwrap());
        let nested = object(
            &mut objects,
            INDEX,
            serde_json::to_vec(&json!({"schemaVersion":2,"manifests":[first]})).unwrap(),
        );
        let root = object(
            &mut objects,
            INDEX,
            serde_json::to_vec(&json!({"schemaVersion":2,"manifests":[nested,second]})).unwrap(),
        );
        Self {
            top: SyntheticImage { root, objects },
            first: first_image,
            second,
            nested,
            config,
            layer,
        }
    }
}

pub(crate) fn stage_image<'a>(
    guard: &'a RefGuard<'a>,
    image: &SyntheticImage,
) -> CompletedStage<'a> {
    let stage = AcquisitionStage::new(guard).unwrap();
    oci::write_layout(&stage.layout_path(), &image.root).unwrap();
    let mut specs = Vec::new();
    for (digest, bytes) in &image.objects {
        fs::write(
            stage.shared_blob_path().join("sha256").join(digest.hex()),
            bytes,
        )
        .unwrap();
        specs.push(BlobSpec {
            digest: digest.clone(),
            size: bytes.len() as u64,
        });
    }
    stage
        .complete(
            &image.root.digest,
            RefMetadata::new(guard.reference().clone(), image.root.digest.clone()),
            specs,
        )
        .unwrap()
}

/// Like the existing primitive publication fixtures, this leaves staging behind.
/// Public transfers own successful staging cleanup in the current implementation.
pub(crate) fn publish_image(
    store: &Store,
    reference: &LocalRef,
    image: &SyntheticImage,
    replace: bool,
) -> PathBuf {
    let guard = store.prepare(Operation::Import).unwrap();
    let selected = guard.lock_ref(reference).unwrap();
    refs::publish(stage_image(&selected, image), replace).unwrap()
}

pub(crate) fn prefix_names() -> (LocalRef, LocalRef) {
    let short = LocalRef::new(format!("{}:t", "a".repeat(58))).unwrap();
    let long = LocalRef::new(format!("{}:tag", "a".repeat(58))).unwrap();
    assert_ne!(short.encoded_path(), long.encoded_path());
    assert!(long.encoded_path().starts_with(short.encoded_path()));
    (short, long)
}

/// Baseline for replacement and interruption scenarios. Retaining the temporary
/// directory keeps every returned path alive until the fixture is dropped.
pub(crate) struct MutationFixture {
    pub directory: tempfile::TempDir,
    pub store: Store,
    pub target: LocalRef,
    pub keeper: LocalRef,
    pub images: SharedImages,
    pub original_raw: PathBuf,
    pub keeper_raw: PathBuf,
}

impl MutationFixture {
    pub fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::new(StoreConfig::new(directory.path().join("store")).unwrap());
        let target = LocalRef::new("analysis/tool:release").unwrap();
        let keeper = LocalRef::new("analysis/tool:keeper").unwrap();
        let images = SharedImages::new();
        let original_raw = publish_image(&store, &target, &images.original, false);
        let keeper_raw = publish_image(&store, &keeper, &images.keeper, false);
        Self {
            directory,
            store,
            target,
            keeper,
            images,
            original_raw,
            keeper_raw,
        }
    }
}

#[test]
fn synthetic_images_have_exact_descriptors_and_expected_sharing() {
    let images = SharedImages::new();
    let nested = NestedImages::new();
    assert_eq!(images.shared.len(), 1);
    let exclusive_sets = [
        &images.original_only,
        &images.keeper_only,
        &images.replacement_only,
    ];
    for (index, exclusive) in exclusive_sets.iter().enumerate() {
        assert_eq!(exclusive.len(), 3); // Manifest, configuration, unique layer.
        assert!(exclusive.is_disjoint(&images.shared));
        for other in &exclusive_sets[index + 1..] {
            assert!(exclusive.is_disjoint(other));
        }
    }
    for (image, exclusive) in [
        (&images.original, &images.original_only),
        (&images.keeper, &images.keeper_only),
        (&images.replacement, &images.replacement_only),
    ] {
        assert_eq!(
            image.digests(),
            images.shared.union(exclusive).cloned().collect()
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&image.objects[&image.root.digest]).unwrap();
        let layers = manifest["layers"].as_array().unwrap();
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0]["digest"], images.shared.first().unwrap().as_str());
        assert!(
            exclusive.contains(&Sha256Digest::new(layers[1]["digest"].as_str().unwrap()).unwrap())
        );
    }
    let directory = tempfile::tempdir().unwrap();
    let store = Store::new(StoreConfig::new(directory.path()).unwrap());
    for (name, image) in [
        ("original", &images.original),
        ("keeper", &images.keeper),
        ("replacement", &images.replacement),
        ("nested", &nested.top),
    ] {
        assert_eq!(
            image.root.size,
            image.objects[&image.root.digest].len() as u64
        );
        for (digest, bytes) in &image.objects {
            assert_eq!(
                digest.as_str(),
                format!("sha256:{:x}", Sha256::digest(bytes))
            );
        }
        publish_image(
            &store,
            &LocalRef::new(format!("fixture:{name}")).unwrap(),
            image,
            false,
        );
    }
    let report = store.check(&crate::CheckScope::All).unwrap();
    assert!(!report.has_errors(), "{report:?}");
    assert_eq!(report.entries.len(), 4);
}
