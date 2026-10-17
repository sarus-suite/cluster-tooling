# HPC image store

This crate stores container images in a user-owned directory on Linux, separate
from Podman's local image storage. It uses the Open Container Initiative (OCI)
image format and shares identical image content between stored images. You can
import an image from local Podman storage and later export it back to local
storage using Skopeo to copy the image data.

The crate provides a synchronous Rust library and the `hpc-image-store` CLI.
The library is private to this workspace and is not published as a package.
The storage foundations from stages 1–2 and the import/export code from stage 3
are implemented and have passed local acceptance with real rootless Podman
and Skopeo transfers. Pull, list, remove, check, and prune are not public
operations yet, and there is no `init` CLI action.

The [filesystem image store design](../../hpc-image-store-design.md) describes
the agreed behavior and guarantees. The
[staged implementation plan](../../hpc-image-store-implementation-plan.md)
describes the implementation order and acceptance criteria. These remain the
primary references for the design and planned work.

## Using the CLI

### Tools and storage context

Import requires Podman and Skopeo; export uses only Skopeo and can run when
Podman is unavailable. Both operations use the caller's existing **local**
containers-storage context, which is the image storage used by Podman. This
includes the current storage driver, normal `storage.conf` discovery,
environment, and user namespace. Remote Podman contexts are outside this
contract.

The library does not supply a storage specifier or read, copy, change, or
reconstruct storage configuration. It also does not select another driver,
because changing the driver changes which images are visible. There are no CLI
flags for overriding storage roots, driver settings, or the environment.

Run the CLI or a program that uses the library directly in your normal Podman context.
If your tools or storage driver report that an existing rootless user namespace is required,
`podman unshare` is a fallback using that same context:

```sh
podman unshare hpc-image-store --store /scratch/me/images import localhost/tool:latest analysis/tool:release
```

The library does not launch `podman unshare` itself. Before a transfer, it checks
that Skopeo supports the required shared-blob option, platform flags when a
platform is selected, and digest preservation when required by image metadata.

### Import and export

An import copies a local image into the filesystem store and gives it a local
reference, or **ref**, such as `analysis/tool:release`. A ref is the name and tag
used to select an image in this store. An export copies the selected image back
to containers-storage under the destination name.

The store's immediate parent directory must already exist and have been safely
prepared by the user or site, as explained under [Store initialization](#store-initialization).
Import can initialize a missing or empty store; export requires an existing
store.

```sh
hpc-image-store --store /scratch/me/images import analysis/tool:release
hpc-image-store --store /scratch/me/images export analysis/tool:release
hpc-image-store --store /scratch/me/images import localhost/tool:latest analysis/tool:release
hpc-image-store --store /scratch/me/images import localhost/tool:latest analysis/tool:release --replace --notes 'updated build'
hpc-image-store --store /scratch/me/images --json export analysis/tool:release localhost/restored:latest
hpc-image-store --store /scratch/me/images --platform linux/arm64/v8 import localhost/tool:latest analysis/tool:arm64
```

The destination is optional for both operations and defaults to the source
argument verbatim. An import source must be a valid store ref to use this
default. Untagged names, image IDs, and registry names with ports require an
explicit destination ref; no tag or name is inferred. Export defaults to its
source store ref, rather than the original import source recorded in metadata.

Import fails if the destination ref already exists, including a damaged ref,
unless `--replace` is supplied. Use `--notes` to attach optional notes to the
import. `--skopeo PATH` and `--podman PATH` select the tool executables.
`--platform` accepts `OS/ARCH[/VARIANT]`; without it, Skopeo uses its host
selection. Selecting a platform does not convert the image's architecture.

Source and destination arguments for containers-storage are image names or IDs.
Unqualified names, including names without explicit tags, are passed to the
tools unchanged, and the tools parse and resolve the full image reference.
Names such as `docker:latest`, `oci:latest`, and `oci:path` are ordinary
repository/tag pairs; the repository name does not select a transport. Explicit
URLs such as `docker://example/image` and embedded storage settings are
rejected. Names inside the persistent filesystem store still require an
explicit tag. A persistent store path containing `:` receives an explicit
error because Skopeo's OCI transport cannot address that path.

Exit status 0 means success, 1 means an operation failed, and 2 means invalid
usage. With `--json`, results go to stdout; errors and warnings go to stderr.
Output from the tools is captured separately so that it does not corrupt JSON
results. A successful import can report cleanup warnings while still returning
status 0; see [Cleanup after import](#cleanup-after-import).

### What transfers preserve and validate

Import first runs one `podman image inspect --format '{{.Id}}'` lookup, then
asks Skopeo to copy that complete image ID. If the source tag moves, the copy
still uses the chosen ID. If the image is removed before the copy, the import
fails without retrying by name. The stored metadata comes from the completed
OCI manifest and configuration, rather than from Podman inspection.

Skopeo writes only into a private temporary directory during import, including
when writing shared blobs. The current transfer operations require a single
OCI manifest as the image root and reject image indexes, which can describe
multiple images or platforms. With OCI defaults, Skopeo compresses uncompressed
layers and preserves supported existing compression. A content digest identifies
the exact bytes of an object. Import can change the manifest, layer
representation, and their digests, so it does not claim to preserve the original
registry identity. The `pull` acquisition kind in
metadata establishes a future policy for exports that preserve digests, but
there is no pull operation or image-index export yet.

Export validates the small files and pointer that describe the selected ref,
then relies on Skopeo to report content or transfer failures. Neither import
nor export hashes layers or checks the entire graph of referenced image
objects. Export leaves the persistent ref unchanged, and any partial data at
the destination belongs to containers-storage. Export creates no temporary
staging directory in the filesystem store.

## Using the Rust library

Create a store configuration and handle, then construct the names and digests
you need:

```rust
use hpc_image_store::{LocalRef, Sha256Digest, Store, StoreConfig};

let store = Store::new(StoreConfig::new("/scratch/me/images")?);
let reference = LocalRef::new("analysis/tool:Release_1")?;
let digest = Sha256Digest::new(format!("sha256:{}", "a".repeat(64)))?;
# Ok::<(), hpc_image_store::Error>(())
```

Constructing the configuration and handle does not change the filesystem.
A relative store path is resolved against the current working directory when
the configuration is constructed. Import and export perform their own store
preparation and locking, so callers do not need to coordinate those steps.

`TransferContext` contains only the executable paths and an optional platform
selection. The tools inherit the caller's existing storage context as described
in the CLI section. Use `ImportOptions` to request replacement or supply notes:

```no_run
use hpc_image_store::{ImportOptions, LocalRef, Store, StoreConfig, TransferContext};

let store = Store::new(StoreConfig::new("/scratch/me/images")?);
let reference = LocalRef::new("analysis/tool:release")?;
let context = TransferContext::default();
let imported = store.import("localhost/tool:latest", Some(&reference), &context,
                            &ImportOptions::default())?;
let exported = store.export(&reference, Some("localhost/restored:latest"), &context)?;

// None uses the source verbatim as the destination.
let imported = store.import("analysis/other:release", None, &context,
                            &ImportOptions::default())?;
let exported = store.export(&imported.local_ref, None, &context)?;
# Ok::<(), hpc_image_store::Error>(())
```

The library accepts `Option<&LocalRef>` for the import destination and
`Option<&str>` for the export destination. An invalid implicit import ref returns
a typed validation error before store preparation or tool invocation. The CLI
reports this as invalid usage with status 2 and asks for an explicit destination.

`ImportReport` identifies the local ref, stored root digest, source `ImageId`,
and content metadata, and includes any `cleanup_warnings`. `ExportReport`
identifies the local ref, stored manifest digest, and destination. The CLI uses
these reports for its JSON results.

### Names, digests, and metadata

Local refs use lowercase OCI-style repository paths and explicit,
case-sensitive tags. Names are limited to 255 ASCII bytes and tags to 128.
The library does not parse registry domains, supply an implicit `latest` tag,
or normalize names. The bare spelling `sha256:<64 hex digits>` is reserved for
future digest selectors, even if its hex digits are uppercase.
`Sha256Digest` values accept only lowercase SHA-256 hex.

`StoreMetadata` and `RefMetadata` serialize to JSON, and their `from_json`
methods check the schema version. Identity values read from JSON undergo the
same validation as values passed to constructors. Store markers use version 1,
and ref metadata uses version 2; obsolete version-1 ref metadata is rejected
without migration.

Ref metadata records `local_ref` and `stored_root`. Its `content` fields cache
information for display: the platform, optional OCI creation time, and unique
content size. The size is the sum of unique manifest, configuration, and layer
objects, excluding the small wrapper metadata files. It does not estimate how
much space removing an image would reclaim, since other images may share its
objects.

The remaining fields describe how the image was acquired:
`acquired_at_unix_seconds`, `acquisition`, and optional `notes`. An import
records the requested source and a separate containers-storage `ImageId`.
It records no original registry digests or alternate lookup keys.

## How images are stored

A ref connects a name such as `analysis/tool:release` to an image record under
`raw_images/`. This record is called a **raw image** and contains a complete OCI
layout with matching metadata. Here, “raw” describes the record below the named
ref; it does not mean that the image is uncompressed or unpacked.

The actual image objects live in a shared **content-addressed store (CAS)**
under `blobs/sha256/`. Each object is named by the SHA-256 digest of its bytes,
so images with identical manifests, configurations, or layers can share those
objects without keeping separate copies for each ref. Once published, these
objects are immutable: their bytes must never change.

The main identifiers have different purposes:

- A **ref key** reversibly encodes a logical `name:tag` for use in a path.
- A **raw-image ID** is randomly generated for one publication of an image.
  It is unrelated to Podman's image ID or the image's content digest.
- A **content digest** identifies exact stored bytes. Two raw images can have
  the same root digest while recording different acquisitions or notes.

The directory layout is:

```text
store/
├── store.json                     # Store format marker
├── blobs/sha256/<digest>           # Shared immutable manifests, configs, layers
├── refs/<encoded-key>/ref          # Relative symlink selecting a raw image
├── raw_images/raw-<id>/            # One complete underlying image record
│   ├── oci-layout                 # OCI layout version
│   ├── index.json                 # Exactly one selected root descriptor
│   ├── metadata.json              # Content display fields, acquisition, notes
│   └── blobs/                     # Empty; objects live in the shared CAS
├── tmp/operation-<id>/             # Private work for one acquisition
│   ├── layout/                    # Skopeo's unpublished OCI layout
│   └── blobs/sha256/               # Skopeo's privately transferred objects
└── locks/
    ├── store.lock                 # Coordinates operations with future prune
    └── refs/<encoded-key>/ref.lock # Stable lock for a logical name
```

The `ref` entry is a relative symlink selecting a raw-image directory. Each raw
image has an empty `blobs/` directory to satisfy the OCI layout structure, while
export supplies Skopeo's source shared-blob option to read the actual objects
from the CAS.

### Encoding names as paths

For example, `analysis/tool:release` is encoded as
`616e616c797369732f746f6f6c3a72656c65617365`. This is hexadecimal text, not a hash
or a database-assigned ID, so the library can calculate the path directly and
recover the exact original name, including tag case. Long keys are split into
path components of at most 120 characters, with exactly 120 in each component
except the last. The fixed terminal entries `ref` and `ref.lock` prevent
collisions when one encoded name is a prefix of another.

Encoding gives all logical names one filesystem-safe representation, keeping
slashes and punctuation within the encoded name and avoiding reserved layout
filenames. The resulting paths are less readable than
`refs/analysis/tool/release/`. Encoding is separate from the choice to publish
images by switching a pointer; a readable nested namespace could also point to
raw images.

### Temporary space and layout compatibility

The `tmp/` directory keeps Skopeo's writes private until an image is published.
New objects are added to the CAS using hard links, so the staging path and CAS
path then refer to the same file. If an object already exists in the CAS, the
new staged copy remains a separate file. Removing successful staging removes
these extra links and duplicate copies without deleting published content.
Staging still needs enough space for an entire acquisition before publication,
even when the final stored content is shared with other images.

This prototype layout has no migration or compatibility fallback. Recreate
outdated developer stores and fixtures explicitly; the library does not remove
them for you. Store metadata remains version 1 and ref metadata remains version 2.
There is no offline qualification machinery.

## Store initialization

### Preparing the location

An operation that changes the store can initialize a missing store directory
or an existing empty one. Its immediate parent must already exist and have
been safely prepared by the user or site. That preparation includes syncing
the parent's creation and the directory entries needed to reach it so that
they survive a crash. The library assumes this has been done; it creates no
missing ancestors and does not inspect or repair them. Stores inside other
stores are unsupported as a setup restriction, but initialization does not
search ancestors to detect nesting.

Initializing a store without a marker requires opening and syncing the
immediate parent, even when the store root already exists and is empty.
Creating a missing root also requires write permission in that parent.
A missing parent causes an error without creating a directory chain. A parent
that allows traversal but cannot be opened for syncing cannot be used to
initialize a store without a marker.

Directories created by the library use mode 0700 and files use mode 0600,
subject to the caller's umask. Skopeo chooses the permissions of the blobs it
creates, which are inside private staging and CAS directories. Existing
permissions are preserved. Reserved setup directories must be actual
directories, and markers and lock files must be regular files; symlinks in
these entries are rejected. Committed ref pointers are intentionally symlinks.
Coordination assumes cooperating processes in user-owned storage.

### Initialization sequence and retries

Initialization checks existing contents before adding setup files. It creates
only the root and minimum lock structure, takes an exclusive lock on the stable
`locks/store.lock` file, then checks the contents again. Concurrent processes
reuse the same root and lock file rather than replacing them. Initialization
can reuse recognized empty setup directories and the regular temporary marker
`.store-init.json`. It rejects populated roots without a store version,
obsolete bootstrap artifacts, unsupported versions, and incompatible reserved
entries without deleting them.

Syncing asks the filesystem to save a file's data or a directory's entries so
that they survive a crash. For a store without a marker, initialization writes
the temporary marker and syncs these items in order:

1. The temporary marker file.
2. The lock file.
3. Internal directories, working from children to parents.
4. The store root.
5. Its immediate parent.

It then atomically renames the temporary marker to `store.json` and syncs the
root before returning success. The parent is always synced before the marker
is published, including on retries and when another process created the root.
No directory above the immediate parent is opened for syncing. A failure before
marker publication leaves recognized setup entries available for retry.

Completed initialization leaves only `store.json`, the stable
`locks/store.lock`, and empty internal directories. Ref entries, raw images,
and operation directories appear later as images are acquired and published.
A supported version-1 store needs no private recovery record.

### Preparing an existing store

For a store that already has a marker, preparation for a change takes a shared
store lock, validates the store, and syncs the marker and root again without
opening the parent. A visible marker does not allow these syncs to be skipped;
repeated sync failures remain errors until syncing succeeds.

A marker or root sync error after marker publication is reported as
`Error::InitializationDurability`. The error reports potentially published
state and leaves the marker in place. Normal operation locking and validation
follow preparation, whether the store was newly initialized or already marked.

Preparation for reading requires an existing lock and supported marker and
does not initialize or repair the store. Reading can still create a ref lock
file as coordination bookkeeping.

## Publishing images and handling failures

**Publication** makes a prepared image available through a ref. It first saves
a complete new raw image, then atomically switches the ref symlink to that
record. Readers therefore select either the complete old layout or the
complete new layout, with its matching metadata. Updating `index.json` and
`metadata.json` individually in a live directory would expose intermediate
states, and an ordinary directory rename cannot replace an existing nonempty
directory.

The internal storage primitives described here also support behavior reserved
for later stages. They do not make removal, discovery, or image-index transfers
public operations.

### Preparing and saving the image

Acquisition staging provides private layout and shared-blob paths under
`tmp/operation-<id>/`. Before completing a stage, the producer must stop all
writers and close writable handles. Completing the stage gives up writable
acquisition access and binds the selected descriptor, metadata, and object list
to the locked ref. A descriptor identifies a referenced OCI object; the
acquisition layer supplies validated SHA-256 identities and expected sizes.
The publication primitives do not hash the objects or validate the complete
image graph.

Publication syncs the staged objects, then inserts them into
`blobs/sha256/<digest>` using hard links on the same filesystem. Existing digest
paths are never overwritten. To reuse one, the library requires a regular file
of the expected size and syncs the file and directory, preserving the file's
modification time. These checks cannot detect corruption that leaves the file
at the same size. If hard links are unsupported, publication fails without
falling back to copying. Published objects must never be modified through
staging paths that still refer to the same files.

The library exclusively creates each raw image with small OCI wrapper files,
matching versioned metadata, and an empty real `blobs/` directory. It normalizes
`index.json` to exactly one selected root descriptor while retaining the
descriptor's extra fields. The internal primitives allow either an OCI manifest
or image index as the root, although the current import/export operations
accept only manifests. Content blobs remain unchanged, including the bytes and
identity of an image-index root.

Publication writes and syncs the raw image files, then syncs its `blobs/`
directory, the raw-image directory, and the `raw_images/` parent. It creates
and syncs the parent directories for the ref and prepares a relative symlink
in private staging. After syncing the staging directory, a single rename
switches the committed ref to the complete raw image. Both rename parents are
then synced before success. The store lock and matching exclusive ref lock
remain held throughout.

### Replacement, removal, and durability errors

An existing destination, including a dangling symlink, causes an error unless
replacement is explicitly requested. Replacement accepts symlinks and regular
files but rejects directory entries. Replaced raw images remain for future
prune; an individual import does not delete them.

The internal removal primitive unlinks the terminal ref entry and syncs its
parent without following the pointer. It leaves shared content, retired raw
images, namespace directories, and lock files in place. Removal is not yet a
public operation.

Before the ref is committed, failures preserve any previous ref. Once a
filesystem entry has changed, a sync failure can mean that the change is
visible but might not survive a crash. `Error::Durability` reports the
potentially visible blob insertion, ref publication, or ref removal, together
with the path whose sync failed. The library never rolls back a committed
pointer after a sync failure, because doing so would introduce another change
instead of resolving whether the committed change was saved.

### Cleanup after import

Once an import has durably published its ref, it removes only its own operation
directory and syncs `tmp/`, while still holding the store and ref locks.
Cleanup does not follow symlinks or scan other operations. Removing these
staging files does not remove the published image.

Cleanup is best effort after successful publication. On normal success,
`ImportReport.cleanup_warnings` is empty. A removal or cleanup-sync failure
adds a structured `CleanupWarning` containing the staging path and a contextual
message, while the ref remains committed and usable. The CLI prints warnings
on stderr, includes them in JSON results, and exits with status 0. A cleanup-sync
warning means deletion may not survive a crash; a removal failure may leave
staging only partly removed.

Acquisition failures, publication failures (including durability errors), and
process termination do not trigger cleanup, because a surviving Skopeo process
may still be writing to its private staging. Abandoned staging, cleanup
remnants, unpublished or retired raw images, and unreferenced objects remain
for future conservative pruning. Any staging paths that still refer to
published blobs must never be used for writes.

### Reading refs and discovering entries

Readers retain their ref and store lock guards while resolving and using a raw
image. Pointers must use the exact relative path into the raw-image area.
Namespace parents, raw-image directories, and small files reject redirection
through symlinks.

Internal discovery reads namespace entries rather than hiding refs with broken
pointers or malformed terminal entries. Unexpected branches and enumeration
failures are retained as diagnostics and mark discovery incomplete. Discovery
validates pointers, but full inventory validation and presentation remain
stage-4 work.

## How locking works

Locks belong to the store and logical ref names rather than individual raw
images. Lock files stay in place across replacement and removal, so processes
continue to coordinate through the same files even when a ref selects a new
image.

The operation lock policy is shown below. Import and export are public today;
pull, remove, list, check, and prune describe the policy for planned operations.

| Operation | Store lock | Ref lock |
| --- | --- | --- |
| import, pull, remove | Shared | Exclusive |
| export, list, check | Shared | Shared |
| prune | Exclusive | None |

Initialization without a marker has a separate phase with an exclusive store
lock. Preparation for a change to a marked store uses shared locking for its
repeated syncs. Either path is followed by normal operation locking and
validation. A ref guard borrows its store guard, which ensures that the store
lock outlives the ref lock.

### Waiting for locks

To acquire a lock, the library makes nonblocking `flock` attempts, retrying
every 100 ms after contention or interruptions. While waiting, it prints a
warning to stderr every 30 seconds containing the lock path, mode, elapsed
time, and timeout. If a lock cannot be acquired within five minutes, the library
returns `Error::LockTimeout`. The deadline starts immediately before
the first lock attempt; filesystem open calls and operations after the lock is
acquired are outside this limit. These timings are internal policy, and tests
use millisecond thresholds instead of waiting for production intervals.

### Locks during subprocess execution

Locks are released when their owning file descriptors close. Lock files are
never truncated, replaced, or removed, including after a timeout.

The parent's lock descriptors are marked close-on-exec, which normally prevents
them from being retained when executing another program. Immediately before
executing a Skopeo copy, the child clears this flag on only the two operation
lock descriptors. The child then shares the existing locked open-file
descriptions, so closing the parent's descriptors does not release the locks
while the child still holds them. No explicit unlock is issued.

If the wrapper terminates during the copy, it publishes no ref. A surviving
Skopeo process can finish its private copy or fail, and its locks are released
when it exits. Operations that need conflicting locks must therefore wait for
the orphaned copy to release them.

## Validation and remaining qualification

### Local checks and test coverage

Run the following commands from the workspace root. The suite needs Podman,
Skopeo, Python 3, tar, and sha256sum, plus permission to create rootless user
namespaces and use overlay storage. Real-tool tests run as part of ordinary
`cargo test`; no `--ignored` flag or external Bash harness is needed.

```sh
cargo check --locked -p hpc-image-store
cargo test --locked -p hpc-image-store
cargo clippy --locked -p hpc-image-store --all-targets -- -D warnings
cargo fmt -p hpc-image-store -- --check
git diff --check
```

Tests cover names and encoding, metadata, initialization recovery, private
permissions, and rejected stores. Initialization tests verify missing-parent
rejection, exact sync ordering, persistent sync failures, unprivileged parent
permissions, and process termination after root creation, lock creation,
scaffolding, parent sync, and marker publication.

Publication tests use complete zero-layer manifest and image-index fixtures.
They exercise insertion races and replacement in separate processes,
termination around the rename boundary, and test-only failure checkpoints at
insertion and sync steps. Synthetic fixtures use version-2 ref metadata, but
their placeholder acquisition fields do not represent real imports. Fault
injection is thread-local and unavailable in production builds.

Stage-3 synthetic and CLI tests exercise source-ID pinning, inherited
environment, platform flags, clean CLI JSON, unchanged caller configuration,
failed copies, metadata, replacement, shared-content reuse, and injected
publication failures. They also verify that transport-like repository names
and unqualified names reach the tools without changing transport selection.
Cleanup tests cover successful imports and replacements, preservation of CAS
inodes, bytes, and modification times, unrelated staging, symlink confinement,
and removal or sync warnings after commit. An unprivileged CLI test exercises
real removal and cleanup-sync failures, successful exit status, and separate
stderr diagnostics in both human and JSON modes.

Coordination tests cover stable lock identities, separate processes, lock-wait
warnings, and acquisition timeouts. Process tests verify both inherited locks
after wrapper termination using readiness handshakes and bounded waits.
The five ignored tests (`lock_worker`, `wait_worker`, `publication_worker`,
`transfer_worker`, and `cleanup_warning_worker`) are internal subprocess helpers
invoked by ordinary tests. Do not run these helpers directly or use an
unfiltered `--ignored` invocation. Neither real-tool acceptance test is ignored.

### Tests with real tools

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

The independent real-Skopeo OCI-to-OCI lock test needs no Podman storage:

```sh
cargo test --locked -p hpc-image-store actual_skopeo_retains_locks_after_parent_termination -- --nocapture
```

### Recorded results and outstanding qualification

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
