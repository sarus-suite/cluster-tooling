FROM mcr.microsoft.com/devcontainers/base:bookworm

ARG SLURM_VERSION=26.5.4

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential ca-certificates clang libclang-dev libmunge-dev \
        bzip2 tar curl pkg-config python3 util-linux \
        podman skopeo uidmap slirp4netns crun iptables fuse-overlayfs \
    && rm -rf /var/lib/apt/lists/*

# Reuse the base image's user, sudo configuration and subordinate IDs. Allocate
# a non-overlapping range only if the image has no allocation for vscode.
RUN set -eu; \
    for file in /etc/subuid /etc/subgid; do \
        touch "$file"; \
        if ! grep -q '^vscode:' "$file"; then \
            start=$(awk -F: 'BEGIN {end=100000} $2+$3>end {end=$2+$3} END {print end}' "$file"); \
            printf 'vscode:%s:65536\n' "$start" >> "$file"; \
        fi; \
        awk -F: '$1=="vscode" && $2>0 && $3>0 {count+=$3} END {exit count<65536}' "$file"; \
    done

# Header-only Slurm setup, following .devcontainer/skybox/Dockerfile.
RUN set -eu; \
    temporary=$(mktemp -d); \
    cd "$temporary"; \
    version=$(echo "$SLURM_VERSION" | awk -F. '{printf "%02d.%02d.%d",$1,$2,$3}'); \
    release=$(echo "$SLURM_VERSION" | awk -F. '{printf "slurm-%02d-%02d-%d-%d",$1,$2,$3,1}'); \
    curl --proto '=https' --tlsv1.2 -fsSL -o slurm.tar.bz2 \
        "https://github.com/SchedMD/slurm/releases/download/${release}/slurm-${version}.tar.bz2"; \
    tar xjf slurm.tar.bz2; \
    cd "slurm-${version}"; \
    ./configure; \
    mkdir -p /usr/include/slurm; \
    cp -a slurm/*.h /usr/include/slurm/; \
    test -s /usr/include/slurm/slurm.h; \
    test -s /usr/include/slurm/spank.h; \
    test -s /usr/include/slurm/slurm_version.h; \
    cd /; \
    rm -rf "$temporary"

COPY .devcontainer/integration/containers.conf /etc/containers/containers.conf
RUN printf 'unqualified-search-registries = ["docker.io"]\n' \
        > /etc/containers/registries.conf

ENV XDG_RUNTIME_DIR=/tmp/cluster-tooling-runtime \
    CARGO_HOME=/home/vscode/.cargo \
    RUSTUP_HOME=/home/vscode/.rustup \
    CARGO_TARGET_DIR=/home/vscode/.cache/cargo-target \
    CONTAINERS_STORAGE_CONF=/home/vscode/.config/containers/integration-native.conf \
    INTEGRATION_OVERLAY_MODE=native \
    PATH=/home/vscode/.cargo/bin:${PATH}

USER vscode
ARG RUST_TOOLCHAIN=stable
RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs -o /tmp/integration-rustup.sh \
    && sh /tmp/integration-rustup.sh -y --profile minimal --default-toolchain "$RUST_TOOLCHAIN" \
    && rm /tmp/integration-rustup.sh \
    && rustup component add rustfmt clippy \
    && mkdir -p "$CARGO_TARGET_DIR"

WORKDIR /workspaces/cluster-tooling
CMD ["sleep", "infinity"]
