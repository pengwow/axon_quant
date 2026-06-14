# AXON 多阶段构建
# ─────────────────────────────────────────────
# 阶段 1：builder - 完整工具链编译
# 阶段 2：runtime - 最小化运行时镜像

# ===== 阶段 1：builder =====
FROM rust:1.96-bookworm AS builder

# 安装 sccache 加速增量编译
RUN cargo install sccache --locked

ENV RUSTC_WRAPPER=sccache
ENV SCCACHE_DIR=/sccache
ENV CARGO_HOME=/usr/local/cargo
ENV CARGO_TARGET_DIR=/tmp/axon-target
RUN mkdir -p /sccache && chmod 777 /sccache

WORKDIR /build

# 复制所有 crate 的 Cargo.toml 用于依赖缓存
COPY Cargo.toml Cargo.lock ./
COPY crates/*/Cargo.toml crates/ */

# 占位源码：仅用于触发依赖编译与缓存
RUN for crate in crates/*/; do \
        mkdir -p "$crate/src"; \
        echo "" > "$crate/src/lib.rs"; \
    done && \
    echo "fn main() {}" > crates/axon-cli/src/main.rs && \
    cargo build --release --workspace 2>/dev/null || true && \
    for crate in crates/*/; do \
        rm -rf "$crate/src"; \
    done

# 复制真实源码
COPY crates/ ./crates/
COPY python/ ./python/
COPY pyproject.toml ./

# Release 编译（Rust 二进制 + Python wheel）
RUN cargo build --release --workspace \
    && strip target/release/axon 2>/dev/null || true

# 安装 maturin 并构建 Python wheel
RUN pip install maturin --no-cache-dir \
    && maturin build --release --out target/wheels

# ===== 阶段 2：wheel =====
# 仅导出 Python wheel（用于 CI 产物或 pip install）
FROM scratch AS wheel
COPY --from=builder /tmp/axon-target/wheels/*.whl /

# ===== 阶段 3：runtime =====
FROM debian:bookworm-slim AS runtime

# 安装运行时依赖
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        tzdata \
        libssl3 \
    && rm -rf /var/lib/apt/lists/*

# 创建非 root 用户
RUN groupadd --system --gid 1000 axon \
    && useradd --system --uid 1000 --gid axon --create-home --shell /bin/bash axon

# 从 builder 复制二进制
COPY --from=builder /tmp/axon-target/release/axon /usr/local/bin/axon

USER axon
WORKDIR /home/axon

ENTRYPOINT ["/usr/local/bin/axon"]
CMD ["--help"]

# 元数据
LABEL org.opencontainers.image.title="axon" \
      org.opencontainers.image.description="AXON - 量化交易回测与强化学习框架" \
      org.opencontainers.image.version="0.1.0" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.source="https://github.com/axon-team/axon"
