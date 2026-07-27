# Multi-stage Dockerfile for Cheburgram Server (Lightweight)
FROM rust:alpine as builder

RUN apk add --no-cache musl-dev

WORKDIR /app
COPY crates/protocol ./crates/protocol
COPY crates/server ./crates/server

# Генерируем минимальный Cargo.toml исключительно для сервера (без GUI и клиента)
RUN printf '[workspace]\nresolver = "2"\nmembers = [\n  "crates/protocol",\n  "crates/server"\n]\n' > Cargo.toml

# Собираем исключительно серверные зависимости (tokio, serde, rand, tracing)
RUN cargo build --release --package cheburgram-server

FROM alpine:latest
WORKDIR /app
COPY --from=builder /app/target/release/cheburgram-server /app/cheburgram-server

# TCP Сигналы: 7878, UDP Медиа-реле: 7879
EXPOSE 7878/tcp
EXPOSE 7879/udp

CMD ["/app/cheburgram-server"]
