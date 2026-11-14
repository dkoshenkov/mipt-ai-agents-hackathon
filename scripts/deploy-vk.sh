#!/usr/bin/env bash
set -euo pipefail

# Run on the VM from the directory with compose.yaml. The registry token comes from stdin.
export IMAGE_PREFIX="${1:?missing image prefix}" IMAGE_TAG="${2:?missing image tag}"
registry_user=${3:?missing registry user}
export DOCKER_CONFIG
DOCKER_CONFIG=$(mktemp -d)
trap 'rm -rf -- "$DOCKER_CONFIG"' EXIT

docker login ghcr.io --username "$registry_user" --password-stdin
docker compose pull --policy always
docker compose up -d --no-build --pull never --wait --wait-timeout 1800
docker compose ps
