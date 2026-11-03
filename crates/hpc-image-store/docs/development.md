# Development and PoC handoff

The first PoC implements single-platform containers-storage import/export,
manifest-preserving registry pull, inventory, structural graph checking,
explicit ref removal, conservative pruning, and library-only availability.
The CLI has seven commands. Store format 2 and ref metadata schema 3 are used.
PoC v2 Stage 2 replaces per-ref locks with one permanent store lock; the content
lifecycle was retained through Stage 3, which adds durable mutation records and
automatic recovery during operation preparation. Stages 4.1 through 4.3 integrate
import, pull, and remove: success includes durable cleanup and immediate
unused-content reclamation. Only import and pull may initialize stores; list
initialization remains planned for Stage 7.
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

## PoC v2 Stage 4.3: complete recorded removal

Stage 4.3 is complete. Public removal now uses `Kind::Remove` through the
existing mutation protocol, retaining one exclusive guard from preparation
through durable cleanup and record clearing. Only import and pull initialize
stores; removal preserves missing roots, empty directories, and interrupted
setup unchanged. List still requires a completed store until Stage 7.
`LocalRef`, `RemoveReport`, CLI result formats, store format 2, ref schema 3,
and mutation snapshot version 1 are unchanged. Availability and prune remain.

Removal records a canonical old pointer and authoritative dependencies before
unlinking. Damaged target identities or declarations reject the operation;
damaged display metadata alone does not block it. Success immediately reclaims
unused recorded wrappers, blobs, and namespace tails while preserving shared
content, longer names, permanent store structure, and unrecorded remnants.
Failures after recording retain typed causes in `Error::Recovery`, including
potentially visible `RefRemoved` state. Recovery abandons pre-unlink removal or
finishes visible removal forward, without rollback, tools, check, or prune.

The agreed order deliberately has no extra reachability preflight. Another
ref's unreadable declarations can block cleanup after the name is gone. Pending
recovery then blocks ordinary operations, including list, check, remove, and
prune, even for healthy images. The architecture and user/library/administration
guides explain the pitfall and preserving recovery evidence. The future repair
note below records restoring ordinary access as a PoC v3-or-later concern;
PoC v2 adds no bypass or repair interface.

REMOVE-01 through REMOVE-04 in the acceptance inventory identify concrete tests.
Eight public-removal library tests cover setup snapshots, sharing and encoded
prefixes, nested dependencies, cache independence, conservative rejection,
blocked recovery, fault boundaries, and process termination. Tests verify every
planned deletion and parent sync, including empty namespace tails after the
last ref. CLI permission tests run without root's DAC override, return clean
failure output in both modes, and recover through repeated JSON list commands
with producer executables removed. Prune tests construct explicit legacy
remnants; the real-transfer lifecycle now checks immediate reclamation before
maintenance rather than depending on prune.

On 3 November 2026, the full crate suite passed 251 tests: 211 library tests,
27 CLI tests, one real-transfer test, five controlled-registry tests, and seven
doctests. The six library subprocess helpers and one CLI helper remained ignored
by top-level runners and were invoked only by their owning tests. The separately
ignored Docker Hub smoke test was not selected; no live registry was contacted.
Real transfers used the documented disposable overlay-storage harness with
Podman 5.4.2 and Skopeo 1.18.0.

Crate check, all-target Clippy with warnings denied, formatting verification,
rustdoc with warnings denied, and diff checks passed. After the complete suite,
the every-deletion retry test was expanded to include last-ref namespace cleanup
and passed separately; all-target Clippy was rerun successfully. Current local
Markdown links and generated API navigation were reviewed. Cargo required access
to its configured cache outside the filesystem sandbox. Initial integration
runs exposed old lifecycle expectations in library/CLI fixtures and an
unsupported-content fixture that attempted to recreate existing wrapper files;
these were corrected before the passing runs.

Local tests establish ordering, coordination, and retry behavior, not power-loss
persistence or Lustre/VAST cross-node qualification. Unrecorded remnants still
need explicit maintenance, and structural damage can block recorded recovery
without a built-in resolution. Stages 5 through 8 remain planned.

## PoC v2 Stage 4.2: complete pull mutations

Pull now uses the same recorded lifecycle as import, through the existing
`Kind::Pull` protocol. Success includes durable publication, unused-content
reclamation, staging cleanup, and record removal. Store format 2, ref metadata
schema 3, and mutation snapshot version 1 are unchanged. Pull's warning field,
CLI warning handling, and shared warning type are removed. Remove retains its
intermediate behavior until Stage 4.3; availability and prune remain public.

Source resolution and capability checks still happen before recording begins.
Later failures retain their typed cause inside `Error::Recovery`. Replacement
requires the same canonical old pointer and authoritative dependencies as import;
damaged cached metadata alone does not block reclamation. Recovery abandons
unfinished acquisition or finishes visible publication forward, without registry
requests, transfer retries, or rollback. The architecture is authoritative;
PULL-01 and PULL-02 in the acceptance inventory identify the concrete evidence.

The controlled registry fixture can select independent accurately hashed layouts
with shared and unique content. Public pull tests cover fresh, changed-content,
and identical-content acquisition, encoded prefixes, nested keepers, damaged
old refs, cache independence, unprovable reachability, acquisition validation,
filesystem faults, and process termination. Surviving copy children exclude
query-triggered recovery until they release the operation's inherited lock.
CLI cleanup tests exercise removal and sync permission failures after pull
publication in both output modes, then recover through repeated `list` with the
transfer executable removed. Successful JSON reports omit `cleanup_warnings`.

On 3 November 2026, the complete Stage 4.2 crate suite passed 241 tests:
203 library tests, 25 CLI tests, one real-transfer test, five controlled-registry
tests, and seven doctests. Focused pull and CLI/registry targets also passed
before the complete run. The six library subprocess helpers and one CLI helper
remained ignored by top-level runners and were invoked only by their owning
tests. The independently ignored Docker Hub smoke test was not selected; no
live registry was contacted. Existing real-tool regressions used Podman 5.4.2
and Skopeo 1.18.0 with the documented disposable-storage harness.

Crate check, all-target Clippy with warnings denied, formatting verification,
rustdoc with warnings denied, and `git diff --check` passed. Current local
Markdown paths/anchors and generated API navigation were reviewed successfully.
Cargo used its
configured build cache outside the filesystem sandbox. The initial sandboxed
focused invocation could not open that cache's lock and was rerun with cache
access. The first compile exposed an obsolete helper assertion and a fixture
lifetime annotation; both were corrected. An initial metadata-damage test
incorrectly used the metadata-reading ref helper after corrupting that cache;
it now reads the authoritative wrapper index directly. All corrected cases
passed in the complete suite.

These results establish local ordering and recovery behavior, not power-loss
persistence or Lustre/VAST qualification. Automatic recovery still does not
repair external damage or adopt unrecorded remnants.

## PoC v2 Stage 4.1: complete import mutations

Import now creates the existing version-1 mutation snapshots and finishes its
recorded lifecycle before success. The authoritative behavior is in the
architecture; IMPORT-01 through IMPORT-03 and CONTENT-01 through CONTENT-04 in
the acceptance inventory identify the public-import and retained primitive
coverage. Pull warning reports, legacy removal, availability, and prune remain
until their later substages. Store format 2 and ref metadata schema 3 are unchanged.

The agreed replacement policy rejects malformed old entries or unreadable old
dependency declarations before publication. It does not extend recovery into
repair; damaged display metadata alone does not authorize or prevent reclamation.
Errors after recording starts retain their underlying typed cause through
`Error::Recovery`. Callers must no longer match transfer errors only at the
outer level. Import's report and JSON omit `cleanup_warnings`.

Completed import and pull acquisitions also validate private staged declarations
with recovery's dependency parser before publication intent or live insertion.
The validated dependency list supplies object identities and deduplicated sizes;
config inspection still supplies metadata. This prevents subject/artifact-bearing
or otherwise unsupported declarations from being published and then blocking
cleanup. Cached structural and JSON findings now return contextual
`Error::InvalidObject` values instead of panicking; operational I/O errors retain
their typed cause chain. Unsupported content already published by an earlier
implementation remains preserved with a recovery error, without automatic repair.

Interrupted Skopeo copies can leave a regular `layout/oci-put-blob<digits>` file.
Recorded recovery recognizes this producer artifact only inside the recorded
operation's real layout directory, using the same grammar for initial cleanup
and saved cleanup plans. The architecture defines the allowance; unknown
contents and redirected or special entries remain preserved with errors.
Unrecorded staging and conservative prune retain their existing behavior.

The controlled transfer harness consumes the shared/unique content fixtures.
Tests cover public replacement and identical-content replacement, encoded
prefixes, nested index keepers, unknown staging contents, immutable sharing,
damaged-ref rejection, typed acquisition errors, filesystem fault boundaries,
and termination during publication and cleanup. Synthetic recovery cases also
cover empty and partial producer temporary files, durable cleanup-plan reload,
deletion/sync retry, malformed names, unexpected locations/types, unknown
siblings, and unrecorded-tree preservation. Surviving transfer children block
reader recovery until their inherited store descriptor closes. CLI tests use
permission failures on recognized staging entries and prove failure status,
empty stdout, and successful later recovery after permissions are restored.

The real-Skopeo interruption regression streams part of a layer through a FIFO,
observes the actual destination temporary file before SIGKILL, and waits for
the operation worker to reap the producer and exit. It then recovers through
repeated `list` and compares committed refs, raw images, CAS bytes/inodes, and
the permanent lock identity. Readiness and termination use bounded waits;
teardown stops producer and worker processes on failure. This OCI-to-OCI test
needs no Podman storage or registry. A second real-Skopeo regression copies an
accurately hashed subject-bearing fixture, verifies that Skopeo retains its
`subject` field, and proves rejection in private recorded staging followed by
successful cleanup through repeated `list`. Controlled imports cover fresh and
replacement rejection for unsupported objects, roles, descriptor extensions,
media-type mismatches, and conflicting declarations. Cached structural/JSON and
I/O error tests exercise repeated dependency calls; open-file tracking verifies
that the declaration pass does not open terminal objects. An already published
unsupported fixture proves repeated `list` and `remove` return recovery errors
while preserving the complete store snapshot.

On 3 November 2026, the complete Stage 4.1 crate suite passed 236 tests:
198 library tests, 25 CLI tests, one real-transfer test, five controlled-registry
tests, and seven doctests. Six focused declaration-validation and recovery
regressions also passed before the complete run. Six library subprocess helpers
and one CLI helper remained ignored by top-level runners and were invoked by
their owning ordinary tests. The independently ignored Docker Hub smoke test
was not selected; no live registry was contacted. The real-Skopeo OCI-to-OCI
regressions used Skopeo 1.18.0. Real transfers used the existing disposable
overlay-storage harness with Podman 5.4.2 and Skopeo 1.18.0. Cargo used its
configured build cache outside the filesystem sandbox.

Crate check, all-target Clippy with warnings denied, formatting verification,
rustdoc with warnings denied, `git diff --check`, and current local Markdown
links/anchors passed. The generated API navigation was reviewed. The first
sandboxed focused-test invocations could not open Cargo's external build-cache
lock; reruns with cache access and all subsequent checks passed.
Existing staged changes were preserved.

During the initial Stage 4.1 focused tests, an old fixture used a config
descriptor as a layer, which the dependency traversal correctly rejected; the
duplicate-layer fixture now uses supported layer descriptors. Another new
assertion attempted to snapshot a deliberately missing wrapper and was corrected
to compare its absence. Both regressions passed in the complete suite; no
required check remains failed or skipped.

These tests establish local ordering and recovery behavior, not power-loss
persistence or Lustre/VAST cross-node qualification. General repair and
reclamation of unrecorded remnants remain outside automatic recovery.

## Pending snapshot naming and lifecycle clarification

The active snapshot is now `pending-mutation.json`, and its temporary replacement
is `.pending-mutation-next.json`. The private layout helpers are
`pending_mutation()` and `pending_mutation_next()`. The mutation module, operation
and record types, public errors, JSON schema, format versions, protocol, and
fault-checkpoint identifiers retain their existing meanings. The architecture
and guides explicitly distinguish normal execution, clean idle state, and
interrupted work, including the single active record and absence of a persistent
recovery queue. See the [architecture](architecture.md#recorded-mutations-and-automatic-recovery)
for the authoritative lifecycle. Public mutators were still unrecorded at that
validation; Stage 4.1 above supersedes import behavior.

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
`cleanup_failure_worker` are internal subprocess helpers invoked by their owning
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

The independent real-Skopeo OCI-to-OCI lock test and the
`killed_real_skopeo_temporary_blob_recovers_on_list` and
`real_skopeo_subject_output_is_rejected_in_private_recorded_staging` regressions
need no Podman storage. All run as part of the ordinary library suite. The
existing lock-lifetime test can also be selected separately:

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

Stage 4.3 retains the existing removal order: record intent, durably unlink the
target, then prove remaining reachability and reclaim content. A damaged
remaining ref can block cleanup after the name is gone, leaving a pending record
that prevents ordinary access even to healthy images. If that damage cannot be
resolved, PoC v2 provides no built-in recovery path to restore access. Assessing
ways to avoid or resolve this lockout belongs to PoC v3 or later, alongside the
repair work below. Stage 4.3 deliberately adds no preflight traversal, diagnostic
bypass, or repair facility.

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

