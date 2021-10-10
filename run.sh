#!/usr/bin/env bash

export TIMEOUT=60
export RUST_LOG=zero_scale_proxy=trace

if [[ "${1}" == 7d2d ]]; then
  export SERVICE=sdtd-tcp,sdtd-udp
  export DEPLOYMENT=sdtd
  export NAMESPACE=default
  export PROXY_TYPE=7d2d
elif [[ "${1}" == csgo ]]; then
  export UPSTREAM_HOST=csgo.local
  export UPSTREAM_PORT=27500
  export PROXY_TYPE=csgo
elif [[ "${1}" == minecraft ]]; then
  export SERVICE=minecraft-minecraft
  export DEPLOYMENT=minecraft-minecraft
  export NAMESPACE=default
  export PROXY_TYPE=minecraft
elif [[ "${1}" == teamspeak ]]; then
  export SERVICE=teamspeak,teamspeak-tcp
  export DEPLOYMENT=teamspeak
  export NAMESPACE=default
  export PROXY_TYPE=teamspeak
fi

cargo run --release
