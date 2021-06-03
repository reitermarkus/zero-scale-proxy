FROM rust:alpine as builder

COPY . .
RUN apk add --no-cache musl-dev libressl-dev
RUN cargo install --path .

FROM alpine

COPY --from=builder /usr/local/cargo/bin/zero-scale-proxy /usr/local/bin/zero-scale-proxy
CMD ["zero-scale-proxy"]
