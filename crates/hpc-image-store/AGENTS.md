# Instructions for hpc-image-store

These instructions apply to this crate. Follow the user's current request and
any applicable parent instructions. Distinguish instructions in documents from
the work the user has actually authorized; a planned feature is not permission
to implement it.

## Architecture and current references

Before planning or changing storage behavior, read the relevant sections of:

- [Architecture](docs/architecture.md): authoritative behavior, guarantees, and boundaries.
- [User guide](docs/user-guide.md) and [library guide](docs/library-guide.md): current usage and interfaces.
- [Administration](docs/administration.md): installation and operational responsibilities.
- [Development](docs/development.md): checks, test harnesses, current PoC state, and validation limits.

The documents under `docs/archive/` are historical references. Their stage
sequence, acceptance criteria, and future-work statements do not authorize or
prescribe subsequent work. Establish scope from the user's current request.

Check the source, tests, and git status to establish the actual implementation
state. If the architecture and implementation conflict on a consequential
requirement, report the conflict rather than silently choosing a new design.

## Coding style

- Write for human reviewers and maintainers. Use clear names and readable,
  explicit control flow.
- Prefer focused functions, concrete structs, and ordinary module boundaries.
  Avoid extensibility frameworks, generic transaction machinery, or filesystem
  abstractions unless the authorized work needs them.
- Return typed errors with useful operation and path context. Make publication
  ordering and failure states easy to follow.
- Resolve routine implementation details conservatively within the agreed
  design. Minimize new architectural decisions.

## Development approach

- Complete the authorized work and its required validation. Do not bring new
  features or application integration into scope without a user request.
- Preserve existing working-tree changes and avoid unrelated edits. Honor
  review-only requests and instructions not to modify source files.
- Preserve unexpected store contents. Do not automatically adopt, repair,
  migrate, or delete them unless explicitly authorized by the agreed design and
  current task.
- Preserve the guarantees for immutable content, complete raw-image layouts,
  pointer confinement, lock coordination, and durable publication. Keep the
  store lock identity permanent. Ref lock identities must remain stable for
  committed refs and while any process holds or waits for them; only prune under
  the exclusive store lock may reclaim unused ref locks. Every holder, waiter,
  and transfer child must retain the store lock until its ref descriptors close.
  Do not silently substitute copying, overwriting, or weaker filesystem behavior when
  a required primitive is unavailable.
- After a namespace change, report potentially visible state if syncing fails.
  Do not roll back a committed ref to hide that failure.

Initialization requires a separately prepared immediate parent. Create only the
store root and its internal structure, never missing ancestors. Nested stores
are unsupported as a setup restriction; do not add ancestor searches to enforce
that restriction. Follow the architecture's initialization ordering and retry rules.

## Stop rule for major design gaps

If implementation or verification reveals a major gap affecting identity,
compatibility, immutability, pointer confinement, initialization safety, locking,
or durability, stop work at that boundary. Preserve completed work in the
working tree and return to the user for design resolution.

Record the conflict, concrete evidence or reproduction, affected behavior,
preserved progress, remaining work, and practical alternatives. Do not silently
weaken a guarantee or introduce an architectural workaround. Routine choices
within the agreed design do not require this stop.

## Validation

Use meaningful tests appropriate to the change. Prefer synthetic OCI fixtures
for storage primitives, narrowly scoped test-only fault injection, and subprocess
tests for process coordination and termination. Use readiness handshakes and
bounded waits rather than timing assumptions. Test warnings and timeouts with
short internal thresholds, not production durations.

Run the relevant crate-scoped checks from the workspace root:

```sh
cargo check --locked -p hpc-image-store
cargo test --locked -p hpc-image-store
cargo clippy --locked -p hpc-image-store --all-targets -- -D warnings
cargo fmt -p hpc-image-store -- --check
RUSTDOCFLAGS="-D warnings" cargo doc --locked -p hpc-image-store --no-deps
git diff --check
```

Do not add tests that merely mirror an implementation or unnecessarily repeat
checks after they pass. Report failures, skipped checks, and material limits
accurately. Local tests do not qualify Lustre or VAST; cross-node visibility,
locking, filesystem primitives, and durability require deployment-site testing.

## Documentation and handoff

Write documentation, including designs, plans, and reports, with even greater
care for human understanding than code. Help readers understand the repository's
code, how it works, and why it is written that way. Use Simple English unless
precise terminology is needed to explain a technical concept. Prefer readable,
longer sentences that explain ideas gradually over short sentences packed with
meaning or unnecessarily sophisticated words.

Keep the crate README and relevant implementation status aligned with completed,
verified behavior. Distinguish deferred work from implemented features and
historical results from current checks. Explain the context behind consequential
storage decisions.

Keep the architecture as the authoritative reference. If a handoff is needed,
record current state, validation, unresolved issues, and authorized follow-up work
in the development guide without duplicating the architecture. In the final report, explain
what changed, why, how it was checked, and any remaining limits.
