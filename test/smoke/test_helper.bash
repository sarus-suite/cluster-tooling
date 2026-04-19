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
  local cleanup_rc=0
  local cleanup_error=""
  local remaining_entries=""
  local cleanup_details=""

  if [ -n "${SMOKE_TMPDIR:-}" ] && [ -d "${SMOKE_TMPDIR}" ]; then
    cleanup_error="$(rm -rf "${SMOKE_TMPDIR}" 2>&1)" || cleanup_rc=$?

    if [ "${cleanup_rc}" -ne 0 ] || [ -d "${SMOKE_TMPDIR}" ]; then
      if [ -d "${SMOKE_TMPDIR}" ]; then
        remaining_entries="$(find "${SMOKE_TMPDIR}" -mindepth 1 -maxdepth 1 -printf '%f\n' 2>/dev/null | paste -sd ', ' -)"
      fi

      if [ -n "${cleanup_error}" ]; then
        cleanup_details="rm exit ${cleanup_rc}: ${cleanup_error}"
      fi
      if [ -n "${remaining_entries}" ]; then
        if [ -n "${cleanup_details}" ]; then
          cleanup_details="${cleanup_details}; "
        fi
        cleanup_details="${cleanup_details}remaining entries: ${remaining_entries}"
      fi

      if [ -n "${cleanup_details}" ]; then
        printf 'smoke cleanup failed for %s (%s)\n' "${SMOKE_TMPDIR}" "${cleanup_details}" >&3
      else
        printf 'smoke cleanup failed for %s\n' "${SMOKE_TMPDIR}" >&3
      fi
    fi
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


smoke_cleanup_busybox_ro_store() {
  run "$PARALLAX_BINARY" \
    --podmanRoot "$PODMAN_ROOT" \
    --roStoragePath "$RO_STORAGE" \
    --log-level info \
    --rmi \
    --image busybox:latest
  assert_success

  run "$PODMAN_BINARY" --root "$PODMAN_ROOT" --runroot "$PODMAN_RUNROOT" rmi busybox:latest
  assert_success

  rm "${RO_STORAGE}/.busybox-ready"
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
