FROM debian:12-slim

ARG SLURM_VERSION=26.5.4
ARG RUST_VERSION=1.98.1
ARG RUSTUP_VERSION=1.29.1
ARG RUSTUP_INIT_SHA256_AMD64=dda7234360b7f578ca8b0ddcb80145646fa61a67c1720a5abc7051b35c9fcb71
ARG RUSTUP_INIT_SHA256_ARM64=15f6e4ce9f583b929c996c91562bad6d4454f3281de858b02cdfdef615fac433

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        bash \ 
	bzip2 \
        ca-certificates \
        curl \
        gcc \
        libc6-dev \
	libclang1 \
	libmunge-dev \
        make \
        pkg-config \
        tar \
    && rm -rf /var/lib/apt/lists/*

RUN test "$(getconf GNU_LIBC_VERSION)" = 'glibc 2.36'

# Install a pinned rustup-init binary instead of executing the mutable
# sh.rustup.rs installer. Checksums are published alongside the archive.
RUN set -eu; \
    case "$(uname -m)" in \
        aarch64) rust_target='aarch64-unknown-linux-gnu'; rustup_sha="${RUSTUP_INIT_SHA256_ARM64}" ;; \
        x86_64) rust_target='x86_64-unknown-linux-gnu'; rustup_sha="${RUSTUP_INIT_SHA256_AMD64}" ;; \
        *) printf 'unsupported Rust build architecture: %s\n' "$(uname -m)" >&2; exit 1 ;; \
    esac; \
    rustup_url="https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/${rust_target}/rustup-init"; \
    curl --proto '=https' --tlsv1.2 -fsSLo /tmp/rustup-init "${rustup_url}"; \
    printf '%s  %s\n' "${rustup_sha}" /tmp/rustup-init | sha256sum -c -; \
    chmod 0755 /tmp/rustup-init; \
    /tmp/rustup-init -y --profile minimal --default-toolchain none; \
    rm -f /tmp/rustup-init; \
    /root/.cargo/bin/rustup toolchain install "${RUST_VERSION}" --profile minimal --target "${rust_target}"; \
    /root/.cargo/bin/rustup default "${RUST_VERSION}"

ENV PATH="/root/.cargo/bin:${PATH}"

RUN set -eu; \
    PREV_FOLDER=$(pwd); \
    mkdir -p /tmp/slurm; \
    cd /tmp/slurm; \
    SLURM_VERSION_FORMATTED=$(echo ${SLURM_VERSION} | awk -F. '{printf "%02d.%02d.%d\n",$1,$2,$3}'); \
    SLURM_RELEASE_FOLDER=$(echo ${SLURM_VERSION} | awk -F. '{printf "slurm-%02d-%02d-%d-%d\n",$1,$2,$3,1}'); \
    curl -sLO "https://github.com/SchedMD/slurm/releases/download/${SLURM_RELEASE_FOLDER}/slurm-${SLURM_VERSION_FORMATTED}.tar.bz2"; \
    tar xjf "./slurm-${SLURM_VERSION_FORMATTED}.tar.bz2"; \
    cd "slurm-${SLURM_VERSION_FORMATTED}"; \
    ./configure; \
    mkdir -p /usr/include/slurm; \
    cp -a slurm/*.h /usr/include/slurm/; \
    cd "${PREV_FOLDER}"

CMD ["bash"]
