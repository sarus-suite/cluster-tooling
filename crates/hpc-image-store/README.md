# HPC image store

A first proof of concept for persistent, user-owned, filesystem-based,
OCI container image storage on Linux.
The workspace-private crate provides a synchronous Rust library and the
`hpc-image-store` development CLI.

Import images from local Podman containers-storage, pull one OCI image manifest
directly from a registry, and export stored images back to containers-storage.
The store shares immutable image objects between refs. It also supports inventory,
structural checking, lightweight existence queries, and explicit ref removal
with immediate unused-content reclamation.

## Quick start

Build the CLI from the workspace root:

```sh
cargo build --locked -p hpc-image-store --release
```

Transfers need Skopeo; import also needs Podman. Use the caller's existing local
containers-storage context. Put the executable from Cargo's release output
directory (normally `target/release/`) on your `PATH` and select a store beneath
an already prepared parent directory. See the
[administration guide](docs/administration.md) for installation and preparation.

```sh
hpc-image-store --store /scratch/me/images import localhost/tool:latest analysis/tool:release
hpc-image-store --store /scratch/me/images list
hpc-image-store --store /scratch/me/images exists analysis/tool:release
hpc-image-store --store /scratch/me/images check analysis/tool:release
hpc-image-store --store /scratch/me/images export analysis/tool:release localhost/restored:latest
hpc-image-store --store /scratch/me/images remove analysis/tool:release
```

Import and pull initialize a missing or empty store beneath its prepared parent.
There is no separate `init` command. Remove requires an initialized store and
finishes unused-content reclamation before success, preserving shared objects
and namespace prefixes needed by other names. Import, pull, and their replacements
also complete their own durable staging cleanup and reclamation. Unrecorded
remnants remain preserved; there is no general reclamation interface.

`exists` is silent: status 0 means present, 1 absent, 125 an operational error,
and 2 invalid usage. It requires an initialized store and does not establish image health;
use explicit `check` for that.

Cleanup failures are errors recovered during later operation preparation. A
removed name can already be absent when damage elsewhere blocks cleanup and
prevents ordinary store access. See the user guide's
[recovery guidance](docs/user-guide.md#recovery-failures-and-manual-resolution);
PoC v2 provides no repair command or recovery bypass.

## Documentation

- [Architecture](docs/architecture.md): authoritative behavior, guarantees, and storage decisions.
- [User guide](docs/user-guide.md): CLI workflows, selection, output, and troubleshooting.
- [Rust library guide](docs/library-guide.md): API usage, reports, errors, and compiled examples.
- [Administration](docs/administration.md): installation, storage preparation, and maintenance.
- [Development and PoC handoff](docs/development.md): source orientation, tests, and current validation results.
- [Deferred validation](docs/deferred-validation.md): qualification still needed beyond this first pass.
- [PoC v2 implementation plan](docs/pocv2-implementation-plan.md): staged next-milestone work; current status: Stage 6.
- [Deferred PoC v3 assessment](docs/pocv3-reference-resolution-assessment.md): reference resolution, inherited Skopeo behavior, and future Sarus integration; outside PoC v2.

The PoC stores one platform-specific OCI manifest per ref. A registry tag may
select it through an index, but complete-index acquisition, multi-platform stored
images, and index export remain unsupported. Structural checks do not hash blobs
or certify that an image will run. Lustre/VAST cross-node behavior and crash
durability remain unqualified. Store format 2 and ref metadata schema 3 are used;
format-1 stores are rejected without migration.
