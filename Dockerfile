FROM rust:slim-buster as builder
WORKDIR /app
RUN apt-get update \
 && apt-get install -y pkg-config libssl-dev

COPY Cargo.* ./
RUN mkdir -p src && touch src/lib.rs \
 && cargo fetch \
 && cargo build --release \
 && rm src/lib.rs

COPY ./src/ ./src/
RUN cargo build --release --bin zero-scale-proxy

FROM debian:buster-slim as runner
RUN apt-get update \
 && apt-get install -y openssl tini \
 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/zero-scale-proxy /usr/local/bin/
ENTRYPOINT ["tini", "--"]
CMD ["zero-scale-proxy"]
