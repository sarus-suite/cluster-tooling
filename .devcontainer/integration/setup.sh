#!/usr/bin/env bash
# Source this file so Podman, Skopeo and Cargo inherit the same configuration.

if [[ $(id -u) == 0 ]]; then
    echo 'Run integration tests as vscode, so Podman exercises its rootless path.' >&2
    return 1
fi

export INTEGRATION_OVERLAY_MODE=${INTEGRATION_OVERLAY_MODE:-native}
case "$INTEGRATION_OVERLAY_MODE" in
    native|fuse) ;;
    *) echo 'INTEGRATION_OVERLAY_MODE must be native or fuse.' >&2; return 2 ;;
esac

export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/tmp/cluster-tooling-runtime}
install -d -m 0700 "$XDG_RUNTIME_DIR"
if [[ ! -O "$XDG_RUNTIME_DIR" ]]; then
    echo "Runtime directory must belong to the current UID: $XDG_RUNTIME_DIR" >&2
    return 1
fi

integration_write_storage_config() {
    local configuration=$1 graphroot=$2 runroot=$3
    python3 - "$configuration" "$graphroot" "$runroot" "$INTEGRATION_OVERLAY_MODE" <<'PY'
import json
import pathlib
import sys

configuration, graphroot, runroot, mode = sys.argv[1:]
contents = (
    '[storage]\ndriver = "overlay"\n'
    f'graphroot = {json.dumps(graphroot)}\n'
    f'runroot = {json.dumps(runroot)}\n'
    '\n[storage.options.overlay]\n'
    f'mount_program = {json.dumps("/usr/bin/fuse-overlayfs" if mode == "fuse" else "")}\n'
)
path = pathlib.Path(configuration)
path.parent.mkdir(parents=True, exist_ok=True)
if not path.exists() or path.read_text() != contents:
    path.write_text(contents)
PY
}

export CONTAINERS_STORAGE_CONF="$HOME/.config/containers/integration-${INTEGRATION_OVERLAY_MODE}.conf"
integration_graphroot="$HOME/.local/share/containers/integration-${INTEGRATION_OVERLAY_MODE}"
install -d -m 0700 "$integration_graphroot"
integration_write_storage_config "$CONTAINERS_STORAGE_CONF" "$integration_graphroot" \
    "$XDG_RUNTIME_DIR/integration-${INTEGRATION_OVERLAY_MODE}"

integration_check_storage() {
    local information
    if ! podman unshare true; then
        echo 'Nested user namespaces failed before overlay validation. Check subordinate IDs, privileged mode and host policy.' >&2
        return 1
    fi
    information=$(podman info --format json) || return
    python3 - "$INTEGRATION_OVERLAY_MODE" "$information" <<'PY'
import json
import sys

mode = sys.argv[1]
info = json.loads(sys.argv[2])
store = info['store']
options = store.get('graphOptions') or {}
print(f'Overlay mode requested: {mode}')
print(f'Rootless: {info["host"]["security"]["rootless"]}')
print(f'Storage: {json.dumps(store, indent=2)}')
if not info['host']['security']['rootless'] or store['graphDriverName'] != 'overlay':
    sys.exit('Expected rootless Podman with the overlay storage driver.')
programs = [value for key, value in options.items() if 'mount_program' in key and value]
if mode == 'native' and programs:
    sys.exit('Native overlay unavailable: Podman selected a mount helper. Select fuse mode explicitly to test it.')
if mode == 'fuse' and '/usr/bin/fuse-overlayfs' not in json.dumps(programs):
    sys.exit('FUSE mode did not select /usr/bin/fuse-overlayfs.')
PY
}
