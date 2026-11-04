# User guide

The `hpc-image-store` CLI operates on a persistent user-owned image store selected
with `--store PATH`. The [administration guide](administration.md) covers building,
installation, and preparing the store's immediate parent. The
[architecture](architecture.md) defines the storage and failure-handling contracts;
the [library guide](library-guide.md) covers Rust callers.

## Tools and storage context

Import requires local Podman and Skopeo. Export and registry pull require Skopeo
only; inventory, checking, existence, and removal invoke neither tool. Import and
export inherit your local containers-storage configuration, environment, driver,
and user namespace. The library does not change that configuration or select
another driver. Remote Podman contexts are unsupported.

Run directly in your normal local context. If the tools report that an existing
rootless user namespace is required, use the same context through this fallback:

```sh
podman unshare hpc-image-store --store /scratch/me/images import localhost/tool:latest analysis/tool:release
```

`--skopeo PATH` and `--podman PATH` select executables. Skopeo must support the
required shared-blob options, platform flags when selected, and digest
preservation for registry pull and every export.

## Import and export

Import copies a local image into the persistent store. A ref such as
`analysis/tool:release` is the name and tag used to select it. Export restores
the selected persistent image into local containers-storage.

```sh
hpc-image-store --store /scratch/me/images import localhost/tool:latest analysis/tool:release
hpc-image-store --store /scratch/me/images import localhost/tool:latest analysis/tool:release --replace --notes 'updated build'
hpc-image-store --store /scratch/me/images --platform linux/arm64/v8 import localhost/tool:latest analysis/tool:arm64
hpc-image-store --store /scratch/me/images export analysis/tool:release localhost/restored:latest
hpc-image-store --store /scratch/me/images --json export analysis/tool:release
```

An omitted destination defaults to the source selector. Import and pull can
initialize a missing or empty store beneath its prepared parent. Export requires
an initialized store. An existing destination, including a damaged ref, requires
`--replace`; there is no automatic repair or replacement. Notes are optional
acquisition metadata.

Import may compress or convert an image, changing its manifest digest. The
stored root is its subsequent local digest identity. Every export requests
preservation of that stored manifest, even for imported images. Unsupported
capabilities or preservation failures are errors without fallback. Export
leaves persistent image data and metadata unchanged; partial destination data
is managed by containers-storage.

`--platform OS/ARCH[/VARIANT]` selects a platform without converting architecture.
Import/export use Skopeo defaults when it is omitted. A store path containing
`:` cannot be used for transfers because Skopeo's OCI transport cannot address it.

Successful import, including `--replace`, durably removes its staging and unused
superseded content before reporting success. Replacement preserves shared blobs
and requires the old ref to have a canonical confined
pointer and readable authoritative dependency declarations. A malformed pointer,
regular-file ref, missing wrapper, or unreadable/unsupported declarations cause
an error before publication, preserving the old entry and content. Damaged cached
metadata alone does not prevent reclamation. `--replace` is not a repair command.

## Names and digests

Import, pull, export, exists, and single-image check use `name[:tag][@digest]`.
Supply a repository name and at least a tag or digest. Registry ports work. Names retain
their spelling: `alpine` and `docker.io/library/alpine` are different repositories.
No `latest` tag or registry prefix is inferred. Bare digests, image IDs,
transport URLs, and embedded storage settings are rejected. Digests require
`sha256:` followed by 64 lowercase hexadecimal digits.

For a local lookup, a digest overrides any tag and searches committed refs in
the same literal repository. `analysis/tool:old@sha256:<R>` can select another
tag of `analysis/tool`, but never a different repository. Use the actual stored
root shown by inventory, not a config digest or original acquisition digest.
In the examples below, replace placeholders with complete digests.

```sh
hpc-image-store --store /scratch/me/images export 'analysis/tool@sha256:<stored-root>' localhost/restored:latest
hpc-image-store --store /scratch/me/images check 'analysis/tool@sha256:<stored-root>'
```

For import/pull naming, keep the destination tag if present. Otherwise derive a
tag from the first 12 hexadecimal characters of the explicit source digest.
A source tag resolved during pull cannot supply that generated tag. Explicit
acquisition destination digests are ignored and reported; they do not assert
the resulting root or supply a tag.

| Acquisition input | Committed name |
| --- | --- |
| `alpine@sha256:<D>`, destination omitted | `alpine:<first12(D)>` |
| Same source, destination `renamed@sha256:<E>` | `renamed:<first12(D)>`; `E` is ignored |
| Same source, destination `renamed:release@sha256:<E>` | `renamed:release`; `E` is ignored |
| `alpine:latest`, destination `renamed@sha256:<E>` | Invalid: the source supplies no digest for a tag |

Generated-tag collisions fail unless replacement is explicit. If import changes
source root `D` into stored root `R`, the tag still derives from `D` and later
digest lookup uses `R`. Export instead passes destination digests to Skopeo for
enforcement, and defaults to the exact supplied source selector.

## Pull from a registry

```sh
hpc-image-store --store /scratch/me/images pull docker.io/library/alpine:3.23
hpc-image-store --store /scratch/me/images --platform linux/arm64/v8 pull registry.example/tool:release analysis/tool:arm64 --notes 'job image'
hpc-image-store --store /scratch/me/images pull registry.example/tool:release --authfile /path/to/auth.json --cert-dir /path/to/certs --policy /path/to/policy.json
```

Pull stores one OCI manifest without populating containers-storage or invoking
Podman. Use export afterward for job-local use. `--authfile`, `--cert-dir`,
`--policy`, and `--tls-verify[=true|false]` apply to both source resolution and
copying. Omitted settings inherit Skopeo configuration and environment, including
`REGISTRY_AUTH_FILE`. Skopeo reads credentials; they are never saved in ref metadata.

A digest source must identify the image manifest itself. Index and manifest-list
digests are rejected even if they contain one image. A tag may resolve through
an OCI index or Docker manifest list to one manifest. Nested-index resolution
and Docker-specific stored image media types are unsupported. The selected
manifest is pinned by digest for copying, so later tag movement does not change
the acquisition. Its digest is preserved without conversion or weaker retries.

| Source | Platform behavior |
| --- | --- |
| Index tag without `--platform` | `linux/amd64` on an `x86_64` library target or `linux/arm64` on `aarch64`; other targets need an explicit platform |
| Index tag with `--platform` | Match OS and architecture, filtering variant only when supplied; require one distinct matching digest |
| Direct manifest without `--platform` | Accept its actual platform |
| Any source with `--platform` | Verify the completed image configuration matches the requested fields |

Multiple distinct matches or no match fail; descriptor order never breaks ties.
CPU variants are not detected. These defaults can differ from Skopeo defaults.
An index used for selection is neither stored nor retained as a lookup alias.
If a pull uses a different destination repository, subsequent local lookup uses
that destination's literal repository name.

## Inventory and checking

```sh
hpc-image-store --store /scratch/me/images list
hpc-image-store --store /scratch/me/images list --digests
hpc-image-store --store /scratch/me/images list --format full
hpc-image-store --store /scratch/me/images list --format json
hpc-image-store --store /scratch/me/images check analysis/tool:release
hpc-image-store --store /scratch/me/images --json check --all
```

The default inventory follows `podman images`: repository, tag, shortened OCI
config image ID, creation age, and stored content size. `--digests` adds the
full stored root. `--format full` exposes metadata, paths, status, and full
identities; JSON includes entries, diagnostics, and enumeration completeness.
Global `--json` selects JSON and conflicts with explicit non-JSON formatting.
`--digests` requires Podman formatting.

Rows are ordered newest first, with name/path tie-breaking. Sizes use decimal
units and three significant digits; IDs are shortened to 12 hexadecimal digits.

Creation time and size are cached display fields. Stored content size differs
from unpacked Podman size and cannot predict reclaimed space. Missing display
values appear as `<none>`. For an exported image already present under the same
config image ID, `podman images` may retain the existing display values; use
`podman image inspect` to inspect added names in `RepoTags`, `RepoDigests`, and
`NamesHistory`.

Inventory inspects small wrapper files without traversing image content.
Healthy entries are therefore `unverified`. Damaged entries remain visible,
and collected results are printed even when damage or incomplete enumeration
causes status 1. Non-UTF-8 paths have quoted byte-escaped representations.

Check accepts one selector or `--all` and traverses the supported local graph,
including encountered nested indexes and all their dependencies. It verifies
structure, supported types, file presence, and sizes. It does not hash blobs,
unpack layers, compare cached display fields against content, or certify execution.
Same-size corruption can remain undetected. Passing internal index checks does
not imply index acquisition or export support. Check never repairs or removes
content and creates no coordination bookkeeping. All-store inventory and checks
hold a shared store lock throughout inspection and observe a stable namespace.
Import, pull, and remove take the exclusive store lock and wait for
readers; writers targeting different names also serialize.

## Test local existence

```sh
hpc-image-store --store /scratch/me/images exists analysis/tool:release
hpc-image-store --store /scratch/me/images exists 'analysis/tool@sha256:<stored-root>'
```

`exists` emits no result text or JSON, even with global `--json`. Its exit status
is 0 for present, 1 for definitely absent, 125 for an operational error, and 2
for invalid usage. Operational errors print contextual diagnostics to stderr;
lock-wait warnings may also appear. Help retains normal parser behavior.

A tagged name counts when its terminal entry exists, even if the pointer is
dangling or malformed. An encoded directory used only by a longer name does not
count. A repository-qualified digest compares authoritative stored roots and
ignores any supplied tag. Lookup retains literal repository spelling and does
not use config IDs, cached metadata, provenance aliases, or remote freshness.
Incomplete relevant discovery or unreadable candidate indexes are errors when
no definite match can be established.

The store must already be initialized. Pending recorded recovery runs first and
can perform cleanup or block the query. Clean lookup reads no blobs and performs
no graph validation or tool invocation. Presence reserves nothing for a later
operation and does not establish image health; request `check` explicitly.

## Remove refs and reclaim content

```sh
hpc-image-store --store /scratch/me/images remove analysis/tool:release
```

Removal accepts only an explicit `name:tag`; digest-bearing selectors are
rejected. It requires an initialized store and never creates a missing root,
resumes setup, or creates missing ancestors. A missing ref is an error.

Successful removal durably removes that name, its unused raw image and blobs,
and unnecessary namespace directories before reporting success. Shared content
and prefixes needed by longer names survive. No later maintenance action is needed.
The target must have a canonical confined pointer, a readable wrapper index,
and supported readable dependency declarations. Damage to those inputs rejects
removal before unlinking; damaged display metadata alone does not block it.
Removal is not a way to repair or discard an otherwise unreadable ref.

An error can occur after the name is removed. In particular, unreadable
dependencies in another ref can block safe cleanup and then prevent all ordinary
store operations. See [recovery failures and manual resolution](#recovery-failures-and-manual-resolution)
before retrying; a retry may finish recovery and then report the original name
missing rather than repeat a successful removal.

Unrecorded remnants remain preserved. There is no general reclamation command;
normal operations clean only the content owned or retired by recorded mutations.
Unexpected content is never adopted as part of that cleanup.

## Output and troubleshooting

For commands other than `exists`, status 0 means success, 1 means an operation
or collected check/report failed, and 2 means invalid CLI usage. The silent
`exists` command uses the presence/error statuses described above. JSON results
go to stdout, with diagnostics, skips, and warnings on stderr. Tools' output is captured so it cannot corrupt
JSON. A store-preparation or selector-resolution failure may occur before any
JSON report exists. Successful import and pull finish durable cleanup and
reclamation. Their cleanup failures return status 1
and contextual stderr diagnostics, with no success text or JSON report. A ref
can already be visible when an error occurs; subsequent operations first finish
recorded recovery. Recovery does not retry a pull or contact the registry. A
separately requested pull resolves its source normally.

| Symptom | Action |
| --- | --- |
| Store cannot initialize | Check the prepared immediate parent and its open/sync permissions; missing ancestors are not created. |
| Destination already exists | Inspect it and choose an unused name or request explicit replacement. Existence is an observation, not a reservation. |
| Export cannot preserve content | Check Skopeo capability and representation support; no conversion fallback is available. |
| Digest cannot be resolved | Use a stored root under the same literal repository. Resolve damaged candidates before treating an incomplete search as absence. |
| Lock wait or timeout | Check active transfers, including surviving copy children. Do not delete lock files to break contention. |
| Recorded recovery is blocked | Preserve the error and pending records; follow [recovery failures and manual resolution](#recovery-failures-and-manual-resolution). |
| Durability error after mutation | Inspect current state before retrying: publication or removal may already be visible. No rollback hides that uncertainty. |

Preserve unexpected contents when investigating errors. Replacing a ref cannot
generally repair damaged shared immutable objects.

## Recovery failures and manual resolution

Operation preparation automatically finishes safe recorded recovery or returns
a contextual error; there is no recovery command. Import, pull, and remove
record their work. Unrecorded remnants still require explicit maintenance. Preserve pending records and the permanent lock when recovery is
blocked. See the
[architecture](architecture.md#recorded-mutations-and-automatic-recovery) for the
snapshot lifecycle and the
[administration guide](administration.md#failures-backups-and-recovery-limits)
for investigating failures, recovery limits, and backup responsibilities.

An ordinary interruption is a case the recovery protocol is designed to handle.
It reconciles the visible ref with the record and tolerates cleanup entries
already removed by an earlier attempt. Recovery stops when it cannot perform
the required filesystem operations or cannot establish that continuing is safe.

| Circumstance | Why recovery stops | Possible resolution |
| --- | --- | --- |
| The store lock cannot be acquired | Another operation or a surviving transfer child still holds it. Lock acquisition can time out before recovery starts. | Let the operation finish. Investigate a genuinely stuck process through normal process management. Never delete or replace the lock file. |
| Filesystem operations fail | Permissions, a read-only or unavailable filesystem, quota/capacity limits, or I/O errors prevent reading, writing snapshots, deleting entries, or syncing directories. | Restore the intended access, available capacity, or filesystem health, then retry recovery through an ordinary command. Required filesystem and durability primitives cannot be bypassed with weaker behavior. |
| The active snapshot is invalid | JSON is malformed, its version is unsupported, fields contradict one another, or a reserved path has an unexpected type such as a symlink. | Preserve the record and investigate corruption, external changes, or a software mismatch/bug. A compatible implementation, coherent backup restore, or maintainer-reviewed repair may be needed. There is no automatic record repair or migration. |
| The live ref contradicts the record | It matches neither the expected old state nor the intended new state, or contradicts an already-recorded cleanup outcome. | Investigate external changes, an inconsistent restore, or a bug. Use a verified repair decision or coherent backup restoration; recovery cannot safely guess the intended state. |
| Safe reclamation cannot be established | Ref discovery is incomplete, pointers or wrapper roots are damaged, or manifest/index declarations are unreadable, conflicting, cyclic, or unsupported. | Restore access when that is the cause. Structural damage needs a coherent backup restore or maintainer-reviewed repair; unsupported declarations need implementation support. Partial reachability cannot authorize deletion. |
| Cleanup encounters unexpected contents | A recorded tree contains unknown entries, redirected directories, wrong file types, unexpected object sizes, or a planned deletion now targets referenced content. | Preserve the contents and investigate their origin. A verified repair plan or coherent restore may be needed. Blindly deleting the offending entry is not a general resolution. |

Failure does not mean that nothing changed. A new ref or removal may already
be visible, and some cleanup may have completed. Recovery does not roll back a
visible committed change. After correcting an environmental problem, recovery
can resume from its recorded state rather than starting cleanup again from
scratch.

A `complete` record means content cleanup is already durable and only record
clearing remains. Removing the record or syncing that removal can still fail.
The latter can return an error even when the file currently appears absent;
if it reappears after a crash, its `complete` phase authorizes only bookkeeping
cleanup.

Because objects are shared, damaged refs other than the operation's target can
prevent recovery from proving that reclamation is safe. An abandoned acquisition
that only needs private staging cleanup can avoid this traversal. Recovery reads
dependency declarations for deletion safety; it does not perform a full content
check or hash every layer.

Removal validates its target, records intent, and durably unlinks the name before
checking remaining refs for safe reclamation. There is no additional early
reachability preflight. A damaged remaining manifest can therefore leave the
name absent but cleanup unfinished. The pending record blocks ordinary access
to the entire store, including list, check, and remove, even for healthy
images. If that damage cannot be resolved, PoC v2 has no built-in way to restore
ordinary access. Recovery never rolls back the visible removal. Addressing this
limitation is future work for PoC v3 or later; do not delete the record or lock
to bypass it.

For manual resolution:

1. Preserve the full error, affected ref and path, software version, store marker,
   and pending snapshot files. Diagnose the underlying cause before changing
   persistent contents. Do not delete `pending-mutation.json`, its temporary
   replacement, or the permanent lock simply to unblock commands.
2. For access, capacity, or filesystem failures, correct the environmental
   problem first. Retry an ordinary query such as `list` to trigger recovery.
   Inspect the resulting state before deciding whether to repeat the original
   mutation.
3. For structural problems, coordinate with the store owner or maintainer,
   ensure active operations and surviving transfers have stopped, and preserve
   a coherent backup or snapshot using site procedures before attempting repair.
   Normal `check` and `remove` also require recovery to succeed; they
   cannot bypass a blocked recovery to repair the store. Investigation may need
   read-only filesystem inspection of the preserved state.
4. Agree on a targeted repair whose identities and content sources can be
   verified, or restore a coherent backup. Avoid mixing unrelated versions of
   refs, wrappers, objects, and recovery records. The current implementation
   provides no general repair facility, so there is no universal supported
   sequence of manual file edits that restores every damaged store.

Recreating a disposable store whose images can be reacquired is a last resort,
not the default resolution. It can lose local names, notes, and provenance that
image blobs alone cannot reconstruct. A tentative
[future repair feature](development.md#future-work-guided-in-place-repair) would
aim to restore a store to working order in place while preserving healthy
content and operational metadata.
