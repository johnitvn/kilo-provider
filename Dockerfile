# syntax=docker/dockerfile:1
# kilo-provider — multi-stage build.
#
# Portable across linux/amd64 and linux/arm64: both base images are official
# multi-arch images, the compiler produces the target arch itself (no
# prebuilt binary downloads), and TLS is done by rustls in pure Rust — so the
# image has no amd64-only steps and no glibc/OpenSSL wrinkles.

# ---------------------------------------------------------------------------
# Stage 1: builder
# ---------------------------------------------------------------------------
# rust:1.98-slim-bookworm matches the local stable toolchain (1.98.x) and is a
# multi-arch (amd64/arm64) official image. rustls makes pkg-config and the
# OpenSSL dev headers unnecessary — only CA certificates are installed.
FROM rust:1.98-slim-bookworm AS builder

WORKDIR /build

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Copy manifest + lock first for better layer caching over the source.
COPY Cargo.toml Cargo.lock ./
COPY src ./src

# --locked: build exactly what Cargo.lock pins, never drift.
# The registry is a build-cache mount so repeated builds stay fast.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo build --locked --release

# ---------------------------------------------------------------------------
# Stage 2: runtime
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# TLS happens in-process via rustls/webpki, so no system CA store is strictly
# required — but keeping ca-certificates is cheap, explicit and helps any tool
# run inside the container. Nothing else is installed.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Non-root runtime user (uid/gid 10000, no home, no shell).
RUN useradd --uid 10000 --user-group --no-create-home --shell /usr/sbin/nologin kilo

# Freshly built binary; no workflow/config assets are copied into the image.
COPY --from=builder /build/target/release/kilo-provider /usr/local/bin/kilo-provider

# Configuration is provided at runtime (e.g. mounted at
# /etc/kilo-provider/config.yaml) — never baked into the image.
ENV CONFIG_PATH=/etc/kilo-provider/config.yaml

# The listen port is not exposed statically on purpose: `PORT` is a *required*
# env var with no default (see README), so there is no fixed port to declare.

USER 10000:10000

# No secrets or credentials in the image; the binary reads PORT and
# CONFIG_PATH from the environment alone.
ENTRYPOINT ["/usr/local/bin/kilo-provider"]
