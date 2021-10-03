#!/usr/bin/env bash

export SERVICE=sdtd-tcp,sdtd-udp
export DEPLOYMENT=sdtd
export NAMESPACE=default
export PROXY_TYPE=7d2d
export TIMEOUT=60
export RUST_LOG=zero_scale_proxy=trace

cargo run --release
