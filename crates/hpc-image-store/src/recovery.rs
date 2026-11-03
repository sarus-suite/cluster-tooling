//! Exclusive, idempotent recovery of recorded store operations. This is not a
//! scanner for legacy remnants or a repair facility for externally changed data.
use crate::{
    Error, Result, Sha256Digest, discovery,
    error::io_error,
    fsutil,
    graph::GraphReader,
    initialization::require_directory,
    layout::Layout,
    locks::StoreGuard,
    mutation::{self, Cleanup, EntryKind, Object, Outcome, Phase, Record, Removal, Target},
    oci, refs,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

pub(crate) fn pending(layout: &Layout) -> Result<bool> {
    Ok(mutation::metadata(&layout.pending_mutation())?.is_some()
        || mutation::metadata(&layout.pending_mutation_next())?.is_some())
}
fn load(layout: &Layout) -> Result<Option<Record>> {
    let Some(metadata) = mutation::metadata(&layout.pending_mutation())? else {
        return Ok(None);
    };
    if !metadata.is_file() {
        return Err(mutation::invalid(
            &layout.pending_mutation(),
            "active record must be a regular file",
        ));
    }
    let bytes = fsutil::read_regular(&layout.pending_mutation())?;
    let record: Record = serde_json::from_slice(&bytes)
        .map_err(|source| mutation::invalid(&layout.pending_mutation(), source.to_string()))?;
    record.validate(layout)?;
    Ok(Some(record))
}
fn scratch(layout: &Layout) -> Result<()> {
    let path = layout.pending_mutation_next();
    if let Some(meta) = mutation::metadata(&path)? {
        if !meta.is_file() {
            return Err(mutation::invalid(
                &path,
                "snapshot scratch must be a regular file",
            ));
        }
        fsutil::checkpoint("recovery.scratch-remove", &path)?;
        fs::remove_file(&path)
            .map_err(|source| io_error("discard uncommitted snapshot", &path, source))?;
        sync(layout.root(), "recovery.scratch-sync")?;
    }
    Ok(())
}
pub(crate) fn recover(guard: &StoreGuard<'_>) -> Result<()> {
    guard.require_exclusive()?;
    let layout = guard.layout();
    let record = load(layout).map_err(|source| Error::Recovery {
        record_path: layout.pending_mutation(),
        reference: None,
        phase: "read record",
        change: None,
        remaining: "recovery is blocked; pending entries are preserved",
        source: Box::new(source),
    })?;
    let Some(mut record) = record else {
        return scratch(layout).map_err(|source| Error::Recovery {
            record_path: layout.pending_mutation_next(),
            reference: None,
            phase: "discard scratch",
            change: None,
            remaining: "uncommitted snapshot cleanup needs retrying",
            source: Box::new(source),
        });
    };
    let result = recover_record(guard, &mut record);
    result.map_err(|error| record.context(layout, error))
}
fn recover_record(guard: &StoreGuard<'_>, record: &mut Record) -> Result<()> {
    let layout = guard.layout();
    scratch(layout)?;
    // Persist a snapshot possibly left visible by a failed rename-parent sync.
    // Do not let any deletion rely on a record that can disappear on crash.
    fsutil::open_regular(&layout.pending_mutation())?
        .sync_all()
        .map_err(|source| {
            io_error(
                "confirm mutation record",
                &layout.pending_mutation(),
                source,
            )
        })?;
    sync(layout.root(), "recovery.record-sync")?;
    if record.phase != Phase::Complete {
        let observed = refs::pointer_identity(layout, &record.reference)?;
        let outcome = if observed == record.old_raw {
            Outcome::Aborted
        } else if record.phase != Phase::Acquiring && observed == record.new_raw {
            if record.kind == mutation::Kind::Remove {
                Outcome::Removed
            } else {
                Outcome::Published
            }
        } else {
            return Err(mutation::invalid(
                &layout.ref_entry(&record.reference),
                "live ref matches neither expected old state nor intended change",
            ));
        };
        if let Some(cleanup) = &record.cleanup
            && cleanup.outcome != outcome
        {
            return Err(mutation::invalid(
                &layout.pending_mutation(),
                "live ref disagrees with durable cleanup outcome",
            ));
        }
        let live = live_content(guard, record, outcome)?;
        sync_namespace(layout, record, &live.directories)?;
        if record.phase != Phase::Cleaning {
            record.cleanup = Some(plan_cleanup(layout, record, outcome, &live)?);
            record.phase = Phase::Cleaning;
            preflight(layout, record, &live)?;
            record.save(guard)?;
            fsutil::checkpoint("recovery.cleaning-durable", &layout.pending_mutation())?;
        } else {
            preflight(layout, record, &live)?;
        }
        for removal in &record.cleanup.as_ref().unwrap().entries {
            let path = record.path(layout, &removal.target);
            // A previous attempt may have removed this entry but failed to sync.
            if ancestors(layout, &path)? && mutation::metadata(&path)?.is_some() {
                fsutil::checkpoint("recovery.remove", &path)?;
                let result = if removal.kind == EntryKind::Directory {
                    fs::remove_dir(&path)
                } else {
                    fs::remove_file(&path)
                };
                result.map_err(|source| {
                    io_error(
                        "remove recorded cleanup entry (earlier removals remain visible)",
                        &path,
                        source,
                    )
                })?;
                fsutil::checkpoint("recovery.removed", &path)?;
            }
            sync_surviving_parent(layout, path.parent().unwrap(), "recovery.cleanup-sync")?;
        }
        fsutil::checkpoint("recovery.cleanup-durable", &layout.pending_mutation())?;
        record.phase = Phase::Complete;
        record.save(guard)?;
        fsutil::checkpoint("recovery.complete-durable", &layout.pending_mutation())?;
    }
    fsutil::checkpoint("recovery.record-remove", &layout.pending_mutation())?;
    fs::remove_file(layout.pending_mutation()).map_err(|source| {
        io_error(
            "remove completed mutation record",
            &layout.pending_mutation(),
            source,
        )
    })?;
    fsutil::checkpoint("recovery.record-removed", &layout.pending_mutation())?;
    sync(layout.root(), "recovery.record-remove-sync")
}

struct Live {
    raw: BTreeSet<String>,
    objects: BTreeSet<Sha256Digest>,
    directories: Vec<PathBuf>,
}
fn live_content(guard: &StoreGuard<'_>, record: &Record, outcome: Outcome) -> Result<Live> {
    let mut live = Live {
        raw: BTreeSet::new(),
        objects: BTreeSet::new(),
        directories: Vec::new(),
    };
    // An interrupted acquisition that never touched live content needs only its
    // private stage cleaned; unrelated damaged images do not obstruct that.
    if record.candidate_objects(outcome).is_empty() && record.candidate_raw(outcome).is_empty() {
        return Ok(live);
    }
    let found = discovery::discover(guard)?;
    for entry in &found.entries {
        if let Some(diagnostic) = &entry.diagnostic {
            return Err(mutation::invalid(
                &entry.path,
                format!("cannot establish reachability: {diagnostic}"),
            ));
        }
    }
    if !found.complete {
        return Err(mutation::invalid(
            &guard.layout().root().join("refs"),
            "incomplete namespace discovery prevents reclamation",
        ));
    }
    let mut dependencies = GraphReader::dependencies(&guard.layout().blobs());
    for entry in found.entries {
        let reference = entry
            .local_ref
            .ok_or_else(|| mutation::invalid(&entry.path, "unrecognized committed entry"))?;
        let raw = refs::raw_image_path(guard, &reference)?;
        live.raw
            .insert(raw.file_name().unwrap().to_str().unwrap().to_owned());
        let root = oci::single_root(&raw)?;
        live.objects.extend(
            dependencies
                .dependency_objects(&root)?
                .into_iter()
                .map(|object| object.digest),
        );
    }
    live.directories = found.directories;
    Ok(live)
}
fn sync(path: &Path, checkpoint: &'static str) -> Result<()> {
    require_directory(path)?;
    fsutil::checkpoint(checkpoint, path)?;
    fsutil::sync_directory(path)
        .map_err(|source| io_error("sync recorded mutation namespace", path, source))
}
/// Check every ancestor without following redirected directories. A missing
/// parent is allowed only so a persisted cleanup plan can describe absent trees.
fn ancestors(layout: &Layout, path: &Path) -> Result<bool> {
    require_directory(layout.root())?;
    let relative = path
        .strip_prefix(layout.root())
        .map_err(|_| mutation::invalid(path, "cleanup escapes store"))?;
    let mut parent = layout.root().to_owned();
    let count = relative.components().count();
    for component in relative.components().take(count.saturating_sub(1)) {
        parent.push(component);
        match mutation::metadata(&parent)? {
            None => return Ok(false),
            Some(meta) if meta.is_dir() => (),
            Some(_) => {
                return Err(mutation::invalid(
                    &parent,
                    "cleanup ancestors must be real directories",
                ));
            }
        }
    }
    Ok(true)
}
fn sync_surviving_parent(layout: &Layout, start: &Path, checkpoint: &'static str) -> Result<()> {
    let mut parent = start;
    loop {
        if ancestors(layout, parent)? && mutation::metadata(parent)?.is_some() {
            return sync(parent, checkpoint);
        }
        if parent == layout.root() {
            return Err(mutation::invalid(parent, "store root disappeared"));
        }
        parent = parent
            .parent()
            .ok_or_else(|| mutation::invalid(start, "missing store ancestor"))?;
    }
}
fn sync_namespace(layout: &Layout, record: &Record, directories: &[PathBuf]) -> Result<()> {
    for directory in directories {
        sync(directory, "recovery.namespace-sync")?;
    }
    for depth in (0..=record.reference.encoded_path().components().count()).rev() {
        let path = record.namespace(layout, depth);
        if ancestors(layout, &path)? && mutation::metadata(&path)?.is_some() {
            sync(&path, "recovery.namespace-sync")?;
        }
    }
    if let Some(id) = &record.staging {
        let stage = layout.staging().join(id);
        if mutation::metadata(&stage)?.is_some() {
            sync(&stage, "recovery.source-sync")?;
        }
        sync(&layout.staging(), "recovery.source-sync")?;
    }
    Ok(())
}
fn member_target(raw: Option<&str>, member: String) -> Target {
    if let Some(id) = raw {
        Target::Raw {
            id: id.to_owned(),
            member,
        }
    } else {
        Target::Staging { member }
    }
}
/// Enumerate only a recorded tree, validating its complete shape before any
/// deletion. Files may be partial; unknown names and redirected entries block.
fn tree(
    layout: &Layout,
    record: &Record,
    raw: Option<&str>,
    member: &str,
    entries: &mut Vec<Removal>,
) -> Result<()> {
    let target = member_target(raw, member.to_owned());
    let kind = if raw.is_some() {
        mutation::raw_member(member)
    } else {
        mutation::staging_member(member)
    }
    .ok_or_else(|| {
        mutation::invalid(
            &record.path(layout, &target),
            "unexpected recorded-tree member; preserved",
        )
    })?;
    let path = record.path(layout, &target);
    if !ancestors(layout, &path)? {
        return Err(mutation::invalid(
            &path,
            "recorded tree has a missing ancestor before cleanup planning",
        ));
    }
    if let Some(meta) = mutation::metadata(&path)? {
        check_kind(layout, record, &path, &meta, kind)?;
        if kind == EntryKind::Directory {
            let mut children = Vec::new();
            for child in fs::read_dir(&path)
                .map_err(|source| io_error("enumerate recorded cleanup tree", &path, source))?
            {
                let child = child
                    .map_err(|source| io_error("read recorded cleanup entry", &path, source))?;
                let name = child
                    .file_name()
                    .into_string()
                    .map_err(|_| mutation::invalid(&path, "non-UTF-8 member in recorded tree"))?;
                children.push(if member.is_empty() {
                    name
                } else {
                    format!("{member}/{name}")
                });
            }
            children.sort();
            for child in children {
                tree(layout, record, raw, &child, entries)?;
            }
        }
    }
    entries.push(Removal { target, kind });
    Ok(())
}
fn check_kind(
    layout: &Layout,
    record: &Record,
    path: &Path,
    meta: &fs::Metadata,
    kind: EntryKind,
) -> Result<()> {
    let matches = match kind {
        EntryKind::File => meta.is_file(),
        EntryKind::Directory => meta.is_dir(),
        EntryKind::Pointer => meta.file_type().is_symlink(),
    };
    if !matches {
        return Err(mutation::invalid(
            path,
            "unexpected cleanup entry type; preserved",
        ));
    }
    if kind == EntryKind::Pointer {
        let id = record
            .new_raw
            .as_ref()
            .ok_or_else(|| mutation::invalid(path, "staging pointer without publication intent"))?;
        let target = fs::read_link(path)
            .map_err(|source| io_error("read staged cleanup pointer", path, source))?;
        if target.as_os_str() != layout.raw_image_target(&record.reference, id).as_os_str() {
            return Err(mutation::invalid(
                path,
                "escaping or inconsistent staged pointer",
            ));
        }
    }
    Ok(())
}
fn plan_cleanup(
    layout: &Layout,
    record: &Record,
    outcome: Outcome,
    live: &Live,
) -> Result<Cleanup> {
    let mut entries = Vec::new();
    if record.staging.is_some() {
        tree(layout, record, None, "", &mut entries)?;
    }
    for id in record.candidate_raw(outcome) {
        if !live.raw.contains(&id) {
            tree(layout, record, Some(&id), "", &mut entries)?;
        }
    }
    for object in record.candidate_objects(outcome) {
        if !live.objects.contains(&object.digest) {
            entries.push(Removal {
                target: Target::Blob {
                    digest: object.digest,
                },
                kind: EntryKind::File,
            });
        }
    }
    // Plan only empty namespace tails, allowing directories made empty by an
    // earlier planned child removal. Preserve all other children and prefixes.
    let depths: Vec<_> = if outcome == Outcome::Removed {
        (1..=record.reference.encoded_path().components().count()).collect()
    } else {
        record.namespace_created.clone()
    };
    let mut planned = BTreeSet::new();
    for depth in depths.into_iter().rev() {
        let path = record.namespace(layout, depth);
        if ancestors(layout, &path)?
            && let Some(meta) = mutation::metadata(&path)?
        {
            check_kind(layout, record, &path, &meta, EntryKind::Directory)?;
            let mut empty = true;
            for entry in fs::read_dir(&path)
                .map_err(|source| io_error("inspect namespace cleanup", &path, source))?
            {
                let child = entry
                    .map_err(|source| io_error("read namespace cleanup entry", &path, source))?
                    .path();
                if !planned.contains(&child) {
                    empty = false;
                }
            }
            if !empty {
                continue;
            }
        }
        planned.insert(path);
        entries.push(Removal {
            target: Target::Namespace { depth },
            kind: EntryKind::Directory,
        });
    }
    Ok(Cleanup { outcome, entries })
}
fn preflight(layout: &Layout, record: &Record, live: &Live) -> Result<()> {
    let cleanup = record.cleanup.as_ref().unwrap();
    // A durable plan may contain entries already removed, but cannot omit any
    // remaining cleanup obligation. Rebuilding uses recorded candidates, never
    // the manifests being deleted, and therefore remains valid midway through.
    let remaining = plan_cleanup(layout, record, cleanup.outcome, live)?;
    for entry in remaining.entries {
        if !cleanup.entries.contains(&entry) {
            return Err(mutation::invalid(
                &record.path(layout, &entry.target),
                "durable cleanup plan omits a required entry",
            ));
        }
    }
    let sizes: BTreeMap<_, _> = record
        .candidate_objects(cleanup.outcome)
        .into_iter()
        .map(|Object { digest, size }| (digest, size))
        .collect();
    let paths: BTreeSet<_> = cleanup
        .entries
        .iter()
        .map(|entry| record.path(layout, &entry.target))
        .collect();
    for entry in &cleanup.entries {
        let path = record.path(layout, &entry.target);
        match &entry.target {
            Target::Raw { id, .. } if live.raw.contains(id) => {
                return Err(mutation::invalid(
                    &path,
                    "cleanup raw image is now referenced",
                ));
            }
            Target::Blob { digest } if live.objects.contains(digest) => {
                return Err(mutation::invalid(&path, "cleanup object is now referenced"));
            }
            _ => (),
        }
        if !ancestors(layout, &path)? {
            // A missing ancestor must itself be an entry in this durable plan.
            let mut parent = path.parent().unwrap();
            while parent != layout.root() {
                if !paths.contains(parent) {
                    require_directory(parent)?;
                }
                parent = parent.parent().unwrap();
            }
            continue;
        }
        if let Some(meta) = mutation::metadata(&path)? {
            check_kind(layout, record, &path, &meta, entry.kind)?;
            if let Target::Blob { digest } = &entry.target
                && meta.len() != sizes[digest]
            {
                return Err(mutation::invalid(
                    &path,
                    "cleanup object size disagrees with recorded descriptor",
                ));
            }
            if entry.kind == EntryKind::Directory {
                for child in fs::read_dir(&path)
                    .map_err(|source| io_error("preflight cleanup directory", &path, source))?
                {
                    let child = child
                        .map_err(|source| io_error("read cleanup directory", &path, source))?
                        .path();
                    if !paths.contains(&child) {
                        return Err(mutation::invalid(
                            &child,
                            "unrecorded member in cleanup tree; preserved",
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
