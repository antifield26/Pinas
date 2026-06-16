# ====== Antifield Cloud (Pi-NAS) — ARM64 多阶段构建 ======

# Stage 1: Build
FROM rust:1.85-slim-bookworm AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /app

# 预缓存依赖（利用 Docker layer caching）
COPY Cargo.toml Cargo.lock ./
COPY pinas-core/Cargo.toml ./pinas-core/
COPY pinas-core/src/ ./pinas-core/src/
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release 2>/dev/null || true

# 完整编译
COPY src/ ./src/
COPY assets/ ./assets/
COPY static/ ./static/
RUN cargo build --release

# Stage 2: Runtime
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates tzdata && rm -rf /var/lib/apt/lists/*
WORKDIR /app

COPY --from=builder /app/target/release/pi_nas .
COPY --from=builder /app/assets/ ./assets/
COPY --from=builder /app/static/ ./static/

RUN mkdir -p /app/uploads /app/logs /app/data
ENV PINAS_DATABASE_URL=sqlite:/app/data/cloud_disk.db

EXPOSE 3000
VOLUME ["/app/uploads", "/app/logs", "/app/data"]
HEALTHCHECK --interval=30s --timeout=3s CMD wget --no-verbose --tries=1 --spider http://localhost:3000/health || exit 1

CMD ["./pi_nas"]
