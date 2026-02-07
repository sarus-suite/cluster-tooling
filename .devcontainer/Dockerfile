FROM --platform=linux/amd64 opensuse/leap:15.5

# Basic tooling + deps
RUN zypper -n refresh && \
    zypper -n install --no-recommends \
      gcc gcc-c++ make \
      pkg-config \
      git \
      clang llvm \
      cmake \
      python3 \
      wget curl \
      gawk \
      bzip2 tar \
      gzip \
      which \
      munge munge-devel \
      hwloc hwloc-devel \
      libnuma-devel \
      ca-certificates \
    && zypper -n clean --all

# ---- Install Rust using rustup (minimal) ----
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo
ENV PATH=${CARGO_HOME}/bin:${PATH}

RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable && \
    rustc -V && cargo -V

WORKDIR /tmp

# ---- Slurm 24.05.8 (headers only) ----
RUN wget -q https://download.schedmd.com/slurm/slurm-24.05.8.tar.bz2 && \
    tar -xjf slurm-24.05.8.tar.bz2 && \
    cd slurm-24.05.8 && \
    ./configure --disable-dependency-tracking && \
    mkdir -p /usr/local/include/slurm && \
    \
    # headers used by plugins / bindings
    cp -a slurm/. /usr/local/include/slurm/ && \
    \
    # SPANK headers
    cp -a contribs/spank/. /usr/local/include/slurm/ || true && \
    \
    # in case of version header
    if [ -f slurm/slurm_version.h ]; then \
      cp -a slurm/slurm_version.h /usr/local/include/slurm/; \
    fi

# Create vscode user expected by devcontainers
RUN zypper -n install --no-recommends shadow sudo && \
    groupadd -g 1000 vscode && \
    useradd -m -u 1000 -g 1000 -s /bin/bash vscode && \
    echo "vscode ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/99-vscode && \
    chmod 0440 /etc/sudoers.d/99-vscode


ENV RUSTUP_HOME=/home/vscode/.rustup \
    CARGO_HOME=/home/vscode/.cargo
ENV PATH=/home/vscode/.cargo/bin:$PATH

RUN mkdir -p /home/vscode/.rustup /home/vscode/.cargo && \
    chown -R vscode:vscode /home/vscode/.rustup /home/vscode/.cargo && \
    chmod 1777 /tmp

USER vscode
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable && \
    rustc -V && cargo -V
USER root

ENV CPATH=/usr/local/include:/usr/local/include/slurm:/usr/include


