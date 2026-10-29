# HPC image store architecture

This is the authoritative architecture reference for the implemented first PoC.
It describes current behavior and the guarantees that changes must preserve.
The source and tests establish what is implemented; consequential differences
from this contract require explicit resolution. The archived design and staged
implementation plan are historical records, not instructions for future work.

## Purpose and boundaries

The store retains OCI image content, including compressed layers, in one
user-owned directory on Linux. Jobs can export that content into separate,
possibly volatile Podman containers-storage. It stores no unpacked filesystems
and provides no registry service, registry push, global catalog, automatic
repair, deep check, or migration of existing stores.

The synchronous library owns storage behavior, preparation, locking, transfers,
and typed diagnostics. The development CLI parses arguments and presents library
reports. Its seven commands are `import`, `pull`, `export`, `list`, `check`,
`remove`, and `prune`. Availability is a library-only query. Presentation code
formats collected reports without opening the store.

Each acquired ref contains one platform-specific OCI image manifest. A tag pull
may select a manifest through an OCI index or Docker manifest-list envelope,
but retains neither that envelope nor other platforms. Complete-index storage,
multi-platform stored images, index-parent inventory, nested-index source
resolution, and platform-selecting index export are unsupported. Complete graph
traversal of encountered indexes remains essential for checking and prune safety.

Stores coordinate cooperating processes owned by one user. Group-writable stores
and application integration require future work. Cross-node visibility, locking,
required filesystem primitives, and crash persistence on Lustre/VAST remain
unqualified; local tests establish ordering and failure handling rather than
deployment guarantees. No weaker filesystem fallback is substituted when a
required primitive is unavailable.

## Storage and identities

```text
store/
├── store.json                     # Store format 1
├── blobs/sha256/<digest>           # Shared immutable manifests, configs, layers
├── refs/<encoded-key>/ref          # Relative symlink selecting a raw image
├── raw_images/raw-<id>/            # Complete underlying image record
│   ├── oci-layout
│   ├── index.json                 # Exactly one selected root descriptor
│   ├── metadata.json              # Ref metadata schema 3
│   └── blobs/                     # Empty real directory
├── tmp/operation-<id>/             # Private acquisition staging
│   ├── layout/
│   └── blobs/sha256/
└── locks/
    ├── store.lock                 # Permanent identity
    └── refs/<encoded-key>/ref.lock # Logical-name coordination
```

A ref is an explicit `name:tag`. Its key is the reversible lowercase hexadecimal
encoding of the complete name, split into components of at most 120 characters.
All components except the last contain exactly 120 characters. Fixed terminal
entries `ref` and `ref.lock` avoid collisions between prefix names. Encoding
provides filesystem-safe paths without a separate name-to-ID database.

A raw-image ID identifies one publication instance. It is a randomly generated
twelve-character alphanumeric value, unrelated to content or Podman's image ID.
Here, “raw image” means the record below the named ref, not uncompressed content.
The ref points by an exact confined relative path into `raw_images/`. Namespace
parents, raw-image directories, and small files reject symlink redirection.

A content digest identifies exact bytes. Identical manifests, configurations,
and layers share objects within a store. The per-ref OCI layout is authoritative
for its root and reachability; there is no mutable store-wide OCI index. Its
`index.json` entry point is distinct from a registry image index. Skopeo's
shared-blob options connect the empty per-layout `blobs/` directory to the CAS.
A raw image is not independently portable without materializing its content.

Metadata records `local_ref`, `stored_root`, `content`, acquisition time,
acquisition provenance, and optional notes. Content fields cache the config
digest (OCI ImageID), platform, optional creation time, and stored size. Size
counts unique manifest, configuration, and layer objects, excluding wrapper
files. It is neither Podman's unpacked size nor an estimate of removal savings;
shared content prevents summing ref sizes into total store use.

Import provenance records the requested source and containers-storage image ID.
Pull records the requested registry selector and resolved manifest digest, which
must equal the stored root. An index used for selection supplies no retained
identity or alias. Cached fields never replace authoritative OCI reachability.
Notes and provenance may be irreplaceable operational metadata; CAS objects alone
cannot reconstruct deleted names and history.

Store format 1 and ref metadata schema 3 are separate from document versions.
Unsupported versions are rejected without migration or automatic deletion.

## Selectors and local lookup

Import, pull, export, single-image check, and availability accept
`name[:tag][@digest]`, requiring a repository and at least one of tag or digest.
Names follow the upstream named-reference grammar with path-safety restrictions.
Repository paths are lowercase; domains may retain uppercase letters and numeric
ports. Names are limited to 255 ASCII bytes and tags to 128. Tags are case
sensitive. There is no registry expansion, normalization, or implicit `latest`.
Bare digests, image IDs, transport URLs, and embedded storage settings are
unsupported. Ordinary names such as `oci:latest` remain repository/tag pairs.
SHA-256 values require all 64 lowercase hexadecimal digits. The ambiguous bare
spelling `sha256:<64 hex digits>` is reserved, including uppercase hex.

Without a digest, local lookup selects the exact tagged ref. With a digest, the
tag is ignored and lookup searches committed OCI roots under the same literal
repository name. It never contacts a registry or uses config IDs, provenance,
or unreferenced CAS objects as aliases. Healthy matches are selected
deterministically in local-name order and revalidated under the retained ref lock.
A healthy match remains usable despite other damage. Without one, damage or
incomplete discovery that prevents proving absence in the requested repository
produces incomplete resolution rather than a definite miss. Known damage in
other repositories does not hide an otherwise definite miss.

Import and pull use the source verbatim when the destination is omitted. Keep
the effective destination tag, or derive a tag from the first 12 hexadecimal
characters of an explicit source digest. A resolved tag's digest cannot supply
that fallback. Digests in explicit acquisition destinations are ignored and
reported; they neither name nor constrain stored content. Conflicts require
explicit replacement, including when generated tags collide.

Export defaults its destination to the supplied source selector verbatim, not
the chosen local ref or acquisition metadata. Destination digests are passed to
Skopeo for enforcement. Removal is deliberately different: it accepts only an
explicit tagged ref and rejects every digest-bearing selector.

## Initialization and preparation

Constructing `StoreConfig` anchors a relative path to the current working
directory. Constructing `Store` performs no filesystem access. Import, pull,
and remove can initialize a missing or empty store; there is no `init` command.
Export, inventory, checking, availability, and both prune modes require an
initialized store and never resume setup.

The immediate parent must already be prepared, including persistence of the
directory names needed to reach it. Initialization opens and syncs that parent;
creating a missing root also needs parent write permission. It creates no missing
ancestors and does not inspect or repair them. Nested stores are unsupported as
a setup restriction, without ancestor searches to detect them.

For an unmarked store, initialization proceeds as follows:

1. Inspect existing contents, rejecting unexpected data and incompatible reserved entries.
2. Create only the root and minimum lock structure if needed, converging with concurrent creators on the same stable store lock.
3. Take the exclusive store lock and recheck contents and any marker published by another process.
4. Reuse recognized empty setup directories and the regular temporary marker `.store-init.json`; create missing structure and write the marker.
5. Sync the temporary marker, lock file, internal directories from children to parents, store root, and immediate parent, in that order.
6. Rename the temporary marker to `store.json` and sync the root before returning success.

The parent sync is required on retries and when the root already exists. No
directory above it is opened for syncing. Interruption leaves only recognized
setup entries eligible for retry; unexpected contents are preserved with an
error. Completed initialization needs no private recovery record.

For a marked store, import/pull/remove preparation validates under a shared
store lock and syncs the marker and root again, without opening the parent.
Seeing a marker alone does not prove its final sync succeeded. A post-publication
initialization failure returns `Error::InitializationDurability`, reports
potentially visible state, and leaves the marker in place. Normal operation
locking and validation follow either preparation path.

Created directories use mode 0700 and files 0600, subject to umask; existing
permissions are preserved. Skopeo chooses blob permissions inside private
directories. Reserved setup directories must be real directories; markers and
lock files must be regular files. Committed ref pointers are intentionally
symlinks. Read operations may create ref coordination bookkeeping.

## Coordination and process lifetime

Acquire the store lock before any ref lock and retain both for the operation's
use of persistent content.

| Operation | Store lock | Ref lock |
| --- | --- | --- |
| Import, pull, remove | Shared | Exclusive on destination or selected name |
| Export, availability | Shared | Shared on selected name throughout use |
| List, check | Shared | Shared while inspecting each ref |
| Prune | Exclusive | None |

List and all-store check provide coherent individual ref observations, not a
single store-wide snapshot. Work on different refs can proceed concurrently;
CAS insertion must therefore also tolerate concurrent identical objects.

The store lock identity is permanent. Ref locks remain stable across ordinary
replacement and removal, while refs are committed, and while processes hold
or wait for them. Every holder, waiter, and transfer child retains its store
lock until its ref descriptors close. A ref guard borrows its store guard;
there are no cached ref descriptors outside this lifetime. Only prune under
the exclusive store lock may reclaim recognized unused ref locks. Later
operations can safely recreate bookkeeping for absent names.

Lock acquisition uses nonblocking `flock` attempts, retrying every 100 ms,
warning to stderr every 30 seconds, and timing out after five minutes. The
deadline covers acquisition attempts, not filesystem opens or subsequent work.
Tests use short internal thresholds. Timeouts never remove lock files.

Parent descriptors are close-on-exec. Immediately before a Skopeo copy, the
child clears that flag only on the operation's store and ref descriptors. It
inherits the same locked open-file descriptions, without an explicit unlock.
If the wrapper dies during copying, surviving Skopeo retains both locks until
its descriptors close. It can finish private work but cannot publish a ref.
Prune and other conflicting operations remain excluded during that lifetime.

## Transfers and platform policy

The caller owns containers-storage configuration, environment, storage driver,
and user namespace. The library neither reconstructs nor changes that context
and does not launch `podman unshare`. Remote Podman contexts are unsupported.
Import requires Podman and Skopeo; export and registry pull need only Skopeo.
Capability checks require shared-blob options, platform flags when selected,
and digest preservation for pull and every export.

Import inspects the local source once with Podman. For a tag source, it copies
the resulting complete image ID, preventing later tag movement from changing
the selection. A digest source uses `name@digest` for inspection and copying,
omitting its tag. Source disappearance fails without retry by name. Import may
convert representation or compress uncompressed layers, so its stored root can
differ from the source digest. Metadata comes from completed OCI content.

Pull resolves registry JSON with `skopeo inspect --raw` before layer transfer:

1. A digest source must directly identify an OCI image manifest. Hash the exact response bytes and verify the requested digest; reject index and manifest-list digests even with one child.
2. Inspect a tag once. A direct manifest supplies its digest from the original bytes. For an OCI index or Docker manifest list, select one supported manifest, inspect it by digest, and verify digest, type, and descriptor size. Do not look up the tag again.
3. Reject malformed or unsupported content and nested-index resolution. Copy the pinned `repository@manifest-digest` into private staging with preservation, then require the completed root to equal that digest.

Index selection without an explicit platform defaults to `linux/amd64` on
`x86_64` and `linux/arm64` on `aarch64`; other library targets need an explicit
platform. Match OS and architecture, filtering variant only when supplied.
There must be one distinct matching manifest digest; consistent duplicate
descriptors collapse, conflicting duplicates fail, and descriptor order never
breaks ties. CPU variants are not detected. A direct manifest needs no host
selection. Any explicit platform must match the completed configuration's OS,
architecture, and supplied variant. Import/export otherwise use Skopeo's
platform defaults. Repository-wide architecture enforcement is outside scope.

Pull uses the same authentication, certificates, trust policy, TLS settings,
and inherited environment for inspection and copying. Skopeo reads credentials;
they never enter ref metadata. Pull sets command retries to zero and performs
no fallback or forced conversion, recompression, or `--all` copy. Understanding
a Docker manifest-list envelope does not add Docker-specific stored media types.

Every export requires `--preserve-digests`, including imported refs. Capability,
representation, and destination-digest failures propagate without a weaker retry.
Export rejects stored indexes and cannot choose another architecture from a
single manifest. It checks the small source wrapper, then relies on Skopeo for
transfer validation without a preliminary full graph check or runtime
post-export digest inspection. It leaves persistent image data and metadata
unchanged; coordination bookkeeping may be created. Partial destination content
belongs to containers-storage. A later import cannot promise reconstruction of
an earlier registry representation.

## Publication and failure states

Import and pull write only into private `tmp/operation-<id>/` staging, including
Skopeo's destination shared-blob directory. Skopeo never writes into the live
CAS. The producer stops writers and closes writable handles before completing
the stage. Completion binds the selected descriptor, metadata, and object list
to the locked ref. Transfers rely on Skopeo validation and small manifest/config
inspection; they do not run availability or independently hash layers or check
the full graph around every copy.

Publication follows this order while retaining the store and exclusive ref locks:

1. Sync staged objects and insert them into the CAS with same-filesystem hard links. Never overwrite existing paths. Reuse requires a regular file of the expected size and successful file and CAS-directory syncs; reuse preserves modification time. Unsupported hard links fail without copying instead.
2. Exclusively create a raw-image record with matching metadata, an empty real `blobs/`, and `index.json` normalized to one root descriptor while retaining its extra fields. Sync wrapper files, `blobs/`, the raw-image directory, and `raw_images/`.
3. Create and sync ref namespace parents. Prepare a relative symlink in staging and sync its directory.
4. Atomically rename the symlink into the committed namespace, then sync both rename parents before reporting success.

Readers therefore see the complete old or new record, rather than separately
updated layout and metadata. Internal publication supports index fixtures, but
public acquisition/export accepts manifests only. Existing CAS paths and
same-size corruption are not rehashed. A published blob must never be modified
through any remaining staging hard link.

Destination conflicts, including damaged entries, require explicit replacement.
Replacement accepts symlinks and regular files, rejects directories and special
entries, and leaves retired raw images for prune. Removal unlinks the terminal
ref without following it, then syncs its parent; content and bookkeeping remain
for explicit maintenance. Missing refs are errors, including after removal has
initialized a previously absent store.

Failures before ref publication preserve the prior ref, although unreferenced
objects or raw images may remain. After a namespace change, `Error::Durability`
identifies the potentially visible blob insertion, ref publication, or removal
and the failed sync path. The library does not roll back committed pointers to
hide uncertainty.

After durable publication, acquisition removes only its own staging and syncs
`tmp/` while still locked. Cleanup failures become structured warnings on a
successful report and CLI status 0. Failed acquisition, publication errors,
and process termination do not initiate cleanup: surviving writers may still
exist. Prune handles recognized abandoned remnants later. Staging must fit an
entire acquisition even when final content deduplicates; duplicate staged copies
and hard links disappear during successful cleanup.

## Inventory, checking, and availability

Discovery enumerates the committed namespace rather than hiding malformed refs.
Inventory inspects pointers and small wrapper files under each ref lock, with
no graph traversal, CAS access, or external tool invocation. Healthy entries
are `unverified`; damaged refs remain visible. Reports preserve independently
readable fields and diagnostics, and distinguish incomplete enumeration from
per-ref damage. Store-level failures are typed errors.

Checking uses the authoritative root to traverse the complete supported graph,
including nested indexes and every platform dependency. It checks descriptor
structure, media types, regular-file type, existence, and sizes, and parses
small manifest, index, and configuration JSON. Metadata damage does not prevent
traversal when the root remains readable. Supported content is schema-version-2
OCI manifests/indexes, OCI configuration JSON, and ordinary or nondistributable
tar layers with no compression, gzip, or zstd. Configuration requires nonempty
OS and architecture and correctly typed optional fields. Layer URLs and embedded
data do not excuse missing local objects. Docker-specific media types,
unknown schemas/types, artifacts, and `subject` content remain unresolved.

Object observations, including failures, are reused within one operation, while
every descriptor declaration is checked. Conflicting declarations and cycles
are errors. No blob hashing, decompression, unpacking, or comparison of cached
display fields against graph-derived values is performed. Same-size corruption
and invalid tar/compressed data can pass. Passing a check does not certify
execution or validate public index workflows. Read operations preserve image
data but may create coordination files.

Availability shares wrapper and graph inspection and holds its selected locks
throughout. `Ready` means structural validation passed; `Missing` means definite
absence in a valid initialized store; `Unusable` carries typed damage or
unsupported-content diagnostics. Missing stores, permission failures, lock
timeouts, and incomplete digest resolution are errors. Operational failure takes
precedence even when damage has also been observed.

Availability invokes no tools, checks no remote freshness, and reserves nothing
for later use. A later acquisition validates its own destination and can still
fail with a conflict after `Missing`. Existing refs require explicit replacement;
reacquiring unsupported content can reproduce the problem, and replacing a ref
cannot generally repair immutable shared CAS damage.

## Conservative pruning

Both modes require initialization and hold the exclusive store lock through a
fresh authoritative ref/graph scan, candidate enumeration, and any mutation.
Neither creates ref locks or invokes tools. Any unresolved committed-ref error,
conflicting descriptor, unreadable namespace, or incomplete candidate inspection
blocks all deletion, including staging cleanup. Previous check/availability
results and cached sizes cannot authorize reclamation. Unknown contents instead
preserve their whole cleanup directory and produce successful skips. Symlinks
are never followed.

Eligible content includes unreferenced regular SHA-256 CAS objects, recognized
raw images, and abandoned acquisition directories. Directory names and contents
must match current internal structures; partial known writes can be cleaned,
but unknown entries, unexpected types, and tool temporaries are preserved.
Broken committed refs are not silently removed.

Content grace defaults to 24 hours, with an explicit override including zero.
One time captured after locking is compared with blob modification times or
staging/raw-image root modification times. This is object age, not time since
removal. Exact boundaries are eligible; future timestamps remain ineligible.
Reusing CAS objects preserves their age.

Unused ref bookkeeping is eligible immediately regardless of content grace:
empty single-link regular `ref.lock` files with valid encoded absent-ref
identities, followed by empty encoded lock and ref directories. Committed locks,
malformed identities, unknown lock subtrees, both bookkeeping roots, and the
permanent store lock remain. Operational inspection failure blocks all cleanup.

Before destructive cleanup with any eligible candidate, sync every scanned ref
directory from children to parents through `refs/`, including empty and
intermediate directories. A visible removal or replacement may have outlived a
failed sync or interrupted publisher. This barrier prevents restoring an older
ref after its retired content is reclaimed. It also applies to bookkeeping-only
cleanup. Failure blocks every deletion, reports a sync diagnostic, and leaves
eligible/removed totals zero with `scan_complete == false`. Dry-run skips this
barrier and cannot certify its success; deletion with no eligible candidates
also skips it.

Delete in order: staging, retired raw images, CAS objects, unused ref locks,
empty lock directories, and empty ref directories. Remove children before
parents and sync affected parents after removal. Stop at the first removal or
sync error and retain partial progress; do not roll back.

Reports distinguish scan completeness, eligible candidates, visible removals,
partial entry/byte totals, and confirmed durability. `removed` means the
candidate's top-level name is absent; `durability_confirmed == false` records
visible removals whose parent sync failed. A later retry cannot certify an
earlier removal that is no longer discoverable. Logical byte counts use regular
file lengths, counting hard links separately, without promising physical space
savings. Bookkeeping candidates have zero bytes.

## Source and evidence

`store`, `initialization`, `layout`, and `locks` own preparation and coordination.
`transfer`, `pull`, and `subprocess` own tool interactions. `staging`, `cas`, and
`refs` own publication. `discovery`, `inventory`, `inspection`, `graph`, `check`,
and `availability` inspect committed content. `prune` and its bookkeeping module
perform maintenance; `main` and `presentation` handle the CLI.

Publication and graph fixtures, process-lock tests, availability tests, prune
and bookkeeping tests, controlled registry fixtures, and local real-transfer
tests provide implementation evidence. The [development guide](development.md)
records current checks and their limits. The [deferred-validation notes](deferred-validation.md)
identify claims requiring further evidence. Upstream formats and transports are
defined by the [OCI image layout](https://github.com/opencontainers/image-spec/blob/main/image-layout.md),
[containers/image transports](https://github.com/containers/image/blob/main/docs/containers-transports.5.md),
and [Skopeo copy](https://github.com/podman-container-tools/skopeo/blob/main/docs/skopeo-copy.1.md).
The store's publication, locking, metadata, and pruning policies are local
architectural decisions.
