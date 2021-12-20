FROM lukemathwalker/cargo-chef as base
WORKDIR /app

FROM base as planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM base as cache
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

FROM base as builder
COPY --from=cache /app/target target
COPY --from=cache /usr/local/cargo /usr/local/cargo
COPY . .
RUN cargo build --release --bin zero-scale-proxy

FROM debian:buster-slim as runner
RUN apt-get update \
 && apt-get install -y openssl tini \
 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/zero-scale-proxy /usr/local/bin/
ENTRYPOINT ["tini", "--"]
CMD ["zero-scale-proxy"]
