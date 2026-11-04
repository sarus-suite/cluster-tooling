# HPC image store architecture

This is the authoritative architecture reference for the implemented PoC v2 Stage 6.
It describes current behavior and the guarantees that changes must preserve.
The source and tests establish what is implemented; consequential differences
from this contract require explicit resolution. The archived design and plan
are historical records. The PoC v2 implementation plan defines the remaining stages, whose features are not yet implemented.

## Purpose and boundaries

The store retains OCI image content, including compressed layers, in one
user-owned directory on Linux. Jobs can export that content into separate,
possibly volatile Podman containers-storage. It stores no unpacked filesystems
and provides no registry service, registry push, global catalog, automatic
repair, deep check, or migration of existing stores.

The synchronous library owns storage behavior, preparation, locking, transfers,
and typed diagnostics. The development CLI parses arguments and presents library
reports. Its seven commands are `import`, `pull`, `export`, `list`, `check`,
`exists`, and `remove`. Presentation code formats collected reports
without opening the store.

Each acquired ref contains one platform-specific OCI image manifest. A tag pull
may select a manifest through an OCI index or Docker manifest-list envelope,
but retains neither that envelope nor other platforms. Complete-index storage,
multi-platform stored images, index-parent inventory, nested-index source
resolution, and platform-selecting index export are unsupported. Complete graph
traversal of encountered indexes remains essential for checking and safe reclamation.

Stores coordinate cooperating processes owned by one user. Group-writable stores
and application integration require future work. Cross-node visibility, locking,
required filesystem primitives, and crash persistence on Lustre/VAST remain
unqualified; local tests establish ordering and failure handling rather than
deployment guarantees. No weaker filesystem fallback is substituted when a
required primitive is unavailable.

## Storage and identities

```text
store/
├── store.json                     # Store format 2
├── pending-mutation.json          # Active snapshot during a recorded mutation
├── .pending-mutation-next.json    # Temporary replacement snapshot, when present
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
    └── store.lock                 # Permanent identity; sole operation lock
```

A ref is an explicit `name:tag`. Its key is the reversible lowercase hexadecimal
encoding of the complete name, split into components of at most 120 characters.
All components except the last contain exactly 120 characters. The fixed terminal
entry `ref` avoids collisions between prefix names. Encoding provides
filesystem-safe paths without a separate name-to-ID database.

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

Store format 2 and ref metadata schema 3 are separate from document versions.
Unsupported versions are rejected without migration or automatic deletion.

## Selectors and local lookup

Import, pull, export, single-image check, and existence accept
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
deterministically in local-name order under the retained shared store lock.
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
directory. Constructing `Store` performs no filesystem access. Import and pull
can initialize a missing or empty store; there is no `init` command.
Remove, export, inventory, checking, and existence require
an initialized store and never create a missing root or resume setup.

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

For a marked store, import/pull preparation validates under an exclusive
store lock and syncs the marker and root again, without opening the parent.
Seeing a marker alone does not prove its final sync succeeded. A post-publication
initialization failure returns `Error::InitializationDurability`, reports
potentially visible state, and leaves the marker in place. Normal operation
locking and validation follow either preparation path.

Created directories use mode 0700 and files 0600, subject to umask; existing
permissions are preserved. Skopeo chooses blob permissions inside private
directories. Reserved setup directories must be real directories; markers and
lock files must be regular files. Committed ref pointers are intentionally
symlinks. Read operations create no coordination bookkeeping. Their preparation
can first finish recorded recovery under exclusive access, as described below.

## Coordination and process lifetime

Every operation retains the single store guard throughout selection and all use
of persistent content. There are no per-ref lock files or guards.

| Operation | Store lock |
| --- | --- |
| Import, pull, remove | Exclusive |
| Export, existence, list, check | Shared |
| Pending mutation recovery | Exclusive |

Shared readers coexist. Writers serialize across all names and exclude readers.
List and all-store check observe a stable namespace throughout their operation.
The permanent store-lock inode is never reclaimed or replaced, including while
processes wait on it. Selection returns an explicit ref identity; resolved
content and acquisition stages borrow the retained store guard. Mutating
primitives require exclusive access.

Lock acquisition uses nonblocking `flock` attempts, retrying every 100 ms,
warning to stderr every 30 seconds, and timing out after five minutes. The
deadline covers acquisition attempts, not filesystem opens or subsequent work.
Tests use short internal thresholds. Timeouts never remove lock files.

Parent descriptors are close-on-exec. Immediately before a Skopeo copy, the
child clears that flag only on the operation's single store descriptor. It
inherits the same locked open-file description, without an explicit unlock.
If the wrapper dies during copying, surviving Skopeo retains the store lock until
its descriptors close. It can finish private work but cannot publish a ref.
Conflicting operations remain excluded during that lifetime; an orphan writer
also excludes readers, while an orphan export permits other readers.

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
unchanged and creates no coordination bookkeeping. Partial destination content
belongs to containers-storage. A later import cannot promise reconstruction of
an earlier registry representation.

## Publication and failure states

Import and pull write only into private `tmp/operation-<id>/` staging, including
Skopeo's destination shared-blob directory. Skopeo never writes into the live
CAS. The producer stops writers and closes writable handles before completing
the stage. Completion binds the selected descriptor, metadata, and object list
to an explicit destination ref under the exclusive store guard. Before publication,
import and pull validate the staged root and dependency declarations with the
same dependency-only parser used by recovery. The validated dependency list
supplies the objects to insert and their deduplicated size total. Unsupported
fields such as `subject` and `artifactType`, invalid descriptor extensions,
unsupported dependency roles, and conflicting declarations fail while content
is still private, before publication intent or live CAS insertion. The acquiring
record then authorizes safe cleanup of the abandoned stage.

Transfers rely on Skopeo for data validation and retain small manifest/config
inspection for metadata. The dependency pass does not open configuration or
layer objects, hash blobs, decompress layers, invoke existence or check, or
make remote requests. Public acquisition still accepts only single manifests.

Publication follows this order while retaining the exclusive store lock:

1. Sync staged objects and insert them into the CAS with same-filesystem hard links. Never overwrite existing paths. Reuse requires a regular file of the expected size and successful file and CAS-directory syncs; reuse preserves modification time. Unsupported hard links fail without copying instead.
2. Exclusively create a raw-image record with matching metadata, an empty real `blobs/`, and `index.json` normalized to one root descriptor while retaining its extra fields. Sync wrapper files, `blobs/`, the raw-image directory, and `raw_images/`.
3. Create and sync ref namespace parents. Prepare a relative symlink in staging and sync its directory.
4. Atomically rename the symlink into the committed namespace, then sync both rename parents before reporting success.

Readers therefore see the complete old or new record, rather than separately
updated layout and metadata. Internal publication supports index fixtures, but
public acquisition/export accepts manifests only. Existing CAS paths and
same-size corruption are not rehashed. A published blob must never be modified
through any remaining staging hard link.

Destination conflicts require explicit replacement. Import and pull replacement
require an existing entry to be a canonical confined pointer with a readable old wrapper
index and supported, readable dependency declarations. A regular-file ref,
malformed or escaping pointer, missing wrapper, or unreadable dependency graph
is rejected before publication, preserving the old entry and content.
Explicit replacement is not a repair facility. Cached metadata does not
authorize reclamation and metadata damage alone does not prevent it.

Import and pull record staging ownership before creating staging, then record
live addition ownership and publication intent before CAS insertion. After durable
publication, each uses the recorded recovery path below to reclaim unused retired
raw images and objects, remove private staging, and durably clear the record.
Success means this entire lifecycle has finished; no later maintenance action is
needed for the operation's recorded candidates. Other refs' shared objects and
required namespace prefixes survive. Unrecorded remnants remain untouched.

Import and pull failures after recording starts carry `Error::Recovery` context,
including transfer and stage-validation failures. The underlying typed cause is
retained. Before publication, recovery abandons acquisition and preserves the
old ref; after publication it finishes forward without restoring the old ref.
There is no destructor cleanup. Cleanup, syncing, and record-clearing failures
are errors, even when the acquired ref is already visible. Source inspection,
registry resolution, and capability failures before recording retain their original error
types. Recovery never retries acquisition or contacts the registry; a separate
new pull request resolves its source normally.

Removal requires an existing initialized store and an explicit `LocalRef`.
Missing refs return `Error::RefMissing`. Before recording intent, it reads the
canonical confined pointer, old wrapper index, and authoritative dependency
declarations. Damaged identities or unreadable/unsupported declarations reject
removal without unlinking; damaged display metadata alone does not block it.
Removal creates no acquisition staging.

The removal intent durably records the old identity and retired dependencies
before unlinking the terminal ref and syncing its parent. The same exclusive
guard then protects recorded recovery, which proves remaining reachability,
reclaims unused recorded content and empty namespace tails, and durably clears
the record before returning success. Shared objects and longer names survive;
unrecorded remnants are preserved. Errors after recording retain typed causes
inside `Error::Recovery`, including potentially visible `RefRemoved` changes.

There is no extra reachability preflight before unlinking. If another ref has
unreadable declarations, removal may already be durable when cleanup fails.
Unsafe-to-delete content and the pending record remain; ordinary operations,
including list, check, and remove, are blocked by preparation until safe
recovery can finish. Recovery does not restore the removed name. Structural
damage can therefore lock users out of ordinary store access without a built-in
resolution. Preserve records and locks; general repair and ways to restore
access are currently deferred and not on the design scope.

After a namespace change, `Error::Durability` identifies the potentially visible
change and failed sync path; recorded mutation errors retain this as their cause.
The library never rolls back committed pointers to hide uncertainty. Staging
must fit an entire acquisition even when final content deduplicates; duplicate
staged copies and hard links disappear during successful cleanup.

## Recorded mutations and automatic recovery

Stage 3 supplies recorded mutation primitives and automatic recovery in every
operation's preparation. Stages 4.1 through 4.3 integrate public import, pull,
and remove with that protocol. Legacy unrecorded remnants are not automatically
recovered. There is no recovery command or public method. Only import and pull
may initialize stores at this stage; format 2/schema 3 remain unchanged.
Intermediate format-2 stores may be recreated as the protocol is completed;
compatibility between incomplete stages is not promised.

`pending-mutation.json` is the durable snapshot of a mutation whose lifecycle
has not yet finished. It exists during normal execution of a recorded mutation;
its presence alone does not indicate interruption, failure, or corruption.
A clean, idle store contains neither `pending-mutation.json` nor
`.pending-mutation-next.json`. An interrupted or failed operation can leave the
snapshot for recovery.

There is at most one active snapshot per store and no persistent recovery queue.
The exclusive store lock serializes mutations, and pending recovery must finish
before another operation proceeds. An unresolved recovery blocks that operation.
The temporary `.pending-mutation-next.json` can coexist with the active snapshot
while preparing a replacement snapshot of the same mutation.

The versioned snapshot carries the operation and ref identities, expected old
and intended new pointer identities, private staging, owned and reused objects, retired raw
images and their dependency declarations, and namespace-directory ownership.
Paths are derived from validated identities and recognized tree-member names. The
record phases are `acquiring`, `intent`, `cleaning`, and `complete`.

Snapshot publication creates `.pending-mutation-next.json` exclusively, writes
and syncs it, renames it over `pending-mutation.json`, and syncs the store root.
Only then may the corresponding live change begin. Staging ownership is durable
before the staging directory is created. Publication intent and ownership of
unused CAS and raw-image identities are durable before inserting live content. Reused
objects never become owned additions. These primitives retain immutable hard
link insertion, complete wrappers, canonical pointers, and namespace syncing.
There is no cleanup in a mutation handle's destructor.

Recovery validates the active record before discarding any regular scratch
snapshot. Scratch is never promoted or interpreted as an instruction to mutate
content. A scratch-only interruption authorizes removal of that reserved
regular file and a root sync. Malformed active records, redirected reserved
entries, unexpected tree members, and inconsistent namespace states are
preserved with errors. Only recognized operation contents are eligible for
cleanup; recovery performs no general repair or discovery of abandoned work.

Recognized staging also includes Skopeo blob temporary files named
`layout/oci-put-blob<digits>`, with one or more ASCII digits and no other suffix.
They must be regular files directly inside the recorded operation's real
`layout/` directory. They can be empty or incomplete and are removed as private
producer artifacts without content or size validation. The same grammar applies
to persisted cleanup plans and their retries. Matching symlinks, directories,
other special entries, names in other locations, and unrelated unknown contents
still block cleanup. This allowance does not adopt unrecorded operation trees.

Recovery compares the actual terminal pointer with the recorded old and new
states. If the old state remains, it abandons acquisition and cleans safe owned
additions. If the intended pointer or removal is visible, it establishes that
namespace's durability and finishes reclamation. A completion flag is not
required to recognize a visible change, and recovery never restores the old
ref. Other states block the requested operation.

Reclamation reads authoritative roots and manifest/index declarations under
the exclusive lock, following every supported branch, including nested indexes.
It does not open configuration or layer files, consult display metadata, or run
check, existence, or external tools. Incomplete discovery, unreadable
declarations, conflicting descriptors, unsupported dependencies, and cycles
block unsafe deletion. This traversal establishes reachability for deletion;
it is not a content check or execution certificate.

Before deleting content, recovery syncs the changed ref namespace and the
namespace used to determine reachability. It persists a `cleaning` snapshot
with the outcome and a complete removal list before the first deletion, so
later attempts need not reopen retired manifests already removed. Cleanup is
limited to recorded candidates that no remaining ref uses. It preserves shared
objects, live raw images, and namespace prefixes needed by longer names.
Unexpected contents and entry types block deletion rather than being adopted.
All planned trees are inspected before deletion, and resumed cleanup rechecks
live reachability and remaining obligations.

Removal proceeds from children to parents, syncing affected parents. Missing
entries explained by the durable plan are valid retry states; recovery syncs
their surviving parent or planned ancestor before confirming cleanup. It then
persists `complete`, unlinks the record, and syncs the root. A failure still
returns an error, including uncertain record removal. A surviving or resurrected
`complete` record authorizes only record clearing, never further content deletion.

Writers recover before beginning their requested action. A reader finding
pending work releases shared access, acquires the same permanent lock
exclusively, revalidates and rechecks pending work, and recovers. It then releases
exclusive access and retries shared preparation, including another pending-work
check. There is no lock upgrade and no second descriptor retained alongside a
held operation lock. A surviving transfer continues to exclude recovery until
its inherited locked descriptor closes.

`Error::InvalidMutationRecord` identifies invalid bookkeeping;
`Error::Recovery` includes record path, available ref identity, phase, potential
namespace change, remaining work, and the contextual underlying error. Recovery
failure blocks ordinary actions. Shared cached structural and JSON findings
return `Error::InvalidObject` with the original finding's path and diagnostic;
operational I/O failures retain their typed cause chain. A pending operation
published by an earlier implementation can still refer to unsupported content.
Recovery returns a contextual error and preserves the committed ref, record,
and content rather than panicking, ignoring the unsupported declaration, or
repairing the store. Local fault and termination tests establish ordering and
retry behavior, not power-loss persistence or Lustre/VAST qualification.

## Inventory, checking, and existence

Discovery enumerates the committed namespace rather than hiding malformed refs.
Inventory inspects pointers and small wrapper files under the shared store lock,
with no graph traversal, CAS access, or external tool invocation. Healthy entries
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
execution or validate public index workflows. Read operations create no
coordination bookkeeping. Pending recorded recovery may change persistent
content before the requested query starts.

`Store::exists(&ImageSelector) -> Result<bool>` observes identity under the shared
store lock, after any prerequisite recorded recovery. It replaces availability
and its diagnostic types. It does not initialize a store.

For a tagged name, only the canonical terminal `ref` entry establishes presence.
Encoded directories used only by longer names do not count. Inspection does not
follow the terminal entry, so dangling or malformed pointers still count;
namespace parents must remain real directories. Explicit check diagnoses damage.

Digest existence ignores a supplied tag and reads authoritative root indexes
through canonical confined pointers in the same literal repository. It validates
the index schema, root descriptor, and single-root structure, without reading
metadata caches, `oci-layout`, raw-image `blobs/` wrappers, or CAS content.
A readable match establishes presence despite other damage. Without a match,
incomplete discovery or unreadable relevant candidate indexes return incomplete
resolution, not absence. Known refs from another repository are irrelevant;
namespace failures whose repository is unknown prevent proving a miss.

Clean lookup changes no persistent content, traverses no graph, invokes no tools,
and makes no remote request. Missing stores, permission failures, lock timeouts,
and unresolved recovery are errors. Results reserve nothing and do not certify
health or execution. Later acquisitions independently enforce destination policy;
existing entries require explicit replacement.

The `exists` CLI produces no result payload, in any output format.
Presence exits 0, definite absence 1, operational errors 125, and invalid usage 2.
Only operational errors and existing lock-wait warnings produce stderr during
lookup; parser diagnostics and help retain normal behavior. Other commands keep
their existing exit behavior.

## Reclamation boundary

Import, pull, replacement, and removal reclaim their unused recorded content
before success. Recovery finishes interrupted recorded work under the same
exclusive store lock. There is no public prune API, maintenance command, or
age-based reclamation policy.

Unrecorded remnants remain preserved, including abandoned trees and objects
that are not owned by an active mutation record. There is no general orphan
collector or automatic repair. Unexpected contents beneath `locks/` are not
adopted or reclaimed, and the permanent store-lock identity always survives
cleanup. These limits also apply to disposable intermediate development stores.

## Source and evidence

`store`, `initialization`, `layout`, and `locks` own preparation and coordination.
`transfer`, `pull`, and `subprocess` own tool interactions. `staging`, `cas`, and
`refs` own publication. `mutation` and `recovery` own recorded operations and
their preparation barrier. `discovery`, `inventory`, `inspection`, `graph`, `check`,
and `exists` inspect committed identities and content. `main` and `presentation`
handle the CLI.

Publication and graph fixtures, process-lock tests, existence tests, mutation
and recovery regressions, controlled registry fixtures, and local real-transfer
tests provide implementation evidence. The [development guide](development.md)
records current checks and their limits. The [deferred-validation notes](deferred-validation.md)
identify claims requiring further evidence. Upstream formats and transports are
defined by the [OCI image layout](https://github.com/opencontainers/image-spec/blob/main/image-layout.md),
[containers/image transports](https://github.com/containers/image/blob/main/docs/containers-transports.5.md),
and [Skopeo copy](https://github.com/podman-container-tools/skopeo/blob/main/docs/skopeo-copy.1.md).
The store's publication, locking, metadata, and reclamation policies are local
architectural decisions.
