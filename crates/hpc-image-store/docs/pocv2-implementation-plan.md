# PoC v2 implementation plan

## Purpose and current baseline

PoC v2 simplifies coordination and makes each mutation finish its own content
lifecycle. This document plans that implementation; it does not describe
features already delivered. The [architecture](architecture.md) remains the
authoritative reference for implemented behavior until the relevant stages
update it. The plans and results under `archive/` remain historical records.

The current PoC provides a synchronous library and a development CLI with
import, pull, export, list, check, remove, and prune. Availability is a
library-only query that validates the selected image graph. Normal operations
hold a shared store lock and acquire per-ref locks; prune holds the store lock
exclusively. Removal only unlinks a ref, replacement leaves retired raw images,
and prune later reclaims unused content. Successful acquisition can report
staging-cleanup warnings. Import, pull, and remove can initialize a store.
The current store format is 1, and ref metadata uses schema 3.

The implementation is primarily in `store`, `initialization`, and `locks` for
preparation; `transfer`, `pull`, `staging`, `cas`, and `refs` for mutation;
`discovery`, `inventory`, `inspection`, `graph`, `check`, and `availability` for
queries; and `prune` for deferred reclamation. `main` and `presentation` own CLI
dispatch and output. Use the existing synthetic fixtures, fault checkpoints,
subprocess workers, controlled registry, and disposable real-transfer harness
described in the [development guide](development.md).

## Target contracts

### Coordination and initialization

Every action acquires the permanent `locks/store.lock` and retains its guard
through all use of persistent content. There are no per-ref lock files or
guards in the completed milestone.

| Action | Store lock | May initialize a store? |
| --- | --- | --- |
| Import, pull | Exclusive | Yes |
| Remove | Exclusive | No |
| List | Shared after any required exclusive initialization | Yes |
| Export, exists, explicit check | Shared | No |
| Recovery of an interrupted mutation | Exclusive | No |

Exclusive writers serialize even when they target different names. Shared
readers can coexist, and list and all-store check observe a store namespace
that cannot change during their operation. Preserve lock wait warnings,
timeouts, and the permanent lock inode; never remove a lock to break contention.

A transfer child inherits only the operation's single lock descriptor, using
the same locked open-file description. The parent descriptor remains
close-on-exec except for this deliberate inheritance in the child. If the
wrapper dies, the surviving transfer retains the lock until its descriptors
close. Recovery cannot clean staging while that writer remains active.

Import, pull, and list may initialize a missing or empty store and resume
recognized retryable interrupted setup. List completes any required
initialization under the exclusive store lock, then acquires its normal shared
operation lock and revalidates before collecting inventory. Remove requires an
existing, initialized store and must not create a root, create lock bookkeeping,
or resume interrupted initialization. Preserve the prepared-parent requirement,
initialization sync ordering, retry rules for import/pull/list, permissions,
and the restriction against creating missing ancestors.

### Authoritative content and complete mutations

Keep the encoded ref namespace, confined relative pointers into `raw_images/`,
complete immutable raw-image wrappers, and shared immutable CAS objects.
Introduce store format 2 for the new coordination and recovery contract, reject
format 1 without migration, and retain ref metadata schema 3. Never reinterpret
an old store or delete its contents to make it usable. Recreate disposable
development stores explicitly.

Successful import, pull, replacement, and removal must leave the affected
namespace and content lifecycle complete and durable. Reclaim superseded raw
images and blobs that no remaining ref uses, remove operation staging, and
remove unnecessary ref directories before reporting success. Preserve shared
objects and namespace parents still needed by other refs. There is no grace
period or later prune requirement. Cleanup failures are errors, including when
the ref change is already visible; they are not warnings on successful reports.

Preserve the existing validation depth. Trust completed transfers and validate
the structures being changed, but do not add independent blob hashing,
decompression, remote-tag freshness checks, or implicit whole-store checking.
The authority of a store maintained by these operations does not imply that
external corruption is impossible or that an image is certified to run.

For reclamation, read authoritative ref indexes and manifest/index dependency
lists to determine reachability. Follow all supported dependency branches,
including encountered indexes, without opening every layer or invoking the
checker. Do not add a reference-count database or use cached display metadata,
prior check results, provenance, or modification times to authorize deletion.
If unreadable or unsupported dependency declarations prevent proving deletion
safe, preserve the content and return a contextual error. This traversal is
for mutation safety, not an automatic check action.

Make the changed ref namespace durable before deleting retired content, and
persist cleanup before clearing the operation record. Preserve no-overwrite
CAS insertion, pointer confinement, required filesystem primitives, and the
rule against rolling back a visible committed ref to hide a sync failure.
Reports and typed errors must explain potentially visible changes and the
remaining recovery work.

### Recovery and query boundaries

Use one active durable mutation record while holding the exclusive store lock.
Record operation-owned staging before acquisition starts, and record the
publication and reclamation intent before changing live content. The record
must distinguish owned additions from pre-existing shared content and retain
the ref identities and paths needed to classify and resume interrupted work.
Keep this mechanism specific to the store rather than introducing a generic
transaction framework.

Before ref publication, recovery abandons the acquisition and safely removes
its recorded additions. Once a ref change is visible, recovery establishes its
durability and finishes reclamation rather than restoring the previous ref.
Reconcile the recorded intent with the observed namespace; do not infer that
a change never occurred merely because its completion was not recorded.
Recovery must be idempotent and safe to interrupt at every step.

No normal action may observe an unfinished recorded mutation. A reader that
finds pending work releases its shared lock, acquires the same lock exclusively,
rechecks and recovers, then retries its shared operation and preparation checks.
Writers recover before beginning another mutation. Failure to establish safe
recovery blocks the requested action with a typed error. Preserve unexpected
contents and inconsistent records instead of adopting, repairing, or deleting
them speculatively. Recovery is for interrupted store operations, not general
repair of externally modified stores.

Replace `Store::availability` and its diagnostic types with
`Store::exists(&ImageSelector) -> Result<bool>`. Name existence means that the
canonical encoded name has a terminal `ref` entry, inspected without following
the pointer. Directory presence alone is insufficient: an encoded directory
can be a prefix used only by a longer name. A dangling terminal pointer still
establishes name presence; explicit check diagnoses its content damage.

Digest existence compares authoritative root digests read through confined
pointers, without consulting metadata caches or blobs. Retain literal
repository scoping and digest precedence over a supplied tag. A definite match
can establish presence; incomplete relevant discovery or unreadable candidate
indexes cannot establish absence. Known damage confined to another repository
does not obscure an otherwise definite miss.

Existence reserves nothing after returning and performs no remote request.
Normal clean-store lookup does not validate wrappers unrelated to resolving
the requested identity, traverse the content graph, access blobs, or invoke
tools. Pending recovery remains a prerequisite to normal lookup. Full graph
checking is performed only when explicitly requested through check.

Remove prune, its public types, CLI command, formatters, and grace-period
handling by the end of PoC v2. Do not retain it as a hidden prerequisite for
successful operations.

## Stage execution and validation

Implement stages in order; Stage 4's substages are separate reviewable changes.
Each stage must leave its applicable tests passing and record any remaining
intermediate behavior. Stages are independently testable commits, not separate
production releases. Format-2 fixtures and disposable stores created during
these intermediate commits may be recreated as the protocol is completed;
do not promise compatibility between incomplete intermediate implementations.
The full target contract is a release requirement after Stage 8.

Run commands below from the workspace root. Each stage lists its validation
commands; add focused cases to those test targets for its acceptance criteria.
Use synthetic OCI content and narrow fault injection for storage paths, and
readiness handshakes with bounded waits for process tests. Short internal lock
thresholds should exercise warnings and timeouts. Never run the internal
ignored subprocess workers directly or select all ignored tests.

### 1. Establish the target contract and acceptance fixtures

**Prerequisites:** review the current architecture, guides, source, tests, and
working-tree status. Confirm the baseline above against the implementation.

**Changes:** establish reusable fixtures for shared and unique blobs,
replacement, long names sharing encoded prefixes, and interrupted mutations.
Use the target contracts above to map each intended behavior to the later
stage and test that will establish it. Record the lock matrix, initialization
policy, format boundary, successful-state invariants, failure semantics, and
public interface changes as the implementation reference.

**Acceptance:** each design change has an observable acceptance case. Fixtures
represent both a short name and a longer name whose encoded path shares its
directory prefix. Existing fixtures and tests still describe current behavior;
do not add failing assertions for features that have not been implemented.

**Validation commands:**

```sh
cargo test --locked -p hpc-image-store --lib
git diff --check
```

**Intermediate limits:** no production behavior changes are delivered by this
stage. Planned guarantees remain targets, not current claims.

### 2. Simplify coordination and introduce format 2

**Prerequisites:** Stage 1's contracts and fixtures.

**Changes:** replace ref guards with store-guard lifetimes and explicit
selected-ref identities. Change normal mutation locks to exclusive and queries
to shared. Update initialization, selection, discovery, staging, CAS,
publication, and subprocess descriptor inheritance to use the single guard.
Introduce format 2 and stop creating per-ref lock directories. Adapt the
still-present availability and prune implementations and tests to operate
without ref locks; remove their obsolete ref-lock bookkeeping assumptions.
Preserve existing preparation behavior for remove until Stage 4.3 changes it.
Keep crate instructions and architecture descriptions aligned with the locking
and format behavior actually completed in this stage.

**Acceptance:** shared readers coexist; a writer excludes readers and other
writers, including writers targeting another name. Selection and content use
remain protected for the full operation. Parent termination leaves a surviving
transfer holding the correct single lock. Initialization races and retry tests
still pass; permanent lock identity, wait policy, and sync ordering are
preserved. Old and unsupported formats are rejected without modification.

**Validation commands:**

```sh
cargo check --locked -p hpc-image-store
cargo test --locked -p hpc-image-store --lib
cargo test --locked -p hpc-image-store --test cli
cargo test --locked -p hpc-image-store --test real_transfers -- --nocapture
git diff --check
```

**Intermediate limits:** mutations still have the old content lifecycle;
availability and prune remain available. Format 2 alone does not yet establish
complete mutation or recovery guarantees.

### 3. Add durable operation bookkeeping and recovery

**Prerequisites:** Stage 2's exclusive mutation coordination and format boundary.

**Changes:** implement the active mutation record and the recovery behavior
specified above, with durable ordering around record creation, intent updates,
namespace changes, cleanup, and record removal. Add preparation hooks for
writers and shared readers. Exercise the protocol with synthetic mutation
fixtures before integrating public actions in Stage 4. Ownership records must
cover staging, newly inserted objects, new raw images, and retired content
without making existing shared content eligible for speculative cleanup.

**Acceptance:** fault injection and process termination at each boundary leave
a clean store or recorded recoverable work. Repeated recovery tolerates already
completed removals and can itself be interrupted. Visible ref changes are made
durable before retired content is deleted. Unknown contents, inconsistent
namespace states, and malformed records are preserved with contextual errors.
Readers recheck after changing lock mode, and recovery cannot proceed while a
surviving transfer holds the operation lock. Preparation does not invoke check.

**Validation commands:**

```sh
cargo test --locked -p hpc-image-store --lib
cargo test --locked -p hpc-image-store --test cli
git diff --check
```

**Intermediate limits:** the protocol is testable, but public mutations do not
all create operation records until their Stage 4 substage is complete. Do not
claim that legacy unrecorded remnants are automatically recoverable.

### 4. Make mutators definitive, one action at a time

All substages depend on Stage 3 and run sequentially. Use authoritative
reachability under the exclusive lock, preserve shared objects, make ref
namespace changes durable before reclamation, and finish durable cleanup before
clearing the record or returning success. Each substage includes its own
failure, retry, and recovery cases. Do not implement this lifecycle by calling
prune or check from a mutator.

#### 4.1. Import

**Prerequisites:** Stage 3's recovery protocol and shared-content fixtures.

**Changes:** integrate bookkeeping into import, including explicit replacement.
Reclaim superseded raw images and blobs no remaining ref uses, and finish
staging cleanup before success. Remove import's cleanup-warning report field
and corresponding CLI warning handling. Preserve source pinning, naming,
platform behavior, immutable insertion, and provenance.

**Acceptance:** import leaves no operation staging after success. Replacement
leaves no retired operation content and preserves blobs used by other refs.
Failures during transfer, publication, deletion, and syncing remain recoverable
and identify potentially visible changes. An acquisition failure cannot damage
the previous committed ref or existing shared objects.

**Validation commands:**

```sh
cargo test --locked -p hpc-image-store --lib
cargo test --locked -p hpc-image-store --test cli
cargo test --locked -p hpc-image-store --test real_transfers -- --nocapture
git diff --check
```

**Intermediate limits:** pull still has cleanup warnings and deferred content
reclamation; remove still has its original preparation and unlink behavior.
The shared cleanup-warning type remains while pull uses it.

#### 4.2. Pull

**Prerequisites:** Stage 4.1's integrated lifecycle and Stage 3's recovery protocol.

**Changes:** apply the same definitive lifecycle to pull and pull replacement.
Preserve pinned-manifest acquisition, digest preservation, platform selection,
registry settings, and provenance. Remove pull's cleanup-warning report field,
its CLI warning handling, and the shared warning type once unused.

**Acceptance:** successful pull and replacement finish all cleanup and preserve
shared content. Controlled registry failures and interruptions recover without
weakening source pinning or digest preservation. Missing capabilities and
unsupported representations still fail without weaker retries or conversion.

**Validation commands:**

```sh
cargo test --locked -p hpc-image-store --lib
cargo test --locked -p hpc-image-store --test registry_pull
cargo test --locked -p hpc-image-store --test cli
git diff --check
```

**Intermediate limits:** remove remains incomplete until Stage 4.3; prune and
availability still exist. Controlled fixtures establish registry behavior;
this stage does not require a live registry test.

#### 4.3. Remove

**Prerequisites:** Stages 4.1 and 4.2, including their reclamation and recovery
primitives and shared-content tests.

**Changes:** require an existing, initialized store. Update operation preparation
so only import and pull can initialize stores at this intermediate stage;
Stage 7 will also enable list. Remove must not create a missing store or
initialize or resume setup in an empty or unmarked directory. Recovery of
recorded mutations in an initialized store remains supported. Retain the
explicit `LocalRef` input and rejection of digest-bearing removal selectors.

Replace ref-only unlinking with durable removal and immediate reclamation of
unused raw images, blobs, and namespace directories. Preserve directory
prefixes needed by other names. If safe reclamation cannot be established,
preserve content and return an error rather than pretending removal is complete.

**Acceptance:**

- Library and CLI removal against a missing store return an error without
  creating its root, lock files, or other entries.
- Empty and partially initialized directories remain unchanged; removal does
  not finish initialization or create missing ancestors.
- A missing ref in an initialized store returns the existing missing-ref error.
- Removing one of two sharing refs preserves shared blobs; removing the last
  ref reclaims them before success.
- Removing a short name preserves namespace prefixes and content needed by
  longer names, without leaving the short name present.
- Partial deletion and sync failures retain recovery information, identify
  potentially visible changes, and never report success prematurely.

**Validation commands:**

```sh
cargo test --locked -p hpc-image-store --lib
cargo test --locked -p hpc-image-store --test cli
cargo test --locked -p hpc-image-store --test real_transfers -- --nocapture
git diff --check
```

**Intermediate limits:** all three mutators now complete their lifecycle, but
availability and prune remain public until Stages 5 and 6. This does not
authorize repairing an externally damaged store or adopting unrecorded data.
List continues to require an initialized store until Stage 7.

### 5. Introduce lightweight existence

**Prerequisites:** Stage 4's complete lifecycle, Stage 3's preparation/recovery
hooks, and the encoded-prefix fixtures from Stage 1.

**Changes:** add `Store::exists(&ImageSelector) -> Result<bool>` and replace
availability and its diagnostic types. Apply the name and digest lookup rules
in the target contracts. Update library examples and any callers or tests of
availability. Keep graph inspection available through explicit check only.

Add `hpc-image-store --store PATH exists SELECTOR`, modeled after
[`podman image exists`](https://docs.podman.io/en/latest/markdown/podman-image-exists.1.html).
Use this CLI contract:

| Outcome | Exit status | Stdout | Stderr |
| --- | --- | --- | --- |
| Present | 0 | Empty | No result message |
| Definitely absent | 1 | Empty | No result message |
| Store access or operational failure | 125 | Empty | Contextual diagnostic |
| Invalid CLI usage | 2 | Empty | Parser diagnostic |

Global `--json` does not add a result payload or change these statuses. Missing
or uninitialized stores, lock failures, unresolved recovery, and incomplete
digest resolution are operational errors, not absence. Apply status 125 only
to this action; preserve other commands' exit behavior. Existing lock-wait
warnings may still appear on stderr. Help retains normal parser behavior.

Invoke the library directly, with no Podman dependency. Preserve the existing
`ImageSelector` grammar: do not add Podman image IDs, bare digests, implicit
tags, repository normalization, or provenance aliases.

**Acceptance:** verify exact statuses and empty stdout for present, absent,
and operational-error cases, both with and without `--json`. Test invalid
usage, missing/uninitialized stores, lock failures, pending recovery,
repository-qualified digests, tag/digest precedence, malformed candidate
indexes, and irrelevant damage in another repository. Test the prefix-name
case and a dangling terminal pointer. Use snapshots or narrow instrumentation
to prove clean-store existence does not initialize, mutate image data, open
blobs, run graph validation, or invoke tools. Keep explicit check tests proving
it can diagnose damage that name existence deliberately does not inspect.

**Validation commands:**

```sh
cargo test --locked -p hpc-image-store --lib
cargo test --locked -p hpc-image-store --test cli
cargo test --locked -p hpc-image-store --doc
git diff --check
```

**Intermediate limits:** existence is an observation, not a reservation or a
health certificate. Pending recovery can do recorded cleanup before lookup.
Prune remains exposed until Stage 6.

### 6. Remove prune

**Prerequisites:** Stage 4's complete mutators and Stage 5's replacement of
availability in callers and lifecycle tests.

**Changes:** remove the prune library method, public types, CLI command,
formatters, grace-period parser, and obsolete bookkeeping cleanup. Move useful
reachability, namespace-sync, unknown-content preservation, partial-removal,
and durability regressions to the relevant mutation or recovery tests before
removing their old test paths. Remove obsolete executable examples and current
usage guidance while keeping historical archive records unchanged.

**Acceptance:** prune is absent from public exports, CLI help, executable
examples, and current usage documentation. The CLI rejects the old command as
invalid usage. No successful lifecycle or recovery path calls prune or depends
on deferred reclamation. The permanent store lock survives all cleanup.

**Validation commands:**

```sh
cargo check --locked -p hpc-image-store
cargo test --locked -p hpc-image-store --lib
cargo test --locked -p hpc-image-store --test cli
cargo test --locked -p hpc-image-store --doc
git diff --check
```

Also search source, tests, README, and non-archive guides for obsolete prune
interfaces and instructions; historical comparisons in this plan may remain.

**Intermediate limits:** removal of the old maintenance interface is complete,
but list initialization remains for Stage 7. The milestone still requires the
integration validation and documentation closure in Stage 8.

### 7. Enable list to initialize a store

**Prerequisites:** Stages 1 through 6, including single-lock coordination,
format-2 initialization, and mutation recovery.

**Changes:** allow `Store::list()` and the CLI `list` action to reuse the
established initialization policy for missing stores, empty directories, and
recognized retryable interrupted setup. Preserve the prepared-parent
requirement, initialization durability ordering, permissions, unexpected-content
preservation, and rejection of unsupported formats.

Complete any required initialization under the exclusive store lock, then
acquire the normal shared operation lock and revalidate before collecting
inventory. Follow the existing mutation-recovery preparation rules. Listing an
already initialized store must retain shared-reader concurrency; do not require
exclusive access unless initialization or recovery needs it.

Preserve existing inventory formats and reports. Listing a newly initialized
store returns a complete empty inventory without invoking transfer tools or
check. Remove, exists, export, and check still require an initialized store.

**Acceptance:**

- Library and CLI listing create a valid format-2 store beneath an already
  prepared parent and return an empty inventory successfully.
- Existing empty directories and recognized interrupted initialization converge
  through the same retry rules as import and pull.
- Concurrent list initializers and list/import initialization races converge on
  the permanent store lock without deadlocks or partial successful results.
- Missing parents, unexpected contents, unsupported formats, permission
  failures, and sync failures produce contextual errors without creating
  ancestors or deleting existing data.
- Listing an initialized store preserves inventory semantics and shared-reader
  concurrency, and follows the existing recovery rules for pending mutations.
- Remove, exists, export, and check retain their requirement for an initialized
  store and do not gain initialization privileges through shared preparation.

**Validation commands:**

```sh
cargo check --locked -p hpc-image-store
cargo test --locked -p hpc-image-store --lib
cargo test --locked -p hpc-image-store --test cli
git diff --check
```

**Intermediate limits:** list can now create persistent store structure; its
inventory remains a lightweight observation rather than graph validation.
Final milestone qualification remains in Stage 8.

### 8. Validate PoC v2 and update documentation

**Prerequisites:** Stages 1 through 7 and all applicable acceptance cases.

**Changes:** update the local lifecycle scenario to exercise fresh-store
listing, existence, import/pull, replacement, export, explicitly requested
check, shared-content retention, and immediate removal. Verify that successful
actions leave no retired operation content without prune. Retain real-tool
process-lifetime tests and recovery tests covering parent termination and
surviving children. Use controlled registry fixtures for pull; do not add a live
registry test requirement.

Align the architecture, user/library/administration guides, development status,
README, and crate instructions with completed, verified behavior. Explain the
format-2 boundary, list's initialization behavior, remove's initialization
restriction, the single-lock concurrency tradeoff, cleanup errors and recovery,
exists output/statuses, and explicit check's validation limits. Remove obsolete
ref-lock instructions.
Keep the library guide's examples valid as rustdoc doctests and preserve
archived plans and historical validation results.

**Acceptance:** every target contract has passing evidence; all current
examples use the new interfaces. The lifecycle and process tests pass in the
documented disposable storage context. Record actual validation outcomes,
tool versions for real transfers, failures, skipped checks, and environmental
limits without treating historical results as current verification.

**Validation commands:**

```sh
cargo check --locked -p hpc-image-store
cargo test --locked -p hpc-image-store
cargo clippy --locked -p hpc-image-store --all-targets -- -D warnings
cargo fmt -p hpc-image-store -- --check
RUSTDOCFLAGS="-D warnings" cargo doc --locked -p hpc-image-store --no-deps
git diff --check
```

The ordinary suite includes the real-transfer and controlled-registry targets;
do not repeat them after a successful complete run unless changes or failures
justify it. Inspect Markdown links and generated API navigation as part of the
documentation review.

**Remaining limits:** local tests establish ordering and failure handling, not
actual crash persistence or Lustre/VAST cross-node qualification. Same-size
corruption and invalid compressed/tar data can remain undetected. These limits
remain documented after PoC v2 is complete.

## Scope and document handoff

Multi-platform acquisition, application integration, group-writable stores,
general repair, migration, backup orchestration, and deployment qualification
remain outside PoC v2. Keep the existing single-platform transfer and selector
semantics except for the explicitly planned interfaces above.

Default registry/namespace expansion, Podman-compatible short-name resolution,
and making this store the authority behind `sarusctl` remain deferred and
outside PoC v2. The [PoC v3 assessment](pocv3-reference-resolution-assessment.md)
records the complexity, inherited Skopeo mirror/remapping support, and candidate
work outline for future planning; it adds no stage or acceptance requirement here.
