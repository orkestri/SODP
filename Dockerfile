# ── Stage 1: chef base ────────────────────────────────────────────────────────
# cargo-chef splits dependency compilation (slow, stable) from app compilation
# (fast, changes frequently), making dependency layers cacheable across builds.
# protoc-bin-vendored bundles precompiled protoc — no system protobuf needed.
FROM lukemathwalker/cargo-chef:latest-rust-1.88-slim AS chef

WORKDIR /app

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential \
      pkg-config \
      libssl-dev \
 && rm -rf /var/lib/apt/lists/*

# ── Stage 2: planner — compute the dependency recipe ──────────────────────────
FROM chef AS planner

COPY Cargo.toml Cargo.lock build.rs ./
COPY proto/           proto/
COPY sodp-middleware/ sodp-middleware/
COPY src/             src/

RUN cargo chef prepare --recipe-path recipe.json

# ── Stage 3: builder — cache deps, then compile the app ───────────────────────
FROM chef AS builder

# Cook dependencies from the recipe.  This layer is invalidated only when
# Cargo.toml / Cargo.lock / build.rs / proto/ change — not on every src edit.
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

# Now build the real application.  Only this layer re-runs on source changes.
COPY Cargo.toml Cargo.lock build.rs ./
COPY proto/           proto/
COPY sodp-middleware/ sodp-middleware/
COPY src/             src/

RUN cargo build --release --bin sodp-server

# ── Stage 4: runtime ──────────────────────────────────────────────────────────
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
