# syntax=docker/dockerfile:1
#
# Multi-stage build for the Anemoi governance daemon (anemoi-daemon).
# Builds from source so the image is reproducible from a checkout; the runtime
# image carries only the release binary plus its dynamic deps.
#
# Notes:
#   - reqwest uses its default native-tls backend -> the binary links openssl 3
#     (libssl.so.3 / libcrypto.so.3). Builder needs pkg-config + libssl-dev;
#     runtime needs libssl3.
#   - rusqlite is compiled with the "bundled" feature -> sqlite is statically
#     linked, so no libsqlite3 is required at runtime (the rust image already
#     ships a C toolchain to compile it).
#   - Builder and runtime are both Debian bookworm so glibc matches by
#     construction (binary requires GLIBC_2.34).

FROM rust:1-bookworm AS builder
WORKDIR /build
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release -p anemoi-daemon

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 curl \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
RUN mkdir -p config
COPY --from=builder /build/target/release/anemoi-daemon /usr/local/bin/anemoi-daemon
COPY --from=builder /build/config/anemoi.example.yaml /app/config/anemoi.yaml
ENV ANEMOI_BIND=0.0.0.0:7070
ENV ANEMOI_CONFIG=/app/config/anemoi.yaml
EXPOSE 7070
ENTRYPOINT ["/usr/local/bin/anemoi-daemon"]
