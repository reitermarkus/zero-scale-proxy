FROM rust:alpine as base
WORKDIR /app
RUN apk add --no-cache musl-dev libressl-dev
RUN cargo install cargo-chef

FROM base as planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM base as cacher
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

FROM base as builder
COPY --from=cacher /app/target target
COPY --from=cacher /usr/local/cargo /usr/local/cargo
COPY . .
RUN cargo build --release --bin zero-scale-proxy

FROM alpine as runner
RUN apk add --no-cache libressl
COPY --from=builder /app/target/release/zero-scale-proxy /usr/local/bin/
CMD ["zero-scale-proxy"]
