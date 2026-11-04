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
| List, check, exists, remove | No Podman or Skopeo invocation |

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

Import, pull, and list initialize missing or empty stores and recover recognized
interrupted setup. Exists, remove, export, and check require completed setup;
they create no root or lock bookkeeping and do not recover pending initialization.
List therefore creates persistent structure even when it returns an empty inventory.

Setup uses recognized empty directories, `.store-init.json`, and the empty regular
`.store-init-pending` file. The pending file is persisted before publication of
`store.json` and removed only after that marker is durable. Failed initialization
returns an error without internal retries. Retry with list, import, or pull to
finish setup, including when the marker is already visible. Completed stores
need no initialization syncing or parent access. Unexpected contents, unsupported
markers, and incompatible reserved entries are preserved with errors, rather
than adopted or deleted. Do not manually remove pending evidence or replace
the permanent store lock to force a retry.

Library-created directories use 0700 and files 0600, subject to umask. Existing
permissions are preserved; Skopeo creates blob files inside private directories.
The store relies on cooperating processes under one owner, not protection from
that owner's direct modification. Keep published blobs immutable and preserve
the permanent `locks/store.lock` identity.

Store format 2 and metadata schema 3 have no migration fallback. Explicitly
recreate obsolete disposable development stores or refs when appropriate;
format 1 is rejected without modification. Never assume the library will delete
or convert existing stores.

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
until cleanup. Successful import, pull, and their replacements durably finish
staging cleanup and reclaim unused superseded content. Successful removal also
durably reclaims unused recorded content and empty namespace tails, preserving
shared objects and longer names. Unrecorded remnants still consume space.
Cached per-image sizes cannot be summed into total usage or predicted physical reclaim totals.

Use `list` and `check` to inspect state, then explicitly remove intended refs.
Unrecorded remnants remain preserved without a general reclamation interface.
Recorded recovery waits for active operations, including surviving copy children.
All mutations hold the exclusive store lock, even across different
names; shared queries can coexist. Do not manually remove locks to bypass a timeout or delete CAS
objects based on cached reports.

## Failures, backups, and recovery limits

Operation preparation attempts pending recorded recovery before using image
content; unresolved recovery blocks the requested operation. Recovery waits for
active operations, including surviving transfer children, and can change the
store even before a query. Preserve pending records and the
permanent lock when investigating errors; never remove them to bypass recovery.
Malformed records, inconsistent ref states, and unexpected cleanup contents are
preserved with contextual errors. See the
[architecture](architecture.md#recorded-mutations-and-automatic-recovery) for
snapshot presence and lifecycle details.

Import, pull, and remove create records and finish their lifecycle before
success. Unrecorded remnants are not automatically recovered. Import and pull
replacement, and removal, reject damaged target identities or unreadable
dependency declarations before changing the ref, preserving evidence.
This does not add general
repair, migration, or a new operational recovery command. Incomplete format-2
development stores may be recreated between stages without a compatibility
promise. Local interruption tests do not qualify power-loss or cross-node behavior.

After a durability error, a namespace change may already be visible. Inspect
state before retrying; the library does not roll back published refs. Import, pull,
and remove cleanup failures are errors with pending-work context; fix environmental
failures and retry an ordinary operation to finish recorded recovery. Recovery
never retries a pull or contacts its registry.

Removal checks remaining reachability after durably unlinking the name. Damage
in another ref can block that cleanup, leaving the name absent and preventing
all ordinary access to the store, including list, check, and remove.
An unresolved structural failure may require coherent backup restoration or
maintainer-reviewed repair; PoC v2 offers no built-in repair or recovery bypass.
Preserve records and locks. Addressing this limitation is deferred to PoC v3
or later.

Back up or snapshot the store, including refs and operational metadata, using
site procedures that preserve a coherent state. Blob content alone cannot
recover accidentally deleted names, provenance, or notes. Structural checking
does not detect every same-size corruption or validate compressed layer data.
Replacing a ref cannot generally repair damaged immutable shared objects.
Automatic repair, backup orchestration, and migration are not implemented.
