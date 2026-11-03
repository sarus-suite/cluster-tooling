# PoC v2 acceptance inventory

The [PoC v2 implementation plan](pocv2-implementation-plan.md) defines intended
requirements and stage boundaries. This inventory connects those requirements
to fixtures, observable acceptance cases, test targets, and evidence. The
[architecture](architecture.md) remains authoritative for implemented behavior.
This inventory introduces no independent requirements.

For context, see the plan's [baseline](pocv2-implementation-plan.md#purpose-and-current-baseline),
[stage execution guidance](pocv2-implementation-plan.md#stage-execution-and-validation),
and [scope](pocv2-implementation-plan.md#scope-and-document-handoff).
Stages 1 through 3 supply the implemented evidence below. Unimplemented acceptance
cases remain **Planned**, even where existing tests preserve part of their behavior.

When requirements change, update the plan first, then update affected acceptance
cases here. Record concrete passing evidence for the full case before changing
its status. The [development guide](development.md) records validation runs and
their limits.

## Fixtures and evidence conventions

All source paths below are relative to the crate. `src/test_support.rs` is
compiled only for library tests and exposes:

- **Shared:** `SharedImages`, with original, keeper, and replacement manifests,
  each using a common layer and a unique layer. Object maps contain actual bytes;
  root descriptors and shared/exclusive digest sets are explicit. Fixture hashes
  and sizes are accurate. Layer bytes are synthetic and do not promise runnable
  or valid tar images.
- **Mutation:** `MutationFixture`, owning a temporary store, target and keeper
  refs, original/keeper raw paths, and Shared images. `stage_image` and
  `publish_image` exercise existing storage primitives; primitive publication
  deliberately leaves staging as it does in the existing publication fixtures.
- **Nested:** `NestedImages`, with a top index referencing a nested first
  manifest and a direct second manifest. Both share configuration and layer
  objects. The first manifest is also independently publishable.
- **Prefix:** `prefix_names()`, returning 58 `a` characters plus `:t` and `:tag`.
  The shorter encoded directory is an actual ancestor of the longer one.

**Interrupted** is `interrupted_fixture` in `src/publication_tests.rs`. It uses
Mutation and the existing publication subprocess worker, readiness handshakes,
bounded waits, and termination at acquisition, pre-rename, post-rename/pre-sync,
or post-unlink/pre-sync boundaries. It retains affected refs, expected roots and
object sets through Mutation, observed staging paths, and any new raw-image path.
These began as format-1 remnants and are now recreated as format-2 fixtures,
still without recovery records. Stage 3 adds recorded variants in `recovery::tests`
using the same synthetic images and mutation store. The original unrecorded
variants remain regression evidence for the intermediate public lifecycle.

**Setup**, **Transfer**, and **Registry** mean the existing initialization/lock
workers, controlled transfer and disposable real-tool harnesses, and controlled
registry fixtures. **Snapshot** means the existing no-symlink-following snapshot
helper. Reuse short internal lock thresholds for warnings/timeouts and narrow
fault injection for sync/removal failures. Never run ignored workers directly.

Test targets below use **lib**, **CLI**, **real**, **registry**, and **doc** for
the library, CLI integration, real-transfer integration, controlled-registry
integration, and rustdoc targets. **Review** includes source/document searches.
Each future row is an acceptance obligation, not an ignored executable test.

## Stage 1 evidence

Requirements: [Stage 1](pocv2-implementation-plan.md#1-establish-the-target-contract-and-acceptance-fixtures-implemented)
maps the plan's [target contracts](pocv2-implementation-plan.md#target-contracts)
to acceptance cases and reusable fixtures.

These cases establish fixture suitability and preserve current behavior. The
development guide records the actual validation run.

| ID | Stage | Fixture | Test evidence and expected observation | Status |
| --- | --- | --- | --- | --- |
| FIX-01 | 1 | Shared, Nested | lib: `test_support::synthetic_images_have_exact_descriptors_and_expected_sharing` verifies exact hashes/sizes, shared and disjoint unique sets, two layers per image, and structurally valid published graphs. | Implemented |
| FIX-02 | 1 | Mutation, Snapshot | lib: `publication_tests::layered_replacement_fixture_preserves_shared_objects_and_retired_content` verifies new selected roots, retained old content, and unchanged shared object and keeper wrapper snapshots. | Implemented |
| FIX-03 | 1 | Prefix | lib: `publication_tests::prefix_fixture_supports_both_names_and_a_longer_name_alone` verifies both names, prefix-only state, and longer-name preservation after short-name removal. | Implemented |
| FIX-04 | 1 | Interrupted, Snapshot | lib: `publication_tests::interrupted_layered_mutations_expose_expected_refs_and_preserve_shared_content` verifies old/new/absent ref states, staging/raw paths, owned additions, and unchanged committed content under check. | Implemented |
| FIX-05 | 1 | Nested | lib: `publication_tests::prune_tests::shared_layers_and_complete_nested_indexes_survive_pruning` retains full traversal, damage-blocked deletion, and shared-content retention after index removal. | Implemented |

## Staged acceptance inventory

### Stage 2: coordination and format

Requirements: [coordination and initialization](pocv2-implementation-plan.md#coordination-and-initialization),
[content and format](pocv2-implementation-plan.md#authoritative-content-and-complete-mutations),
and [Stage 2](pocv2-implementation-plan.md#2-simplify-coordination-and-introduce-format-2-implemented).

| ID | Stage | Fixture | Target | Expected observation | Status |
| --- | --- | --- | --- | --- | --- |
| LOCK-01 | 2 | Setup, Mutation | lib | Shared readers coexist; writers exclude readers and writers targeting either the same or a different name. List and all-store check retain stable namespaces throughout inspection. | Implemented |
| LOCK-02 | 2 | Transfer | lib, real | Only the single operation lock is deliberately inherited with the same open-file description; the parent stays close-on-exec. A surviving transfer excludes writers after parent termination until descriptors close. | Implemented |
| LOCK-03 | 2 | Setup | lib | Warning/timeout thresholds and permanent lock inode survive waits, timeouts, initialization races, and retries; no contention path removes a lock. | Implemented |
| LOCK-04 | 2 | Mutation, Nested | lib, CLI | Selection, staging, publication, and all content use retain the store guard; no ref locks or ref-lock directories are created. Intermediate availability/prune still work. | Implemented |
| FORMAT-01 | 2 | Setup, Snapshot | lib, CLI | New stores use format 2/schema 3; format 1 and unsupported versions fail without changing store contents or migrating them. | Implemented |
| SETUP-01 | 2 | Setup | lib | Import/pull initialization races and recognized retries preserve prepared-parent sync ordering, permissions, stable identities, and no ancestor creation; unexpected contents and missing primitives remain errors. Remove retains its intermediate preparation policy. | Implemented |

Stage 2 evidence (test names are relative to their target):

| Cases | Passing evidence |
| --- | --- |
| LOCK-01 | lib: `tests::separate_processes_obey_the_lock_matrix`; `publication_tests::{inventory_retains_stable_namespace_after_enumeration,digest_resolution_retains_candidates_after_namespace_scan,checking_retains_stable_namespace_after_discovery}` use paused readers and actual contended writers. Availability and selected-content tests retain their operation guard through graph/content use. |
| LOCK-02 | lib: `subprocess::tests::copy_inherits_only_its_single_store_descriptor_and_parent_stays_close_on_exec`; `transfer::tests::{actual_skopeo_retains_lock_after_parent_termination,orphaned_transfer_retains_store_lock_and_inherits_environment}`; `pull::tests::terminated_pull_keeps_prior_ref_and_orphan_copy_locks`. Real: `real_rootless_round_trip`, including orphaned export. Tests distinguish surviving writer exclusion from shared export access. |
| LOCK-03 | lib: existing `locks::tests` warning, timeout, interruption, and close-on-exec cases; initialization inode/retry tests; `publication_tests::bookkeeping_tests::store_lock_holders_exclude_cleanup_and_waiters_retain_permanent_identity`. A paused waiter does not itself hold a lock; prune preserves its inode before acquisition and waits once it acquires shared access. |
| LOCK-04 | lib: `publication_tests::shared_guards_reject_mutation_and_metadata_cannot_change_the_destination`, existing publication/interruption and nested-index prune regressions; `publication_tests::bookkeeping_tests::missing_lookups_create_no_bookkeeping_and_namespace_cleanup_requires_the_barrier`. CLI: `prune_cli_cleans_bookkeeping_with_default_grace_and_matches_library_reports` verifies four remaining summary kinds and absence of ref-lock output. |
| FORMAT-01 | lib: `tests::format_two_preserves_schema_three_and_rejects_old_stores_without_changes`; CLI: `unsupported_store_formats_are_rejected_without_modification`. Snapshot comparisons cover format 1 and unsupported version 99 across operations. |
| SETUP-01 | lib: `tests::{concurrent_initialization_and_termination_release_locks,interrupted_scaffolding_can_resume,initialization_syncs_only_the_root_and_its_immediate_parent_in_order,interrupted_initialization_preserves_root_and_lock_identities,obsolete_empty_ref_lock_scaffolding_is_preserved_and_rejected}` plus existing permission, missing-parent, and sync-failure tests. Concurrent setup mixes import and pull; remove retains its initialization privilege. |

### Stage 3: bookkeeping and recovery

Requirements: [recovery](pocv2-implementation-plan.md#recovery-and-query-boundaries),
[mutation durability](pocv2-implementation-plan.md#authoritative-content-and-complete-mutations),
and [Stage 3](pocv2-implementation-plan.md#3-add-durable-operation-bookkeeping-and-recovery-implemented).

| ID | Stage | Fixture | Target | Expected observation | Status |
| --- | --- | --- | --- | --- | --- |
| REC-01 | 3 | Recorded Mutation/Interrupted variants | lib | One active record exists only under exclusive coordination; staging is recorded before acquisition, additions and intent before live mutation, with identities sufficient to distinguish pre-existing shared objects. | Implemented |
| REC-02 | 3 | Recorded Interrupted variants | lib | Before visible publication, repeated recovery removes safe owned additions/staging and preserves the previous ref and every pre-existing shared object. | Implemented |
| REC-03 | 3 | Recorded Interrupted variants | lib | Visible replacement/removal is reconciled even without a completion update; ref namespace durability precedes deletion, persisted cleanup precedes record removal, and no rollback conceals failed syncing. | Implemented |
| REC-04 | 3 | Recorded Interrupted variants | lib | Fault/termination at each record, namespace, cleanup, and record-removal boundary leaves clean or recorded recoverable work; recovery tolerates completed removals and interruption of recovery itself. | Implemented |
| REC-05 | 3 | Setup, recorded Mutation | lib, CLI | Readers release shared access, exclusively recheck/recover, and retry shared preparation; writers recover first; unresolved recovery blocks the requested action with typed context and never invokes check. | Implemented |
| REC-06 | 3 | Transfer, recorded Interrupted variants | lib, CLI | Recovery cannot remove staging while a surviving transfer holds the operation lock. | Implemented |
| REC-07 | 3 | Recorded variants, Snapshot | lib, CLI | Malformed/inconsistent records, escaping pointers, unknown contents, and unrecorded remnants are preserved with contextual errors; recovery performs no general repair or speculative adoption. | Implemented |

Stage 3 evidence (library names below are relative to `recovery::tests`):

| Cases | Passing evidence |
| --- | --- |
| REC-01 | `ownership_is_recorded_before_staging_and_shared_objects_remain_immutable`, `failed_snapshots_never_authorize_unrecorded_live_mutations`, and `fresh_publication_and_abort_finish_owned_namespace_directories` cover the single record, absent staging before acquisition, intent ordering, reused objects, and owned namespace paths. |
| REC-02 | `aborted_publication_recovers_owned_additions_and_preserves_legacy_remnants` covers interruption before publication, repeated preparation, and preservation of old/keeper wrappers, shared objects, unrecorded staging, and the permanent lock inode. |
| REC-03 | `visible_replacement_recovers_forward_without_a_completion_update`, `removal_reclaims_only_unused_content_and_preserves_longer_names`, and `namespace_sync_failure_blocks_all_reclamation_and_keeps_the_visible_ref` establish forward recovery and the namespace barrier. `completed_record_can_be_cleared_after_a_later_namespace_change` limits completed records to clearing bookkeeping. |
| REC-04 | `failed_snapshots_never_authorize_unrecorded_live_mutations`, `snapshot_failures_during_cleanup_and_completion_remain_recoverable`, `cleanup_retries_after_each_removal_and_failed_sync_without_retired_manifests`, `termination_at_record_publication_and_cleanup_boundaries_is_recoverable`, and `recovery_itself_can_be_terminated_and_repeated` cover snapshots, every planned removal, parent syncing, record clearing, and process death. |
| REC-05 | `readers_recheck_after_releasing_shared_and_after_recovery` exercises another process recovering first and another record appearing before shared retry. `clean_preparation_never_traverses_dependencies_or_runs_check` preserves the clean preparation boundary. CLI: `commands_finish_recorded_recovery_before_execution_and_keep_json_clean` and `recovery_errors_block_commands_and_preserve_pending_evidence` cover every command, including dry-run prune, with contextual failures and clean JSON. |
| REC-06 | `a_surviving_transfer_prevents_recovery_from_removing_recorded_staging` terminates the wrapper while a controlled child retains its inherited locked description, observes a reader timeout without store changes, and then releases the child for recovery. Existing real-Skopeo lifetime tests remain regression coverage. |
| REC-07 | `malformed_records_and_reserved_entries_are_preserved`, `inconsistent_namespace_unknown_tree_and_escaping_pointer_block_cleanup`, and `cleanup_snapshot_rejects_escaping_unauthorized_and_omitted_entries` compare preserved snapshots. `unreadable_or_unsupported_live_dependencies_block_reclamation` and `dependency_cycles_and_unknown_branches_cannot_authorize_cleanup` cover incomplete safety proofs. `nested_reachability_preserves_all_live_branches_without_opening_terminals_or_metadata` verifies traversal and open-file boundaries. CLI preservation is covered by `recovery_errors_block_commands_and_preserve_pending_evidence`. |

### Stage 4: complete mutators

Requirements: [complete mutations](pocv2-implementation-plan.md#authoritative-content-and-complete-mutations),
[recovery](pocv2-implementation-plan.md#recovery-and-query-boundaries), and the
[import](pocv2-implementation-plan.md#41-import),
[pull](pocv2-implementation-plan.md#42-pull), and
[remove](pocv2-implementation-plan.md#43-remove) substages of Stage 4.
Removal's initialization policy follows the
[coordination contract](pocv2-implementation-plan.md#coordination-and-initialization).

| ID | Stage | Fixture | Target | Expected observation | Status |
| --- | --- | --- | --- | --- | --- |
| IMPORT-01 | 4.1 | Mutation, Transfer | lib, CLI, real | Import and replacement finish durable staging cleanup and unused-content reclamation; keeper objects and needed parents survive; success has no cleanup-warning field or CLI warning path. | Planned |
| IMPORT-02 | 4.1 | Interrupted, Transfer | lib, CLI | Acquisition, publication, cleanup, deletion, and sync failures remain recorded/recoverable, identify visible changes, and preserve previous refs/shared objects before commit. | Planned |
| IMPORT-03 | 4.1 | Transfer | lib, real | Source-ID pinning, explicit replacement, naming, platform selection, provenance, and allowed import representation changes retain existing behavior. | Planned |
| CONTENT-01 | 4.1 | Mutation, Snapshot | lib | CAS reuse preserves bytes/inodes; insertion never overwrites; unsupported hard links and other required primitives fail without weaker fallback. Complete wrappers and confined relative pointers remain enforced. | Planned |
| CONTENT-02 | 4.1 | Nested, Shared | lib | Reachability follows authoritative roots and every manifest/index branch; shared objects survive. Unreadable or unsupported declarations preserve unsafe-to-delete content with errors. | Planned |
| CONTENT-03 | 4.1 | Mutation, fault instrumentation | lib | Caches, provenance, timestamps, previous checks, or reference counts never authorize deletion; mutation reclamation calls neither prune nor check and does not open/hash every layer, decompress, or check remote freshness. | Planned |
| CONTENT-04 | 4.1 | Interrupted | lib | Namespace syncing precedes retired-content deletion; partial deletion/sync failure remains visible and recorded; cleanup is persisted before clearing the record or returning success. | Planned |
| PULL-01 | 4.2 | Mutation, Registry | lib, registry, CLI | Pull/replacement finish the same lifecycle; shared objects survive; cleanup failures are errors. Pull warning fields/CLI handling and the now-unused shared warning type disappear. | Planned |
| PULL-02 | 4.2 | Registry, Interrupted | lib, registry, CLI | Pinned manifest acquisition, digest preservation, platform selection, registry settings and provenance survive failure/recovery; missing capabilities and unsupported representations fail without weaker retry or conversion. | Planned |
| REMOVE-01 | 4.3 | Setup, Snapshot | lib, CLI | Missing roots/parents, empty directories, and interrupted setup remain unchanged, without lock bookkeeping; initialized stores support recovery and retain the missing-ref error. | Planned |
| REMOVE-02 | 4.3 | Shared, Mutation | lib, CLI, real | Removing one ref preserves shared content; removing the last use durably reclaims blobs, raw images, staging, and unnecessary namespace directories before success, without grace or prune. | Planned |
| REMOVE-03 | 4.3 | Prefix | lib, CLI | Removing the short name leaves its terminal ref absent and preserves longer names, their content, and necessary namespace parents. | Planned |
| REMOVE-04 | 4.3 | Interrupted | lib, CLI | Partial deletion and failed sync report visible changes, retain recovery information, and never report premature success or roll back. Removal still requires `LocalRef` and rejects digest selectors. | Planned |

### Stage 5: lightweight existence

Requirements: [query boundaries](pocv2-implementation-plan.md#recovery-and-query-boundaries)
and [Stage 5](pocv2-implementation-plan.md#5-introduce-lightweight-existence),
including its CLI status and output contract.

| ID | Stage | Fixture | Target | Expected observation | Status |
| --- | --- | --- | --- | --- | --- |
| EXISTS-01 | 5 | Prefix, Mutation | lib, CLI | Exact terminal entries count; directory-only prefixes do not; dangling terminal pointers count as present without following the target. Explicit check still diagnoses their damage. | Planned |
| EXISTS-02 | 5 | Shared, damaged wrapper variants | lib | Digest lookup reads authoritative roots through confined pointers, ignores cached aliases, preserves literal repository scope and tag/digest precedence, and accepts a definite match. | Planned |
| EXISTS-03 | 5 | Damaged discovery/index variants | lib, CLI | Incomplete relevant enumeration or unreadable candidate indexes cannot establish absence; known damage in another repository does not obscure a definite miss. | Planned |
| EXISTS-04 | 5 | Snapshot, narrow instrumentation | lib | Clean lookup reserves nothing, initializes nothing, mutates no image data, opens no blobs, traverses no graph, validates no unrelated wrappers, and invokes no tools or remote requests. | Planned |
| EXISTS-05 | 5 | Setup, recorded Interrupted variants | lib, CLI | Missing/uninitialized stores, permission/lock failures, incomplete digest resolution, and unresolved recovery are operational errors; pending recovery runs before lookup and may perform recorded cleanup. | Planned |
| EXISTS-06 | 5 | Mutation, Setup | CLI | Present/absent/error/invalid usage return exactly 0/1/125/2 with empty stdout, both with and without `--json`; result silence, contextual errors, lock warnings, and normal help behavior match the contract. Other commands retain exit behavior. | Planned |
| EXISTS-07 | 5 | Selector cases, source review | lib, CLI, doc | New bool API replaces availability and diagnostics; examples compile; CLI calls the library without Podman and retains existing selector grammar without IDs, bare digests, normalization, implicit tags, or aliases. | Planned |

### Stages 6–8: retirement, list, and closure

Requirements: [query and interface boundaries](pocv2-implementation-plan.md#recovery-and-query-boundaries),
[coordination and initialization](pocv2-implementation-plan.md#coordination-and-initialization),
[Stage 6](pocv2-implementation-plan.md#6-remove-prune),
[Stage 7](pocv2-implementation-plan.md#7-enable-list-to-initialize-a-store), and
[Stage 8](pocv2-implementation-plan.md#8-validate-poc-v2-and-update-documentation).
The plan's [execution guidance](pocv2-implementation-plan.md#stage-execution-and-validation)
and [scope](pocv2-implementation-plan.md#scope-and-document-handoff) define
intermediate limits and exclusions.

| ID | Stage | Fixture | Target | Expected observation | Status |
| --- | --- | --- | --- | --- | --- |
| PRUNE-01 | 6 | CLI and API examples | lib, CLI, doc, Review | Prune types/method/command/formatters/grace parsing and obsolete bookkeeping disappear; old command is invalid usage. Current instructions/examples no longer prescribe prune; archives remain intact. | Planned |
| PRUNE-02 | 6 | Nested, Interrupted, unknown-content variants | lib, Review | Reachability, namespace-sync, partial-removal, unknown-content, durability, and permanent-lock regressions survive under mutations/recovery; no successful path depends on prune. | Planned |
| LIST-01 | 7 | Setup | lib, CLI | Missing, empty, and recognized interrupted stores initialize as valid format-2 stores and return complete empty inventories without tools or check. | Planned |
| LIST-02 | 7 | Setup workers | lib, CLI | Concurrent lists and list/import initializers converge without deadlock or partial success; exclusive setup precedes shared operation locking and revalidation. | Planned |
| LIST-03 | 7 | Setup, Snapshot | lib, CLI | Missing parents, unexpected data, unsupported formats, permissions, and sync faults fail contextually without ancestor creation or data deletion; existing permissions and initialization ordering remain intact. | Planned |
| LIST-04 | 7 | Mutation, recorded Interrupted variants | lib, CLI | Initialized list retains shared concurrency, recovery preparation, and existing inventory reports. Remove, exists, export, and check still cannot initialize stores. | Planned |
| CLOSE-01 | 8 | Disposable Transfer, Registry, Shared | Full suite | Fresh listing, exists, import/pull, replacement, export, explicit check, shared retention and immediate removal form a complete lifecycle without retired operation content or prune. Controlled registry coverage suffices; no live registry requirement is added. | Planned |
| CLOSE-02 | 8 | Transfer, recorded Interrupted variants | Full suite | Real-tool parent termination and surviving-child lock lifetimes remain covered alongside recovery; every target case has passing evidence. | Planned |
| CLOSE-03 | 8 | Documentation and examples | doc, Review | README, architecture, guides, instructions, rustdoc and links describe verified behavior, format boundary, concurrency tradeoff, initialization, cleanup errors/recovery, exists, and explicit check limits. Historical archives remain unchanged. | Planned |
| CLOSE-04 | 8 | Validation record | Review | Actual results, tool versions, failures/skips and environment are recorded. Local tests do not claim crash persistence or Lustre/VAST qualification; same-size corruption and invalid tar/compressed data remain documented limits. | Planned |
