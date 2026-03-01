# ── Stage 1: build ────────────────────────────────────────────────────────────
# protoc-bin-vendored bundles precompiled protoc for all targets;
# no system protobuf-compiler needed.
FROM rust:1.88-slim AS builder

WORKDIR /app

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential \
      pkg-config \
      libssl-dev \
 && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock build.rs ./
COPY proto/            proto/
COPY sodp-middleware/  sodp-middleware/
COPY src/              src/

RUN cargo build --release --bin sodp-server

# ── Stage 2: runtime ──────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/sodp-server /usr/local/bin/

EXPOSE 7777

# Override CMD to change bind address or add log dir / schema file:
#   docker run ghcr.io/orkestri/sodp-server 0.0.0.0:8888 /data/log /etc/sodp/schema.json
ENTRYPOINT ["sodp-server"]
CMD ["0.0.0.0:7777"]
