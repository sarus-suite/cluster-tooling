# Filesystem OCI Image Store Design

Version 1.5 · 20 October 2026 · Conceptual design baseline

## 1 Purpose and scope

Provide persistent, user-owned image storage on an HPC parallel filesystem. Each store is one directory, for example `/scratch/user/.sarus-imagestore`; it does not manage a hierarchy of store locations. Group-writable stores remain a future extension. Jobs use a separate, volatile Podman containers-storage store that may disappear after each job. The persistent store retains OCI image content, including compressed tar layers, without storing unpacked layer filesystems.

The architecture consists of a **shared immutable blob CAS, small per-ref OCI layouts, per-ref JSON metadata, and a thin wrapper around Skopeo**. Filesystem locks coordinate access. Removing a reference and reclaiming its content are separate operations.

This document fixes the general design for subsequent in-project planning with Codex. Directory encoding, schemas, lock primitives, publication mechanisms, and exact CLI syntax remain implementation choices.

The external API has seven actions:

| Action | Purpose |
| --- | --- |
| `import` | Copy an image from containers-storage into the persistent store |
| `pull` | Copy an image directly from a remote registry into the persistent store, preserving its manifest or index digest |
| `export` | Copy a persistent image into containers-storage |
| `list` | List committed refs and report obvious metadata damage |
| `remove` | Remove one logical persistent ref |
| `prune` | Reclaim unreferenced content and abandoned internal data |
| `check` | Check metadata and the reachable image graph |

Import and export are named relative to the persistent store. Pull adds direct registry-to-store transfers so users can retain the original registry image identity without first unpacking and recompressing it through containers-storage. There is no hosted registry service, registry push action, SQLite dependency, automatic repair, or deep check functionality.

## 2 Storage organization

The store separates published logical refs, underlying raw-image layouts, shared
content, and private transfer staging. For `analysis/tool:release`, the ref key
is the reversible hexadecimal encoding of that complete logical name.

```text
store/
├── store.json
├── blobs/sha256/<digest>           # Shared immutable content
├── refs/<encoded-key>/ref          # Relative symlink into raw_images/
├── raw_images/raw-<id>/            # Complete OCI layout and matching metadata
│   ├── oci-layout
│   ├── index.json
│   ├── metadata.json
│   └── blobs/                     # Empty; content lives in the shared CAS
├── tmp/operation-<id>/             # Unpublished acquisition work
│   ├── layout/
│   └── blobs/sha256/
└── locks/                         # Stable coordination identities
    ├── store.lock
    └── refs/<encoded-key>/ref.lock
```

The encoded ref key identifies the logical name, the random raw-image ID identifies
one publication instance, and content digests identify exact bytes. Each complete
raw-image layout selects its root from the common CAS; it does not contain an
unpacked filesystem. Atomic replacement switches the logical ref symlink after
the new raw-image layout and metadata are durable. Retired raw images remain for
prune. Encoding ref names is separate from that publication mechanism; it provides
filesystem-safe names without a separate name-to-ID database.

### Shared blob CAS

The content-addressed store identifies immutable objects by digest. Identical stored bytes are reused across refs, especially shared compressed layers. Here, “shared” means sharing between images within one store. Deduplication is based on stored content rather than equivalence of unpacked filesystems.

Skopeo handles image serialization, conversion, and transfer validation. The wrapper ensures that incomplete writes remain unpublished and that concurrent insertion cannot damage an existing object. Published blobs are never modified in place.

### Per-ref OCI layouts

Each logical name and tag selects one image root through its own small OCI layout. That root may be a manifest or a supported image index. The OCI layout is authoritative for the mapping from ref to image content; there is no store-wide mutable `index.json`.

Skopeo's shared-blob options connect these layouts to the common content pool. A ref depends on that pool and is not independently portable unless its required content is materialized with it.

### Per-ref JSON metadata

Each ref has a `metadata.json` containing the small amount of information needed for listing, such as:

- Name and tag, plus the stored root digest.
- The config digest (OCI ImageID), cached from the completed image manifest.
- Import time and, when available, image creation time.
- Platform and cached stored content size.
- Optional provenance from containers-storage or the source registry, and user notes.

For pulled images, record the requested registry reference and its resolved manifest or index digest. The selected stored root must match that digest. Registry tags are mutable, so the recorded digest identifies the content actually pulled. Distinguish this root digest from both the per-ref layout's `index.json` and a containers-storage image ID.

Export and single-image check use a tagged logical ref or a repository name
qualified by its stored root digest, as specified below. This allows users to
reuse a pulled image's original registry digest with the matching stored name.
Digest selection resolves committed content in this store; it does not trigger
another registry lookup or use acquisition provenance as an alias.

Config digests identify image configuration JSON, while manifest/index root
digests identify the stored image representation. Config digests and acquisition
source IDs are display/provenance fields, not alternate selection keys. The
stage-4 metadata schema is version 3 and requires the cached config digest;
older development metadata is rejected without migration.

The OCI layout determines reachability. JSON fields derived from OCI content are display caches; import records and user notes may be irreplaceable operational metadata. Commit metadata with the ref and give its schema a version.

Cached size describes stored image content, not Podman's unpacked size or the space reclaimed by removal. Shared content means sizes across refs cannot be summed to obtain total storage use.

No global catalog is necessary initially. Listing reads the small per-ref files directly, without scanning layers.

### Image selectors

Use one validated `ImageSelector` for image arguments to import, export, pull,
and single-image check. Its string form is `name[:tag][@digest]`, with a mandatory
repository name and at least one of tag or digest. Parsing exposes both the
reference component (`name[:tag]`) and optional digest, with access to the
repository name and optional tag. Display preserves the supplied spelling.
Bare digests, image IDs, untagged names without digests, malformed suffixes, and
unsupported algorithms are rejected. Digests remain full canonical lowercase
SHA-256 values.

Follow the upstream [named-reference grammar](https://github.com/containers/image/blob/main/docker/reference/reference.go),
including registry ports, while retaining path-safety rules. Preserve literal
names without registry expansion, aliases, or an inferred `latest` tag. Thus
`alpine` and `docker.io/library/alpine` are distinct repository names in this
store. Explicit transport URLs and embedded containers-storage settings remain
unsupported. Ordinary repository/tag pairs such as `oci:latest` remain names
and never select a transport. This is a deliberately narrower contract than
every input accepted by Podman or Skopeo.

Without a digest, resolve the exact `name:tag`. With a digest, ignore any supplied
tag and search committed refs with the same literal repository name for the
requested stored root. A selector `alpine:old@digest` may therefore select a
different tag of `alpine`, but cannot select an unrelated repository. Use only
the roots in committed OCI layouts, never cached provenance or config IDs.

Choose deterministically among healthy matching refs and hold the selected
shared ref lock through use, revalidating before use. A healthy match remains
usable when other entries are damaged. If no healthy match exists and damage
or incomplete enumeration prevents establishing absence in the requested
repository, report incomplete resolution rather than declaring the digest
missing. This requires no global catalog.

Stored namespace identities remain explicitly tagged `LocalRef` values, with
registry-port parsing consistent with selectors. Removal is the intentional
exception to the common selector API: it accepts only an explicit `name:tag`
and rejects every digest-bearing selector. It can therefore remove a damaged
ref without resolving its content.

### Store initialization

The store's parent directory must already exist and have been safely prepared by the site or user. This includes saving its creation and the directory names needed to reach it before store initialization begins. Seeing a directory does not prove that those changes will survive a crash. The library assumes this preparation has happened; it does not verify or repair ancestors.

Initialization must be able to open and sync the immediate parent. Creating a missing store also requires permission to write there. A missing parent causes an error; the library creates only the store directory and its internal structure. Stores inside other stores are unsupported. This is a setup restriction, not a requirement to search ancestors for other stores.

A new store contains empty `blobs/sha256`, `refs`, `raw_images`, `tmp`, and `locks/refs` directories, the stable `locks/store.lock` file, and `store.json` containing the store format version. Image-specific directories and files appear only during later image operations. The store format remains version 1; the design document version is separate.

Initialization follows this order:

1. Validate the location and any existing contents. Reject unexpected contents, unsupported markers, and incompatible reserved entries before adding setup files.
2. Create the store directory and minimum lock structure if missing. Concurrent processes must converge on the same directory and stable lock file, without replacing either.
3. Take the exclusive store lock and recheck the store. If another process has published a supported marker, follow the marked-store behavior below.
4. For an unmarked store, reuse recognized empty setup directories, create missing pieces, and write the temporary marker `.store-init.json`.
5. Sync the temporary marker, lock file, internal directories from children to parents, store directory, and immediate parent before publishing `store.json`.
6. Rename the temporary marker to `store.json`, then sync the store directory before reporting success. A failure after marker publication reports that the marker may already be visible.

Always sync the immediate parent for an unmarked store, including retries and existing empty roots. This saves the store's name even if another process created it. It deliberately replaces the earlier requirement to sync only parents known to have changed. No directory above the immediate parent is opened for syncing.

After interruption, a retry reuses only recognized empty setup directories and the reserved temporary marker. It preserves the store directory and lock identity, rejects unexpected contents, and never automatically deletes them. No recovery record or history of directory creation is needed.

For an already marked store, mutating preparation validates it under the store lock and syncs the marker and store directory again, without opening its parent or other ancestors. This completes a save interrupted after marker publication; seeing the marker alone is not proof that the final sync succeeded. Read operations use the existing lock-and-validation behavior and do not initialize missing stores. Image operations use the lock modes in section 4 after initialization.

## 3 Skopeo and the wrapper

Use Skopeo for copies between `containers-storage:` and `oci:`, and from registry sources using `docker://` into `oci:`, with its shared-blob integration. Preserve the job's containers-storage context, including its storage roots and rootless configuration.

For imports and pulls, run Skopeo into private staging and confine `--dest-shared-blob-dir` to that staging area. After a successful copy, the library publishes completed objects into the store's shared blob directory without overwriting existing objects. Skopeo never writes directly into the live shared content pool. For exports, use `skopeo copy --src-shared-blob-dir <shared-blob-directory>` to read from the store's shared pool.

Pull uses `--preserve-digests` and copies from the resolved registry digest. For a multi-platform index, use `--all` to retain the index and its complete image graph; copying only a selected platform would select a different root identity. Do not force manifest conversion or recompression. If the source representation cannot be preserved by the OCI destination, fail the pull rather than silently storing a different digest. Registry authentication and trust configuration remain Skopeo's responsibility; credentials do not belong in per-ref metadata.

**Rely on Skopeo's own transfer validation rather than repeating a complete graph check before and after every copy.** The wrapper checks the selected ref and its own metadata, coordinates access, and publishes completed results. A separate graph traversal is needed for `check` and `prune`, not routinely around imports, pulls, and exports.

A successful copy is the normal basis for accepting the transferred image. The wrapper should inspect only the resulting root and the information needed to construct or reconcile metadata. It should not independently rehash copied layers or rerun the dedicated checker.

Skopeo does not supply a transaction covering the wrapper's ref and metadata. The wrapper remains responsible for private staging, immutable object insertion, and atomic publication of the completed ref and metadata.

Importing from containers-storage may convert representations or recompress layers, changing digests. Import therefore cannot guarantee the original registry manifest digest; recording it as provenance does not restore that identity. Pull is the digest-preserving acquisition path. Any supported image index must be stored with its required local dependencies.

## 4 Locking

Use a store-wide reader/writer lock and a reader/writer lock for each logical ref.

| Action | Store lock | Ref lock |
| --- | --- | --- |
| Import | Shared | Exclusive on destination |
| Pull | Shared | Exclusive on destination |
| Export | Shared | Shared on source |
| List | Shared | Shared while reading each ref |
| Remove | Shared | Exclusive on selected ref |
| Check | Shared | Shared while checking each ref |
| Prune | Exclusive | No additional ref lock needed |

Acquire the store lock before a ref lock. Hold locks for the operation's use of persistent content, including the entire import, pull, or export transfer.

This allows work on different refs to proceed concurrently while preventing prune from deleting content used by an active operation. Concurrent insertion of the same digest must also be safe; ref locks alone do not protect shared blobs.

Lock identities remain stable across ref replacement and removal. Lock behavior must work across nodes on the actual parallel filesystem. These locks coordinate the persistent store; Skopeo and containers-storage retain their own storage coordination.

List and check need coherent observations of individual refs, but do not promise a single snapshot of the entire store.

## 5 Import pull and export

### Import

Import accepts a source `ImageSelector` and an optional destination of the same
type in both the library and CLI. Default an omitted destination to the source
selector, then derive an explicitly tagged committed ref from that effective
destination.

Keep a tag supplied in the effective destination. If it is tagless, generate the
tag from the first 12 hexadecimal characters of the explicit **source digest**,
excluding `sha256:`. If an explicit destination is tagless and the source has no
digest, reject the import before store preparation or tool invocation. Do not
resolve a source tag to obtain a digest merely for naming.

Ignore digests in explicit import destination arguments: they cannot be enforced,
do not supply generated tags, and do not create lookup aliases. Communicate this
through the library report and CLI stderr, and explain it in relevant validation
errors. Such a notice alone does not make a successful import fail.

The examples below use `D` and `E` as placeholders for full SHA-256 digests:

| Source | Explicit destination | Committed ref |
| --- | --- | --- |
| `alpine@D` | omitted | `alpine:<first12(D)>` |
| `alpine@D` | `renamed@E` | `renamed:<first12(D)>`; ignore `E` |
| `alpine@D` | `renamed:release@E` | `renamed:release`; ignore `E` |
| `alpine:latest` | `renamed@E` | Reject; require a destination tag |

Pass a digest-bearing source to Skopeo as `name@digest`, omitting any supplied
tag so that it cannot override the digest. Retain existing image-ID pinning for
sources selected by tag, and retain acquisition metadata without losing the
requested digest from Skopeo's source argument.

Import can change the digest through conversion or compression. The completed
OCI root remains the stored identity. If importing `alpine@D` produces root `R`,
the generated ref remains `alpine:<first12(D)>`, while later digest selection
uses `alpine@R`. Generated tags are naming conveniences, not unique digest
identifiers. Prefix collisions use the ordinary destination-conflict and explicit
replacement rules; never invent an alternative tag.

Under the appropriate locks:

1. Select the source image and destination ref.
2. Let Skopeo copy and convert into an unpublished OCI representation in private staging, with any destination shared-blob directory confined to staging.
3. Read the completed result to prepare matching JSON metadata.
4. Publish the ref and metadata together.
5. After durable publication succeeds, remove this operation's private staging
   without following symlinks, then sync `tmp/`, while retaining both locks.
   Report cleanup failures as structured warnings; the acquisition remains
   successful and its committed ref is not rolled back.

Only publish after Skopeo succeeds and the wrapper's required metadata is complete. A failed import preserves any previous committed ref; unreferenced content or abandoned staging can remain for later pruning.

By default, import fails if the destination ref already exists. Explicit replacement is a mode of import. Replacement switches readers from one complete raw-image layout to another and leaves the old content for prune.

A completed new directory may be published by atomic rename. Replacing an existing ref requires a mechanism that commits the OCI layout and metadata together. The implementation uses a relative ref symlink into `raw_images/`, replaced by one atomic rename after the new layout and metadata have been saved.

### Pull

Pull remains planned for stage 6. Use the common `ImageSelector` for its registry
source and optional destination. Default the destination to the supplied source
selector and reuse import's naming rules: keep an effective destination tag or
generate one from an explicit source digest, ignore and report explicit
destination digests, and reject a tagless destination without a source digest
before preparation or tool invocation. A digest resolved from a source tag does
not supply a generated destination tag.

Resolve the requested source to an immutable top-level manifest or index digest,
giving an explicit source digest priority over any tag. Let Skopeo copy directly
into the persistent store with digest preservation enabled. For an index, retain
the complete graph rather than selecting only the current host platform.

Reuse import's staging, shared CAS insertion, locks, metadata creation, existing-ref policy, atomic publication, and best-effort cleanup after successful publication. The differences are source resolution, registry provenance, and the digest-preserving copy policy. After Skopeo succeeds, reconcile the resulting selected root digest with the resolved source digest as part of preparing metadata; this is a small identity check, not another full image validation pass.

A failed pull preserves any previous committed ref. Explicit replacement follows the same policy as import. Pull does not populate containers-storage; export remains the action that restores a cached image for job use.

### Export

Both source and optional destination
use `ImageSelector`. The destination defaults to the supplied source selector
verbatim, rather than acquisition metadata or the ref chosen during digest
lookup.

Pass export destination digests through to Skopeo and report digest-mismatch
failures. Follow the upstream [destination digest check](https://github.com/containers/image/blob/main/storage/storage_dest.go);
never silently discard these digests. This differs from import's explicitly
ignored destination digest and does not change the preservation policy based on
acquisition kind.

Resolve the selected ref under its locks, confirm that its small ref files identify a usable source, and invoke Skopeo to copy it into the selected containers-storage destination.

Do not perform a separate full graph check first. Let Skopeo detect content and transfer failures during the copy and report them to the user. Export does not change the persistent ref or rewrite metadata to record access.

An export failure leaves the persistent store unchanged. Partial destination content remains under containers-storage's management.

For pulled images, use `--preserve-digests` on export to preserve the copied image manifests, and report failure if the destination cannot honor it. Exporting one platform from a stored index preserves that platform's manifest identity, not the parent index identity; retain the parent index in the CAS and make the selection explicit. A containers-storage image ID remains a separate identifier. A later import from containers-storage still cannot promise reconstruction of the original registry representation.

## 6 Listing and checking

### List

List behaves like an image inventory, displaying name, tag, digest, timestamps, platform, and cached size where available.

The default table follows `podman images`, with repository, tag, a 12-character
OCI ImageID, relative creation time, and human-readable stored-content size.
An optional digest column follows `podman images --digests` and shows the full
stored root. Rich tables and JSON expose the additional cached fields, status,
and diagnostics. Reusable string formatters belong to a presentation module,
separate from storage operations. Matching display units does not make stored
content size equal to Podman's image size.

Its embedded quick check reads only ref structure, `oci-layout`, `index.json`, and `metadata.json`. It detects missing or malformed files, ambiguous root selection, and disagreement between the ref and its cached identity fields. It does not traverse the image graph or inspect every layer.

Keep damaged refs visible and report the problem while continuing to list other refs. A successful metadata check does not imply healthy image content; use “unverified” if a status is shown.

Discover committed refs from the namespace, not only from valid OCI files, so malformed refs are not silently hidden.

### Check

Single-image checks use the same repository-scoped stored-root
resolution, selected-ref lock, and diagnostics as export. Check performs the
metadata check, traverses supported indexes and manifests, reads the relevant
small JSON objects, and checks that referenced content exists and matches
descriptor sizes.

Check is read-only. It reports damaged or unsupported content and returns failure when errors are found or the requested scope cannot be checked. It never repairs or removes refs.

There is no blob hashing, tar unpacking, or deep check mode. Same-size byte corruption may therefore go undetected. Share graph-reading logic between check and prune, and avoid repeatedly inspecting shared objects within one run.

## 7 Removal and pruning

### Remove

Remove targets one explicit tagged logical ref (`LocalRef`). Digest-bearing
selectors are intentionally unsupported, including `name:tag@digest`; removal
does not perform digest resolution. Under its locks, make that ref absent
from the committed namespace as one logical change. Retire its associated raw
image, including layout and metadata, for later pruning. Leave shared blobs,
namespace directories, and stable lock identities untouched.

Other refs and containers-storage are unaffected. A damaged ref can be explicitly removed; discovering damage is never itself a reason to remove it.

### Prune

Prune is explicit maintenance, with an optional dry-run mode.

Under the exclusive store lock:

1. Enumerate all committed refs and traverse their image graphs.
2. Collect the live content set from their OCI descriptors.
3. Identify unreferenced CAS objects.
4. Apply a grace period and report or delete eligible objects.
5. Clean abandoned staging and retired internal ref data when safe.

The initial policy is conservative: **any unresolved committed-ref error or incomplete scan blocks destructive pruning**. Never treat an unreadable root or unsupported graph as having no dependencies. Cached metadata and previous check results cannot replace the current reachability scan.

The grace period provides a recovery margin, not concurrency protection. Prune cleans only recognized internal data and must not delete active lock identities or silently discard broken committed refs.

Removing a ref and pruning its content are intentionally separate. The ref's cached image size is not an estimate of space that prune will reclaim.

## 8 Operational principles and planning boundaries

The essential guarantees are:

- Published content is immutable and refs expose complete raw images.
- Failed imports and pulls preserve prior refs.
- Pulled refs retain the resolved registry manifest or index digest.
- Active operations are protected against pruning.
- List and check report damage without deleting data.
- Prune relies on a complete scan of authoritative refs.

Current stores are user-owned; group-writable stores require a future design extension. Validate cross-node initialization, locking, publication visibility, and durability on the target parallel filesystem. Backups or snapshots are needed to recover accidentally deleted refs and operational metadata; the CAS alone cannot reconstruct their names and history.

Concrete planning should resolve ref naming, metadata schema, publication strategy, Skopeo version and shared-blob behavior, supported registry representations, digest preservation into containers-storage, compression and platform policy, and pruning grace periods. Keep these choices within the seven-action API. Human-readable results and structured output for scripts can share the same operation semantics.

## References

- [OCI Image Layout Specification](https://github.com/opencontainers/image-spec/blob/main/image-layout.md)
- [Containers image transport documentation](https://github.com/containers/image/blob/main/docs/containers-transports.5.md)
- [Skopeo copy documentation](https://github.com/podman-container-tools/skopeo/blob/main/docs/skopeo-copy.1.md)
- [Named-reference grammar](https://github.com/containers/image/blob/main/docker/reference/reference.go)
- [Containers-storage destination digest checks](https://github.com/containers/image/blob/main/storage/storage_dest.go)

The storage and transfer formats come from these upstream interfaces. Locking, metadata, publication, removal, and pruning policies are decisions of this design.
