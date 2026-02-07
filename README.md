# Sarus Suite's Cluster Tooling Super-Crate Workspace

This repository is a **Rust workspace (“super-crate”)** that brings together the main components of the Sarus Suite for the cluster tooling so they can be developed and built **from a single checkout**.
The workspace is intended to make it easy to build tooling artifacts like **`skybox`** (a Slurm SPANK plugin written in Rust) inside a reproducible environment that already contains the required system dependencies such as Slurm headers.

Instead of consuming `raster` and `podman-driver` directly from git, this workspace **patches those dependencies to local paths**, allowing you to iterate on all components together.

---

## Workspace Layout

```
.
├── crates/
│   ├── skybox/            # Slurm SPANK plugin (cdylib)
│   ├── sarusctl/          # Test CLI and utilities
│   ├── podman-driver/     # sarus-suite-podman-driver crate
│   └── raster/            # EDF rendering / validation library
├── .devcontainer/         # Root devcontainer with Rust + Slurm headers
└── Cargo.toml             # Workspace manifest (super-crate)
```

### Members

| Crate                         | Purpose                                                           |
| ----------------------------- | ----------------------------------------------------------------- |
| **skybox**                    | Slurm SPANK plugin implemented in Rust, built as a shared library |
| **sarusctl**                  | CLI for testing raster + podman integration                       |
| **sarus-suite-podman-driver** | Library for constructing Podman invocations from EDF              |
| **raster**                    | EDF schema, rendering, and validation utilities                   |

---

## Local Development Model

Downstream crates declare dependencies using git URLs:

```toml
raster = { git = "https://github.com/sarus-suite/raster" }
sarus-suite-podman-driver = { git = "https://github.com/sarus-suite/podman-driver" }
```

The workspace root overrides these with **path patches** so that local versions are used:

```toml
[patch."https://github.com/sarus-suite/podman-driver"]
sarus-suite-podman-driver = { path = "crates/podman-driver" }

[patch."https://github.com/sarus-suite/raster"]
raster = { path = "crates/raster" }
```

This allows:

* editing `raster` or `podman-driver`
* immediately rebuilding `skybox`
* without publishing to git or modifying individual crates

---

## Devcontainer (Recommended Build Environment)

The repository includes a **root devcontainer** called:

> **skybox-dev (rust + slurm-dev)**

### What the container provides

* openSUSE Leap base image
* Rust toolchain via `rustup`
* gcc/clang, cmake, pkg-config, and build essentials
* **Slurm 24.05.x headers including SPANK**
* Environment configured with:

```
CPATH=/usr/local/include:/usr/local/include/slurm:/usr/include
```

---

## Starting the Devcontainer

### Using the Devcontainer CLI

From the repository root:

```bash
devcontainer up --workspace-folder . --config .devcontainer/opensuse/devcontainer.json
devcontainer exec --workspace-folder . --config .devcontainer/opensuse/devcontainer.json -- bash
```

You are now inside the prepared build environment.

### VS Code

Open the repository in VS Code and choose:

```
Reopen in Container
```

using the root devcontainer configuration.

---

## Building Skybox

All commands should be executed **from the workspace root** inside the devcontainer.

### Clean + update (optional but recommended after changes)

```bash
cargo clean
cargo update
```

### Build only skybox

```bash
cargo build -p skybox
```

### Release build

```bash
cargo build -p skybox --release
```

The result is a shared library under:

```
target/debug/   (or target/release/)
```

---


### Building from a subdirectory

Always run cargo commands from the **workspace root**.
Running `cargo build` inside `crates/skybox` can bypass workspace configuration.

---

## Typical Workflow

1. Start devcontainer
```bash
devcontainer up --workspace-folder .
devcontainer exec --workspace-folder . -- bash
```
2. Edit:

   * raster schema
   * podman-driver logic
   * skybox plugin

3. Rebuild skybox:
```bash
cargo build -p skybox
```

4. Rebuild devcontainer in case of devcontainer/Dockerfile changes
* Edit .devcontainer/Dockerfile or .devcontainer/devcontainer.json
* Get the container ID of the devcontainer with `devcontainer up --workspace-folder .`
* `docker stop <container ID>`
* `docker rm <container ID>`
* Restart devcontainer as in 1

---

## License

See LICENSE file for this repository.
See individual crates for their licensing terms.

