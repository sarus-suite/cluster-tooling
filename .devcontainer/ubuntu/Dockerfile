FROM --platform=linux/amd64 ubuntu:24.04 AS base

ENV DEBIAN_FRONTEND=noninteractive

# can be set at build time with --build-arg SLURM_VERSION=24.05.8
ARG SLURM_VERSION=""

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
      build-essential \
      gcc g++ make \
      pkg-config \
      git \
      clang llvm \
      cmake \
      python3 \
      wget curl \
      gawk \
      bzip2 tar gzip \
      which \
      munge libmunge-dev \
      hwloc libhwloc-dev \
      libnuma-dev \
      ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN set -eux; \
    if ! id -u vscode >/dev/null 2>&1; then \
      groupadd -r vscode; \
      useradd -m -s /bin/bash -g vscode vscode; \
    fi; \
    apt-get update && apt-get install -y --no-install-recommends sudo && rm -rf /var/lib/apt/lists/*; \
    echo "vscode ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/99-vscode; \
    chmod 0440 /etc/sudoers.d/99-vscode

USER vscode
ENV RUSTUP_HOME=/home/vscode/.rustup \
    CARGO_HOME=/home/vscode/.cargo
ENV PATH=/home/vscode/.cargo/bin:${PATH}

RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable && \
    cargo install cargo-chef --locked && \
    rustc -V && cargo -V && cargo chef --version

USER root
RUN mkdir -p /opt/cargo-target && chown -R vscode:vscode /opt/cargo-target
RUN mkdir -p /home/vscode/.cargo/git && chown -R vscode:vscode /home/vscode/.cargo
ENV CARGO_TARGET_DIR=/opt/cargo-target

WORKDIR /tmp

# Slurm (either tarball if SLURM_VERSION is set or from apt if not)
RUN set -eux; \
    if [ -n "${SLURM_VERSION}" ]; then \
      echo "Installing Slurm headers from tarball: ${SLURM_VERSION}"; \
      wget -q "https://download.schedmd.com/slurm/slurm-${SLURM_VERSION}.tar.bz2"; \
      tar -xjf "slurm-${SLURM_VERSION}.tar.bz2"; \
      cd "slurm-${SLURM_VERSION}"; \
      ./configure --disable-dependency-tracking; \
      mkdir -p /usr/local/include/slurm; \
      cp -a slurm/. /usr/local/include/slurm/; \
      cp -a contribs/spank/. /usr/local/include/slurm/ || true; \
      if [ -f slurm/slurm_version.h ]; then cp -a slurm/slurm_version.h /usr/local/include/slurm/; fi; \
    else \
      echo "Installing Slurm from apt (services + client) and dev headers"; \
      apt-get update; \
      apt-get install -y --no-install-recommends \
        munge \
        slurmctld slurmd slurm-client \
        libslurm-dev \
      ; \
      rm -rf /var/lib/apt/lists/*; \
      if [ -d /usr/include/slurm ]; then \
        mkdir -p /usr/local/include; \
        ln -sfn /usr/include/slurm /usr/local/include/slurm; \
      fi; \
    fi; \
    rm -rf /tmp/*

ENV CPATH=/usr/local/include:/usr/local/include/slurm:/usr/include

FROM base AS chef-planner

RUN mkdir -p /workspaces/cluster-tooling && chown -R vscode:vscode /workspaces/cluster-tooling
WORKDIR /workspaces/cluster-tooling
COPY --chown=vscode:vscode . .

USER vscode
RUN cargo chef prepare --recipe-path recipe.json

FROM base AS chef-builder

RUN mkdir -p /workspaces/cluster-tooling && chown -R vscode:vscode /workspaces/cluster-tooling
WORKDIR /workspaces/cluster-tooling
COPY --from=chef-planner --chown=vscode:vscode /workspaces/cluster-tooling/recipe.json ./recipe.json

USER vscode
RUN cargo chef cook --recipe-path recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

FROM base

COPY --from=chef-builder --chown=vscode:vscode /home/vscode/.cargo/registry /home/vscode/.cargo/registry
COPY --from=chef-builder --chown=vscode:vscode /home/vscode/.cargo/git /home/vscode/.cargo/git
COPY --from=chef-builder --chown=vscode:vscode /opt/cargo-target /opt/cargo-target
