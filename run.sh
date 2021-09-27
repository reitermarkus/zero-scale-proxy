#!/usr/bin/env bash

export SERVICE=minecraft-minecraft
export DEPLOYMENT=minecraft-minecraft
export NAMESPACE=default
export PROXY_TYPE=7d2d
export UPSTREAM_IP=7d2d.local
export UPSTREAM_PORT=26900
export PORTS=26900,26901/udp,26902/udp,26903/udp
export RUST_LOG=zero_scale_proxy=trace

cargo run --release
