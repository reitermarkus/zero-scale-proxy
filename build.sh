#!/usr/bin/env bash

set -euo pipefail

: "${image:=reitermarkus/zero-scale-proxy}"
: "${cache_image:=${image}:cache}"

if [[ "${1-}" = '--push' ]]; then
  cache_to=( --cache-to "type=registry,ref=${cache_image},mode=max" )
else
  cache_to=()
fi

docker buildx create --name mybuilder || true
docker buildx use mybuilder

docker buildx build . \
  --platform linux/amd64,linux/arm64 \
  --cache-from "type=registry,ref=${cache_image}" \
  "${cache_to[@]}" \
  --target cache \
  --tag "${cache_image}" \
  "${@}"

docker buildx build . \
  --platform linux/amd64,linux/arm64 \
  --cache-from "type=registry,ref=${cache_image}" \
  "${cache_to[@]}" \
  --tag "${image}" \
  "${@}"
