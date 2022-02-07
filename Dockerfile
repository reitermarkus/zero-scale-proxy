FROM rust:slim-buster as base
WORKDIR /app
RUN apt-get update \
 && apt-get install -y pkg-config libssl-dev
RUN cargo install cargo-chef

FROM base as planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM base as builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release --bin zero-scale-proxy

FROM debian:buster-slim as runner
RUN apt-get update \
 && apt-get install -y openssl tini \
 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/zero-scale-proxy /usr/local/bin/
ENTRYPOINT ["tini", "--"]
CMD ["zero-scale-proxy"]
