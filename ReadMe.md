Zero-scale Proxy

---

# Environment Variables

| Variable     | Description                                        | Default Value |
|--------------|----------------------------------------------------|---------------|
| `SERVICE`    | name of the Kubernetes service                     | N/A           |
| `DEPLOYMENT` | name of the Kubernetes deployment                  | N/A           |
| `NAMESPACE`  | namespace of the Kubernetes deployment and service | N/A           |
| `TIMEOUT`    | timeout in seconds after which the deployment is scaled down again | `600` |

# Testing

```sh
env SERVICE=minecraft-minecraft DEPLOYMENT=minecraft-minecraft NAMESPACE=default PROXY_TYPE=7d2d UPSTREAM_IP=7d2d.local UPSTREAM_PORT=26900 RUST_LOG=zero_scale_proxy=trace cargo run --release
```
