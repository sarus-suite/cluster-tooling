# Filesystem OCI Image Store Design

Version 1.8 · 21 October 2026 · Conceptual design baseline

## 1 Purpose and scope

Provide persistent, user-owned image storage on an HPC parallel filesystem. Each store is one directory, for example `/scratch/user/.sarus-imagestore`; it does not manage a hierarchy of store locations. Group-writable stores remain a future extension. Jobs use a separate, volatile Podman containers-storage store that may disappear after each job. The persistent store retains OCI image content, including compressed tar layers, without storing unpacked layer filesystems.

The architecture consists of a **shared immutable blob CAS, small per-ref OCI layouts, per-ref JSON metadata, and a thin wrapper around Skopeo**. Filesystem locks coordinate access. Removing a reference and reclaiming its content are separate operations.

This document defines the architecture, behavior, guarantees, and boundaries
of the store. Concrete interfaces and data representations must preserve these
requirements.

The library and CLI share seven actions:

| Action | Purpose |
| --- | --- |
| `import` | Copy an image from containers-storage into the persistent store |
| `pull` | Copy one platform-specific image directly from a remote registry into the persistent store, preserving its manifest digest |
| `export` | Copy a persistent image into containers-storage |
| `list` | List committed refs and report obvious metadata damage |
| `remove` | Remove one logical persistent ref |
| `prune` | Reclaim unreferenced content and abandoned internal data |
| `check` | Check metadata and the reachable image graph |

The library also provides an image availability query for callers deciding
whether to acquire content. It distinguishes images that pass structural
validation, definite misses, and unusable stored images, while reporting
operational failures separately. Availability has no CLI command and performs
no automatic acquisition or recovery.

Import and export are named relative to the persistent store. Pull adds direct registry-to-store transfers so users can retain the original registry image identity without first unpacking and recompressing it through containers-storage. There is no hosted registry service, registry push action, SQLite dependency, automatic repair, or deep check functionality.

Acquisition supports one platform-specific OCI image manifest per ref.
Tag pulls may select it through a registry index, but do not retain the parent
index or other platforms. Multi-platform stored images, complete-index storage,
index-parent metadata and inventory, platform-selecting index export,
nested-index source resolution, and their qualification are future extensions
outside this design's acquisition and export scope. Graph checking and pruning
must still account for every dependency of any encountered index.

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

Each logical name and tag selects one image root through its own small OCI
layout. Acquisition selects a single image manifest; acquiring or exporting
indexes and describing index parents are outside scope. Checking and pruning must nevertheless
handle encountered index graphs completely or report an incomplete inspection.
The OCI layout is authoritative for the mapping from ref to image content;
there is no store-wide mutable `index.json`. The per-ref layout entry point
is distinct from a registry image index used only for source selection.

Skopeo's shared-blob options connect these layouts to the common content pool. A ref depends on that pool and is not independently portable unless its required content is materialized with it.

### Per-ref JSON metadata

Each ref has a `metadata.json` containing the small amount of information needed for listing, such as:

- Name and tag, plus the stored root digest.
- The config digest (OCI ImageID), cached from the completed image manifest.
- Import time and, when available, image creation time.
- Platform and cached stored content size.
- Optional provenance from containers-storage or the source registry, and user notes.

For pulled images, record the requested registry reference separately from the
resolved platform-specific manifest digest. That resolved digest must equal
the stored image's root digest. Registry tags are mutable, so the digest
identifies the image actually pulled. An index used for tag selection is not
stored and never supplies a lookup alias. Digest sources must identify the image manifest
directly. Distinguish the stored root from the per-ref layout's `index.json`
and a containers-storage image ID.

Export, single-image check, and image availability use a tagged logical ref or
a repository name qualified by its stored root digest, as specified below.
This allows users to reuse a pulled image's original registry digest with the matching stored name.
Digest selection resolves committed content in this store; it does not trigger
another registry lookup or use acquisition provenance as an alias.

Config digests identify image configuration JSON, while root digests identify
the stored image representation. Cache the config digest for display.
Config digests and acquisition source IDs provide display information or
provenance, not alternate selection keys.

The OCI layout determines reachability. JSON fields derived from OCI content are display caches; import records and user notes may be irreplaceable operational metadata. Commit metadata with the ref and give its schema a version.

Cached size describes stored image content, not Podman's unpacked size or the space reclaimed by removal. Shared content means sizes across refs cannot be summed to obtain total storage use.

No global catalog is necessary initially. Listing reads the small per-ref files directly, without scanning layers.

### Image selectors

Use a common validated image selector for import, export, pull, single-image
check, and library image availability. Its string form is `name[:tag][@digest]`,
with a mandatory repository name and at least one of tag or digest. Keep the repository name, optional tag,
and optional digest distinct, and preserve the supplied spelling for display.
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

Stored namespace identities remain explicitly tagged references, with
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

Always sync the immediate parent for an unmarked store, including retries and existing empty roots. This saves the store's name even if another process created it. No directory above the immediate parent is opened for syncing.

After interruption, a retry reuses only recognized empty setup directories and the reserved temporary marker. It preserves the store directory and lock identity, rejects unexpected contents, and never automatically deletes them. No recovery record or history of directory creation is needed.

For an already marked store, mutating preparation validates it under the store lock and syncs the marker and store directory again, without opening its parent or other ancestors. This completes a save interrupted after marker publication; seeing the marker alone is not proof that the final sync succeeded. Read operations use the existing lock-and-validation behavior and do not initialize missing stores. Image operations use the lock modes in section 4 after initialization.

## 3 Skopeo and the wrapper

Use Skopeo for copies between `containers-storage:` and `oci:`, and from registry sources using `docker://` into `oci:`, with its shared-blob integration. Preserve the job's containers-storage context, including its storage roots and rootless configuration.

For imports and pulls, run Skopeo into private staging and confine `--dest-shared-blob-dir` to that staging area. After a successful copy, the library publishes completed objects into the store's shared blob directory without overwriting existing objects. Skopeo never writes directly into the live shared content pool. For exports, use `skopeo copy --src-shared-blob-dir <shared-blob-directory>` to read from the store's shared pool.

Pull resolves a single manifest before layer transfer using
`skopeo inspect --raw`, then uses `--preserve-digests` to copy from that manifest's pinned
registry digest. It does not use `--all`, force manifest conversion or
recompression, or fall back to another digest. If the representation cannot be
preserved by the OCI destination, fail the pull. Registry authentication and
trust configuration remain Skopeo's responsibility; use the same configuration
for resolution and copying, and never put credentials in per-ref metadata.

**Rely on Skopeo's own transfer validation rather than repeating a complete graph check before and after every copy.** The wrapper checks the selected ref and its own metadata, coordinates access, and publishes completed results. A separate graph traversal is needed for checking, availability inspection, and pruning. Transfers do not automatically run availability or routinely traverse the complete graph around imports, pulls, and exports.

A successful copy is the normal basis for accepting the transferred image. The wrapper should inspect only the resulting root and the information needed to construct or reconcile metadata. It should not independently rehash copied layers or rerun the dedicated checker.

Skopeo does not supply a transaction covering the wrapper's ref and metadata. The wrapper remains responsible for private staging, immutable object insertion, and atomic publication of the completed ref and metadata.

Importing from containers-storage may convert representations or recompress
layers, changing digests. Import therefore cannot guarantee the original
registry manifest digest; recording it as provenance does not restore that
identity. Pull preserves the selected image manifest. Acquisition does not
retain indexes; checking and pruning must nevertheless account for every
dependency of any encountered index or report incomplete traversal.

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
| Image availability (library only) | Shared | Shared on selected ref throughout inspection |
| Prune | Exclusive | No additional ref lock needed |

Acquire the store lock before a ref lock. Hold locks for the operation's use of persistent content, including the entire import, pull, or export transfer.

This allows work on different refs to proceed concurrently while preventing prune from deleting content used by an active operation. Concurrent insertion of the same digest must also be safe; ref locks alone do not protect shared blobs.

Lock identities remain stable across ref replacement and removal. Lock behavior must work across nodes on the actual parallel filesystem. These locks coordinate the persistent store; Skopeo and containers-storage retain their own storage coordination.

List and check need coherent observations of individual refs, but do not promise a single snapshot of the entire store.

Availability holds its locks throughout inspection. Its result describes the
observed image and does not reserve it for later acquisition or use. A later
operation acquires its own locks and validates its own preconditions.

## 5 Import pull and export

### Import

Import accepts a source image selector and an optional destination selector.
Default an omitted destination to the source selector, then derive an explicitly
tagged committed ref from that effective destination.

Keep a tag supplied in the effective destination. If it is tagless, generate the
tag from the first 12 hexadecimal characters of the explicit **source digest**,
excluding `sha256:`. If an explicit destination is tagless and the source has no
digest, reject the import before store preparation or tool invocation. Do not
resolve a source tag to obtain a digest merely for naming.

Ignore digests in explicit import destination arguments: they cannot be enforced,
do not supply generated tags, and do not create lookup aliases. Communicate this
through operation results and user-facing diagnostics, and explain it in
relevant validation errors. Such a notice alone does not make a successful
import fail.

The examples below use `D` and `E` as placeholders for full SHA-256 digests:

| Source | Explicit destination | Committed ref |
| --- | --- | --- |
| `alpine@D` | omitted | `alpine:<first12(D)>` |
| `alpine@D` | `renamed@E` | `renamed:<first12(D)>`; ignore `E` |
| `alpine@D` | `renamed:release@E` | `renamed:release`; ignore `E` |
| `alpine:latest` | `renamed@E` | Reject; require a destination tag |

Pass a digest-bearing source to Skopeo as `name@digest`, omitting any supplied
tag so that it cannot override the digest. Pin tag-selected sources by image
ID for the transfer, and retain acquisition metadata without losing the
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

A completed new directory may be published by atomic rename. Replacing an
existing ref must commit the OCI layout and metadata together. A relative ref
symlink into the raw-image area provides this indirection: replace it by one
atomic rename after the new layout and metadata have been saved.

### Pull

Pull uses the common image selector for its registry source and optional
destination. Default the destination to the supplied source selector and follow
import's naming rules: keep an effective destination tag or
generate one from an explicit source digest, ignore and report explicit
destination digests, and reject a tagless destination without a source digest
before preparation or tool invocation. A digest resolved from a source tag does
not supply a generated destination tag.

Keep registry pull configuration separate from containers-storage settings.
Allow optional target-platform selection by OS, architecture, and variant,
with `--platform OS/ARCH[/VARIANT]` as the command-line form. Pull requires
Skopeo, not Podman.

Resolve the source before layer transfer with `skopeo inspect --raw`:

1. For a digest source, ignore any supplied tag and inspect exactly
   `repository@digest`. Require a supported image manifest and verify its
   SHA-256 digest from the original response bytes. Reject index and
   manifest-list digests, even when they contain just one image; never select
   a child or fall back to a tag for these sources.
2. For a tag source, inspect the tag once. A direct image manifest supplies
   its digest through SHA-256 of the exact raw bytes, without reserializing
   JSON. For an OCI index or Docker manifest list, select one supported image
   manifest using the platform policy below. Inspect that child by digest,
   verifying its type, digest, and descriptor size. Do not look up the mutable
   tag again.
3. Reject malformed objects, unsupported schemas or representations, and
   resolution requiring nested-index traversal. Reading a Docker manifest-list
   envelope does not add support for storing Docker-specific image content.
4. Copy from `repository@manifest-digest` into private staging with digest
   preservation. The completed layout must select exactly one supported image
   manifest with that resolved digest. Publish only it and its dependencies;
   do not retain a source index or its other platforms.

Normal `skopeo inspect` reports the top-level digest, so its `Digest` field alone
cannot resolve the selected child. Raw response hashing applies to small JSON
objects, not layers or the complete image graph.

Acquisition uses a bounded platform policy, rather than promising Skopeo's
defaults or compatibility fallbacks:

- An explicit target platform supplies OS, architecture, and optional variant.
  Without it, index selection follows the acquiring process's architecture:
  `linux/amd64` on AMD64 and `linux/arm64` on ARM64. Other architectures need
  an explicit platform for index selection. Do not detect CPU variants.
- Match descriptor OS and architecture; filter by variant only when one is
  supplied. Require one distinct matching manifest digest. Consistent
  duplicate descriptors may collapse to one candidate. No match or multiple
  distinct matches fails with actionable diagnostics; never use descriptor
  order to break a tie.
- Direct manifests need no host selection. Without an explicit platform,
  accept the actual image platform. With an explicit platform, verify the
  completed configuration's OS, architecture, and any supplied variant before
  publication. Missing or mismatching required values fail.

Repository-wide architecture enforcement is outside scope. Architecture-specific
repository names remain an operational convention, not a store-enforced policy.

Pull follows the same requirements as import for private staging, immutable
content insertion, locks, metadata, destination conflicts, atomic publication,
and best-effort cleanup after successful publication. Its differences are
source resolution, registry provenance, and the digest-preserving copy policy.
After Skopeo succeeds, require the completed root digest to equal the resolved
manifest digest before publication. This is a small identity check, not another
full image validation pass.

A failed pull preserves any previous committed ref. Explicit replacement follows the same policy as import. Pull does not populate containers-storage; export remains the action that restores a cached image for job use.

### Export

Both source and optional destination use the common image selector. The
destination defaults to the supplied source selector verbatim, rather than
acquisition metadata or the ref chosen during digest lookup.

Pass export destination digests through to Skopeo and report digest-mismatch
failures. Follow the upstream [destination digest check](https://github.com/containers/image/blob/main/storage/storage_dest.go);
never silently discard these digests. This differs from import's explicitly
ignored destination digest. Every export uses the same preservation policy,
regardless of acquisition kind.

Resolve the selected ref under its locks, confirm that its small ref files identify a usable source, and invoke Skopeo to copy it into the selected containers-storage destination.

Do not perform a separate full graph check first. Let Skopeo detect content and transfer failures during the copy and report them to the user. Export does not change the persistent ref or rewrite metadata to record access.

An export failure leaves the persistent store unchanged. Partial destination content remains under containers-storage's management.

Every export must use `--preserve-digests` and report failure if
containers-storage cannot preserve the stored manifest digest. This applies
to both imported and pulled images, including existing refs, without a metadata
migration. Never retry without preservation. Import itself may change image
representation during acquisition; the completed stored manifest is the identity
that subsequent exports must ask Skopeo to preserve. Do not add round-trip
identity validation for imported images or runtime post-export digest inspection.
Report the stored manifest digest and reject stored indexes. Export cannot choose
another architecture from a single stored manifest. A containers-storage image
ID remains a separate identifier. A later import still cannot promise
reconstruction of the original registry representation.

## 6 Listing, checking, and image availability

### List

List behaves like an image inventory, displaying name, tag, digest, timestamps, platform, and cached size where available.

The default table follows `podman images`, with repository, tag, a 12-character
OCI ImageID, relative creation time, and human-readable stored-content size.
An optional digest column follows `podman images --digests` and shows the full
stored root. Rich tables and JSON expose the additional cached fields, status,
and diagnostics. Presentation remains separate from storage operations.
Matching display units does not make stored content size equal to Podman's
image size.

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

Checking must account for the complete supported graph, including any
encountered indexes and nested indexes. This safety requirement does not imply
support for index acquisition, index-parent metadata, or index export.

There is no blob hashing, tar unpacking, or deep check mode. Same-size byte
corruption may therefore go undetected. Checking and pruning must use consistent
reachability rules and avoid repeatedly inspecting shared objects within one run.

### Image availability

Availability is a library-only query that helps callers decide whether to reuse
stored content or attempt acquisition. It applies the same structural validation
as checking, including the complete supported dependency graph, and distinguishes
these outcomes:

| Outcome | Meaning and caller policy |
| --- | --- |
| Ready | The selected image passes structural validation and can be reused within the checker's limits. |
| Missing | The image is definitively absent from a valid initialized store; the caller can attempt pull or import. |
| Unusable | The selected image has detected damage or unsupported content; the caller decides whether to report it or attempt explicit replacement. |

Structured diagnostics distinguish damage from unsupported content without
requiring callers to parse messages. They retain context and allow multiple findings.
Operational failures that prevent determining an outcome are reported as errors,
including inaccessible stores, permission failures, lock timeouts, and incomplete
selector resolution. A missing or uninitialized store is an error, not an image
miss. The query never initializes it.

Use the same literal repository-scoped stored-root selection and digest
precedence as export and single-image checking, with deterministic matching-ref
selection. A damaged candidate that prevents establishing a digest match leaves
resolution incomplete; it does not prove absence.

Availability describes local stored content. It invokes no external tools or
registry requests, checks no remote-tag freshness, and treats acquisition
provenance as descriptive rather than an alternate lookup identity. Like other
read operations, it preserves image data and may create only existing forms of
coordination files and directories while keeping lock identities stable.

The query does not acquire, repair, replace, or remove images. A later pull or
import must still check its destination under its own locks and require explicit
replacement when a ref exists. Reacquiring unsupported content may reproduce the
same outcome, and replacing a ref cannot generally repair damaged immutable
shared objects.

A ready outcome is limited to the structural checks described above. It does
not verify blob hashes, decompress or unpack layers, or certify execution.
Availability does not extend index acquisition or export support.

## 7 Removal and pruning

### Remove

Remove targets one explicit tagged logical ref. Digest-bearing
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

Previous availability outcomes also cannot replace the fresh authoritative scan,
justify omitting a committed ref, imply an empty dependency set, or authorize
deletion. Unusable committed refs still block destructive pruning. Active
availability inspections are protected by their shared store locks.

Use the same authoritative reachability rules as checking. Any encountered
index must be traversed completely, including every platform dependency, or
block destructive pruning. Excluding index acquisition and export from scope
never justifies ignoring dependencies.

The grace period provides a recovery margin, not concurrency protection. Prune cleans only recognized internal data and must not delete active lock identities or silently discard broken committed refs.

Removing a ref and pruning its content are intentionally separate. The ref's cached image size is not an estimate of space that prune will reclaim.

## 8 Operational principles and scope boundaries

The essential guarantees are:

- Published content is immutable and refs expose complete raw images.
- Failed imports and pulls preserve prior refs.
- Pulled refs retain the resolved platform-specific registry manifest digest.
- Active operations are protected against pruning.
- List, check, and availability report damage without deleting data.
- Prune relies on a complete scan of authoritative refs.

Current stores are user-owned; group-writable stores require a future design extension. Validate cross-node initialization, locking, publication visibility, and durability on the target parallel filesystem. Backups or snapshots are needed to recover accidentally deleted refs and operational metadata; the CAS alone cannot reconstruct their names and history.

Concrete choices for ref naming, metadata representation, publication,
Skopeo integration, supported registry representations, compression, and
pruning grace periods must preserve these guarantees across the shared actions
and library-only availability query. The CLI retains seven commands.
Human-readable results and structured output for scripts share the same
operation semantics.

Complete-index acquisition and storage, nested-index source resolution,
multi-platform stored images, index-parent inventory, and platform-selecting
index export remain future extensions. Single-platform transfer and storage
checks do not qualify these workflows or establish Lustre or VAST deployment
guarantees.

## References

- [OCI Image Layout Specification](https://github.com/opencontainers/image-spec/blob/main/image-layout.md)
- [Containers image transport documentation](https://github.com/containers/image/blob/main/docs/containers-transports.5.md)
- [Skopeo copy documentation](https://github.com/podman-container-tools/skopeo/blob/main/docs/skopeo-copy.1.md)
- [Skopeo inspect documentation](https://github.com/podman-container-tools/skopeo/blob/main/docs/skopeo-inspect.1.md)
- [Named-reference grammar](https://github.com/containers/image/blob/main/docker/reference/reference.go)
- [Containers-storage destination digest checks](https://github.com/containers/image/blob/main/storage/storage_dest.go)

The storage and transfer formats come from these upstream interfaces. Locking, metadata, publication, removal, and pruning policies are decisions of this design.
