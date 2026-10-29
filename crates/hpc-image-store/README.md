# HPC image store

A first proof of concept for persistent, user-owned, filesystem-based,
OCI container image storage on Linux.
The workspace-private crate provides a synchronous Rust library and the
`hpc-image-store` development CLI.

Import images from local Podman containers-storage, pull one OCI image manifest
directly from a registry, and export stored images back to containers-storage.
The store shares immutable image objects between refs. It also supports inventory,
structural checking, explicit ref removal, conservative pruning, and a library-only
availability query.

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
hpc-image-store --store /scratch/me/images check analysis/tool:release
hpc-image-store --store /scratch/me/images export analysis/tool:release localhost/restored:latest
hpc-image-store --store /scratch/me/images remove analysis/tool:release
hpc-image-store --store /scratch/me/images prune --dry-run
```

Import, pull, and remove initialize a missing or empty store beneath its prepared
parent. There is no separate `init` command. Removal and content reclamation are
separate operations; review prune's report before deleting content.

## Documentation

- [Architecture](docs/architecture.md): authoritative behavior, guarantees, and storage decisions.
- [User guide](docs/user-guide.md): CLI workflows, selection, output, and troubleshooting.
- [Rust library guide](docs/library-guide.md): API usage, reports, errors, and compiled examples.
- [Administration](docs/administration.md): installation, storage preparation, and maintenance.
- [Development and PoC handoff](docs/development.md): source orientation, tests, and current validation results.
- [Deferred validation](docs/deferred-validation.md): qualification still needed beyond this first pass.

The PoC stores one platform-specific OCI manifest per ref. A registry tag may
select it through an index, but complete-index acquisition, multi-platform stored
images, and index export remain unsupported. Structural checks do not hash blobs
or certify that an image will run. Lustre/VAST cross-node behavior and crash
durability remain unqualified. Store format 1 and ref metadata schema 3 are retained;
obsolete development stores are not migrated automatically.
