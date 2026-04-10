#!/usr/bin/env bash

smoke_repo_root() {
  cd "${BATS_TEST_DIRNAME}/../.." && pwd
}

smoke_require_cmds_or_skip() {
  local cmd
  for cmd in "$@"; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
      skip "missing required command: $cmd"
    fi
  done
}

smoke_init_file_env() {
  export SMOKE_REPO_ROOT="$(smoke_repo_root)"
  export PODMAN_BINARY="/usr/local/bin/podman"
  export PARALLAX_BINARY="/usr/local/bin/parallax"
  export MOUNT_PROGRAM_PATH="/usr/local/bin/parallax-mount-program"
  export MKSQUASHFS_PATH="/usr/bin/mksquashfs"
  export SARUSCTL_BINARY="${SMOKE_REPO_ROOT}/dist/sarusctl"
  export PARALLAX_MP_UID="$(id -u)"
  export PARALLAX_MP_GID="$(id -g)"
  export PARALLAX_MP_SQUASHFUSE_CMD="/usr/bin/squashfuse_ll"

  export SMOKE_TMPDIR="$(mktemp -d)"
  export HOME="${SMOKE_TMPDIR}/home"
  export PODMAN_ROOT="${SMOKE_TMPDIR}/podman-root"
  export PODMAN_RUNROOT="${SMOKE_TMPDIR}/podman-runroot"
  export RO_STORAGE="${SMOKE_TMPDIR}/ro-storage"
  export CLEAN_ROOT="${SMOKE_TMPDIR}/clean-root"
  export ANNOTATION_LOGFILE="${SMOKE_TMPDIR}/annotation.log"

  mkdir -p "$HOME/.edf" "$PODMAN_ROOT" "$PODMAN_RUNROOT" "$RO_STORAGE" "$CLEAN_ROOT"
}

smoke_cleanup_file_env() {
  if [ -n "${SMOKE_TMPDIR:-}" ] && [ -d "${SMOKE_TMPDIR}" ]; then
    sleep 5

    chmod -R u+rwX "${SMOKE_TMPDIR}" 2>/dev/null || true

    if command -v "${PODMAN_BINARY:-podman}" >/dev/null 2>&1; then
      "${PODMAN_BINARY:-podman}" unshare chmod -R u+rwX "${SMOKE_TMPDIR}" 2>/dev/null || true
    fi

    rm -rf "${SMOKE_TMPDIR}" 2>/dev/null || true
  fi
}

smoke_prepare_busybox_ro_store() {
  if [ -f "${RO_STORAGE}/.busybox-ready" ]; then
    return 0
  fi

  run "$PODMAN_BINARY" --root "$PODMAN_ROOT" --runroot "$PODMAN_RUNROOT" pull busybox:latest
  assert_success

  run "$PARALLAX_BINARY" \
    --podmanRoot "$PODMAN_ROOT" \
    --roStoragePath "$RO_STORAGE" \
    --mksquashfsPath "$MKSQUASHFS_PATH" \
    --log-level info \
    --migrate \
    --image busybox:latest
  assert_success

  touch "${RO_STORAGE}/.busybox-ready"
}

smoke_write_edf() {
  local name="$1"
  local image="$2"
  local extra="${3:-}"
  local file="${HOME}/.edf/${name}.toml"

  cat >"${file}" <<EOF
image = "${image}"
EOF

  if [ -n "${extra}" ]; then
    printf '\n%s\n' "${extra}" >>"${file}"
  fi
}
