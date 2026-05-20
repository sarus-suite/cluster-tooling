#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${1:?missing repo root argument}"
INSTALL_CACHE_ROOT="/var/tmp/cluster-tooling-host-tools-cache"
INSTALL_STATE_ROOT="/var/tmp/cluster-tooling-host-tools-state"
INSTALL_BIN_ROOT="/var/tmp/cluster-tooling-host-tools-bin"
INSTALL_SUPPORT_ROOT="/var/tmp/cluster-tooling-host-tools-support"
COMMON_STAGE_ROOT="/var/tmp/cluster-tooling-common-stage"
SARUS_STAGE_ROOT="/var/tmp/cluster-tooling-sarus-stage"
DEPLOY_INSTALL_CACHE_ROOT="/var/tmp/cluster-tooling-deploy-installer-cache"
DEPLOY_INSTALL_STATE_ROOT="/var/tmp/cluster-tooling-deploy-installer-state"
DEPLOY_INSTALL_BIN_ROOT="/var/tmp/cluster-tooling-deploy-installer-bin"
DEPLOY_INSTALL_SUPPORT_ROOT="/var/tmp/cluster-tooling-deploy-installer-support"
DEPLOY_COMMON_STAGE_ROOT="/var/tmp/cluster-tooling-deploy-common-stage"
DEPLOY_SARUS_STAGE_ROOT="/var/tmp/cluster-tooling-deploy-sarus-stage"
ROOTLESS_PODMAN_USER="vagrant"
ROOTLESS_PODMAN_SUBID_START="100000"
ROOTLESS_PODMAN_SUBID_COUNT="65536"

log() {
  printf '[test-host-tools] %s\n' "$*"
}

ensure_subid_entry() {
  local file="$1"
  local user="$2"
  local start="$3"
  local count="$4"

  if grep -q "^${user}:" "${file}"; then
    sed -i "s/^${user}:.*/${user}:${start}:${count}/" "${file}"
    return 0
  fi

  printf '%s:%s:%s\n' "${user}" "${start}" "${count}" >> "${file}"
}

configure_rootless_podman_user() {
  log "configuring rootless Podman subordinate ID ranges for ${ROOTLESS_PODMAN_USER}"
  ensure_subid_entry /etc/subuid "${ROOTLESS_PODMAN_USER}" "${ROOTLESS_PODMAN_SUBID_START}" "${ROOTLESS_PODMAN_SUBID_COUNT}"
  ensure_subid_entry /etc/subgid "${ROOTLESS_PODMAN_USER}" "${ROOTLESS_PODMAN_SUBID_START}" "${ROOTLESS_PODMAN_SUBID_COUNT}"
}

configure_containers_policy() {
  log "installing containers policy for test Podman pulls"
  mkdir -p /etc/containers
  cat > /etc/containers/policy.json <<'EOF'
{
  "default": [{"type": "insecureAcceptAnything"}]
}
EOF
  chmod 0644 /etc/containers/policy.json
}

publish_test_tool_path() {
  cat > /etc/profile.d/cluster-tooling-host-tools.sh <<EOF
export PATH="${DEPLOY_INSTALL_BIN_ROOT}:${INSTALL_BIN_ROOT}:\$PATH"
EOF
  chmod 0644 /etc/profile.d/cluster-tooling-host-tools.sh
}

require_binary_in_prefix() {
  local prefix="$1"
  local tool="$2"
  [ -x "${prefix}/${tool}" ] || {
    echo "expected installed binary not found in ${prefix}: ${tool}" >&2
    exit 1
  }
}

verify_installed_tools() {
  local prefix="$1"

  for tool in \
    podman conmon crun fuse-overlayfs fusermount3 pasta \
    squashfuse squashfuse_ll mksquashfs unsquashfs \
    parallax parallax-mount-program
  do
    require_binary_in_prefix "${prefix}" "${tool}"
  done
}

main() {
  log "installing VM prerequisites"
  export DEBIAN_FRONTEND=noninteractive
  apt-get update
  apt-get install -y --no-install-recommends \
    build-essential \
    ca-certificates \
    curl \
    git \
    libattr1-dev \
    libfuse3-dev \
    liblz4-dev \
    liblzma-dev \
    liblzo2-dev \
    libzstd-dev \
    pkg-config \
    python3-yaml \
    tar \
    uidmap \
    zlib1g-dev

  configure_rootless_podman_user
  configure_containers_policy

  mkdir -p \
    "${INSTALL_CACHE_ROOT}" "${INSTALL_STATE_ROOT}" "${INSTALL_BIN_ROOT}" "${INSTALL_SUPPORT_ROOT}" \
    "${DEPLOY_INSTALL_CACHE_ROOT}" "${DEPLOY_INSTALL_STATE_ROOT}" "${DEPLOY_INSTALL_BIN_ROOT}" "${DEPLOY_INSTALL_SUPPORT_ROOT}"
  export PATH="${DEPLOY_INSTALL_BIN_ROOT}:${INSTALL_BIN_ROOT}:${PATH}"

  log "running general deploy installer for common host tools"
  python3 "${REPO_ROOT}/scripts/deploy/installer.py" common \
    --manifest "${REPO_ROOT}/scripts/deploy/manifests/host-tools.yaml" \
    --cache-dir "${DEPLOY_INSTALL_CACHE_ROOT}/common" \
    --install-prefix "${DEPLOY_INSTALL_BIN_ROOT}" \
    --support-root "${DEPLOY_INSTALL_SUPPORT_ROOT}/common" \
    --stage-root "${DEPLOY_COMMON_STAGE_ROOT}" \
    --state-root "${DEPLOY_INSTALL_STATE_ROOT}"

  log "running general deploy installer for Sarus Suite tools"
  python3 "${REPO_ROOT}/scripts/deploy/installer.py" sarus \
    --manifest "${REPO_ROOT}/scripts/deploy/manifests/sarus-suite-tools.yaml" \
    --cache-dir "${DEPLOY_INSTALL_CACHE_ROOT}/sarus" \
    --install-prefix "${DEPLOY_INSTALL_BIN_ROOT}" \
    --support-root "${DEPLOY_INSTALL_SUPPORT_ROOT}/sarus" \
    --stage-root "${DEPLOY_SARUS_STAGE_ROOT}" \
    --state-root "${DEPLOY_INSTALL_STATE_ROOT}"

  log "verifying installed binaries from deploy installer"
  verify_installed_tools "${DEPLOY_INSTALL_BIN_ROOT}"

  log "installed manifests from deploy installer"
  ls -l "${DEPLOY_INSTALL_STATE_ROOT}"

  log "publishing test tool PATH for login shells"
  publish_test_tool_path
}

main "$@"
