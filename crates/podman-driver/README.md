# README

A tiny Rust driver that turns a single declarative file (EDF) into the specific `podman run` you needed to deploy it successfully.
The crate composes all the moving pieces (image, mounts, devices, env, annotations, workdir, read-only mode) into one reliable command.

## Quick start
To use this library, add it to your project Cargo.toml:

```toml
[dependencies]
sarus-suite-podman-driver = { git = "https://github.com/sarus-suite/podman-driver" }
raster = { git = "https://github.com/sarus-suite/raster" }
```

This crate is named `sarus-suite-podman-driver` and depends on the `raster` library for EDF rendering.
