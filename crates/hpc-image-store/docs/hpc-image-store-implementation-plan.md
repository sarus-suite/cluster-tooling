# Staged implementation of the HPC OCI image store

Status: stages 1–7 are implemented in `crates/hpc-image-store`.
The crate provides single-platform containers-storage import and export,
registry pull preserving one OCI manifest digest, inventory, read-only graph
checking, and explicit-name removal. Stage 4.1 supplies common selectors and
repository-scoped stored-root lookup; stage 5 checks complete supported OCI
graphs, including nested indexes. Stage 6.1 adds raw registry resolution,
bounded platform selection, and separate registry configuration while retaining
store format 1 and ref metadata schema 3. Stage 6.2 requires digest preservation
on every export, including imported images, and qualifies the registry round
trip. Stage 6.3 adds library-only image availability with typed diagnostics and
operational errors. Stage 7 adds conservative library and CLI pruning. Stage 8
remains planned.
Stage 8 completes the first proof of concept (PoC).
Multi-platform stored images and complete-index storage remain outside that
PoC. Lustre/VAST behavior remains unqualified pending site tests.

Design baseline: [Filesystem OCI Image Store Design](hpc-image-store-design.md), version 1.10.

Implementation update, 2026-10-22: stage 7 now reclaims recognized unused ref
locks and empty ref and lock directories immediately, independently of the image
content grace period. The permanent store lock and locks belonging to committed
refs remain. Exclusive-store prune can reclaim an unused ref lock because every
holder, waiter, and transfer child keeps its shared store lock until its ref-lock
descriptors close. Later operations can recreate bookkeeping for absent refs.
The namespace durability barrier runs before bookkeeping-only cleanup as well.
Unknown lock subtrees are preserved, inspection failures block all deletion,
and removal or sync errors retain the existing partial-progress semantics.

The complete suite passed 190 ordinary tests, including seven doctests and both
existing real-tool tests using Podman 5.4.2 and Skopeo 1.18.0. Crate check,
Clippy with warnings denied, formatting, and diff checks passed. Six internal
subprocess helpers and the Docker Hub smoke test were ignored; no external
registry test was run. New tests cover immediate cleanup with young image
content retained, shared prefixes, unknown lock data, missing-ref lookups,
inspection and mutation failures, actual waiters, lock recreation, and CLI
report parity. Orphaned transfer regressions verify that unused locks survive
while children run and can be reclaimed after exit. Public reports gain
`RefLock`, `RefLockDirectory`, and `RefDirectory` kinds; store format 1 and
metadata schema 3 remain unchanged. Site qualification remains pending.
The earlier validation records below retain their historical meaning.

Implementation correction, 2026-10-21: stage 7 now persists the scanned ref
namespace before any destructive cleanup with eligible candidates. The exclusive
store lock remains held while every scanned directory, including empty leaves,
intermediate name directories, and `refs/`, is synced from children to parents.
This prevents an older ref from reappearing after a crash once its retired
content has been reclaimed. Namespace sync failure blocks all cleanup and
returns a sync diagnostic with zero eligible and removed totals. Dry-run skips
the barrier and does not certify that it would succeed.

The full crate suite passed 182 ordinary tests, including seven doctests and
both existing real-tool tests with Podman 5.4.2 and Skopeo 1.18.0. Crate check,
Clippy with warnings denied, formatting, and diff checks passed. Six internal
subprocess helpers and the Docker Hub smoke test were ignored; no external
registry test was run. New regressions cover failed removal/replacement syncs,
a publisher terminated after rename, every level of the ref namespace, retry,
lock retention, dry-run behavior, and real CLI sync failures in human and JSON
modes. Public interfaces, store format 1, and metadata schema 3 remain unchanged.
Actual crash persistence and Lustre/VAST behavior require site qualification.
The earlier results below are historical records.

Implementation update, 2026-10-21: stage 7 adds
`Store::prune(&PruneOptions)` and `prune [--dry-run] [--grace-period DURATION]`.
Both modes require an initialized store and hold the exclusive store lock
through a fresh authoritative scan and any deletion and syncing, without
creating ref-lock files. Incomplete scans block every deletion. Unknown contents
preserve their whole cleanup directory, while removal and sync failures stop
pruning with partial-progress and durability diagnostics. The default grace is
24 hours, with an explicit override including zero.

The complete crate suite passed 177 ordinary tests, including seven doctests
and both existing real-tool tests using Podman 5.4.2 and Skopeo 1.18.0. Six
internal subprocess helpers and the Docker Hub smoke test were ignored; no
external registry test was run. Crate check, Clippy with warnings denied,
formatting, and diff checks passed. New coverage includes shared layers,
nested-index reachability, conflicting descriptors across refs, blocked scans,
unknown-tree preservation, grace boundaries, permissions, partial deletion and
sync errors, acquisition remnants, and lock exclusion including orphaned copy
processes. Store format 1 and ref metadata schema 3 are unchanged. Stage 8 and
Lustre/VAST site qualification remain pending. Earlier records below are
historical.

Implementation update, 2026-10-21: stage 6.3 adds
`Store::availability(&ImageSelector)` with `Ready`, `Missing`, and
`Unusable { diagnostics }` outcomes. Dedicated diagnostics classify damage and
unsupported content, while operational failures and incomplete digest resolution
remain errors. Checking and availability share typed wrapper and complete graph
inspection; checker reports and CLI interfaces retain their existing behavior.
The query holds both shared locks throughout inspection, invokes no tools,
preserves stored content, and grants no reservation for later acquisition.

The full crate suite passed 158 ordinary tests, including six doctests and both
existing real-tool tests using Podman 5.4.2 and Skopeo 1.18.0. Six internal
subprocess helpers and the Docker Hub smoke test were ignored; no external
registry test was requested or run. Coverage includes permissions, short actual
lock timeouts, process coordination, selector races, multiple typed findings,
content preservation, and destination conflicts after availability observations.
Final targeted validation and crate checks are recorded in the README.
Store format 1 and ref metadata schema 3 are unchanged. Pruning, broader stage-8
qualification, and Lustre/VAST site testing remain pending. The dated records
below describe earlier stages and planning revisions.

Implementation update, 2026-10-21: stage 6.2 passed 142 ordinary tests,
including five doctests and both existing real-tool tests. Six internal
subprocess helpers and the Docker Hub smoke test were ignored. Crate check,
Clippy with warnings denied, formatting, and diff checks passed. Every export
now requires `--preserve-digests`, including existing imported refs, and returns
capability or preservation failures without fallback. Import acquisition still
permits representation changes; no imported-image round-trip digest comparisons
were added. Controlled tests cover both acquisition kinds, genuine pull results,
selectors and destination digests, CLI failures, stored-index rejection, and
persistent-store snapshots across successful and failed exports.

The extended Docker Hub smoke test passed on its only attempt using Podman 5.4.2
and Skopeo 1.18.0 with fresh disposable overlay storage. One acquisition of
`docker.io/library/alpine:3.23` for `linux/amd64` supplied named and qualified-digest
exports through the library and JSON CLI, with omitted and explicit destinations.
Exact manifest bytes and digest were preserved. Matching destination digests
succeeded, a mismatch failed, the configuration image ID was checked separately,
and persistent-store snapshots and caller configuration remained unchanged.
No registry probes, retries, or additional pulls were performed.
Test-owned storage was cleaned up. Store format 1 and ref metadata schema 3 remain
unchanged. Stage 6.3, pruning, broader stage-8 qualification, and Lustre/VAST site
testing remain pending.
Earlier planning revisions and validation results below are historical records.

Planning revision, 2026-10-21: add library-only image availability in stage 6.3
to help callers decide whether to acquire an image. Stages 7 and 8 retain
conservative pruning and add availability qualification. This revision changes
documentation only; availability remains unimplemented and adds no CLI command,
automatic acquisition, or repair. Existing implementation results below remain
historical records.

Implementation update, 2026-10-21: stage 6.1 passed 138 ordinary tests,
including five doctests and both existing real-tool tests, with six internal
subprocess helpers and the Docker Hub smoke test ignored. Crate check, Clippy
with warnings denied, formatting, and diff checks passed. Real local acceptance
used Podman 5.4.2 and Skopeo 1.18.0 with fresh disposable overlay storage.
The explicitly selected Docker Hub smoke test then passed on its only attempt,
pulling `docker.io/library/alpine:3.23` for `linux/amd64` and checking the result
locally. It stored manifest
`sha256:1f3591b8a02ea153f41c5bba878ad477f63ab3d19349762cb77504db02a23e15`.
Ordinary tests and CI do not contact Docker Hub. Controlled fixtures cover raw
identity, index selection, tag movement, configuration forwarding, replacement,
publication failures, cleanup, concurrent content insertion, and orphaned-copy
locks. Store format 1 and ref metadata schema 3 are unchanged. Stage 6.2,
pruning, broader stage-8 qualification, and Lustre/VAST site testing remain
pending. The earlier planning revision and implementation results below are
historical records.

Planning revision, 2026-10-21: stage 6 now targets single-platform registry
acquisition and manifest-preserving export. Tag sources may use a registry index
to select one manifest, but digest sources must identify that manifest directly.
Source resolution uses raw registry JSON and the bounded library platform policy
below. This revision changes documentation only; stages 6.1, 6.2, 7, and 8 are
not implemented or newly validated. Dated implementation results below are
historical records.

Implementation update, 2026-10-20: stage 5 passed 120 tests, including four
doctests and both real-tool tests, with the five existing subprocess helpers
ignored. Crate check, Clippy with warnings denied, formatting, and diff checks
passed. Real acceptance used Podman 5.4.2 and Skopeo 1.18.0 with fresh disposable
overlay storage and also checked the imported image through the library and
JSON CLI. Final graph-fixture refinements and the new documentation example
passed targeted graph tests and all doctests after the full run. Store format 1
and ref metadata schema 3 remain unchanged. Stage 6 is next; pull and prune
remain future work. The README records supported content, reporting semantics,
test coverage, and checking limits. Lustre/VAST qualification remains separate.

Earlier update, 2026-10-20: stage 4.1 passed 103 tests, including three
doctests and both real-tool tests, with only five internal subprocess helpers
ignored. Crate check, Clippy with warnings denied, formatting, and diff checks
passed. Real acceptance used Podman 5.4.2 and Skopeo 1.18.0 in fresh disposable
storage outside the sandbox. It verified digest-selected imports, source-derived
tags, repository-qualified exports, and matching and mismatching export
destination digests. The README records the breaking API changes and results.
The dated results below describe earlier stages and their historical APIs.

The optional-destination refinement was verified on 2026-10-17: import and export
accept omitted destinations in the CLI and library, using the source verbatim.
The full crate suite passed 74 tests including doctests and both real-tool tests,
with five subprocess helpers ignored; crate check, Clippy, formatting, and diff
checks also passed. The real containers-storage round trip used Podman 5.4.2 and
Skopeo 1.18.0 outside the sandbox. This adds no operations from stages 4–8.

Stage 4 was verified on 2026-10-18: the full crate suite passed 96 tests,
including three doctests and both real-tool tests, with five internal subprocess
helpers ignored. Crate check, Clippy, formatting and diff checks passed. Real
acceptance used Podman 5.4.2 and Skopeo 1.18.0 and verified digest export and the
OCI config ID displayed beside Podman's image ID. Inventory, removal, digest
resolution, all output formats, malformed paths, and concurrent ref changes
have local coverage. Ref metadata now requires schema 3; existing development
refs must be recreated explicitly. Stages 5–8 and site qualification remain
future work. The README records the interfaces and validation details.

## Summary

Create a workspace-private `hpc-image-store` crate containing a synchronous library and a thin development CLI. The CLI supports development and debugging; the library owns all storage behavior and can later become a dependency of `sarusctl` and Skybox.

Implement the stages below in order, with each stage independently reviewable.
The first PoC ends at stage 8 and supports single-platform containers-storage
imports and digest-preserving registry pulls of one platform-specific OCI image
manifest per ref. Tag pulls may select that manifest from an OCI index or Docker
manifest list; the parent and other platforms are not retained. Complete-index
storage, multi-platform stored images, nested-index source resolution, and related
features are deferred beyond the PoC. Application integration, Parallax cache
management, and group-writable stores also remain future work. Early stages
provide plumbing and need not expose CLI-usable features.

## Implementation stages

### 1. Store foundations and coordination

Establish plumbing without requiring a usable CLI command.

- Add the crate to the workspace with Rust edition 2024, `publish = false`, typed errors, and separate library and executable entrypoints.
- Define explicit store configuration, validated local `name:tag` references, SHA-256 digest types, and versioned store/ref metadata.
- Use reversible filesystem-safe encoding of logical refs, independent of OCI transport naming. Reject traversal components and omitted tags.
- Establish shared blobs, committed refs, raw-image directories, staging, a permanent store lock, and ref locks protected by the store-lock lifetime.
- Implement Linux advisory reader/writer locks with RAII guards, acquiring the store lock before any ref lock. Preserve lock files across ordinary removal; only exclusive prune may reclaim recognized locks for absent refs, after all holders, waiters, and transfer children have closed their ref-lock descriptors.
- Follow the design's lock matrix: import/pull/export/list/remove/check hold a shared store lock; import/pull/remove take an exclusive ref lock; export/list/check take shared ref locks; prune takes the exclusive store lock.
- This read policy extends to library availability: a shared store lock and a shared selected-ref lock remain held throughout inspection.
- Import, pull, and remove initialize stores for mutation; read operations and prune reject missing or unsupported stores. Do not add an initialization action to the seven-command API.

#### Initialization contract

Each store is one user-owned directory, for example `/scratch/user/.sarus-imagestore`. Shared blobs are shared between images within that store. The immediate parent must already exist and have been safely prepared by the site or user, including saving its creation and the directory names needed to reach it. The library assumes this preparation; existence alone cannot prove it. Group-writable stores remain future work.

Initialization must be able to open and sync the immediate parent. Creating a missing store also requires permission to write there. Reject a missing parent without creating any ancestors. Stores inside other stores are unsupported as a setup restriction; do not search ancestors to enforce it.

Use this initialization order:

1. Validate the location and any existing contents; reject unexpected contents, unsupported markers, and incompatible reserved entries before adding setup files.
2. Create only the store directory and minimum lock structure if missing. Concurrent processes must use the same directory and stable lock file, without replacing either.
3. Acquire the exclusive store lock and recheck. If another initializer has published a supported marker, use the marked-store behavior below.
4. For an unmarked store, reuse recognized empty setup directories, create missing pieces, and write `.store-init.json`. Create empty `blobs/sha256`, `refs`, `raw_images`, `tmp`, and `locks/refs`; image-specific entries belong to later operations.
5. Sync the temporary marker, lock file, internal directories from children to parents, store directory, and immediate parent before publishing `store.json`.
6. Rename the temporary marker to `store.json`, then sync the store directory before success. Retain the error reporting a potentially published marker if the final sync fails.

Always sync the immediate parent for an unmarked store, including existing empty roots and retries, even if another process created the root. This replaces the earlier rule to sync only parents known to have changed. Never sync directories above the immediate parent. Retry interrupted setup by reusing only recognized empty directories and the reserved temporary marker, while preserving the store directory and lock identity. Reject unexpected contents without deleting them.

For a marked store, import/pull/remove preparation validates it under the store lock and syncs the marker and store directory again without opening the parent or other ancestors. A marker alone is not evidence that the final save succeeded. Read operations keep the existing lock-and-validation behavior and do not initialize missing stores. Keep the store format at version 1 and the seven CLI commands unchanged.

#### Current working-tree alignment

Initialization now creates only the store directory beneath a prepared parent.
The superseded recovery records, private tree publication, recovery-required
error, and initialization-specific `RENAME_NOREPLACE` dependency are removed.
Supported version-1 stores require no private recovery record. Unexpected
contents are preserved, and obsolete development fixtures need explicit
recreation rather than migration or offline qualification.

The crate README documents the implemented ordering. Local checks cover
persistent sync failures, unprivileged parent permissions, exact sync ordering,
interrupted initialization, stable lock identities, and existing stage-2
publication behavior. Lustre/VAST qualification remains separate site work.

**Acceptance:** naming round trips; invalid paths are rejected; concurrent initialization is safe; separate processes obey the lock matrix; process termination releases abandoned locks.

**Initialization acceptance:**

- Missing parents fail without creating a directory chain; missing roots and existing empty roots initialize successfully.
- Newly initialized refs and image-content directories are empty.
- Concurrent processes initialize the same root without replacing its directory or lock identity.
- Interruption at each important step permits a retry with the same lock, including interruption after another process creates the root and after marker publication.
- Recorded sync ordering shows the immediate parent is synced before marker publication, including retries and roots created by another process. No directory above that parent is synced.
- Repeated final-sync failures remain failures; a visible marker alone never makes a retry report success.
- Marked stores work without opening their parent; unmarked stores fail clearly when parent syncing is unavailable.
- Unexpected contents, symlink substitutions, and unsupported versions are rejected without deletion.

The old ignored shared-ancestor regression has been replaced with missing-parent rejection coverage. Its directory-creation scenario is no longer supported. Existing stage-2 publication checks remain in the suite.

### 2. Immutable CAS and transactional refs

Implement publication primitives using synthetic fixtures, without depending on Skopeo. These requirements apply to publishing image content and refs after store preparation; empty-store initialization follows the separate contract in stage 1.

- Publish completed staged blobs without overwriting existing digest paths. Use same-filesystem hard-link insertion; an existing object is reused only after confirming regular-file type and expected size.
- Store each complete layout and matching metadata in a unique `raw_images/raw-<id>/` directory. Raw-image IDs identify publication instances, independently of encoded ref keys and content digests.
- Publish a logical ref through an atomically renamed relative symlink pointing to its raw image. Validate that pointers stay within the raw image area.
- Replacement switches the pointer only after the new raw image is complete. Removal unlinks the committed pointer; old raw images remain for prune.
- Use the same staging and publication primitives for imports and pulls. A failed acquisition preserves any previous committed ref; explicit replacement follows the same policy for both operations.
- Sync content and affected directories before reporting success. Distinguish failures before publication from durability failures after a namespace change, which may already be visible.
- Discover committed refs from namespace entries, including broken pointers and malformed entries.

**Acceptance:** duplicate insertion races preserve objects; failed replacement preserves the previous ref; readers observe complete raw images; fault injection exercises publication and sync boundaries.

### 3. Containers-storage transfers and initial CLI

Add library `import` and `export` operations and corresponding CLI commands.

The implemented stage-3 contract below is historical; stage 4.1 supersedes its
argument types and implicit-import-ref restrictions.

Import and export destinations are optional in both interfaces. The library uses
`Option<&LocalRef>` for import and `Option<&str>` for export. An omitted destination
uses the source argument verbatim. For import, validate that source as an explicit
local `name:tag` before store preparation or tool invocation; otherwise require an
explicit destination. The CLI reports an invalid implicit ref as usage failure
(status 2). Export defaults to the source store ref, not recorded acquisition
metadata. Explicit destinations retain their existing behavior.

- Define an application-independent transfer context carrying executable paths and optional platform selection. Inherit the caller’s existing containers-storage configuration, current driver, environment, and namespace unchanged. Provide no storage roots, driver/options, configuration, or environment overrides; do not reconstruct `storage.conf` or discover settings through `podman info`. This follows the stage-3 scope revision agreed during implementation.
- Run Skopeo directly from the selected containers-storage source into private staging. Use its resolved local source throughout the transfer and derive content metadata from the completed staged result. This is intended to avoid source-tag races. Qualify source-tag movement and source-removal behavior with real tools. It is not necessary to preserve original registry digest for images imported from Podman's containers-storage.
- Run Skopeo into private staging, with its shared-blob directory confined to that staging area. Publish completed objects through stage 2 primitives. Never let Skopeo write into the live shared CAS.
- Use OCI compression defaults: compress uncompressed layers and preserve supported existing compression. Accept representation and digest changes.
- Import one platform, defaulting to Skopeo's host selection; allow an explicit OS/architecture/variant override. Reject an unexpected index root before publication.
- Construct metadata from the completed root, manifest/config, acquisition timestamp, optional image creation time, platform, unique stored-content size, containers-storage source information, and optional notes.
- Keep the metadata schema versioned. Separate cached fields derived from OCI content from operational metadata. Record acquisition kind as import or pull for provenance; stage 6.2 requires digest preservation on every export; do not introduce alternate digest lookup keys.
- Fail when the destination exists unless replacement is explicit. Export performs the small ref validation and lets Skopeo detect transfer/content failures.
- After successful durable import publication, remove only that operation's private staging tree without following symlinks, then sync `tmp/` while both locks remain held. Return removal or cleanup-sync failures as structured cleanup warnings on a successful import report; the CLI reports them on stderr and in JSON with status 0. This cleanup is brought forward from stage 7. Failed, interrupted, or publication-durability-error operations retain staging; export creates none.
- Hold locks throughout transfers, including when the parent terminates while Skopeo remains running. Do not independently hash layers or run the full checker around copies.
- Preserve the caller’s normally selected local containers-storage and rootless context. Callers enter `podman unshare` themselves when needed. Exercise unchanged configuration/environment inheritance with real tools before considering transfers complete.
- Document a tested Skopeo version and check required capabilities; do not package a project-managed build.

Private staging is necessary because the upstream OCI destination implementation writes manifests directly to digest paths. Shared-blob options alone do not establish the wrapper's immutability guarantee. See the [OCI destination implementation](https://github.com/containers/image/blob/main/oci/layout/oci_dest.go) and [Skopeo copy documentation](https://github.com/podman-container-tools/skopeo/blob/main/docs/skopeo-copy.1.md).

**Acceptance:** real local rootless Podman round trip using unchanged caller-selected storage configuration, driver, and environment; shared-layer deduplication; failed copies; replacement; platform selection; parent termination during transfer; actionable subprocess errors; successful staging cleanup preserving CAS and unrelated staging; cleanup failure warnings without ref rollback.

### 4. Inventory, stored-root resolution, and removal

Add `list`, explicit-name `remove`, and digest selectors for `export` and single-ref `check` once checking is available.

The implemented stage-4 refinement exposes
`ImageSelector::{LocalRef(LocalRef), Digest(Sha256Digest)}`,
`Store::list`, `Store::remove`, and `Store::export_selected`, while retaining
named `export` as a wrapper. `ListReport` returns partial entries and structured
diagnostics; its completion flag describes enumeration independently of health.
Digest resolution retains the selected shared ref lock through transfer and
uses the supplied selector verbatim as the omitted export destination.

This enum, the separate named-export wrapper, and the bare-digest lookup syntax
describe completed stage 4. Stage 4.1 supersedes them with a common
selector and repository-scoped digest lookup. In particular, stage 4's exclusion
of qualified registry-provenance selectors below does not prohibit the planned
`name@stored-root` syntax; provenance remains excluded as a lookup key.

Ref metadata advances to version 3 with a required
`ContentMetadata.config_digest`, populated from the completed staged manifest.
Older development refs are rejected without migration; store format remains 1.
The OCI config digest supplies the displayed image ID, independently of the
stored-root lookup digest and the acquisition source ID.

Reusable `format_list_podman(&ListReport, bool) -> String` formatting uses
Podman's column order, abbreviated config IDs, relative creation ages and
decimal sizes. Its optional digest column displays full stored roots. The CLI
offers `list --format podman|full|json` (default `podman`) and `--digests` for
Podman formatting. Global `--json` remains an alias and conflicting choices are
usage errors. Rich output includes timestamps, platform, exact bytes, status
and paths; JSON includes all diagnostics. Damaged or incomplete inventories
print collected results and exit 1. Size remains stored-content size.

- List small ref files only, preserving damaged entries and reporting diagnostics alongside healthy entries.
- Validate layout version, root selection, metadata schema, and cached identity agreement. Healthy inventory entries remain content-unverified.
- Resolve local `name:tag` or bare `sha256:…` selectors from the authoritative root descriptors in committed OCI layouts, without a global catalog or registry lookup. Metadata never supplies alternate identities.
- Treat multiple healthy refs to the same root digest as one content identity. Choose deterministically by local ref name, under shared locks, and revalidate the selected raw image before use.
- Use a healthy matching ref even if other entries are damaged. If no healthy match exists and damage prevents a complete root scan, report incomplete resolution rather than asserting that the digest is absent.
- Do not accept qualified registry-provenance selectors or caller-supplied digest aliases. Imported images are selected only by their actual stored roots.
- Permit explicit removal of damaged refs without following their pointers or touching shared blobs.
- Index-parent inventory is deferred beyond stage 8. The PoC displays the single platform and config digest of each acquired manifest; it does not assign a host platform to an index parent.
- Add human-readable and JSON results, keeping CLI formatting outside library operations.

**Acceptance:** stored-root lookup for imported and pulled images; duplicate refs to identical roots; obsolete registry digests never resolve through metadata; damaged layouts and metadata; concurrent replacement/removal; structured diagnostics.

### 4.1. Unified selectors and digest-aware transfers (implemented)

This stage precedes stage 5. It changes image arguments and resolution,
while keeping single-platform import, storage publication, locking, and
durability guarantees. It does not implement pull, check, or prune.

#### Selector and lookup contract

- Replace the `ImageSelector` enum with a validated value for `name[:tag][@digest]`. Expose the reference component (`name[:tag]`), repository name, optional tag, and optional digest separately. Parsing and display preserve the supplied spelling.
- Require a repository name and at least one of tag or digest. Reject untagged names without digests, bare digests, image IDs, malformed suffixes, and unsupported algorithms. Continue requiring canonical full lowercase SHA-256 digests.
- Follow the upstream [named-reference grammar](https://github.com/containers/image/blob/main/docker/reference/reference.go), including registry ports, while retaining the store's path-safety rules. Preserve literal names without registry expansion, aliases, or an inferred `latest`. Bare IDs, explicit transport URLs, and embedded storage settings are outside this contract, even where external tools accept them. Ordinary repository/tag pairs such as `oci:latest` remain names and never select a transport.
- Without a digest, resolve the exact `name:tag`. With a digest, search committed refs with the same literal repository name for that stored root, ignoring any supplied tag. For example, `alpine:old@digest` may select `alpine:new` with the requested root, but never another repository. `alpine` and `docker.io/library/alpine` remain distinct names.
- Use only authoritative roots in committed OCI layouts. Choose deterministically among healthy matching refs, retain the selected shared ref lock through use, and revalidate before use. A healthy match remains usable despite other damaged entries. If no healthy match exists and damage or incomplete enumeration prevents establishing absence in the requested repository, report incomplete resolution rather than a missing digest.
- Retain `LocalRef` as the explicitly tagged storage namespace identity, extending its name parsing to support registry ports consistently. Removal continues accepting only `LocalRef` and rejects all digest-bearing selectors. This is an intentional boundary; it must remain possible to remove a damaged ref without resolving its content.

#### Transfer interfaces and import naming

Use `ImageSelector` for import/export sources and optional destinations in both
the library and CLI. `Store::import` and `Store::export` take a selector source
and an optional selector destination. Consolidate `export_selected` into
`export` and migrate callers. These strict arguments replace permissive
containers-storage names and image IDs. Removal retains its `LocalRef` API.

For import, default an omitted destination to the source selector, then derive
the committed tagged ref:

- Keep an explicit tag in the effective destination. Otherwise generate its tag from the first 12 hexadecimal characters of the explicit **source digest**, excluding `sha256:`.
- Reject an explicit tagless destination when the source has no digest, before store preparation or tool invocation. Do not resolve a source tag merely to generate a destination tag.
- Ignore digests in explicit import destinations. They cannot be enforced, do not supply generated tags, and do not create lookup aliases. Communicate this in the library report and CLI stderr; explain it in relevant validation errors. An ignored-digest notice does not itself make a successful import fail.
- Pass a digest-bearing source to Skopeo as `name@digest`, removing any tag from the source-selection argument. Keep the existing image-ID pinning for sources selected by tag. Retain acquisition metadata, including source image-ID reporting, without discarding the requested source digest from Skopeo's argument.
- Preserve existing conversion and compression behavior. The completed OCI root is the stored digest identity, which may differ from the source digest. Report the actual committed ref and stored root; do not introduce source-digest aliases.

The following examples use `D` and `E` as placeholders for full SHA-256 digests:

| Source | Explicit destination | Committed ref |
| --- | --- | --- |
| `alpine@D` | omitted | `alpine:<first12(D)>` |
| `alpine@D` | `renamed@E` | `renamed:<first12(D)>`; ignore `E` |
| `alpine@D` | `renamed:release@E` | `renamed:release`; ignore `E` |
| `alpine:latest` | `renamed@E` | Reject; require a destination tag |

If the first example produces stored root `R`, digest lookup subsequently uses
`alpine@R`, while its committed ref remains `alpine:<first12(D)>`. Generated tags
are naming conveniences, not unique digest identifiers. Collisions follow the
ordinary destination-conflict rule: fail unless replacement is explicit;
never generate an alternative tag.

#### Export and later-stage alignment

- Default an omitted export destination to the supplied source selector, preserving its spelling rather than using acquisition metadata or the ref selected during lookup.
- Pass export destination digests through to Skopeo and report digest-mismatch failures; never silently discard them. Follow the upstream [destination digest check](https://github.com/containers/image/blob/main/storage/storage_dest.go). Stage 6.2 requires preservation of the stored manifest on every export.
- Use this selector for stage-5 single-image checking. Align stage-6.1 pull with the same selector, optional destination, and source-derived fallback naming rules, while retaining digest-preserving registry acquisition. Pull implementation remains in stage 6.1.
- Keep store format 1 and ref metadata schema 3; no storage migration is required. The README, CLI help, library examples, and implementation status now describe the verified stage-4.1 interfaces.

**Acceptance:**

- Parse and display tagged, digested, combined, and registry-port references without normalization; reject malformed and newly unsupported inputs, including bare IDs and digests.
- Cover repository-scoped lookup, ignored source tags when a digest is present, distinct literal names, deterministic duplicate-root selection, damaged or incomplete scans, and concurrent replacement/removal with lock retention.
- Exercise all import naming examples above, explicit tags with and without source digests, ignored-destination-digest notices, generated-tag collisions, replacement, and source-to-stored digest changes without aliases.
- Verify Skopeo receives `name@digest` for digest-bearing imports and pinned image IDs for tag-selected imports. Cover export defaults, combined selectors, and destination-digest mismatch failures.
- Verify CLI/library parity, validation before store preparation or tool invocation, and removal's rejection of digest-bearing selectors.
- Extend real Podman/Skopeo round trips in disposable storage to cover digest-selected imports, source-derived tags, and qualified stored-root exports. Run the existing crate check, full test suite, Clippy with warnings denied, formatting check, and diff check; record tool versions and results without claiming site qualification.

### 5. Read-only graph checking (implemented)

Add `check <selector>` and `check --all`, with graph-reading logic reusable by prune.
Single-image checks use the stage-4.1 `ImageSelector` and repository-scoped
stored-root resolution, including its selected-ref lock and diagnostics.

- Traverse OCI manifests and complete OCI indexes, including nested indexes and every referenced manifest, config, and layer. Validate descriptor structure, supported media types, local blob existence, and descriptor sizes.
- Require layers locally; do not fetch external URLs.
- Report unknown schemas and unsupported graph content as unresolved errors; supported OCI indexes are valid roots.
- Detect cycles and inconsistent descriptors. Cache shared-object observations within one operation while still validating each descriptor's expected size.
- Report errors without repair, deletion, layer hashing, or tar extraction.
- Continue checking other committed refs when an individual ref is damaged, but return failure if the requested scope contains errors or cannot be checked completely.

**Acceptance:** named and repository-qualified digest selection; complete multi-platform and nested-index fixtures; missing children and blobs; size mismatches; malformed JSON; unsupported roots; cycles and conflicting descriptors; shared objects; all-ref checks continuing after individual failures. Document that same-size byte corruption may remain undetected.

Preserve this implemented graph traversal and its tests. Internal index-reading
capability does not imply supported index acquisition, parent metadata, or export.

### 6. Registry acquisition, digest-preserving export, and image availability

Implement the three sub-stages below in order. Each acquired ref selects one
platform-specific OCI image manifest. Complete-index storage and index export
are explicit PoC limitations, deferred beyond stage 8.

#### 6.1. Add registry pull in the library and CLI (implemented)

Add library `pull` and its CLI command. Pull does not populate containers-storage
and requires Skopeo, not Podman.

- Expose `pull <source-selector> [destination-selector]` using the common `ImageSelector` in both library and CLI, with explicit replacement and optional notes. Default the destination to the supplied source selector. Reuse stage 4.1's source-derived fallback tags, ignored explicit destination digests and notices, and early validation; a source selected by tag cannot supply a generated destination tag.
- Keep registry pull configuration separate from containers-storage settings. Add optional `Platform` selection to the registry pull context and reuse CLI `--platform OS/ARCH[/VARIANT]`, following the library policy below.
- Resolve sources before layer transfer with `skopeo inspect --raw`, using consistent registry configuration across inspection and copying. Parse and validate the raw object's schema and media type. Normal `skopeo inspect` reports the top-level digest; its `Digest` field alone cannot identify a selected child manifest.
- For a digest-bearing source, ignore any supplied source tag and inspect exactly `repository@digest`. Require a supported image manifest and verify its SHA-256 identity from the original response bytes. Reject indexes and manifest lists, including single-entry indexes, without selecting a child or falling back to a tag.
- For a tag-only source, inspect the tag once. If it identifies a supported image manifest, calculate its SHA-256 digest from the exact raw bytes, without reserializing JSON. If it identifies an OCI index or Docker manifest list, select one supported image-manifest descriptor using the policy below. Inspect that child by digest and verify its type, digest, and descriptor size before copying. Never resolve the mutable tag again.
- Reject malformed objects, unsupported schemas or representations, and resolution requiring nested-index traversal. Reading a Docker manifest-list envelope does not add support for storing Docker-specific image representations. Do not retain the source index or acquire its other platforms.
- Copy only from the resolved `repository@manifest-digest`, using private staging and `--preserve-digests`, without `--all`, forced conversion, recompression, or fallback to another digest. If the OCI destination cannot preserve the representation, fail without publishing a ref.
- Require the completed layout to select exactly one supported image manifest whose digest equals the resolved manifest digest. When a platform is explicit, check its completed configuration as specified below before publication. Rely on Skopeo's transfer validation rather than invoking the full checker or rehashing layers.
- Reuse import's immutable CAS insertion, destination-ref policy, locks, sync ordering, atomic publication, and best-effort successful-staging cleanup, including protection while an orphaned Skopeo process is still running.
- Record the requested registry reference separately from the resolved platform-specific manifest digest. Keep `resolved_digest == stored_root`; provenance and index digests never become lookup aliases. Retain store format 1 and ref metadata schema 3.
- Derive config digest, actual platform, optional image creation time, and unique stored-content size from the completed manifest, configuration, and layers, without a separate full validation pass. Metadata describes one image, not an index parent.
- Delegate registry authentication and trust to Skopeo, with consistent configuration for resolution and copying. Support its authentication-file, certificate, trust-policy, and environment configuration without storing credentials in ref metadata.

**Library platform policy:**

- An explicit `Platform` supplies OS, architecture, and optional variant. For index selection without an explicit platform, default to `linux/amd64` for an `x86_64` library target and `linux/arm64` for an `aarch64` target. Other targets require an explicit platform for index selection. Perform no CPU-variant detection.
- Match descriptor OS and architecture. Require the variant when one is supplied; otherwise do not filter by variant. Require one distinct matching manifest digest. Consistent duplicate descriptors may collapse to one candidate. No match or multiple distinct matches fails with actionable diagnostics; descriptor order never breaks a tie.
- Direct-manifest sources need no host selection. Without an explicit platform, accept their actual platform. With an explicit platform, require the completed configuration's OS, architecture, and any supplied variant to match; missing or mismatching required values fail before publication.
- Document that these are library defaults and can differ from Skopeo's platform preferences and compatibility fallbacks. Repository-wide architecture enforcement remains outside scope.

The tool interfaces are described in [Skopeo copy documentation](https://github.com/podman-container-tools/skopeo/blob/main/docs/skopeo-copy.1.md) and [Skopeo inspect documentation](https://github.com/podman-container-tools/skopeo/blob/main/docs/skopeo-inspect.1.md). Source resolution hashes small raw JSON objects; it does not introduce layer hashing or another full graph check.

**Acceptance:**

- Direct-manifest tags, index tags, and explicit manifest digests; reject index and manifest-list digests, including single-entry indexes and combined tag/digest selectors. Unsupported nested-index resolution fails clearly.
- Exact raw-byte digest calculation, child type/digest/size verification, pre-transfer manifest pinning, tag movement during resolution and copy, and completed-root mismatch.
- Library defaults, explicit platforms, variants, consistent duplicates, ambiguous candidates, missing platforms, and completed-configuration mismatches, including direct-manifest sources.
- Common selectors, optional destinations, source-derived fallback tags, ignored-destination-digest notices, CLI/library parity, provenance, inventory, checking, and stored-root equality.
- Authentication and incompatible-representation failures, preservation failures, interrupted pulls preserving prior refs, shared-content insertion races, and cleanup. Include real registry acquisition of a selected single-platform manifest; retain existing import/export regression coverage.

#### 6.2. Extend and qualify digest-preserving export (implemented)

Extend export preservation to all stored single-manifest images, including
those acquired by import. Qualify the real registry-to-store-to-containers-storage
round trip using the single opt-in Docker Hub acquisition. Import may change
representation during acquisition; do not add imported-image round-trip identity
validation or runtime post-export digest inspection.

- Require `--preserve-digests` and its tool capability for every export, regardless of acquisition kind. Report failure if containers-storage cannot honor it, without retrying under a weaker policy. Existing imported refs receive this stricter behavior without migration; import acquisition itself still permits representation changes.
- Retain repository-scoped stored-root selection, optional destinations, destination-digest enforcement, and stored-manifest digest reporting. No parent-index or selected-child digest reporting is needed.
- Continue rejecting stored index roots. Export cannot select a different architecture from one stored manifest.
- Leave persistent refs and metadata unchanged during export. Partial destination data remains managed by containers-storage.
- Keep containers-storage image IDs distinct from stored manifest digests. A later import still cannot promise reconstruction of the original registry representation.

**Acceptance:** real registry-to-store-to-containers-storage manifest-byte and digest preservation from one opt-in acquisition; universal export preservation and capability/failure handling for imported and genuinely pulled refs; export by name and qualified manifest digest; omitted and explicit destinations; matching and mismatching destination digests; no fallback copies; persistent-store snapshots unchanged by successful or failed export; stored-index rejection; existing import/export and lock regressions. Add no imported-image round-trip digest assertions.

#### 6.3. Library image availability for acquisition decisions

Add a library-only query that helps callers decide whether to reuse stored
content or attempt pull/import. Expose this method on `Store`:

```rust
pub fn availability(
    &self,
    selector: &ImageSelector,
) -> Result<ImageAvailability>;
```

The target outcomes and caller policy are:

| Outcome | Meaning and caller action |
| --- | --- |
| `Ready` | The selected image passes the same structural validation as `check`; reuse it. |
| `Missing` | The image is definitively absent from a valid initialized store; attempt pull or import. |
| `Unusable { diagnostics }` | The selected image has detected damage or unsupported content; the caller chooses whether to report it or attempt explicit replacement. |
| `Err` | Store preparation, selection, or inspection could not establish an outcome; propagate or retry the error. |

- Provide structured diagnostics that distinguish damage from unsupported content without parsing messages. Preserve operation/path, ref, and object context where available, and support multiple findings. Permission failures, lock timeouts, and other operational inspection failures remain errors rather than evidence of absence or damage.
- Reuse authoritative ref inspection and graph validation. Preserve typed causes before formatting diagnostics; do not derive availability solely from `CheckReport::has_errors()` or message strings. Preserve existing `check` behavior, reports, CLI output, and exit statuses.
- Retain literal repository-scoped stored-root selection, digest precedence over a supplied tag, and deterministic matching-ref selection. Report incomplete digest resolution as an error, including when damaged candidates prevent establishing a match; never infer `Missing` from an incomplete search.
- Return an error for a missing or uninitialized store and never initialize it. Preserve image data, refs, metadata, staging, and stable lock identities; permit only the existing coordination-file creation performed by read operations.
- Hold the shared store lock and shared selected-ref lock throughout inspection, in the existing lock order. Results describe the image when inspected and do not reserve it. Pull/import must retain destination checks under their own locks and require explicit replacement of an existing ref.
- Invoke no external tools or registry requests. Availability concerns local stored content, not remote-tag freshness or acquisition provenance. Do not automatically run availability around transfers.
- Retain complete supported graph validation and the checker's limits: no blob hashing, decompression, or unpacking. `Ready` does not certify execution or expand index acquisition/export support.
- Keep acquisition and recovery policy in callers. Unsupported content may remain unsupported after reacquisition, and explicit ref replacement cannot generally repair damaged immutable CAS objects. Add no automatic acquisition, repair, replacement, or removal.
- Add no CLI command, flag, output format, or exit-status mapping for availability. Keep store format 1 and ref metadata schema 3 unchanged.

**Acceptance:**

- Cover `Ready`, definite named and digest misses, damaged wrappers and dependencies, unsupported content, and multiple findings with typed diagnostic classification.
- Verify missing/uninitialized-store errors, permissions and other inspection failures, lock timeouts, incomplete digest resolution, selector parity, digest precedence, literal repository boundaries, and deterministic matching-ref selection.
- Verify shared-lock retention during inspection and concurrent replacement/removal using readiness handshakes and bounded waits. Show that later acquisition still performs its own destination checks.
- Verify content preservation and the absence of external-tool calls. Retain all existing checker and graph regressions, including complete internal index traversal without expanding public index workflows.
- Document library usage for reuse, acquisition after a definite miss, caller-controlled handling of unusable content, and propagation of operational errors. Run the crate-scoped checks when implementing this substage; this planning revision does not claim new implementation validation.

### 7. Conservative pruning (implemented)

Add `prune`, using the checker's authoritative graph traversal, including any
shared inspection changes made in stage 6.3.

- Hold the exclusive store lock for enumeration, reachability analysis, and deletion.
- Both modes require an initialized store, create no ref coordination files, and never resume initialization.
- Before destructive cleanup with eligible candidates, sync all scanned ref directories from children to parents through `refs/`, including empty and intermediate encoded-name directories. This persists visible removals and replacements before reclaiming their retired content. A namespace sync failure blocks all deletion and reports the failing path with zero eligible and removed totals. Dry-run skips this barrier and cannot certify it will succeed.
- Preserve an entire cleanup directory if any contents are unrecognized; report unknown entries as successful skips.
- Stop at the first removal or sync failure, retain partial progress, and report visible removals separately from confirmed durability.
- Complete the scan before deletion. Reuse stage 5's authoritative reader, including its existing index traversal: any encountered index must be traversed completely or block destructive pruning. Never treat deferred public index support as permission to ignore dependencies. Any unresolved committed-ref error or incomplete enumeration blocks destructive pruning.
- Reclaim recognized unreferenced CAS objects, abandoned import/pull staging and cleanup remnants, and unreferenced raw images. Successful operations normally clean their own staging immediately; stage 7 handles what remains. Preserve active raw images, locks for committed refs, the permanent store lock, broken committed refs, and unknown files.
- Reclaim unused ref bookkeeping immediately, independently of content grace: empty single-link regular ref locks for absent refs, then empty lock and ref directories from children to parents. Keep both roots. Preserve malformed lock identities and lock subtrees containing unknown contents; operational inspection failures block all cleanup. Every ref-lock holder, waiter, and transfer child retains its shared store lock until the ref descriptors close.
- Report bookkeeping as `ref_lock`, `ref_lock_directory`, and `ref_directory` candidates and totals, with zero logical bytes. These add public `PruneKind` variants without changing the store or metadata versions.
- Apply a default 24-hour grace period, with an explicit duration override including zero. Use blob modification time and staging/raw-image directory modification time; preserve blob age on reuse. Document that this measures object age, not time since removal.
- Provide dry-run reports with eligible counts and bytes; report scan and deletion failures clearly.
- Do not use cached image sizes or past check results as a substitute for the current reachability scan. Report actual candidate bytes rather than estimating reclaimed space from removed refs.
- Perform a fresh authoritative scan even after availability queries. Never use availability outcomes or prior reports to omit committed refs, infer an empty dependency set, or authorize deletion. An unusable committed ref still blocks destructive pruning.

**Acceptance:** immediate bookkeeping cleanup preserves committed locks and shared prefixes; actual holders, waiters, and surviving transfer children exclude cleanup; recreated locks enforce mutual exclusion; unknown lock subtrees remain; failed removal/replacement syncs and interrupted publishers require successful ref-namespace syncing before reclamation; failures at empty leaf, intermediate, or root directories block all deletion; dry-run skips the barrier; shared layers survive; internal index fixtures retain every reachable dependency or block deletion on incomplete traversal; missing children, unsupported graphs, inconsistent descriptors, and damaged refs block deletion; dry-run changes nothing; grace boundaries; abandoned import/pull cleanup; unknown files remain; active imports, pulls, exports, and availability inspections block prune. Prior availability queries leave pruning decisions unchanged, including refusal to delete when unusable refs remain. Use readiness handshakes and bounded waits to verify exclusion. These safety fixtures do not qualify public index acquisition or export.

### 8. Qualification and developer documentation

Complete local verification of the first PoC and prepare external filesystem
acceptance procedures. Stage 8 is the PoC completion point; complete-index and
multi-platform stored-image workflows are not acceptance requirements.

- Run crate checks, formatting checks, unit tests, subprocess concurrency tests, and opt-in real Podman/Skopeo integration tests.
- Add registry integration tests using a disposable test registry, with direct manifests, indexes for tag selection and digest-source rejection, mutable tags, and authentication fixtures. Exercise single-platform import/pull, library availability, digest-preserving export, checking, and pruning together. Complete-index round trips and nested-index acquisition qualification remain deferred.
- Verify availability before and after acquisition and removal, incomplete digest resolution, unusable content, and operational failures. Cover concurrent ref changes and acquisition races with readiness handshakes and bounded waits; verify that availability never bypasses destination-conflict or explicit-replacement checks.
- Provide a two-node test procedure for Lustre and VAST covering concurrent initialization of the same store beneath a prepared parent, interrupted initialization and retry, lock exclusion, concurrent imports and pulls, shared single-platform content, publication visibility, interrupted transfers, replacement/removal, and pruning. Include availability observations across publication, replacement, and removal, and exclusion between active availability inspections and pruning.
- Document sync ordering and distinguish locally tested behavior from filesystem durability guarantees requiring site validation.
- Document CLI examples, library usage, temporary-space costs, requested-source versus stored-identity semantics, supported digest-preserving registry representations, and external-tool versions. Explain library platform defaults, explicit platform checks, manifest-only digest sources, and preservation failures without conversion.
- Add library availability examples for reuse, acquisition after a definite miss, caller-controlled handling of unusable content, and propagation of operational errors. Explain structural-validation limits, local-only observations, acquisition races, explicit replacement, and CAS repair limitations. Availability stays library-only; application integration and a CLI predicate remain outside scope.
- Review summaries, stage references, API contracts, assumptions, and future-work statements for consistency. Explicitly document the lack of multi-platform stored images and index acquisition/export, while distinguishing index-based tag selection and internal graph traversal. Preserve historical validation records.

**Acceptance:** local checks pass, including the availability and pruning interactions above; library examples cover every availability outcome without implying automatic recovery; external procedures are reproducible and include availability locking and visibility. Lustre/VAST support remains explicitly unqualified until site tests are completed. These filesystems cannot be validated in the current development environment.

## Deferred beyond the first PoC

The following work is outside stages 1-8 planned in this document: complete-index
acquisition and storage; multi-platform stored images; index-parent metadata and
inventory; platform-selecting export from stored indexes; nested-index source
resolution; complete-index round trips and related qualification. Existing
stage-5 graph traversal and tests remain in place as storage safety foundations,
without promising these public workflows. Repository-wide architecture
enforcement is also outside the PoC; architecture-specific repository names
remain an operational convention.

## Library and CLI contract

This section describes the target contract after the relevant stages. Stage
4.1 implements the common selectors and containers-storage transfer arguments;
stage 6.1 implements pull and stage 6.2 requires universal export preservation
and qualifies the registry round trip.
Stage 6.3 implements library-only availability. Stage 7 implements pruning,
while stage 5 implements checking. Earlier interfaces and their historical
validation are recorded above.

The library exposes a store handle, a common `ImageSelector`, tagged `LocalRef` namespace identities, separate containers-storage transfer and registry pull options, operation reports, and typed failures. Its target operations are `import`, `pull`, `export`, `list`, `remove`, `check`, `prune`, and the library-only `availability` query. The CLI retains its seven commands and gains no availability command or flags. Storage semantics and diagnostics belong to the library; argument parsing and presentation belong to the executable.

Availability accepts the same `ImageSelector` as single-image checking and
returns `Result<ImageAvailability>` with `Ready`, `Missing`, or
`Unusable { diagnostics }` as specified in stage 6.3. A missing/uninitialized
store, incomplete resolution, or operational inspection failure returns an
error. Callers decide whether to acquire or explicitly replace; the query does
not perform those actions or reserve the observed ref.

The executable uses the same name as the crate and requires `--store PATH`. Import, export, pull, and single-image check identify images with `name[:tag][@digest]`, requiring a name and at least one of tag or digest. Import, export, and pull accept optional destinations of the same type. Bare digests, image IDs, and implicit `latest` are unsupported. Removal alone requires an explicit `name:tag` and rejects digest-bearing selectors. Names, including registry ports, are preserved without normalization. Stored-root selection is confined to refs with the same literal repository name, ignores any supplied tag when a digest is present, and never triggers registry resolution. Multiple healthy matching refs resolve deterministically.

Import and pull default an omitted destination to the supplied source selector,
then keep the effective destination tag or generate one from the first 12
hexadecimal characters of the explicit source digest. An explicit tagless
destination without a source digest is invalid before preparation or tool
invocation. Digests in explicit import/pull destinations are ignored and reported;
they neither supply generated tags nor assert stored roots. Export defaults its
destination to the supplied source selector verbatim and passes destination
digests through to Skopeo for validation. Import's source digest may differ from
the completed stored root; only that actual root serves as the store digest
identity.

Import accepts explicit replacement, optional platform selection, and optional
notes. Pull accepts a registry source by tag or manifest digest, explicit
replacement, optional notes, registry configuration, and optional platform
selection using stage 6.1's library defaults and verification policy. It accepts
no caller-supplied provenance aliases. Export requires preservation of the stored
manifest for every acquisition kind, reports that digest, and rejects stored
indexes; it cannot choose another architecture from one manifest. Prune accepts dry-run and a grace-period override as integer seconds with an optional `s`, `m`, `h`, or `d` suffix, including zero. Both modes require an initialized store.

Metadata records the acquisition kind and a cached stored root checked against
the OCI layout. Pull additionally records the requested registry reference and
resolved platform-specific manifest digest, which must equal that stored root.
Index-source digests are rejected, and an index used for tag selection never
becomes a retained identity or lookup alias. Imported-image registry digest
collections and alternate lookup keys are absent. Store format 1 and ref
metadata schema 3 remain unchanged.

Diagnostics go to stderr; JSON operation results go to stdout. Exit statuses distinguish success, operation/check failure, and invalid CLI usage. Listing returns collected entries and diagnostics even when some refs are damaged, and reports incomplete or damaged inventory as failure.

The stored-content size is the sum of unique manifest, config, and layer object
sizes in the acquired image, excluding wrapper metadata. Registry indexes used
only for selection are not stored or counted. This size is neither Podman's
unpacked size nor an estimate of what removal will reclaim; ref sizes cannot be
summed to calculate total store usage.

## Assumptions and boundaries

- Linux, user-owned stores, concurrent processes across nodes, single-platform containers-storage imports and registry pulls, and SHA-256 content. Stage 8 completes the first PoC; multi-platform stored images and complete-index storage remain unsupported beyond that completion point.
- The site's or user's prepared parent directory already exists and can be opened and synced during initialization. The library creates no missing ancestors, supports no stores inside other stores, and does not inspect ancestors to prove preparation or detect nesting.
- All stored-content selection and reachability derive from committed OCI layouts. Pull preserves the resolved platform-specific registry manifest; import may convert or recompress and therefore cannot promise that identity. Pull provenance is descriptive and must agree with the stored root, never act as an alias.
- Stage 4.1's selector and transfer contracts are implemented before stage 5. They preserve literal names, use only qualified stored-root digests for lookup, and retain explicit tagged-ref-only removal. Generated tags use an explicit source digest; ignored acquisition-destination digests never become identities or naming inputs. Store format 1 and ref metadata schema 3 remain unchanged, with no stage-4.1 migration.
- Tag pulls may resolve a manifest through a registry index, but retain only that manifest and its dependencies. Digest pulls require the manifest itself. Library platform defaults may differ from Skopeo; explicit platform selection requires a matching completed configuration. Stored indexes cannot be exported.
- Successful mutations sync before returning; post-publication sync errors explicitly report potentially committed state.
- File locks, hard-link insertion, atomic rename, publication visibility, and sync behavior require validation on the actual Lustre and VAST deployments. Do not silently downgrade these guarantees if a primitive is unavailable.
- No hosted registry service, registry push action, automatic repair, deep checking, global catalog, or migration of existing Parallax stores.
- Availability is a library-only observation with the same structural-validation limits as checking. It neither initializes missing stores nor performs automatic acquisition or recovery. A CLI predicate and application integration remain outside scope; storage format and metadata schema are unchanged.
- The crate is development work with initialization aligned to design version 1.3 and locally verified; no production metadata migration is planned. Recreate obsolete development fixtures rather than introducing migration or offline qualification machinery, and never automatically delete unexpected contents.
- `sarusctl` and Skybox remain unchanged during these stages; their later integration uses the library through workspace path dependencies.
- Backups or filesystem snapshots remain necessary to recover removed logical refs and operational metadata; the CAS alone cannot reconstruct their names and provenance.
