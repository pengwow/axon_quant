# AXON 本地开发命令
# 使用 `make help` 查看所有可用命令

.DEFAULT_GOAL := help

# ==================== 元数据 ====================
WORKSPACE := axon
RUSTFLAGS ?= -D warnings

# ==================== 帮助 ====================
.PHONY: help
help: ## 显示本帮助信息
	@echo "AXON 本地开发命令"
	@echo ""
	@echo "用法：make <目标>"
	@echo ""
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-15s\033[0m %s\n", $$1, $$2}'
	@echo ""

# ==================== 编译 ====================
.PHONY: build
build: ## 编译整个 workspace
	cargo build --workspace

.PHONY: build-release
build-release: ## Release 模式编译
	cargo build --workspace --release

.PHONY: build-cli
build-cli: ## 编译 CLI 工具
	cargo build -p axon-cli

# ==================== 测试 ====================
.PHONY: test
test: ## 运行单元测试
	cargo test --workspace

.PHONY: test-doc
test-doc: ## 运行文档测试
	cargo test --workspace --doc

.PHONY: test-ci
test-ci: ## CI 等价：fmt + clippy + test
	make fmt-check
	make clippy
	make test

# ==================== 静态检查 ====================
.PHONY: fmt
fmt: ## 格式化代码
	cargo fmt --all

.PHONY: fmt-check
fmt-check: ## 检查格式（不修改）
	cargo fmt --all -- --check

.PHONY: clippy
clippy: ## 运行 clippy 静态分析
	cargo clippy --workspace --all-targets -- $(RUSTFLAGS)

# ==================== 清理 ====================
.PHONY: clean
clean: ## 清理构建产物
	cargo clean

# ==================== 文档 ====================
.PHONY: doc
doc: ## 构建 API 文档
	cargo doc --workspace --all-features --no-deps

.PHONY: doc-open
doc-open: ## 构建并打开 API 文档
	cargo doc --workspace --all-features --no-deps --open

# ==================== 工具 ====================
.PHONY: outdated
outdated: ## 检查过期依赖
	cargo install cargo-outdated --locked
	cargo outdated --workspace

.PHONY: audit
audit: ## 安全审计
	cargo install cargo-audit --locked
	cargo audit

.PHONY: tree
tree: ## 打印依赖树
	cargo tree --workspace

# ==================== 安装 ====================
.PHONY: install
install: ## 安装 axon CLI 到本地
	cargo install --path crates/axon-cli --locked

# ==================== Python ====================
.PHONY: python-build
python-build: ## 构建 Python wheel（包含 Rust 扩展）
	maturin build --release

.PHONY: python-develop
python-develop: ## 安装 Python 包到当前环境（开发模式）
	maturin develop

.PHONY: python-install
python-install: python-build ## 安装 Python wheel
	pip install target/wheels/axon_quant-*.whl --force-reinstall

.PHONY: python-wheel-docker
python-wheel-docker: ## 通过 Docker 构建 wheel（多阶段导出）
	docker build --target wheel --output target/wheels .

.PHONY: python-clean
python-clean: ## 清理 Python 构建产物
	rm -rf target/wheels/
	rm -rf python/axon_quant/*.so
	rm -rf python/axon_quant/_native*.so
	rm -rf python/axon_quant/__pycache__
	rm -rf python/axon_quant/*/__pycache__

# ==================== 验证完整流程 ====================
.PHONY: verify
verify: fmt-check clippy test build ## 完整本地验证（等价于 CI）
	@echo "✅ 所有本地检查通过"
