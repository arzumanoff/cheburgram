# Multi-stage Dockerfile для Cheburgram Server v3
FROM rust:alpine AS builder
RUN apk add --no-cache musl-dev

WORKDIR /app
COPY Cargo.toml ./
COPY crates ./crates

# Лок-файл не храним в репо — генерируем при сборке
RUN cargo generate-lockfile

# Собираем исключительно бинарник сервера (audiopus/GUI не нужны)
RUN cargo build --release --package cheburgram-server

FROM alpine:latest
WORKDIR /data

COPY --from=builder /app/target/release/cheburgram-server /usr/local/bin/cheburgram-server

# Данные (clients.json) — в /data, монтируется volume'ом и переживает пересоздание контейнера
VOLUME ["/data"]

# TCP Сигналы: 7878, UDP Медиа-релей: 7879
EXPOSE 7878/tcp
EXPOSE 7879/udp

HEALTHCHECK --interval=30s --timeout=5s --start-period=5s \
  CMD nc -z 127.0.0.1 7878 || exit 1

CMD ["cheburgram-server"]
