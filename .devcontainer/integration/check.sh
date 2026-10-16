#!/usr/bin/env bash
set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/setup.sh"

id
podman --version
skopeo --version
rustc --version
printf 'Graphroot backing filesystem: '
stat -f -c %T "$integration_graphroot"
integration_check_storage
