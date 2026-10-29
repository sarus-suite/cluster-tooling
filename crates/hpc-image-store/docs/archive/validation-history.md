# Historical validation results

> Archived from the crate README on 28 October 2026. These dated results
> describe earlier implementations and environments. They are not current
> checks or instructions for future work. See the
> [development guide](../development.md) for the PoC closure record and
> [deferred validation](../deferred-validation.md) for qualification limits.


On 2026-10-22, unused-ref bookkeeping cleanup passed 190 ordinary tests,
including seven doctests and both existing real-tool tests with Podman 5.4.2
and Skopeo 1.18.0. Crate check, Clippy with warnings denied, formatting, and
diff checks passed. Six internal subprocess helpers and the Docker Hub smoke
test were ignored; no external registry test was run. Cargo validation used
the configured build cache outside the sandbox.

Default prune now immediately removes recognized unused ref locks and empty ref
and lock directories while leaving image content subject to its grace period.
Coverage includes the three-ref example, dry-run/library/CLI parity, shared
encoded prefixes, missing-ref lookup locks, unknown lock subtrees and hard links,
inspection failures, partial removal and syncing failures, and safe retry.
A real contended ref-lock waiter retains the store lock even after its original
holder exits; tests verify exclusion, lock recreation, and mutual exclusion in
two cleanup cycles. Existing orphaned import, pull, and export tests now verify
that unused locks survive while transfer children run and can be cleaned after
they exit. Committed ref locks and the permanent store lock retain their
identities. Public reports gain three bookkeeping kinds; store format 1 and
metadata schema 3 remain unchanged. Cross-node coordination and crash durability
still require site qualification. Earlier records below remain historical.

On 2026-10-21, the stage-7 namespace durability correction passed 182 ordinary
tests, including seven doctests and both existing real-tool tests with Podman
5.4.2 and Skopeo 1.18.0. Crate check, Clippy with warnings denied, formatting,
and diff checks passed. Six internal subprocess helpers and the Docker Hub
smoke test were ignored; no external registry test was run.

Destructive prune now persists every scanned ref directory before reclaiming
eligible content. Regressions cover failed removal and replacement syncs, a
publisher terminated after rename, failures at empty leaf and intermediate
namespace directories and `refs/`, successful retry, and lock retention during
the barrier. A child-local seccomp filter forces actual CLI `fsync` calls to
return `EIO`, verifying human and JSON failure reports without adding production
fault controls. Dry-run and operations without eligible candidates skip the
barrier. Local fault injection verifies ordering and failure handling; crash
persistence and Lustre/VAST behavior still require deployment-site testing.
Earlier validation records below describe the implementation at those points.

On 2026-10-21, stage 7 passed 177 ordinary tests, including seven doctests
and both existing real-tool tests using Podman 5.4.2 and Skopeo 1.18.0. Six
internal subprocess helpers and the Docker Hub smoke test were ignored; no
external registry test was run. Crate check, Clippy with warnings denied,
formatting, and diff checks passed. Cargo checks used the configured build cache
outside the sandbox, and real-tool acceptance used disposable local storage.

Pruning is implemented through the library and CLI. Verified coverage includes
shared layers after ref removal, complete nested indexes, descriptor conflicts
across refs, damaged and incomplete scans, dry-run preservation without ref-lock
files, grace boundaries, future timestamps, logical-byte accounting with hard
links, unknown trees and symlinks, partial deletion and sync failures, permission
failures, import/pull remnants, and both directions of lock exclusion. Existing
orphaned transfer tests also exercise public prune lock timeouts. Store format 1
and ref metadata schema 3 are unchanged. Stage 8 qualification and Lustre/VAST
site testing remain pending. Earlier records below retain their historical
meaning.

On 2026-10-21, stage 6.3 passed 158 ordinary tests, including six doctests and
both existing real-tool tests using Podman 5.4.2 and Skopeo 1.18.0. Six internal
subprocess helpers and the Docker Hub smoke test were ignored. Availability is
implemented and remains library-only. It shares typed wrapper and graph
inspection with checking, preserves existing checker reports and CLI behavior,
and performs no automatic acquisition or recovery. The suite covers every
availability outcome, operational failures, incomplete digest resolution,
multiple findings, preservation, process coordination, and destination conflicts
after a definite miss. After the final classification and cached-cause test
refinements, all 15 focused availability tests and eight graph tests passed.
Crate check, Clippy with warnings denied, formatting, and diff checks passed.
Earlier validation records below retain their historical meaning.
Pruning, broader stage-8 qualification, and Lustre/VAST site testing
remain pending; store format 1 and ref metadata schema 3 are unchanged.

On 2026-10-21, stage 6.2 passed 142 ordinary tests, including five doctests
and both existing real-tool tests. Six internal subprocess helpers and the
Docker Hub smoke test were ignored in that run. Crate check, Clippy with warnings
denied, formatting, and diff checks passed. Controlled export tests cover both
acquisition kinds, genuinely pulled refs, required capabilities, preservation
failures without fallback, selector and destination semantics, CLI failures,
stored-index rejection, and unchanged persistent-store snapshots. Import still
works without preservation capability, and no imported-image round-trip digest
comparisons were added.

The extended Docker Hub test was explicitly run once and passed using Podman
5.4.2 and Skopeo 1.18.0 in fresh disposable overlay storage. Its single
`docker.io/library/alpine:3.23` acquisition for `linux/amd64` retained manifest
`sha256:1f3591b8a02ea153f41c5bba878ad477f63ab3d19349762cb77504db02a23e15`
through four exports using library and JSON CLI calls, named and qualified-digest
sources, and omitted and explicit destinations. Exact manifest bytes and digest
matched after each export. A matching destination digest succeeded, a mismatching
digest failed, and Podman's image ID matched the separately recorded OCI
configuration digest. Persistent-store snapshots and caller configuration
remained unchanged. There were no registry probes, retries, or additional pulls;
test-owned storage was cleaned up. Stage 6.3 availability, pruning, broader
stage-8 qualification, and Lustre/VAST site testing remain pending. Store format 1
and ref metadata schema 3 are unchanged. The records below describe earlier stages
and retain their historical qualification limits.

On 2026-10-21, stage 6.1 passed 138 ordinary tests, including five doctests and
both existing real-tool tests. Six internal subprocess helpers and the Docker
Hub smoke test were ignored in that run. Crate check, Clippy with warnings
denied, formatting, and diff checks passed. Real local acceptance used Podman
5.4.2 and Skopeo 1.18.0 with fresh disposable overlay storage.

The Docker Hub smoke test was explicitly run once and passed, storing
`docker.io/library/alpine:3.23` for `linux/amd64` as manifest
`sha256:1f3591b8a02ea153f41c5bba878ad477f63ab3d19349762cb77504db02a23e15`.
Its metadata, inventory, stored-root selection, and graph checks passed locally.
No registry retries or additional pulls were performed. Controlled tests cover
registry resolution, platform policy, option forwarding, CLI/library parity,
failed replacement, publication and cleanup faults, concurrent pulls, and
orphaned-copy lock retention. Store format 1 and ref metadata schema 3 are
unchanged. Stage 6.2 export qualification, pruning, and Lustre/VAST site testing
remain pending.


On 2026-10-20, stage 5 passed 120 tests, including four doctests and both
real-tool tests, with the existing five internal subprocess helpers ignored.
Crate check, Clippy with warnings denied, formatting, and diff checks passed.
The full suite used Podman 5.4.2 and Skopeo 1.18.0 with fresh disposable overlay
storage. After the full run, the final graph-fixture refinements and added
documentation example passed targeted graph tests and all four doctests.
No Lustre or VAST qualification was performed. Store format 1 and ref metadata
schema 3 remain unchanged; stage 6 is next.

On 2026-10-20, stage 4.1 passed 103 tests, including three doctests and both
real-tool tests, with five internal subprocess helpers ignored. Crate check,
Clippy with warnings denied, formatting, and diff checks passed. Cargo checks
used its configured build cache outside the sandbox, and real-tool tests used
fresh disposable overlay storage with Podman 5.4.2 and Skopeo 1.18.0.

Stage-4.1 coverage includes literal selector parsing and registry ports,
repository-scoped lookup, deterministic duplicate-root selection, damaged and
incomplete scans, lock retention during concurrent changes and orphaned copies,
source-derived tag collisions and replacement, ignored import destination
digests, and early CLI/library validation. Real acceptance verified imports by
containers-storage manifest digest, source-derived tags, qualified stored-root
exports, and matching and mismatching export destination digests. Skopeo may
reject a mismatch before copying or during its destination digest check; both
failures are propagated. The fixture empties its verified disposable image
cache before the missing-content test because digest destinations can retain
additional names. Store format 1 and ref metadata schema 3 are unchanged.

On 2026-10-18, stage 4 passed the full crate suite: 96 tests passed, including
three doctests and both real-tool tests, with five internal subprocess helpers
ignored. The rootless containers-storage test used Podman 5.4.2 and Skopeo
1.18.0 with fresh disposable storage. It verified export by root digest and
that the displayed OCI config ID matches Podman's image ID. Crate check,
Clippy with warnings denied, formatting, and diff checks passed.

New fixtures cover damaged and incomplete inventories, missing CAS content,
schema-2 rejection without modification, partial JSON results (including
non-UTF-8 paths), formatter ages and sizes, duplicate-root selection, and
explicit damaged-ref removal. Subprocess handshakes exercise replacement and
removal after enumeration, retention of the selected ref lock, and orphaned
Skopeo lock retention for digest export. These are local results; no Lustre or
VAST qualification was performed.

On 2026-10-17, the optional-destination refinement passed the complete crate
suite: 74 tests passed, including doctests and both real-tool tests, with five
subprocess helpers ignored. The real containers-storage round trip used Podman
5.4.2 and Skopeo 1.18.0 outside the sandbox. Cargo check, Clippy with warnings
denied, formatting, and diff checks passed. Focused fixtures cover omitted
destinations, invalid implicit import refs, replacement, and export defaults
that differ from the recorded acquisition source.

On 2026-10-16, local acceptance passed in the integration devcontainer with
Podman 4.3.1 and Skopeo 1.9.3 using rootless native overlay. The real
containers-storage test passed both directly and with an outer `podman unshare`;
the direct run exercised both library operations and CLI commands. An outer
`podman unshare` was unnecessary in this environment.

The complete Cargo suite passed with both real-tool tests enabled: 70 tests
passed, with only the five subprocess helpers marked ignored. Cargo check,
Clippy with warnings denied, formatting, and diff checks also passed. No
production transfer changes or Bash harnesses were needed. Cleanup failure
warnings and unexpected index rejection remain covered by controlled fixtures.
FUSE overlay remains unverified because this container has no `/dev/fuse`.

Earlier checks with Podman 4.9.5 and Skopeo 1.14.4 encountered a `newuidmap`
permission failure; those historical environment limits do not describe this
successful run. In the current sandbox, namespace setup initially failed with
`/proc/.../setgroups: permission denied`; the real containers-storage tests
passed after running outside that sandbox. The first disposable configuration
also exposed Podman's need for `rootless_storage_path`, and reusing storage
from a failed run exposed cached-layer interference. The Cargo fixture now
prepares fresh storage and checks its selection and initial emptiness.

Local tests do not qualify Lustre or VAST locking, cross-node visibility,
atomic rename, or durability guarantees. Those require deployment-site
filesystem testing, and no site filesystem support is claimed here. If a
required filesystem primitive is unavailable, the library returns an error
without substituting a weaker fallback.
