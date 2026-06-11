# ── Stage 1: Build ────────────────────────────────────────────────────────────
FROM rust:1.91-trixie AS builder

WORKDIR /build

# Copy workspace manifests first for better layer caching
COPY Cargo.toml Cargo.lock ./
COPY crates/corre-sdk/Cargo.toml crates/corre-sdk/Cargo.toml
COPY crates/corre-core/Cargo.toml crates/corre-core/Cargo.toml
COPY crates/corre-mcp/Cargo.toml crates/corre-mcp/Cargo.toml
COPY crates/corre-llm/Cargo.toml crates/corre-llm/Cargo.toml
COPY crates/corre-news/Cargo.toml crates/corre-news/Cargo.toml
COPY crates/corre-plugin/Cargo.toml crates/corre-plugin/Cargo.toml
COPY crates/corre-host/Cargo.toml crates/corre-host/Cargo.toml
COPY crates/corre-safety/Cargo.toml crates/corre-safety/Cargo.toml
COPY crates/corre-dashboard/Cargo.toml crates/corre-dashboard/Cargo.toml
COPY crates/corre-registry/Cargo.toml crates/corre-registry/Cargo.toml
COPY crates/corre-cli/Cargo.toml crates/corre-cli/Cargo.toml
COPY crates/mcp-smtp/Cargo.toml crates/mcp-smtp/Cargo.toml
COPY crates/mcp-telegram/Cargo.toml crates/mcp-telegram/Cargo.toml
COPY apps/daily-brief/Cargo.toml apps/daily-brief/Cargo.toml
COPY apps/rolodex/Cargo.toml apps/rolodex/Cargo.toml
COPY apps/corre-gym/Cargo.toml apps/corre-gym/Cargo.toml

# Stub out lib.rs / main.rs so cargo can resolve deps and cache them
RUN for c in corre-sdk corre-core corre-mcp corre-llm \
             corre-plugin corre-host corre-safety corre-dashboard corre-registry; do \
        mkdir -p "crates/$c/src" && echo "" > "crates/$c/src/lib.rs"; \
    done && \
    mkdir -p crates/corre-news/src && \
    echo "" > crates/corre-news/src/lib.rs && \
    echo "fn main() {}" > crates/corre-news/src/main.rs && \
    for c in corre-cli mcp-smtp mcp-telegram; do \
        mkdir -p "crates/$c/src" && echo "fn main() {}" > "crates/$c/src/main.rs"; \
    done && \
    for c in daily-brief rolodex corre-gym; do \
        mkdir -p "apps/$c/src" && echo "" > "apps/$c/src/lib.rs" && echo "fn main() {}" > "apps/$c/src/main.rs"; \
    done

RUN cargo build --release -p corre-gym 2>/dev/null || true

# Now copy the real source and rebuild
COPY . .
RUN touch crates/*/src/*.rs apps/*/src/*.rs && cargo build --release -p corre-gym

# ── Stage 2: Runtime ─────────────────────────────────────────────────────────
FROM debian:trixie-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
    && rm -rf /var/lib/apt/lists/*

RUN groupadd -g 1000 corre && useradd -u 1000 -g corre -m corre

WORKDIR /app
RUN mkdir -p /data && chown corre:corre /app /data

COPY --from=builder /build/target/release/corre-gym /app/corre-gym

EXPOSE 5520

USER corre
CMD ["/app/corre-gym", "-c", "/data/corre.toml"]
