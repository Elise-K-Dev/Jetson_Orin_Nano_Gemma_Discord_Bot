#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

runtime="${DEV_SANDBOX_RUNTIME:-podman}"
image="${DEV_SANDBOX_IMAGE:-localhost/komi-dev:latest}"

"$runtime" build --tag "$image" --file sandbox/Containerfile sandbox
