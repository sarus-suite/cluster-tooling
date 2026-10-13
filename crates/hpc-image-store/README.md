# HPC image store

Workspace-private synchronous Rust library for a Linux, user-owned filesystem
OCI image store. This crate currently implements **stage 1 foundations only**.
It does not yet import, pull, export, publish, list, remove, check, or prune images.

The executable prints an unimplemented-command diagnostic to stderr and exits
with status 2. It does not initialize a store. There is no `init` CLI action.

## Library surface

```rust
use hpc_image_store::{LocalRef, Sha256Digest, Store, StoreConfig};

let store = Store::new(StoreConfig::new("/scratch/me/images")?);
let reference = LocalRef::new("analysis/tool:Release_1")?;
let digest = Sha256Digest::new(format!("sha256:{}", "a".repeat(64)))?;
# Ok::<(), hpc_image_store::Error>(())
```

Constructing configuration and a handle performs no filesystem mutation.
Relative store paths are anchored to the current working directory when the
configuration is constructed. Operation preparation and locking are internal
until the image operations are implemented.

Local refs use lowercase OCI-style repository paths and explicit, case-sensitive
tags. Names are limited to 255 ASCII bytes and tags to 128. There is no registry
domain parsing, implicit `latest`, or name normalization. A bare
`sha256:<64 hex digits>` spelling is reserved for future digest selectors, even
when the hex is uppercase. Digest values accept only lowercase SHA-256 hex.

`StoreMetadata` and `RefMetadata` serialize to JSON; their `from_json` methods
check the schema version. The stage-1 ref envelope records only version,
`local_ref`, and `stored_root`; it is not yet the complete schema for published
images. Identity deserialization uses the same validation as constructors.

## Filesystem and initialization

```text
store/
├── store.json                  # {"version":1}
├── blobs/sha256/
├── refs/<encoded-key>/ref      # future committed pointer
├── generations/               # future complete layouts and metadata
├── tmp/                       # future private acquisition staging
└── locks/
    ├── store.lock
    └── refs/<encoded-key>/ref.lock
```

The encoded key is the full logical `name:tag` converted to lowercase hex and
split into directory components of at most 120 characters. Each nonfinal chunk
has exactly 120 characters. The fixed terminal entries keep a key that is a
prefix of another key from colliding with it. Decoding rejects noncanonical paths.

Mutation preparation initializes missing or empty stores and can resume only
recognized empty bootstrap scaffolding. A regular `.store-init.json` is the only
recognized initialization temporary file. Populated unversioned directories,
unsupported versions, and incompatible reserved entries are rejected. No data
is adopted, repaired, or removed automatically.

Initialization checks the root before adding bootstrap files, takes an exclusive
lock on the stable `locks/store.lock`, rechecks the store, and creates directories.
It writes and syncs the temporary marker, syncs the lock file and directories,
then atomically renames the marker to `store.json`. It syncs the root and ancestors
before returning success. Errors after the rename use
`Error::InitializationDurability`: the marker may already be visible.

New directories use mode 0700 and new files mode 0600, subject to the caller's
umask. Existing permissions are preserved. Reserved directories must be actual
directories, and markers and lock files must be regular files; internal symlinks
are rejected. Coordination assumes cooperating processes in user-owned storage.

Read preparation requires an existing lock and supported marker. It does not
initialize or repair a store. Ref-lock creation is coordination bookkeeping and
may occur during read operations.

## Locking

| Operation | Store lock | Ref lock |
| --- | --- | --- |
| import, pull, remove | Shared | Exclusive |
| export, list, check | Shared | Shared |
| prune | Exclusive | None |

Initialization has its own exclusive-lock phase, followed by ordinary operation
locking and revalidation. A ref guard borrows its store guard so the store lock
outlives the ref lock. Acquisition uses nonblocking `flock` attempts with a 100 ms
retry interval for contention and interruptions. While waiting, it prints a
warning to stderr every 30 seconds with the lock path, mode, elapsed time, and
timeout. Each acquisition fails with `Error::LockTimeout` after five minutes.
The deadline begins immediately before the first lock attempt; filesystem open
calls and operations after acquisition are outside this limit. Timing is an
internal policy; tests supply millisecond thresholds rather than waiting for
production intervals.

Locks release when their owning descriptors close. Lock files are never
truncated, replaced, or removed, including on timeout.
Descriptors are close-on-exec. Transfer-child inheritance and orphaned Skopeo
protection will be implemented in stage 3.

## Validation

Run from the workspace root:

```sh
cargo check --locked -p hpc-image-store
cargo test --locked -p hpc-image-store
cargo clippy --locked -p hpc-image-store --all-targets -- -D warnings
cargo fmt -p hpc-image-store -- --check
```

Tests cover names and encoding, metadata, initialization recovery, private
permissions, rejected stores, stable lock identities, and separate-process
coordination, lock-wait warnings, and acquisition timeouts. The ignored
`lock_worker` and `wait_worker` unit tests are internal subprocess helpers invoked
by the ordinary tests; do not run them directly.

Local tests do **not** qualify Lustre or VAST locking, visibility, atomic rename,
or durability guarantees. Cross-node and site-specific filesystem validation
remains required. Unavailable filesystem primitives cause errors; no weaker
fallback is provided.
