# Podman and Skopeo integration environment

Open the workspace in VS Code, run **Dev Containers: Reopen in Container**, and
select **Workspace integration tests (rootless Podman + Skopeo)**. With the
Dev Container CLI, use:

```sh
devcontainer up --workspace-folder . --config .devcontainer/integration/devcontainer.json
devcontainer exec --workspace-folder . --config .devcontainer/integration/devcontainer.json bash .devcontainer/integration/test.sh overlay-smoke
```

The image extends `mcr.microsoft.com/devcontainers/base:bookworm` and reuses its
`vscode` user and sudo setup. `updateRemoteUserUID` is enabled so Linux checkouts
remain writable after the user's UID/GID is matched to the host. Rust and Cargo
data live under `/home/vscode`; the private runtime directory is
`/tmp/cluster-tooling-runtime`, independent of the numeric UID.

Rust is installed separately with rustup, including rustfmt and Clippy. The
`RUST_TOOLCHAIN` build argument defaults to `stable`; set it to a Rust release
to pin the toolchain. `SLURM_VERSION` defaults to `26.5.4`. As in the Skybox
devcontainer, the build downloads and configures that Slurm source release and
installs only its headers under `/usr/include/slurm`. Compiler tools, libclang
and Munge development files are included; Slurm binaries and services are not.
Set these build arguments through the configuration's `build.args`, then rebuild.
Building the image and pulling test images require network access.

## Nested overlay storage

The outer container is privileged to allow nested user namespaces and mounts.
A rootful Docker or Podman host is the supported baseline; host policy can still
block user namespaces. Rootless outer engines may not have enough subordinate
IDs for another rootless engine. Existing username-based subordinate allocations
are preserved; missing allocations are added at image build time. UID rewriting
does not enlarge the outer engine's UID/GID mapping.

Inside the container, both Podman and Skopeo use the `overlay` storage driver:

| Mode | Mount implementation | Selection |
| --- | --- | --- |
| `native` | Kernel overlayfs | Default |
| `fuse` | `/usr/bin/fuse-overlayfs` | `INTEGRATION_OVERLAY_MODE=fuse` |

The modes have separate configurations under `~/.config/containers`, separate
graphroots under `~/.local/share/containers`, and separate runtime directories.
Graphroots remain on the devcontainer writable filesystem: this intentionally
tests nested storage rather than avoiding it with a host volume. Native overlay
may reject that backing filesystem. If Podman automatically chooses a mount
helper, native validation fails; it does not count as a native-overlay pass.
There is no VFS fallback. FUSE mode requires `/dev/fuse` access, supplied by the
privileged outer container when the host supports it.

Ordinary terminals inherit native storage. To select FUSE for interactive work
in the current Bash shell:

```bash
export INTEGRATION_OVERLAY_MODE=fuse
source .devcontainer/integration/setup.sh
```

No host container socket or host image store is mounted. Do not forward host
settings such as `CONTAINERS_STORAGE_CONF`, `CONTAINER_HOST`, or
`CONTAINER_CONNECTION`. Rebuild when changing storage setup; existing VFS stores
are not migrated, and the new graphroots do not reuse them.

## Running checks

From a container terminal at the workspace root:

```bash
# Verify versions, user namespaces, rootless status and the selected driver.
bash .devcontainer/integration/check.sh

# Pull Alpine, inspect the mounted filesystem and exercise a writable layer.
bash .devcontainer/integration/test.sh overlay-smoke

# Verify Skybox compilation against the downloaded Slurm headers.
cargo build --locked -p skybox

# Ordinary workspace tests, including Podman driver integration tests.
bash .devcontainer/integration/test.sh workspace

# Disposable Podman/Skopeo round trip and real-Skopeo lock acceptance.
bash .devcontainer/integration/test.sh image-store

# Test the FUSE implementation explicitly; results are separate from native.
INTEGRATION_OVERLAY_MODE=fuse bash .devcontainer/integration/test.sh overlay-smoke
INTEGRATION_OVERLAY_MODE=fuse bash .devcontainer/integration/test.sh workspace
INTEGRATION_OVERLAY_MODE=fuse bash .devcontainer/integration/test.sh image-store
```

Workspace tests run serially because driver tests share tags, container names
and a pidfile. Avoid concurrent runs within a mode. Image-store acceptance uses
a fresh disposable configuration inherited by both tools, with the selected
overlay implementation, and enters `podman unshare`. Its OCI fixtures are local
and require no registry access. Cleanup removes its temporary storage after
success or failure. Only named acceptance tests are selected; an unfiltered
`--ignored` would also execute internal test helpers.

Default images and Cargo artifacts survive restarts but are discarded on rebuild.
The Bats tests under `test/smoke` additionally require configured Slurm services
and the custom Podman/Parallax HPC stack from
`.github/workflows/slurm-single-node.yml`; they are not provisioned here.
Local overlay tests do not qualify HPC filesystem behavior.

## Validation results

See the implementation report for observed build and runtime results. A failure
of `podman unshare true` is a namespace prerequisite failure before overlay
validation; it does not establish whether native or FUSE overlay works.
