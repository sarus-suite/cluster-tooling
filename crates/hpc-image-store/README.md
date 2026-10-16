# HPC image store

Workspace-private synchronous Rust library for a Linux, user-owned filesystem
OCI image store. This crate implements **stage 1 foundations and stage 2 internal
storage primitives**. It does not yet expose import, pull, export, list, remove,
check, or prune operations.

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
check the schema version. The stage-1/2 ref envelope records only version,
`local_ref`, and `stored_root`; it is not yet the complete schema for published
images. Identity deserialization uses the same validation as constructors.

## Filesystem and initialization

```text
store/
├── store.json                  # {"version":1}
├── blobs/sha256/
├── refs/<encoded-key>/ref      # relative symlink into raw_images/
├── raw_images/raw-<id>/
│   ├── oci-layout
│   ├── index.json              # exactly one selected root descriptor
│   ├── metadata.json
│   └── blobs/                  # empty; content lives in the shared CAS
├── tmp/operation-<id>/         # private acquisition staging
└── locks/
    ├── store.lock
    └── refs/<encoded-key>/ref.lock
```

The tree above includes entries created by later publication. Initialization
creates only `store.json`, the stable `locks/store.lock`, and empty internal
directories; there are initially no refs, raw images, or operation directories.

The encoded key is the full logical `name:tag` converted to lowercase hex and
split into directory components of at most 120 characters. Each nonfinal chunk
has exactly 120 characters. The fixed terminal entries keep a key that is a
prefix of another key from colliding with it. Decoding rejects noncanonical paths.

Mutation preparation initializes a missing store directory or an existing empty
one. Its immediate parent must already exist and have been safely prepared by
the site or user, including saving its creation and the directory names needed
to reach it. The library assumes this preparation. It creates no missing
ancestors and does not inspect or repair them. Stores inside other stores are
unsupported as a setup restriction; initialization does not search ancestors to
detect nesting.

Unmarked-store initialization must be able to open and sync the immediate
parent; creating the root also requires permission to write there. A missing
parent causes an error without creating a directory chain. An existing empty
root still requires parent syncing. A parent that permits traversal but cannot
be opened for syncing cannot be used to initialize an unmarked store.

Initialization checks existing contents before adding setup files, creates only
the root and minimum lock structure, then takes the exclusive stable
`locks/store.lock` and rechecks. It reuses only recognized empty setup directories
and the regular `.store-init.json` temporary marker. Populated unversioned
roots, obsolete bootstrap artifacts, unsupported versions, and incompatible
reserved entries are rejected without deletion. Concurrent processes reuse the
same root and lock file rather than replacing them.

For an unmarked store, initialization writes the temporary marker and syncs, in
order, that file, the lock file, internal directories from children to parents,
the root, and its immediate parent. It then atomically renames the marker to
`store.json` and syncs the root before success. The parent sync always precedes
marker publication, including retries and roots created by another process.
No directory above the immediate parent is opened for syncing. Failures before
publication leave recognized setup entries available for retry.

For a marked store, mutation preparation takes a shared store lock, validates
the store, and syncs the marker and root again. It does not open the parent.
A visible marker does not bypass these syncs: repeated sync failures remain
failures until syncing succeeds. Marker/root sync errors after publication use
`Error::InitializationDurability`, reporting potentially published state without
removing the marker. Normal operation locking and validation follow preparation.

No private recovery record is required for a supported version-1 store. There
is no migration or offline qualification machinery. Recreate obsolete
development fixtures explicitly; initialization never cleans them automatically.

New directories use mode 0700 and new files mode 0600, subject to the caller's
umask. Existing permissions are preserved. Reserved directories must be actual
directories, and markers and lock files must be regular files. Symlinks in those
setup entries are rejected; committed ref pointers are intentionally symlinks.
Coordination assumes cooperating processes in user-owned storage.

Read preparation requires an existing lock and supported marker. It does not
initialize or repair a store. Ref-lock creation is coordination bookkeeping and
may occur during read operations.

## Internal publication primitives

Acquisition staging exposes private layout and shared-blob paths under
`tmp/operation-<id>/`. Completing a stage consumes writable acquisition access
and binds the selected descriptor, metadata, and object list to the locked ref.
The producer must stop all writers and close writable handles before completion.
The acquisition layer supplies validated SHA-256 identities and expected sizes;
these primitives do not hash objects or validate the complete image graph.

Publication syncs staged objects, then inserts them by same-filesystem hard link
into `blobs/sha256/<digest>`. Existing digest paths are never overwritten: reuse
requires a regular file of the expected size, followed by file and directory
syncs. Modification time is preserved on reuse. A same-size corrupt object is
not detected by these checks. Unsupported hard links fail without a copying
fallback. Published objects must never be modified through their staging aliases.

Each exclusively created raw image contains small OCI wrapper files, matching
versioned metadata, and an empty real `blobs/` directory. The layout index is
normalized to exactly one selected root descriptor, retaining its extra fields.
The root may be an OCI manifest or image index. Content blobs are unchanged,
including the bytes and identity of an image-index root. Future Skopeo transfers
must supply the shared-blob option to read the common CAS.

Publication writes and syncs raw image files, then syncs the raw image's
`blobs/` directory, raw-image directory, and `raw_images/` parent. It creates
and syncs namespace parents and prepares a relative symlink in private staging.
After syncing the staging directory, one rename switches the committed ref to
the complete raw image. It then syncs both rename parents before success.
The store lock and matching exclusive ref lock remain held throughout.

An existing destination fails unless replacement is explicit, including a
dangling symlink. Replacement accepts symlinks and regular files; directory
entries are rejected. Removal unlinks the terminal entry and syncs its parent
without following the pointer or touching shared content, retired raw images,
namespace directories, or lock files.

Before ref commit, failures preserve any previous ref. After namespace changes,
`Error::Durability` identifies a potentially visible blob insertion, ref
publication, or ref removal, plus the failed sync path. A committed pointer is
never rolled back after a sync failure. Staging and unpublished or retired
raw images remain for later conservative pruning; automatic cleanup is not
implemented. Staging aliases left after publication are inactive and must not be
used for later writes.

Readers retain their ref and store guards while resolving and using a raw image.
Pointers must use the exact relative path into the raw image area. Namespace
parents, raw-image directories, and small files reject symlink redirection.
Discovery reads namespace entries rather than hiding refs with broken pointers
or malformed terminal entries. Unexpected branches and enumeration failures are
retained as diagnostics and mark discovery incomplete. Discovery validates
pointers; full inventory validation and presentation remain stage-4 work.

## Locking

| Operation | Store lock | Ref lock |
| --- | --- | --- |
| import, pull, remove | Shared | Exclusive |
| export, list, check | Shared | Shared |
| prune | Exclusive | None |

Unmarked initialization has its own exclusive-lock phase; marked-store mutation
preparation uses shared locking for its repeated syncs. Ordinary operation
locking and revalidation follow either path. A ref guard borrows its store guard
so the store lock outlives the ref lock. Acquisition uses nonblocking `flock` attempts with a 100 ms
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
coordination, lock-wait warnings, and acquisition timeouts. Initialization tests
also verify missing-parent rejection, exact sync ordering, persistent sync
failures, unprivileged parent-permission behavior, and process termination after
root creation, lock creation, scaffolding, parent sync, and marker publication.
The ignored
`lock_worker`, `wait_worker`, and `publication_worker` unit tests are internal
subprocess helpers invoked by the ordinary tests; do not run them directly.

Publication tests use complete zero-layer manifest and image-index fixtures,
separate-process insertion races and replacement, process termination around the
rename boundary, and test-only failure checkpoints at insertion and sync steps.
Metadata fixtures are temporary test data rather than a production acquisition
schema. Fault injection is thread-local and unavailable in production builds.

Local tests do **not** qualify Lustre or VAST locking, visibility, atomic rename,
or durability guarantees. Cross-node and site-specific filesystem validation
remains required. Unavailable filesystem primitives cause errors; no weaker
fallback is provided.
