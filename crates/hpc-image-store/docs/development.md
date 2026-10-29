# Development and PoC handoff

The first PoC implements single-platform containers-storage import/export,
manifest-preserving registry pull, inventory, structural graph checking,
explicit ref removal, conservative pruning, and library-only availability.
The CLI has seven commands. Store format 1 and ref metadata schema 3 are retained.
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
| Inspection | `discovery`, `inventory`, `inspection`, `graph`, `check`, `availability`; complete graph and availability tests |
| Maintenance | `prune` and its bookkeeping module; reachability, partial-removal, namespace-sync, and lock-reclamation regressions |
| CLI and presentation | `main`, `presentation`; CLI integration tests, human/JSON parity |
| Real tools | `tests/real_transfers.rs`, `tests/registry_pull.rs`, shared disposable-storage and registry fixtures |

`src/lib.rs` includes the library guide in generated crate documentation, so its
Rust examples remain doctests. The crate README is the short navigation entry
point. Keep relative repository links out of the included library guide; use
API references or self-contained explanations so generated rustdoc navigation
also works.

## Current closure record

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

The six ignored unit tests (`lock_worker`, `wait_worker`, `publication_worker`,
`transfer_worker`, `pull_worker`, and `cleanup_warning_worker`) are internal
subprocess helpers invoked by their owning ordinary tests. Never select them
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
terminates the CLI parent, proves that the surviving Skopeo retains both store
and ref locks, and verifies that the exported image is complete after it exits.

The independent real-Skopeo OCI-to-OCI lock test needs no Podman storage and
also runs as part of the ordinary crate suite:

```sh
cargo test --locked -p hpc-image-store actual_skopeo_retains_locks_after_parent_termination -- --nocapture
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

