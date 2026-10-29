# Administration and installation

The first PoC supports cooperating processes in a user-owned store on Linux.
Group-writable stores are not supported. Local results do not qualify Lustre or
VAST deployments; see [deferred validation](deferred-validation.md). The
[architecture](architecture.md) is the reference for initialization, locking,
publication, and durability contracts.

## Build and install

Use the repository's Rust toolchain with edition 2024 support. From the workspace
root, build and install the workspace-private package for the intended user:

```sh
cargo install --locked --path crates/hpc-image-store
hpc-image-store --help
```

Cargo installs into its configured binary directory, normally `$HOME/.cargo/bin`;
ensure that directory is on `PATH`. To build without installing, use
`cargo build --locked -p hpc-image-store --release`. Its executable is under
Cargo's configured target directory, normally `target/release/`.

The library is consumed through a workspace path dependency and is not published
to crates.io. There is no service or daemon to install. Application integration
is outside this PoC.

| Operation | External requirements |
| --- | --- |
| Import | Local Podman and Skopeo using the same containers-storage context |
| Pull | Skopeo, registry connectivity, and any required authentication/trust configuration |
| Export | Skopeo and the destination containers-storage context |
| List, check, availability, remove, prune | No Podman or Skopeo invocation |

The library checks Skopeo capabilities rather than enforcing a numeric version
floor: shared-blob support, platform flags when requested, and preservation for
pull/export are required. Earlier local validation used Podman 5.4.2 and Skopeo
1.18.0; those are historical tested versions, not a deployment compatibility
promise. Current run versions belong in the [development record](development.md).

## Prepare a store location

Select a dedicated location such as `/scratch/user/.sarus-imagestore` beneath an
already prepared immediate parent. Preparation includes persisting creation of
that parent and the ancestor directory entries needed to reach it. Seeing the
directory is not proof of crash persistence. Site provisioning must complete
this preparation before image-store initialization; a plain `mkdir -p` example
does not establish it.

The user must be able to traverse the path and open and sync the immediate
parent during first initialization. Creating a missing store also needs write
permission in that parent. The crate creates only the store root and its
internal structure, never missing ancestors. Do not put a store inside another
store; initialization does not search ancestors to detect that setup error.

Import, pull, and remove initialize missing or empty stores. Other operations,
including dry-run prune, require a completed store. An interrupted setup can be
retried using recognized empty setup entries and `.store-init.json`. Unexpected
contents, unsupported markers, and incompatible reserved entries are preserved
with errors, rather than adopted or deleted. Do not manually replace the store
lock to force a retry. Initialization durability errors may leave a visible
marker and are resolved only by successful subsequent syncing.

Library-created directories use 0700 and files 0600, subject to umask. Existing
permissions are preserved; Skopeo creates blob files inside private directories.
The store relies on cooperating processes under one owner, not protection from
that owner's direct modification. Keep published blobs immutable and preserve
the permanent `locks/store.lock` identity.

Store format 1 and metadata schema 3 have no migration fallback. Explicitly
recreate obsolete disposable development stores or refs when appropriate;
never assume the library will delete or convert them.

## Containers-storage and registry configuration

Import/export inherit the caller's local storage driver, roots, `storage.conf`
discovery, environment, and user namespace. Changing the driver changes which
images are visible. The library provides no storage-root or driver overrides,
does not reconstruct configuration, and does not support remote Podman contexts.
Use `--podman`/`--skopeo` only to select executable paths.

Run directly in the intended local context. If the installed tools require an
existing rootless namespace, `podman unshare` is a caller-controlled fallback;
the library never launches it automatically. Rootless operation may require
site-configured user namespaces and subordinate UID/GID mappings. Test fixtures
use disposable storage and have additional requirements documented separately
in the development guide.

Registry pull keeps authentication, certificate, policy, and optional TLS settings
separate from containers-storage. Skopeo reads inherited configuration or the
explicit pull flags. Keep credentials outside the store; they are never written
to image metadata. The [user guide](user-guide.md) documents the flags and the
single-platform manifest restriction.

## Capacity and maintenance

Keep staging and the CAS on a filesystem that supports the required hard links,
locks, atomic rename, and syncing. Unsupported primitives return errors without
copying, overwriting, or weaker fallbacks. Store paths containing `:` cannot be
addressed by Skopeo's OCI transport for transfers.

Budget temporary space for an entire acquisition even if the final content is
already shared. Newly linked staged files share storage with CAS files, while
duplicate transfers of existing CAS objects consume separate temporary space
until cleanup. Wrapper records, abandoned staging, and retired raw images also
consume space. Cached per-image sizes cannot be summed into total usage or
predicted physical reclaim totals.

Use `list` and `check` to inspect state, then explicitly remove intended refs and
review `prune --dry-run`. Default content grace is 24 hours of object age, while
recognized unused bookkeeping is immediately eligible. Prune requires a complete
fresh authoritative scan and waits for active operations, including surviving
copy children. Do not manually remove locks to bypass a timeout or delete CAS
objects based on cached reports.

## Failures, backups, and recovery limits

After a durability error, a namespace change may already be visible. Inspect
state before retrying; the library does not roll back published refs. Successful
acquisition may report staging-cleanup warnings, leaving committed content usable.
Prune preserves unknown data and reports partial progress when removal or syncing
fails. Keep that evidence rather than treating every failure as an uncommitted
operation.

Back up or snapshot the store, including refs and operational metadata, using
site procedures that preserve a coherent state. Blob content alone cannot
recover accidentally deleted names, provenance, or notes. Structural checking
does not detect every same-size corruption or validate compressed layer data.
Replacing a ref cannot generally repair damaged immutable shared objects.
Automatic repair, backup orchestration, and migration are not implemented.
