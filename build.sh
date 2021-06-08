#!/usr/bin/env bash

set -euo pipefail

: "${image:=reitermarkus/zero-scale-proxy}"
: "${cache_image:=${image}:cache}"

docker buildx create --name mybuilder || true
docker buildx use mybuilder
docker buildx build . \
  --platform linux/amd64,linux/arm64 \
  --cache-from "type=registry,ref=${cache_image}" \
  --cache-to "type=registry,ref=${cache_image},mode=max" \
  --tag "${image}" \
  "${@}"
