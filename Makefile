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
# 项目规则:必须使用本仓库 `.venv` 下的 Python/maturin/pip,
# 不能用 miniconda3 下的环境(否则 PYO3_PYTHON 与 cargo 链接的 libpython
# 会不一致,导致 build/import 失败)。

VENV_PYTHON ?= .venv/bin/python
VENV_MATURIN ?= .venv/bin/maturin
VENV_PIP ?= .venv/bin/pip

.PHONY: python-build
python-build: ## 构建 Python wheel（包含 Rust 扩展）
	PYO3_PYTHON=$(VENV_PYTHON) $(VENV_MATURIN) build --release

.PHONY: python-develop
python-develop: ## 安装 Python 包到当前环境（开发模式）
	PYO3_PYTHON=$(VENV_PYTHON) $(VENV_MATURIN) develop

.PHONY: python-install
python-install: python-build ## 安装 Python wheel(默认 --no-deps,避免拉 numpy 等大依赖)
	$(VENV_PIP) install --no-deps --force-reinstall target/wheels/axon_quant-*.whl

.PHONY: python-clean
python-clean: ## 清理 Python 构建产物
	rm -rf target/wheels/
	rm -rf python/axon_quant/*.so
	rm -rf python/axon_quant/_native*.so
	rm -rf python/axon_quant/__pycache__
	rm -rf python/axon_quant/*/__pycache__

.PHONY: python-publish-test
python-publish-test: python-build ## 发布到 TestPyPI（测试）
	$(VENV_PIP) install twine
	$(VENV_PIP) run twine upload --repository testpypi target/wheels/*

.PHONY: python-publish
python-publish: python-build ## 发布到 PyPI（正式）
	$(VENV_PIP) install twine
	$(VENV_PIP) run twine upload target/wheels/*

# ==================== 示例 ====================
.PHONY: example example-quick example-ppo example-sac

example-quick: ## 运行快速入门示例
	PYTHONPATH=examples $(VENV_PYTHON) examples/01_getting_started/01_quick_start.py

example-ppo: ## 运行 PPO 训练示例
	PYTHONPATH=examples $(VENV_PYTHON) examples/02_rl_training/train_ppo.py --timesteps 5000

example-sac: ## 运行 SAC 训练示例
	PYTHONPATH=examples $(VENV_PYTHON) examples/02_rl_training/train_sac.py --timesteps 5000

# ==================== 性能基准 ====================
.PHONY: bench bench-cmp bench-one bench-report

bench: ## 跑全 workspace bench(本地,不进 CI)
	cargo bench --workspace --no-fail-fast -- --output-format bencher

bench-cmp: ## 存 main baseline,用于 PR 对比
	@echo "Saving baseline 'main'..."
	cargo bench --workspace --no-fail-fast -- --save-baseline main

bench-report: ## 跑基准测试并生成报告到 docs
	@echo "Running benchmarks and generating reports..."
	cargo bench --workspace --no-fail-fast
	@echo "Copying reports to docs/zh/report and docs/en/report..."
	@rm -rf docs/zh/report docs/en/report
	@mkdir -p docs/zh/report docs/en/report
	@cp -r target/criterion/* docs/zh/report/
	@cp -r target/criterion/* docs/en/report/
	@echo "Benchmark reports saved to docs/zh/report/ and docs/en/report/"
	@echo "Report index: docs/zh/report/report/index.html"

# 跑单个 bench(用法:make bench-one CRATE=axon-core BENCH=event_builder_tick)
bench-one: ## 跑单个 bench(需 CRATE + BENCH 参数)
	@if [ -z "$(CRATE)" ] || [ -z "$(BENCH)" ]; then \
		echo "Usage: make bench-one CRATE=<crate> BENCH=<bench-name>"; \
		echo "  e.g. make bench-one CRATE=axon-core BENCH=event_builder_tick"; \
		echo "  e.g. make bench-one CRATE=axon-backtest BENCH=submit_linear_impact"; \
		exit 1; \
	fi
	cargo bench -p $(CRATE) -- $(BENCH)

# ==================== 验证完整流程 ====================
.PHONY: verify
verify: fmt-check clippy test build ## 完整本地验证（等价于 CI）
	@echo "✅ 所有本地检查通过"

# ==================== 版本管理(单一来源) ====================
# 策略:
#   - Cargo.toml   [workspace.package].version  = Rust 全部 23 个 crate 权威源
#   - pyproject.toml [project].version         = Python wheel 权威源
#   - CHANGELOG.md                              = 人类可读发布日志
#   - _native.__version__    = env!("CARGO_PKG_VERSION")(编译时注入)
#   - 3 个 Python 辅助包       = importlib.metadata.version("axon-quant")
# `version-check` 校验三源对齐,`version-bump` 一处改三处,杜绝遗漏

.PHONY: version-check
version-check: ## 校验 Cargo.toml / pyproject.toml / Cargo.lock 版本号一致
	@CARGO_VERSION=$$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/'); \
	PYPROJECT_VERSION=$$(grep '^version' pyproject.toml | head -1 | sed 's/.*"\(.*\)"/\1/'); \
	LOCK_VERSION=$$(awk '/^name = "axon-backtest"/{flag=1; next} flag && /^version = /{print; exit}' Cargo.lock | sed 's/.*"\(.*\)".*/\1/'); \
	echo "Cargo.toml:    $$CARGO_VERSION"; \
	echo "pyproject:     $$PYPROJECT_VERSION"; \
	echo "Cargo.lock:    $$LOCK_VERSION (axon-backtest)"; \
	STATUS=0; \
	if [ "$$CARGO_VERSION" != "$$PYPROJECT_VERSION" ]; then \
		echo "❌ Cargo.toml 与 pyproject.toml 版本号不一致"; \
		STATUS=1; \
	fi; \
	if [ "$$CARGO_VERSION" != "$$LOCK_VERSION" ]; then \
		echo "❌ Cargo.toml 与 Cargo.lock 版本号不一致(请先 cargo build)"; \
		STATUS=1; \
	fi; \
	if [ $$STATUS -eq 0 ]; then \
		echo "✅ 版本号三源对齐"; \
	else \
		exit 1; \
	fi

.PHONY: version-bump
version-bump: ## 升级版本号(用法:make version-bump VERSION=0.3.2)
	@if [ -z "$(VERSION)" ]; then \
		echo "Usage: make version-bump VERSION=<new-version>"; \
		echo "  e.g. make version-bump VERSION=0.3.2"; \
		exit 1; \
	fi
	@OLD=$$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/'); \
	echo "==> 升级版本: $$OLD -> $(VERSION)"; \
	echo "==> 改 Cargo.toml [workspace.package].version"; \
	sed -i '' 's/^version = "'$$OLD'"/version = "$(VERSION)"/' Cargo.toml; \
	echo "==> 改 pyproject.toml [project].version"; \
	sed -i '' 's/^version = "'$$OLD'"/version = "$(VERSION)"/' pyproject.toml; \
	echo "==> 重新生成 Cargo.lock"; \
	cargo build --workspace >/dev/null 2>&1 || cargo build --workspace; \
	echo ""; \
	echo "✅ Cargo.toml + pyproject.toml + Cargo.lock 已同步到 $(VERSION)"; \
	echo ""; \
	echo "接下来请:"; \
	echo "  1. 编辑 CHANGELOG.md:在 [Unreleased] 之下新增 ## [$(VERSION)] - $$(date +%Y-%m-%d) 节"; \
	echo "  2. 运行 make version-check 校验三源对齐"; \
	echo "  3. git add -A && git commit -m 'bump: $(VERSION)'"

# ==================== 文档站(mkdocs + Material) ====================
# 部署到 GitHub Pages 由 .github/workflows/docs.yml 处理
# 本地命令:docs-install / docs-serve / docs-build / docs-validate / docs-clean

.PHONY: docs-install
docs-install: ## 安装 mkdocs 依赖(运行一次或 requirements-docs.txt 变更时)
	@echo "==> 安装 mkdocs 依赖"
	@python3 -m pip install -r requirements-docs.txt

.PHONY: docs-serve
docs-serve: docs-install ## 本地预览中文文档站(开发时用,自动 reload)
	@echo "==> 启动 mkdocs 开发服务器,访问 http://localhost:8000"
	@mkdocs serve

.PHONY: docs-serve-en
docs-serve-en: docs-install ## 本地预览英文文档站
	@echo "==> 启动英文文档开发服务器,访问 http://localhost:8001"
	@mkdocs serve -f mkdocs-en.yml -a 127.0.0.1:8001

.PHONY: docs-build
docs-build: docs-install ## 构建中文静态站点(产出在 site/ 目录)
	@echo "==> 构建 mkdocs 静态站点"
	@mkdocs build --strict

.PHONY: docs-build-en
docs-build-en: docs-install ## 构建英文静态站点(产出在 site/en/ 目录)
	@echo "==> 构建英文 mkdocs 静态站点"
	@mkdocs build -f mkdocs-en.yml --strict

.PHONY: docs-build-all
docs-build-all: docs-install ## 构建所有语言文档
	@echo "==> 构建所有语言文档"
	@mkdocs build --strict
	@mkdocs build -f mkdocs-en.yml --strict

.PHONY: docs-validate
docs-validate: docs-build ## 严格校验 mkdocs 配置 + 链接 + 引用
	@echo "==> mkdocs 站点构建成功,链接 / 引用校验通过(由 --strict 触发)"

.PHONY: docs-clean
docs-clean: ## 清理 mkdocs 临时产物
	@rm -rf site/ .cache/

# ==================== 代码覆盖率 ====================
.PHONY: coverage coverage-all coverage-report

coverage: ## 运行代码覆盖率分析（排除 Python 模块）
	@echo "==> 运行代码覆盖率分析（排除 Python 模块）"
	cargo tarpaulin --workspace \
		--exclude-files "*/tests/*" "*/benches/*" "*/examples/*" "*/python/*" \
		--skip-clean \
		--out Stdout

coverage-all: ## 运行代码覆盖率分析（包含 Python 模块）
	@echo "==> 运行代码覆盖率分析（包含 Python 模块）"
	cargo tarpaulin --workspace \
		--exclude-files "*/tests/*" "*/benches/*" "*/examples/*" \
		--skip-clean \
		--out Stdout

coverage-report: ## 生成 HTML 覆盖率报告
	@echo "==> 生成 HTML 覆盖率报告"
	cargo tarpaulin --workspace \
		--exclude-files "*/tests/*" "*/benches/*" "*/examples/*" "*/python/*" \
		--skip-clean \
		--out Html \
		--output-dir target/coverage
	@echo "覆盖率报告已生成到 target/coverage/tarpaulin.html"
