# syntax=docker/dockerfile:1

# ─ Builder ─
FROM rust:1-alpine@sha256:7e9e2d068a107a2adafd19930011178d14a78326053cd4c3a9f7ad3fd049557d AS builder

RUN apk add --no-cache musl-dev

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN cargo build -p technogym-mcp --release --locked && \
  mkdir -p /out && \
  cp target/release/technogym-mcp /out/technogym-mcp

# ─ Runtime ─
FROM alpine:3@sha256:be2656061b536c1883c4034790e21d0a09c5aaaf51968b89368a3c0371ea2c9a

# The binary is dynamically linked against musl. The runtime base MUST stay
# Alpine-compatible (any image shipping the matching musl loader). Do not change
# to debian-slim or distroless without also building for `*-unknown-linux-musl`
# with `RUSTFLAGS=-C target-feature=+crt-static`.
RUN addgroup -S -g 10001 technogym \
  && adduser -S -D -u 10001 -G technogym technogym

COPY --from=builder /out/technogym-mcp /usr/local/bin/technogym-mcp

USER 10001:10001

EXPOSE 3000

STOPSIGNAL SIGTERM

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD wget --no-verbose --tries=1 --spider http://127.0.0.1:3000/mcp/healthz || exit 1

ARG IMAGE_VERSION=0.1.0
ARG IMAGE_REVISION=unknown

LABEL org.opencontainers.image.title="technogym-mcp" \
  org.opencontainers.image.description="Hosted MCP server for Technogym mywellness (unofficial)" \
  org.opencontainers.image.source="https://github.com/StephanMeijer/technogym-rs" \
  org.opencontainers.image.version="${IMAGE_VERSION}" \
  org.opencontainers.image.revision="${IMAGE_REVISION}" \
  org.opencontainers.image.licenses="MPL-2.0"

# Stateless: PUBLIC_URL, SEAL_KEYS and optionally PORT / CLIENT_IP_HEADER come from
# the environment; nothing is written to disk.
ENTRYPOINT ["/usr/local/bin/technogym-mcp", "serve-http"]
