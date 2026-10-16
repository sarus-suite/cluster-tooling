#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."

case "${1:-workspace}" in
    workspace|image-store|overlay-smoke) ;;
    *) echo "Usage: bash $0 [workspace|image-store|overlay-smoke]" >&2; exit 2 ;;
esac
source .devcontainer/integration/setup.sh

case "${1:-workspace}" in
    workspace)
        bash .devcontainer/integration/check.sh
        # The driver tests share image tags, container names and a pidfile.
        cargo test --locked --workspace -- --test-threads=1
        ;;
    image-store)
        bash .devcontainer/integration/check.sh
        session=$(mktemp -d /tmp/cluster-tooling-transfers.XXXXXXXX)
        export CONTAINERS_STORAGE_CONF="$session/storage.conf"
        export HPC_IMAGE_STORE_TEST_DISPOSABLE=1
        cleanup() {
            # Image layers can contain subordinate-UID-owned directories.
            podman unshare rm -rf -- "$session"
        }
        trap cleanup EXIT
        integration_write_storage_config "$CONTAINERS_STORAGE_CONF" "$session/storage" "$session/run"
        integration_check_storage
        podman unshare cargo test --locked -p hpc-image-store \
            --test real_transfers -- --ignored --nocapture --test-threads=1
        cargo test --locked -p hpc-image-store \
            actual_skopeo_retains_locks_after_parent_termination -- --ignored --nocapture
        ;;
    overlay-smoke)
        bash .devcontainer/integration/check.sh
        container=$(podman create docker.io/library/alpine:3.22 \
            sh -ec 'echo nested-overlay-ok > /overlay-check; test "$(cat /overlay-check)" = nested-overlay-ok; cat /overlay-check')
        cleanup() { podman rm --force "$container" >/dev/null; }
        trap cleanup EXIT
        podman unshare bash -s -- "$container" "$INTEGRATION_OVERLAY_MODE" <<'SH'
set -euo pipefail
container=$1
mode=$2
mountpoint=$(podman mount "$container")
trap 'podman unmount "$container" >/dev/null' EXIT
filesystem=$(findmnt --noheadings --raw --output FSTYPE --target "$mountpoint")
printf 'Mounted container filesystem: %s\n' "$filesystem"
case "$mode:$filesystem" in
    native:overlay|fuse:fuse-overlayfs|fuse:fuse.overlayfs|fuse:fuse.fuse-overlayfs) ;;
    *) echo "Unexpected mounted filesystem for $mode overlay: $filesystem" >&2; exit 1 ;;
esac
SH
        podman start --attach "$container"
        status=$(podman inspect --format '{{.State.ExitCode}}' "$container")
        [[ "$status" == 0 ]]
        ;;
esac
