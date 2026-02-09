FROM --platform=linux/amd64 ubuntu:24.04

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
    rustc -V && cargo -V
USER root

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
      # Some Ubuntu builds place headers under /usr/include/slurm; ensure it's present (helpful for tools expecting it)
      if [ -d /usr/include/slurm ]; then \
        mkdir -p /usr/local/include; \
        ln -sfn /usr/include/slurm /usr/local/include/slurm; \
      fi; \
    fi; \
    rm -rf /tmp/*


# Match your include path behavior
ENV CPATH=/usr/local/include:/usr/local/include/slurm:/usr/include

