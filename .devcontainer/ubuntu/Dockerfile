FROM ghcr.io/sarus-suite/cluster-tooling/skybox-dev-base:latest AS base

FROM base AS chef-planner

USER root
RUN mkdir -p /workspaces/cluster-tooling && chown -R vscode:vscode /workspaces/cluster-tooling
WORKDIR /workspaces/cluster-tooling
COPY --chown=vscode:vscode Cargo.toml Cargo.lock ./
COPY --chown=vscode:vscode crates/skybox/Cargo.toml crates/skybox/Cargo.toml
COPY --chown=vscode:vscode crates/sarusctl/Cargo.toml crates/sarusctl/Cargo.toml
COPY --chown=vscode:vscode crates/podman-driver/Cargo.toml crates/podman-driver/Cargo.toml
COPY --chown=vscode:vscode crates/raster/Cargo.toml crates/raster/Cargo.toml
RUN mkdir -p crates/skybox/src crates/sarusctl/src crates/sarusctl/tests crates/podman-driver/src crates/raster/src && \
    printf 'pub fn placeholder() {}\n' > crates/skybox/src/lib.rs && \
    printf 'pub fn placeholder() {}\n' > crates/sarusctl/src/lib.rs && \
    printf 'fn main() {}\n' > crates/sarusctl/src/main.rs && \
    printf '#[test]\nfn placeholder() {}\n' > crates/sarusctl/tests/cli.rs && \
    printf '#[test]\nfn placeholder() {}\n' > crates/sarusctl/tests/smoke.rs && \
    printf 'pub fn placeholder() {}\n' > crates/podman-driver/src/lib.rs && \
    printf 'pub fn placeholder() {}\n' > crates/raster/src/lib.rs

USER vscode
RUN cargo chef prepare --recipe-path recipe.json

FROM base AS chef-builder

USER root
RUN mkdir -p /workspaces/cluster-tooling && chown -R vscode:vscode /workspaces/cluster-tooling
WORKDIR /workspaces/cluster-tooling
COPY --from=chef-planner --chown=vscode:vscode /workspaces/cluster-tooling/recipe.json ./recipe.json

USER vscode
RUN cargo chef cook --recipe-path recipe.json
RUN cargo test --locked -p sarusctl --test cli --no-run
RUN cargo chef cook --release --recipe-path recipe.json

FROM base

COPY --from=chef-builder --chown=vscode:vscode /home/vscode/.cargo/registry /home/vscode/.cargo/registry
COPY --from=chef-builder --chown=vscode:vscode /home/vscode/.cargo/git /home/vscode/.cargo/git
COPY --from=chef-builder --chown=vscode:vscode /opt/cargo-target /opt/cargo-target
