#!/usr/bin/env bash
set -euo pipefail

id
podman version
echo "Podman store configuration:"
podman info -f json | jq .store
echo "========================"
skopeo --version
rustc --version
