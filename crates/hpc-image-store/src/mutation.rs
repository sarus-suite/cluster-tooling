//! Store-specific write-ahead snapshots of operations being performed that
//! mutate the store contents. If operations are interrupted and these snapshots
//! are left behind, they are used by recovery operations to bring the store
//! to a stable, usable state again.
//! Import, pull, and remove use these primitives.
//! No Drop implementation performs filesystem work: a surviving producer may
//! still own the locked open-file description after its wrapper disappears.
use crate::{
    Error, LocalRef, NamespaceChange, Result, Sha256Digest, cas,
    error::io_error,
    fsutil,
    graph::GraphReader,
    initialization::require_directory,
    layout::{Layout, valid_raw_image_id},
    locks::StoreGuard,
    oci, refs,
    staging::{AcquisitionStage, BlobSpec, CompletedStage},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, DirBuilder, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, symlink},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Import,
    Pull,
    Remove,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Acquiring,
    Intent,
    Cleaning,
    Complete,
}
impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Acquiring => "acquiring",
            Self::Intent => "intent",
            Self::Cleaning => "cleaning",
            Self::Complete => "complete",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Outcome {
    Aborted,
    Published,
    Removed,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Object {
    pub digest: Sha256Digest,
    pub size: u64,
}
impl From<BlobSpec> for Object {
    fn from(spec: BlobSpec) -> Self {
        Self {
            digest: spec.digest,
            size: spec.size,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Retired {
    pub raw: String,
    pub objects: Vec<Object>,
}

/// Every persisted path is derived from one of these confined identities.
/// Tree members additionally have a fixed grammar, checked before path joins.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Target {
    Staging { member: String },
    Raw { id: String, member: String },
    Blob { digest: Sha256Digest },
    Namespace { depth: usize },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EntryKind {
    File,
    Directory,
    Pointer,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Removal {
    pub target: Target,
    pub kind: EntryKind,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Cleanup {
    pub outcome: Outcome,
    pub entries: Vec<Removal>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    pub version: u32,
    pub id: String,
    pub kind: Kind,
    pub reference: LocalRef,
    pub phase: Phase,
    pub old_raw: Option<String>,
    pub new_raw: Option<String>,
    pub staging: Option<String>,
    pub owned: Vec<Object>,
    pub reused: Vec<Object>,
    pub retired: Vec<Retired>,
    pub namespace_created: Vec<usize>,
    pub cleanup: Option<Cleanup>,
}

pub(crate) fn invalid(path: &Path, reason: impl Into<String>) -> Error {
    Error::InvalidMutationRecord {
        path: path.to_owned(),
        reason: reason.into(),
    }
}
pub(crate) fn metadata(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error("inspect mutation entry", path, error)),
    }
}
pub(crate) fn valid_id(id: &str) -> bool {
    id.len() == 12 && id.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

impl Record {
    pub fn context(&self, layout: &Layout, source: Error) -> Error {
        Error::Recovery {
            record_path: layout.pending_mutation(),
            reference: Some(self.reference.clone()),
            phase: self.phase.label(),
            change: if self.phase == Phase::Acquiring {
                None
            } else {
                Some(if self.kind == Kind::Remove {
                    NamespaceChange::RefRemoved
                } else {
                    NamespaceChange::RefPublished
                })
            },
            remaining: if self.phase == Phase::Complete {
                "content cleanup is durable; record removal may need retrying"
            } else {
                "recorded work remains pending; ref changes are not rolled back"
            },
            source: Box::new(source),
        }
    }
    pub fn namespace(&self, layout: &Layout, depth: usize) -> PathBuf {
        layout.root().join("refs").join(
            self.reference
                .encoded_path()
                .components()
                .take(depth)
                .collect::<PathBuf>(),
        )
    }
    pub fn path(&self, layout: &Layout, target: &Target) -> PathBuf {
        match target {
            Target::Staging { member } => tree_path(
                layout
                    .staging()
                    .join(self.staging.as_ref().expect("validated staging identity")),
                member,
            ),
            Target::Raw { id, member } => tree_path(layout.raw_image(id), member),
            Target::Blob { digest } => layout.blob(digest),
            Target::Namespace { depth } => self.namespace(layout, *depth),
        }
    }
    pub fn candidate_objects(&self, outcome: Outcome) -> Vec<Object> {
        let mut objects = self.owned.clone();
        if outcome != Outcome::Aborted {
            objects.extend(self.retired.iter().flat_map(|raw| raw.objects.clone()));
        }
        objects.sort_by(|a, b| a.digest.cmp(&b.digest));
        objects.dedup_by(|a, b| a.digest == b.digest);
        objects
    }
    pub fn candidate_raw(&self, outcome: Outcome) -> Vec<String> {
        if outcome == Outcome::Aborted {
            self.new_raw.iter().cloned().collect()
        } else {
            self.retired.iter().map(|raw| raw.raw.clone()).collect()
        }
    }
    pub fn validate(&self, layout: &Layout) -> Result<()> {
        let reject = |reason: &str| invalid(&layout.pending_mutation(), reason);
        if self.version != 1 || !valid_id(&self.id) {
            return Err(reject("unsupported version or invalid operation identity"));
        }
        if self
            .old_raw
            .iter()
            .chain(self.new_raw.iter())
            .any(|id| !valid_raw_image_id(id))
            || self.new_raw.is_some() && self.old_raw == self.new_raw
        {
            return Err(reject("invalid or indistinguishable raw-image identities"));
        }
        if self.kind == Kind::Remove {
            if self.phase == Phase::Acquiring
                || self.old_raw.is_none()
                || self.new_raw.is_some()
                || self.staging.is_some()
                || !self.owned.is_empty()
                || !self.reused.is_empty()
                || !self.namespace_created.is_empty()
            {
                return Err(reject("inconsistent removal intent"));
            }
        } else if self.staging.as_deref() != Some(format!("operation-{}", self.id).as_str())
            || (self.phase == Phase::Acquiring && self.new_raw.is_some())
            || (self.phase == Phase::Intent && self.new_raw.is_none())
        {
            return Err(reject("inconsistent acquisition or publication intent"));
        }
        if self.kind != Kind::Remove
            && self.new_raw.is_none()
            && (!self.owned.is_empty()
                || !self.reused.is_empty()
                || !self.retired.is_empty()
                || !self.namespace_created.is_empty())
        {
            return Err(reject(
                "acquisition without intent cannot own live additions",
            ));
        }
        let retirement_expected = if self.kind == Kind::Remove || self.new_raw.is_some() {
            self.old_raw.iter().collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if self.retired.iter().map(|raw| &raw.raw).collect::<Vec<_>>() != retirement_expected {
            return Err(reject("retired identities disagree with previous ref"));
        }
        let mut declarations = BTreeMap::new();
        for object in self
            .owned
            .iter()
            .chain(&self.reused)
            .chain(self.retired.iter().flat_map(|raw| &raw.objects))
        {
            if let Some(previous) = declarations.insert(&object.digest, object.size)
                && previous != object.size
            {
                return Err(reject("conflicting recorded object sizes"));
            }
        }
        let mut additions = BTreeSet::new();
        for object in self.owned.iter().chain(&self.reused) {
            if !additions.insert(&object.digest) {
                return Err(reject("duplicate or overlapping owned/reused objects"));
            }
        }
        let count = self.reference.encoded_path().components().count();
        let mut depths = BTreeSet::new();
        for depth in &self.namespace_created {
            if *depth == 0 || *depth > count || !depths.insert(depth) {
                return Err(reject("invalid namespace ownership"));
            }
        }
        if self
            .namespace_created
            .windows(2)
            .any(|pair| pair[1] != pair[0] + 1)
            || self
                .namespace_created
                .last()
                .is_some_and(|last| *last != count)
        {
            return Err(reject(
                "owned namespace directories must be a contiguous suffix",
            ));
        }
        if matches!(self.phase, Phase::Cleaning | Phase::Complete) != self.cleanup.is_some() {
            return Err(reject("cleanup plan disagrees with phase"));
        }
        if let Some(cleanup) = &self.cleanup {
            if cleanup.outcome == Outcome::Published
                && (self.kind == Kind::Remove || self.new_raw.is_none())
                || cleanup.outcome == Outcome::Removed && self.kind != Kind::Remove
            {
                return Err(reject("cleanup outcome disagrees with intent"));
            }
            let raw = self.candidate_raw(cleanup.outcome);
            let blobs: BTreeSet<_> = self
                .candidate_objects(cleanup.outcome)
                .into_iter()
                .map(|object| object.digest)
                .collect();
            let mut paths = BTreeSet::new();
            for removal in &cleanup.entries {
                let expected = match &removal.target {
                    Target::Staging { member } if self.staging.is_some() => staging_member(member),
                    Target::Raw { id, member } if raw.contains(id) => raw_member(member),
                    Target::Blob { digest } if blobs.contains(digest) => Some(EntryKind::File),
                    Target::Namespace { depth }
                        if *depth > 0
                            && *depth <= count
                            && (cleanup.outcome == Outcome::Removed
                                || self.namespace_created.contains(depth)) =>
                    {
                        Some(EntryKind::Directory)
                    }
                    _ => None,
                };
                if expected != Some(removal.kind) {
                    return Err(reject(
                        "cleanup entry is outside recorded ownership or has an invalid type",
                    ));
                }
                let path = self.path(layout, &removal.target);
                if !paths.insert(path.clone())
                    || paths
                        .iter()
                        .any(|previous| previous != &path && path.starts_with(previous))
                {
                    return Err(reject("duplicate or incorrectly ordered cleanup entries"));
                }
            }
        }
        Ok(())
    }
    pub fn save(&self, guard: &StoreGuard<'_>) -> Result<()> {
        guard.require_exclusive()?;
        let layout = guard.layout();
        self.validate(layout)?;
        let scratch = layout.pending_mutation_next();
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&scratch)
            .map_err(|source| io_error("create mutation snapshot", &scratch, source))?;
        fsutil::checkpoint("mutation.snapshot-created", &scratch)?;
        let bytes = serde_json::to_vec(self)
            .map_err(|source| invalid(&layout.pending_mutation(), source.to_string()))?;
        fsutil::checkpoint("mutation.snapshot-write", &scratch)?;
        file.write_all(&bytes)
            .map_err(|source| io_error("write mutation snapshot", &scratch, source))?;
        fsutil::checkpoint("mutation.snapshot-written", &scratch)?;
        fsutil::checkpoint("mutation.snapshot-file-sync", &scratch)?;
        file.sync_all()
            .map_err(|source| io_error("sync mutation snapshot", &scratch, source))?;
        fsutil::checkpoint("mutation.snapshot-synced", &scratch)?;
        // Exclusive coordination and validation exclude an unexpected target.
        if let Some(meta) = metadata(&layout.pending_mutation())?
            && !meta.is_file()
        {
            return Err(invalid(
                &layout.pending_mutation(),
                "active record must be a regular file",
            ));
        }
        fs::rename(&scratch, layout.pending_mutation()).map_err(|source| {
            io_error(
                "publish mutation snapshot",
                &layout.pending_mutation(),
                source,
            )
        })?;
        fsutil::checkpoint("mutation.snapshot-renamed", &layout.pending_mutation())?;
        fsutil::checkpoint("mutation.snapshot-parent-sync", layout.root())?;
        fsutil::sync_directory(layout.root())
            .map_err(|source| io_error("sync mutation record parent", layout.root(), source))?;
        fsutil::checkpoint("mutation.snapshot-durable", &layout.pending_mutation())
    }
}

fn tree_path(root: PathBuf, member: &str) -> PathBuf {
    // Joining an empty member adds a trailing slash. On Linux, even lstat then
    // follows a symlink at the tree root. Inspect the root spelling directly.
    if member.is_empty() {
        root
    } else {
        root.join(member)
    }
}

pub(crate) fn raw_member(member: &str) -> Option<EntryKind> {
    match member {
        "" | "blobs" => Some(EntryKind::Directory),
        "oci-layout" | "index.json" | "metadata.json" => Some(EntryKind::File),
        _ => None,
    }
}
pub(crate) fn staging_member(member: &str) -> Option<EntryKind> {
    match member {
        "" | "layout" | "layout/blobs" | "layout/blobs/sha256" | "blobs" | "blobs/sha256" => {
            Some(EntryKind::Directory)
        }
        "layout/oci-layout" | "layout/index.json" => Some(EntryKind::File),
        "ref-pointer" => Some(EntryKind::Pointer),
        _ => {
            let (parent, name) = member.rsplit_once('/')?;
            if parent == "layout" {
                // Skopeo can leave an incomplete blob here when interrupted.
                // Only its exact temporary-file spelling is operation-owned;
                // recovery still requires a regular file under real parents.
                let suffix = name.strip_prefix("oci-put-blob")?;
                return (!suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()))
                    .then_some(EntryKind::File);
            }
            if matches!(parent, "blobs/sha256" | "layout/blobs/sha256")
                && Sha256Digest::new(format!("sha256:{name}")).is_ok()
            {
                Some(EntryKind::File)
            } else {
                None
            }
        }
    }
}

fn unused_identity(parent: &Path, prefix: &str) -> Result<String> {
    // Reserve names in the durable record before creating any owned directory.
    let mut entropy = fs::File::open("/dev/urandom")
        .map_err(|source| io_error("open identity entropy", Path::new("/dev/urandom"), source))?;
    loop {
        let mut bytes = [0_u8; 12];
        entropy
            .read_exact(&mut bytes)
            .map_err(|source| io_error("read identity entropy", parent, source))?;
        let suffix: String = bytes
            .iter()
            .map(|byte| {
                b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
                    [usize::from(*byte) % 62] as char
            })
            .collect();
        let identity = format!("{prefix}{suffix}");
        if metadata(&parent.join(&identity))?.is_none() {
            return Ok(identity);
        }
    }
}

pub(crate) struct Mutation<'a> {
    guard: &'a StoreGuard<'a>,
    record: Record,
}
impl<'a> Mutation<'a> {
    /// Attach pending-work context without losing the acquisition failure.
    pub fn context(&self, source: Error) -> Error {
        self.record.context(self.guard.layout(), source)
    }

    pub fn begin(guard: &'a StoreGuard<'a>, reference: &LocalRef, kind: Kind) -> Result<Self> {
        guard.require_exclusive()?;
        let layout = guard.layout();
        if crate::recovery::pending(layout)? {
            return Err(invalid(
                &layout.pending_mutation(),
                "another record or scratch snapshot is pending",
            ));
        }
        let old_raw = refs::pointer_identity(layout, reference)?;
        if kind == Kind::Remove && old_raw.is_none() {
            return Err(Error::RefMissing(reference.clone()));
        }
        let staging = if kind == Kind::Remove {
            None
        } else {
            Some(unused_identity(&layout.staging(), "operation-")?)
        };
        let id = match &staging {
            Some(name) => name[10..].to_owned(),
            None => unused_identity(layout.root(), "")?,
        };
        let mut record = Record {
            version: 1,
            id,
            kind,
            reference: reference.clone(),
            phase: Phase::Acquiring,
            old_raw,
            new_raw: None,
            staging,
            owned: Vec::new(),
            reused: Vec::new(),
            retired: Vec::new(),
            namespace_created: Vec::new(),
            cleanup: None,
        };
        if kind == Kind::Remove {
            record.phase = Phase::Intent;
            record.retired = retired(guard, &record)?;
        }
        record
            .save(guard)
            .map_err(|error| record.context(layout, error))?;
        Ok(Self { guard, record })
    }
    pub fn stage(&self) -> Result<AcquisitionStage<'a>> {
        if self.record.phase != Phase::Acquiring {
            return Err(Error::Coordination("acquisition phase required"));
        }
        let path = self
            .guard
            .layout()
            .staging()
            .join(self.record.staging.as_ref().expect("acquisition staging"));
        fsutil::checkpoint("mutation.before-staging", &path)
            .map_err(|error| self.record.context(self.guard.layout(), error))?;
        AcquisitionStage::recorded(self.guard, &self.record.reference, path)
            .map_err(|error| self.record.context(self.guard.layout(), error))
    }
    pub fn publish(&mut self, stage: CompletedStage<'_>) -> Result<PathBuf> {
        let result = self.publish_inner(stage);
        result.map_err(|error| self.record.context(self.guard.layout(), error))
    }
    fn publish_inner(&mut self, stage: CompletedStage<'_>) -> Result<PathBuf> {
        let layout = self.guard.layout();
        if self.record.phase != Phase::Acquiring
            || !std::ptr::eq(self.guard, stage.guard())
            || stage.reference() != &self.record.reference
            || Some(stage.directory())
                != self
                    .record
                    .staging
                    .as_ref()
                    .map(|id| layout.staging().join(id))
                    .as_deref()
        {
            return Err(Error::Coordination(
                "publication requires this mutation's completed stage",
            ));
        }
        if refs::pointer_identity(layout, &self.record.reference)? != self.record.old_raw {
            return Err(invalid(
                &layout.pending_mutation(),
                "destination changed during acquisition",
            ));
        }
        self.record.new_raw = Some(unused_identity(&layout.raw_images(), "raw-")?);
        self.record.retired = retired(self.guard, &self.record)?;
        for blob in stage.blobs() {
            let object = Object {
                digest: blob.digest.clone(),
                size: blob.size,
            };
            if metadata(&layout.blob(&blob.digest))?.is_some() {
                fsutil::sized_object(&layout.blob(&blob.digest), blob.size)?;
                self.record.reused.push(object);
            } else {
                self.record.owned.push(object);
            }
        }
        for depth in 1..=self.record.reference.encoded_path().components().count() {
            let path = self.record.namespace(layout, depth);
            if let Some(meta) = metadata(&path)? {
                if !meta.is_dir() {
                    return Err(invalid(&path, "namespace parent must be a real directory"));
                }
            } else {
                self.record.namespace_created.push(depth);
            }
        }
        self.record.phase = Phase::Intent;
        self.record.save(self.guard)?;
        fsutil::checkpoint("mutation.intent-durable", &layout.pending_mutation())?;
        for blob in stage.blobs() {
            if self
                .record
                .owned
                .iter()
                .any(|object| object.digest == blob.digest)
                && metadata(&layout.blob(&blob.digest))?.is_some()
            {
                return Err(invalid(
                    &layout.blob(&blob.digest),
                    "recorded unused CAS identity unexpectedly exists",
                ));
            }
            cas::insert(self.guard, blob)?;
        }
        let raw = layout.raw_image(self.record.new_raw.as_ref().unwrap());
        DirBuilder::new()
            .mode(0o700)
            .create(&raw)
            .map_err(|source| io_error("create recorded raw image", &raw, source))?;
        crate::initialization::create_directory(&raw.join("blobs"))?;
        oci::write_layout(&raw, stage.root())?;
        let bytes = serde_json::to_vec(stage.metadata()).map_err(|source| Error::Metadata {
            kind: "ref",
            source,
        })?;
        fsutil::write_new(&raw.join("metadata.json"), &bytes)?;
        fsutil::sync_before_commit(&raw.join("blobs"), "raw_image.blobs-sync")?;
        fsutil::sync_before_commit(&raw, "raw_image.directory-sync")?;
        fsutil::sync_before_commit(&layout.raw_images(), "raw_image.parent-sync")?;
        for depth in &self.record.namespace_created {
            let path = self.record.namespace(layout, *depth);
            DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .map_err(|source| io_error("create recorded namespace", &path, source))?;
        }
        for depth in (0..=self.record.reference.encoded_path().components().count()).rev() {
            fsutil::sync_before_commit(
                &self.record.namespace(layout, depth),
                "ref.namespace-sync",
            )?;
        }
        let pointer = stage.directory().join("ref-pointer");
        symlink(
            layout.raw_image_target(
                &self.record.reference,
                self.record.new_raw.as_ref().unwrap(),
            ),
            &pointer,
        )
        .map_err(|source| io_error("prepare recorded pointer", &pointer, source))?;
        fsutil::sync_before_commit(stage.directory(), "ref.temporary-sync")?;
        let destination = layout.ref_entry(&self.record.reference);
        fsutil::checkpoint("ref.rename", &destination)?;
        fs::rename(&pointer, &destination)
            .map_err(|source| io_error("publish recorded pointer", &destination, source))?;
        fsutil::sync_after_commit(
            destination.parent().unwrap(),
            "ref.destination-sync",
            NamespaceChange::RefPublished,
            &destination,
        )?;
        fsutil::sync_after_commit(
            stage.directory(),
            "ref.source-sync",
            NamespaceChange::RefPublished,
            &destination,
        )?;
        Ok(raw)
    }
    pub fn remove(&mut self) -> Result<()> {
        let result = (|| {
            if self.record.kind != Kind::Remove || self.record.phase != Phase::Intent {
                return Err(Error::Coordination("removal intent required"));
            }
            if refs::pointer_identity(self.guard.layout(), &self.record.reference)?
                != self.record.old_raw
            {
                return Err(invalid(
                    &self.guard.layout().pending_mutation(),
                    "removal destination changed",
                ));
            }
            refs::remove(self.guard, &self.record.reference)
        })();
        result.map_err(|error| self.record.context(self.guard.layout(), error))
    }
    pub fn finish(self) -> Result<()> {
        crate::recovery::recover(self.guard)
    }
}
fn retired(guard: &StoreGuard<'_>, record: &Record) -> Result<Vec<Retired>> {
    let Some(id) = &record.old_raw else {
        return Ok(Vec::new());
    };
    let raw = guard.layout().raw_image(id);
    require_directory(&raw)?;
    let root = oci::single_root(&raw)?;
    let objects = GraphReader::dependencies(&guard.layout().blobs()).dependency_objects(&root)?;
    Ok(vec![Retired {
        raw: id.clone(),
        objects: objects.into_iter().map(Object::from).collect(),
    }])
}
