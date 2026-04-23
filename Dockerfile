# syntax=docker/dockerfile:1.7

# ----- builder ---------------------------------------------------------------
FROM rust:1-bookworm AS builder
WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential pkg-config libssl-dev ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# Single-stage build. The caching gymnastics with a stub `fn main(){}` file
# can silently ship the stub binary when the fingerprint invalidation misses,
# so we prefer a straightforward full build that BuildKit still layer-caches
# whenever Cargo.lock and Cargo.toml are unchanged.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY templates ./templates
COPY static ./static
COPY db ./db
RUN cargo build --release --locked --bin transactvault \
 && strip /app/target/release/transactvault \
 && ls -la /app/target/release/transactvault

# ----- runtime ---------------------------------------------------------------
# debian-slim gives us a shell, ca-certificates for HTTPS, and glibc —
# small trade-off in image size for debuggable `docker exec` sessions.
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libssl3 \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/transactvault /app/transactvault
COPY --from=builder /app/static /app/static
COPY --from=builder /app/templates /app/templates
COPY --from=builder /app/db /app/db
EXPOSE 37420
ENTRYPOINT ["/app/transactvault"]
