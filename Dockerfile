# ==========================================
# Stage 1: Build static binary
# ==========================================
FROM rust:1-alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN cargo build --release --bin miabd --bin miab

# ==========================================
# Stage 2: Minimal runtime image (<15 MB)
# ==========================================
FROM alpine:3.20

RUN apk add --no-cache ca-certificates tzdata && \
    addgroup -g 1000 miab && \
    adduser -u 1000 -G miab -s /bin/sh -D miab && \
    mkdir -p /storage && \
    chown -R miab:miab /storage

COPY --from=builder /app/target/release/miabd /usr/local/bin/miabd
COPY --from=builder /app/target/release/miab /usr/local/bin/miab

USER miab
WORKDIR /storage
VOLUME ["/storage"]
EXPOSE 6422

ENTRYPOINT ["/usr/local/bin/miabd"]
CMD ["-b", "0.0.0.0:6422", "-s", "/storage"]
