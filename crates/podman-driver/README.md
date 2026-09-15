# sarus-suite-podman-driver

A small Rust driver that turns an EDF (execution description file) into Podman commands. It
composes the image, mounts, devices, environment, annotations, workdir, read-only mode, and
container stream settings into one command without invoking a shell.

## Quick start
To use this library, add it to your project Cargo.toml:

```toml
[dependencies]
sarus-suite-podman-driver = { git = "https://github.com/sarus-suite/podman-driver" }
raster = { git = "https://github.com/sarus-suite/raster" }
```

This crate is named `sarus-suite-podman-driver` and depends on the `raster` library for EDF rendering.
