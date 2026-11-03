# Development and PoC handoff

The first PoC implements single-platform containers-storage import/export,
manifest-preserving registry pull, inventory, structural graph checking,
explicit ref removal, conservative pruning, and library-only availability.
The CLI has seven commands. Store format 2 and ref metadata schema 3 are used.
PoC v2 Stage 2 replaces per-ref locks with one permanent store lock; the content
lifecycle of public mutators remains unchanged pending Stage 4. Stage 3 adds
durable mutation records and automatic recovery during operation preparation.
The crate remains private to this workspace; application integration is deferred.

The [architecture](architecture.md) is authoritative. The [user guide](user-guide.md),
[library guide](library-guide.md), and [administration guide](administration.md)
serve their respective readers. Documents under `archive/` preserve historical
results and design/planning context; they do not prescribe future work.

## Source orientation

| Area | Modules and tests |
| --- | --- |
| Preparation and coordination | `store`, `initialization`, `layout`, `locks`; initialization and subprocess tests |
| Transfers and publication | `transfer`, `pull`, `subprocess`, `staging`, `cas`, `refs`; publication fixtures and controlled tool tests |
| Recorded mutations and recovery | `mutation`, `recovery`, and dependency-only traversal in `graph`; recorded synthetic fixtures, fault checkpoints, and a subprocess worker |
| Inspection | `discovery`, `inventory`, `inspection`, `graph`, `check`, `availability`; complete graph and availability tests |
| Maintenance | `prune` and its bookkeeping module; reachability, partial-removal, namespace-sync, and empty-ref-directory regressions |
| CLI and presentation | `main`, `presentation`; CLI integration tests, human/JSON parity |
| Real tools | `tests/real_transfers.rs`, `tests/registry_pull.rs`, shared disposable-storage and registry fixtures |

`src/lib.rs` includes the library guide in generated crate documentation, so its
Rust examples remain doctests. The crate README is the short navigation entry
point. Keep relative repository links out of the included library guide; use
API references or self-contained explanations so generated rustdoc navigation
also works.

## Pending snapshot naming and lifecycle clarification

The active snapshot is now `pending-mutation.json`, and its temporary replacement
is `.pending-mutation-next.json`. The private layout helpers are
`pending_mutation()` and `pending_mutation_next()`. The mutation module, operation
and record types, public errors, JSON schema, format versions, protocol, and
fault-checkpoint identifiers retain their existing meanings. The architecture
and guides explicitly distinguish normal execution, clean idle state, and
interrupted work, including the single active record and absence of a persistent
recovery queue. See the [architecture](architecture.md#recorded-mutations-and-automatic-recovery)
for the authoritative lifecycle. Public mutators still begin recording in Stage 4.

On 3 November 2026, crate check, all 183 library tests, all 25 CLI tests,
all-target Clippy with warnings denied, formatting, the rustdoc build with
warnings denied, and `git diff --check` passed for this follow-up. The six
library subprocess helpers and one CLI helper remained ignored by the top-level
runners and were invoked by their owning ordinary tests. Existing tests now
assert the renamed snapshot and scratch paths, their absence after cleanup,
and renamed paths in CLI diagnostics. Current local documentation links and
stale-name searches passed. The existing staged changes were preserved.

The library target includes the existing actual-Skopeo lifetime test. The
real-transfer integration, controlled-registry integration, and doctest targets
were not rerun for this naming and documentation change; the Stage 3 full-suite
results below remain historical. Cargo validation used the configured cache
outside the filesystem sandbox. These checks add no power-loss or Lustre/VAST
qualification.

## PoC v2 Stage 3: durable mutation records and recovery

Stage 3 is complete. The [architecture](architecture.md#recorded-mutations-and-automatic-recovery)
describes the protocol, reserved files, ownership, cleanup ordering, reader
retries, and failure semantics. The [acceptance inventory](pocv2-acceptance.md)
maps REC-01 through REC-07 to passing library and CLI tests. The internal
mutation handle supplies acquisition, publication, removal, and cleanup entry
points; it never performs cleanup in its destructor. Every public operation
finishes any pending recorded work before using persistent image content.

Recorded variants reuse the original/keeper/replacement, nested-index, and
encoded-prefix fixtures. Tests inject failures around each snapshot phase,
namespace changes, every planned removal, syncing, and record clearing. Bounded
subprocess handshakes cover mutation and recovery termination, competing reader
retries, and a surviving transfer retaining the operation lock. Open-file
instrumentation verifies that reclamation reads dependency declarations without
opening configuration/layer files or display metadata. Unrecorded fixtures
remain as regressions for the intermediate public lifecycle.

On 2 November 2026, `cargo test --locked -p hpc-image-store -- --nocapture`
passed 221 tests: 183 library tests, 25 CLI tests, one real-transfer test, five
controlled-registry tests, and seven doctests. Six library subprocess helpers,
one CLI helper, and the Docker Hub smoke test remained ignored by the top-level
runner; owning ordinary tests invoked the helpers. No live registry request was
made. Real transfers used the disposable overlay-storage harness with Podman
5.4.2 and Skopeo 1.18.0. Cargo and real-tool tests used the configured cache and
disposable storage outside the filesystem sandbox.

Early checks exposed a preservation test that expected unchanged bookkeeping
after an unexpected CAS entry, a CLI fixture missing `check --all`, and Clippy
findings for a nested condition and a duplicate test-module declaration. Cleanup
preflight now precedes publishing its snapshot; the fixture and mechanical
findings were corrected. The passing full suite above followed those behavioral
test corrections. Final review also found that joining an empty tree member
adds a trailing slash, allowing Linux to follow a redirected tree root during
inspection. Root paths now retain their direct spelling, and preservation cases
cover redirected staging and raw-image roots. After that correction, all 21
recovery tests and all 25 CLI tests passed again. Crate check, all-target Clippy
with warnings denied, formatting, the rustdoc build with warnings denied, and
`git diff --check` passed. Current documentation links were checked locally.

Public import, pull, and remove still create no operation records. Stage 4 will
integrate them separately; import/pull cleanup warnings and retired content,
availability, and prune remain. Remove can still initialize, and list cannot
initialize yet. Recovery does not adopt legacy unrecorded remnants or repair
external corruption. Format 2/schema 3 remain, without migration or compatibility
promises between incomplete stages. Local fault and process tests establish
ordering and retry behavior, not power-loss persistence or Lustre/VAST qualification.

## PoC v2 Stage 2: single-store coordination and format 2

Stage 2 is complete. Import, pull, remove, and both prune modes hold the
exclusive store lock; export, list, check, and availability hold shared access.
Selection returns an explicit ref identity, while stages and resolved content
borrow the retained store guard. Copy children inherit one locked descriptor;
the parent's descriptors remain close-on-exec. There are no ref-lock directories
in new stores. Format 1 and unsupported markers are rejected without migration,
and ref metadata remains schema 3. Prune still reclaims content and empty encoded
ref directories; its obsolete ref-lock report variants and output were removed.

The [acceptance inventory](pocv2-acceptance.md) maps LOCK-01 through LOCK-04,
FORMAT-01, and SETUP-01 to concrete tests. Discovery tests now pause a reader and
prove a writer reaches actual contention before letting the reader finish.
Identical-content writers publish serially, and concurrent setup mixes import
and pull. Controlled descriptor inspection verifies single-lock inheritance and
unrelated descriptor closure. Orphan tests also verify that a surviving writer
excludes readers while a surviving export permits shared access.

On 2 November 2026, the full `cargo test --locked -p hpc-image-store -- --nocapture`
run passed 198 tests: 162 library tests, 23 CLI tests, one real-transfer test,
five controlled-registry tests, and seven doctests. Five library subprocess
helpers, one CLI helper, and the Docker Hub smoke test remained ignored by the
top-level runner; owning ordinary tests invoked the helpers. No live registry
request was made. Real transfers used the existing disposable overlay-storage
harness with Podman 5.4.2 and Skopeo 1.18.0.

The initial run exposed an obsolete test assumption that a paused waiter itself
blocks prune after the holder exits. The corrected regression permits prune
before the waiter acquires its lock and verifies that the permanent inode
survives; once acquired, the waiter excludes prune. Clippy also identified
mechanical test cleanup after removing the old lock categories. After the full
suite passed, the affected library and CLI targets were rerun following final
test cleanup and strengthened mutation/format assertions; all 185 tests passed.
Crate check, all-target Clippy with warnings denied, formatting, the API
documentation build with rustdoc warnings denied, and `git diff --check` passed.
Current Markdown links and generated API navigation were reviewed. Cargo and
real-tool tests used the configured cache and disposable storage outside the
filesystem sandbox.

Import/pull cleanup warnings, retired content, availability, and prune remain.
Remove can still initialize, and list cannot initialize yet. At Stage 2 completion
there was no durable mutation record or automatic recovery; Stage 3 above adds
that foundation. Format 2 alone does not imply the final
PoC v2 lifecycle guarantees or compatibility between incomplete stages. Local
process tests demonstrate coordination and failure ordering, without qualifying
crash persistence or Lustre/VAST cross-node behavior.

## PoC v2 Stage 1: contracts and acceptance fixtures

Stage 1 of the [PoC v2 implementation plan](pocv2-implementation-plan.md) is
complete. The [acceptance inventory](pocv2-acceptance.md) connects the plan's
requirements to fixtures, observable acceptance cases, test targets, and evidence,
with stable acceptance identifiers assigned to later stages. The implementation
plan defines the target contracts and stage boundaries.
At Stage 1 completion, production changes remained planned and the architecture
described format 1, metadata schema 3, per-ref locking, availability, prune,
and deferred reclamation. Stage 2 above supersedes its locking and format state.

The test-only `test_support` module supplies accurately hashed synthetic images
with shared and unique layers, original/keeper/replacement content sets, a
branching nested-index graph, publication adapters, a temporary mutation store,
and encoded-prefix names. Publication tests reuse the existing subprocess worker
to capture abandoned acquisition, interruption before ref rename, visible
replacement before sync, and visible removal before sync. These fixtures expose
current filesystem remnants and expected content sets without introducing an
operation-record format or claiming recovery. The existing nested-index prune
regression now uses the reusable graph and retains its safety assertions.

On 30 October 2026, `cargo test --locked -p hpc-image-store --lib` passed
159 tests, with zero failures and five existing internal subprocess helpers
ignored. This adds four tests to the verified baseline of 155 passing library
tests. The library run included the existing actual-Skopeo lock-retention test.
`cargo fmt -p hpc-image-store -- --check` and `git diff --check` passed.
The acceptance inventory was reviewed against each target-contract paragraph,
and the new documentation links were checked locally. No production code,
public interface, CLI behavior, store format, or dependency changed.

This Stage 1 run did not select the CLI, controlled-registry, real-transfer
integration, or doctest targets, nor repeat the full check/Clippy/rustdoc checks.
The historical full-suite results below remain historical. Synthetic layers
exercise structural storage behavior, not image execution; local interruption
tests establish observed states rather than crash persistence or Lustre/VAST
qualification. At that time Stage 2 was the next planned implementation stage,
covering the single store lock and format boundary.

## PoC v1 closure record

The reduced PoC wrap-up replaces the original broad qualification stage with
documentation consolidation, one local lifecycle scenario, and crate-scoped
verification. It adds no production feature, public API, or format migration.
The lifecycle scenario shares the existing real-transfer harness invocation and
local image fixture: a keeper and target share content, availability changes
from Missing to Ready to Missing, export/checking succeed, dry-run preserves
state, and pruning retains live shared content before reclaiming it after the
last removal. The permanent store lock survives reclamation.

On 28 October 2026, the complete crate suite passed 190 tests: 155 library tests,
22 CLI tests, one real containers-storage test, five controlled registry tests,
and seven doctests. The real-transfer test includes the new lifecycle scenario;
the other real-tool test is the existing Skopeo lock-retention test within the
library suite. Tools were Podman 5.4.2 and Skopeo 1.18.0. Six internal subprocess
helpers and the Docker Hub smoke test remained ignored; no external registry
test was run.

Crate check, Clippy with warnings denied, formatting, the documentation build
with rustdoc warnings denied, and diff checks passed. Repository Markdown links
and generated API navigation were inspected. The original design and plan bodies
and README validation chronology were preserved in the archive. Earlier results
are in [validation history](archive/validation-history.md).

The initial integration run exposed a test-only interaction: the lifecycle
export added another name for the source image ID, changing a later Podman
listing assertion. The lifecycle now untags only its own exported name before
the existing scenario continues; the full suite then passed. Cargo used its
configured cache outside the filesystem sandbox, and real transfers used the
existing disposable overlay-storage harness. No production behavior change or
new implementation issue requiring next-phase resolution was identified in this
bounded review.

Lustre/VAST cross-node coordination and crash durability remain unqualified.
Structural checking does not hash blobs or certify execution. Multi-platform
stored images and public index acquisition/export remain unsupported. Broader
registry/deployment coverage is recorded in [deferred validation](deferred-validation.md)
without preparing external acceptance procedures. Next-phase priorities must be
chosen separately from the archived plan.

## Local checks

Run from the workspace root:

```sh
cargo check --locked -p hpc-image-store
cargo test --locked -p hpc-image-store
cargo clippy --locked -p hpc-image-store --all-targets -- -D warnings
cargo fmt -p hpc-image-store -- --check
RUSTDOCFLAGS="-D warnings" cargo doc --locked -p hpc-image-store --no-deps
git diff --check
```

Ordinary tests include local real-tool tests and subprocess coordination tests.
They need Podman, Skopeo, Python 3, tar, sha256sum, and permission to create
rootless user namespaces and use overlay storage. No network registry test is
selected by ordinary Cargo tests. Run checks once after changes settle; repeat
only affected checks when a failure or subsequent change justifies it.

The suite covers initialization/retry and permissions, publication boundaries,
immutable insertion races, complete nested-index graph traversal, availability
outcomes and acquisition races, prune reachability and grace, unknown-content
preservation, partial removal/sync failures, CLI reports, and process-lock
lifetimes. Synthetic OCI fixtures and narrow test-only fault injection cover
failure paths; readiness handshakes and bounded waits coordinate processes.
Local failure injection demonstrates ordering, not actual crash persistence.

The six ignored library tests (`lock_worker`, `wait_worker`, `publication_worker`,
`transfer_worker`, `pull_worker`, and `recovery_worker`) and the CLI's
`cleanup_warning_worker` are internal subprocess helpers invoked by their owning
ordinary tests. Never select them
directly or use an unfiltered `--ignored` invocation. The independently ignored
Docker Hub test requires explicit selection, as described below.

## Tests with real tools

Run the containers-storage acceptance test directly with Cargo:

```sh
cargo test --locked -p hpc-image-store --test real_transfers -- --nocapture
```

By default, the test caller prepares a fresh temporary overlay storage
configuration, verifies Podman's effective graph root, and launches the
acceptance test in a child process with `CONTAINERS_STORAGE_CONF` set. Podman,
Skopeo, the library, and the CLI all inherit this same context. The library does
not prepare or change storage configuration. The caller resets only its own
disposable storage after the child exits, including after test failure, then
removes its temporary files. No registry access or image download is needed.

To qualify another configuration or driver, prepare an **empty disposable**
containers-storage context yourself, then opt in explicitly:

```sh
export CONTAINERS_STORAGE_CONF=/path/to/disposable/storage.conf
export HPC_IMAGE_STORE_TEST_DISPOSABLE=1
cargo test --locked -p hpc-image-store --test real_transfers -- --nocapture
```

Both tools must select the same disposable storage. With the tested older
rootless Podman, set `rootless_storage_path` as well as `graphroot` in that
configuration. Do not select your ordinary image store: the test retags and
removes images to exercise races and failures. It rejects nonempty storage
because cached layers can mask missing-source export failures. Caller-supplied
storage is left for the caller to clean up and must be recreated or emptied
before another run.

These commands run without an outer `podman unshare`. If the deployment's tools
require an existing rootless namespace, prefix the Cargo command with
`podman unshare` as a fallback. A sandbox that prohibits user namespace setup
must also permit the real-tool subprocesses; wrapping them in `podman unshare`
does not grant that permission.

The test covers library and JSON CLI round trips, OCI compression, shared-layer
reuse, explicit platform selection, replacement, tag movement after inspection,
image removal before copying, missing-content export failure, staging cleanup,
and unchanged configuration. During a real containers-storage export, it also
terminates the CLI parent, proves that the surviving Skopeo retains the single
store lock, and verifies that the exported image is complete after it exits.

The independent real-Skopeo OCI-to-OCI lock test needs no Podman storage and
also runs as part of the ordinary crate suite:

```sh
cargo test --locked -p hpc-image-store actual_skopeo_retains_lock_after_parent_termination -- --nocapture
```

### Docker Hub smoke test (explicit only)

Ordinary `cargo test` and CI do not contact Docker Hub. All registry edge cases
use controlled Skopeo fixtures. The following single ignored test performs one
CLI acquisition of `docker.io/library/alpine:3.23` for `linux/amd64` into a
temporary store, then verifies metadata, inventory, checking, and exports
through real containers-storage. It checks exact manifest bytes and digests
after named and qualified-digest exports through the library and JSON CLI,
including omitted and explicit destinations, matching and mismatching destination
digests, separate configuration image IDs, and unchanged persistent-store state:

```sh
cargo test --locked -p hpc-image-store --test registry_pull dockerhub_alpine_smoke -- --ignored --exact --nocapture
```

Run it at most once per acceptance session and never in a CI matrix. It performs
no preliminary registry probe, additional library pull, automatic retry, or
fallback to another image. Network, authentication, rate-limit, and unsupported
representation failures remain failures and leave live registry validation
incomplete. Existing caller authentication may be inherited without modifying
it. The test uses the same disposable overlay-storage preparation as the local
real-transfer test, with an empty-storage check and cleanup even after failure.
It accepts the same explicit disposable-storage override; caller-supplied storage
is never reset. The temporary image store is discarded when the test exits.
Imported images gain no round-trip digest comparisons.

The six ignored subprocess helpers, including `pull_worker`, are invoked only
by their owning tests. The Docker Hub smoke test is separately ignored for its
network access. Never use an unfiltered `--ignored` invocation.

## Future work: guided in-place repair

Recorded on 3 November 2026 as a tentative capability for PoC v3 or later. This is
a candidate for future design, outside PoC v2, with no approved implementation
plan, delivery commitment, public API, or change to current recovery guarantees.
The motivation is to help users restore a damaged store to working order in
place, preserving healthy images, local names, notes, and provenance instead
of requiring them to discard and recreate the store. Current operational
guidance is in the [user guide](user-guide.md#recovery-failures-and-manual-resolution).

A future assessment should consider:

- Diagnostics that can inspect a store whose pending recovery is blocked,
  while leaving ordinary operation preparation's recovery requirement intact.
- A proposed repair plan and preview that distinguish verified corrections,
  operator decisions, and irrecoverable losses. Preserve unexpected contents
  and recovery evidence, potentially through a documented quarantine mechanism.
- Targeted restoration of missing or damaged immutable objects from trusted
  backups or explicitly selected sources, with verification against the stored
  digest. Mutable tags and cached display metadata must not establish identity.
- Reconciliation of refs, wrappers, and pending records only where the intended
  identity and state can be established. Preserve healthy shared content and
  operational metadata, and require explicit decisions where evidence conflicts.
- Exclusive coordination through the permanent store lock, respect for
  surviving transfer children, and durable, resumable repair work with a clear
  report of changes and remaining failures.

Before implementation, decide which damage classes are supported, what evidence
authorizes each correction, how replacement sources are selected, how quarantine
and backups are managed, and when operator input is required. Validation should
include interrupted repair, preservation of unaffected refs and shared objects,
and honest reporting when original bytes or metadata cannot be recovered. A
repair facility cannot reconstruct information that has been lost without a
trustworthy source; it must not silently drop damaged refs or metadata to report
a healthy store. No general repair is currently implemented.

