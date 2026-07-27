# Multi-stage Dockerfile for Cheburgram Server
FROM rust:alpine as builder

RUN apk add --no-cache musl-dev

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN cargo build --release --bin cheburgram-server

FROM alpine:latest
WORKDIR /app
COPY --from=builder /app/target/release/cheburgram-server /app/cheburgram-server

# TCP Сигналы: 7878, UDP Медиа-реле: 7879
EXPOSE 7878/tcp
EXPOSE 7879/udp

CMD ["/app/cheburgram-server"]
