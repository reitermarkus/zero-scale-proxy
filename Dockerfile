FROM lukemathwalker/cargo-chef:0.1.34-rust-1.59.0-alpine3.14 as base
WORKDIR /app

FROM base as planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM base as builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release --bin zero-scale-proxy

FROM alpine:3.15.0 as runner
# renovate: datasource=repology depName=alpine_3_14/tini versioning=loose
ARG TINI_VERSION=0.19.0-r0
RUN apk add --no-cache tini=${TINI_VERSION}
COPY --from=builder /app/target/release/zero-scale-proxy /usr/local/bin/
ENTRYPOINT ["tini", "--"]
CMD ["zero-scale-proxy"]
