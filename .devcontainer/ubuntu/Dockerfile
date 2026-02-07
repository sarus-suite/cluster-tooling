FROM --platform=linux/amd64 ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive

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

# ---- Slurm 24.05.8 headers (incl. SPANK) ----
RUN wget -q https://download.schedmd.com/slurm/slurm-24.05.8.tar.bz2 && \
    tar -xjf slurm-24.05.8.tar.bz2 && \
    cd slurm-24.05.8 && \
    ./configure --disable-dependency-tracking && \
    mkdir -p /usr/local/include/slurm && \
    cp -a slurm/. /usr/local/include/slurm/ && \
    cp -a contribs/spank/. /usr/local/include/slurm/ || true && \
    if [ -f slurm/slurm_version.h ]; then cp -a slurm/slurm_version.h /usr/local/include/slurm/; fi


# Match your include path behavior
ENV CPATH=/usr/local/include:/usr/local/include/slurm:/usr/include

