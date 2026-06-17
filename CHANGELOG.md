# Changelog

All notable changes to AXON will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed
- **`Llm*` → `LLM*` 重命名(breaking)**:全项目把 CamelCase `Llm` 前缀统一改为大写 `LLM`,与 LLM 行业惯例(LLM/Llama/LLaMA 一致使用全大写)对齐。涉及 12 个文件 243 处替换:
  - Rust 结构体:`LlmConfig` → `LLMConfig`(crates/axon-llm/src/config.rs),`LlmConfigOverride` → `LLMConfigOverride`,`PyLlmBackend` → `PyLLMBackend`(src/python/{mod,backend}.rs)。
  - PyO3 `pyclass(name = ...)` 暴露给 Python 的类名同步更新:`name = "LlmBackend"` → `"LLMBackend"`,`name = "LlmMessage"` → `"LLMMessage"`(src/python/backend.rs),所以 `repr(backend)` 现在是 `LLMBackend(OpenAICompatBackend)`,`repr(message)` 是 `LLMMessage(role=..., content=...)`。
  - Python 顶层 API:`python/axon_quant/llm.py` 与 `__init__.py` 的 `LlmBackend` / `LlmMessage` 全部改为 `LLMBackend` / `LLMMessage`;`tests/python/test_llm_python_api.py` 的 `TestLlmMessage` 测试类与方法同步重命名。
  - 调用方:`crates/axon-llm/src/backends/openai_compat.rs`、`examples/{live,integrated}_trading_demo.rs`、`tests/python_binding_test.rs` 同步。
  - 文档:`docs/superpowers/plans/2026-06-15-axon-llm-ai-advanced-launch.md` 与 `specs/2026-06-15-axon-llm-ai-advanced-launch-design.md` 全部 `Llm*` → `LLM*`。
  - **不重命名**:`from_llm_config`(snake_case 方法名,内含小写 `llm_` 段,不属于 CamelCase 前缀)、`axon-llm` / `axon_llm` crate 名、`axon_quant.llm` Python 子模块名。
- **`make python-install` 默认加 `--no-deps`**:避免在断网 / PyPI 超时时(如本机 `pip install ... --force-reinstall` 拉 numpy≥1.24 失败)整个 target 失败;axon-quant 自身只依赖 Python 标准库 + Rust 扩展,numpy 等大依赖是用户的应用级需求,不应在 wheel install 时强拉。

### Added
- **axon-llm** 统一 `LlmConfig` 类型(`config.rs`):支持 TOML 配置文件 + 字典构造 + 字段级 `merged_override` + 5 级 fallback 解析。涵盖 `BackendConfig` / `RetryConfig` / `ExplainConfig` 子结构与 `ConfigError` 错误类型。`resolve_with_fallback` 5 级优先级:显式路径 > `config.local.toml` > `config.toml` > 仓库内置 demo 配置 > 默认值(validate 失败,要求显式填 api_key)。`to_template_toml(include_secrets)` 支持生成不含敏感字段的模板。补齐 16 个测试覆盖解析/验证/override/fallback/模板。
- **axon-llm** `OpenAICompatConfig::from_llm_config(&LlmConfig, index)` 工厂方法(在 `backends/openai_compat.rs`):从统一配置构造 backend,索引越界返回 `BackendInitError`;补齐 2 个测试覆盖字段映射与索引越界。
- **axon-llm** `live_trading_demo` 重构:移除硬编码 `DEEPSEEK_API_KEY` 环境变量读取,改为 `--config <path>` / `AXON_LLM_CONFIG` 环境变量 + 5 级 fallback 解析,任意 OpenAI 兼容厂商(DeepSeek / OpenAI / Mimo / 本地 Ollama)均可使用。
- **axon-llm** `demo/bin/config.toml` 升级:从单 `[backend]` 表改为 `[[backends]]` 数组 + `[retry]` + `[explain]` 子段,支持多厂商与可解释性集成配置;附带详细注释说明复制/编辑/运行流程。
- **axon-llm** 新增 `integrated_trading_demo` example:三阶段演示(多 backend 串行对话 → ensemble `HardVoteStrategy` 投票 → `ReportGenerator` 渲染 Markdown 决策报告)。`axon-ensemble` 与 `axon-explain` 作为 `demo` feature 的可选依赖引入(`Cargo.toml`)。
- **axon-llm** PyO3 绑定(`src/python/{mod,backend}.rs`,`python` feature 隐含启用 `backends`):暴露 `make_backend(config_dict)` / `LlmBackend` / `LlmMessage` 给 Python;`make_backend` 内部用 `LlmConfig::from_dict` + `OpenAICompatConfig::from_llm_config` 校验并构造 backend,`LlmBackend::chat([LlmMessage, ...])` 同步桥接 `tokio::block_on`。`LlmMessage` 用 `#[pyclass(from_py_object)]` 显式 opt-in FromPyObject(pyo3 0.28 强制要求);`LlmBackend` 字段为 `pub(crate)` 供 `mod.rs` 内部构造。
- **axon-llm** 交易工具模块(`src/trading/`,7 个文件):`TradingBackend` trait + `TradingError` + `OrderAck` / `BalanceSnapshot` / `PositionSnapshot` / `PortfolioSnapshot` 等共享类型;`SafetyMode` 三态(DryRun 记录 / Direct 直接调 / TwoPhase 两次确认)+ `RiskLimits`(白名单/单笔金额/单日订单数) + `DailyCounter` + `PendingOrder` 待确认表;`MockTradingBackend` 内存模拟(USDT+BTC 默认余额) + `FailureInjector` 错误注入;`PlaceOrderTool`(LLM 下单工具,支持 DryRun/Direct/TwoPhase,extras 透传底层 Order 字段) + `QueryPortfolioTool`(余额+持仓查询,按 symbol 过滤)。补齐 40 个单元测试覆盖 types/backend/safety/mock/place_order/query_portfolio。
- **axon-python** `lib.rs` 在 `_native` 下挂载 `llm` 子模块(通过 `axon_llm::python::axon_llm` 注册);`Cargo.toml` 的 `axon-llm` 依赖启用 `["python", "backends"]` features。Workspace `Cargo.toml` 添加 `axon-llm` workspace 依赖。
- **axon_quant(顶层 Python API)** `python/axon_quant/llm.py`:Python 端 `LLMConfig` dataclass(`backends` / `retry` / `explain` 字段)+ `make_backend(config)`(接受 dataclass 或 dict)+ `load_config_from_toml(path)`(从 TOML 文件加载)+ `LlmBackend` / `LlmMessage` 类型别名。`python/axon_quant/__init__.py` 顶层 re-export `LLMConfig` / `LlmBackend` / `LlmMessage` / `make_backend` / `load_config_from_toml`,并把 `llm` 加入 `__all__`。注意:`_native` 是 cdylib 单文件(不是 Python package 目录),所以 `from ._native.llm import ...` 不可用,改用 `from axon_quant._native import llm` 取子模块对象后再属性访问。
- **axon-inference** 新增 `affinity` 模块(`src/affinity.rs`):跨平台 CPU 绑核(Linux + macOS 基于 `core_affinity 0.8`,Windows 编译期拒绝) + CUDA / Metal GPU 亲和性(`tch-backend` feature 启用时 `tch::Cuda::set_device` / macOS MPS 探测)。`BatchConfig` 新增 `collect_cpu_cores: Vec<u32>` / `collect_gpu_device_id: Option<u32>` 两字段,`BatchInferencePipeline::new` 启动时自动调 `affinity::pin_to`,绑核失败仅 warn 不阻断。新增 7 单元测试覆盖 plan / builder / pin / Metal-non-macOS 错误路径。
- **axon-data** `DataService::stream(source_name, req)` 流式入口(旁路缓存,直透源):返回 `Pin<Box<dyn Stream<Item = DataResult<RecordBatch>> + Send>>`,不写 L1/L2;`source_name` 未知时返回 `DataError::SourceNotFound`。新增 `cache_control()` 句柄提供 `clear_l1` / `clear_l2`(`mmap-cache` feature) / `resize_l1` 三个管理操作,句柄与 DataService 共享同一 `Arc<DataServiceInner>`。`DataService` 内部状态重构为 `Arc<DataServiceInner>`,builder 阶段用 `Arc::get_mut` 独占修改,运行期 clone 共享。`cache/mod.rs` 架构图同步更新(stream 旁路缓存,直透源)。补齐 6 单元测试(stream 透传 / 源未找到 / 不写 L1 / clear_l1 / resize_l1 / clone 共享) + 2 集成测试(多 batch 消费 / 100K tick 首 batch < 500ms)。`MockSource::stream` 升级为基于 `ticks_to_batches` 的列式 yield,与 `query` 等价但流式。
- **axon-exchange** Stage 4' D 杠杆/合约支持(生产就绪):扩展 `ExchangeAdapter` trait 新增 8 个方法(`set_leverage` / `set_margin_type` / `get_leverage_brackets` / `set_position_mode` / `get_funding_rate` / `get_account_info` / `get_open_interest` / `get_long_short_ratio`),覆盖 Binance USDⓈ-M + OKX V5 完整合约 API。`types.rs` 新增 7 个数据类型(`MarginType` / `PositionMode` / `LeverageBracket` / `FundingRate` / `AccountInfo` / `OpenInterest` / `LongShortRatio`),`ExchangeConfig` 新增 `fapi_base_url: Option<String>` 字段支持自定义合约 base URL(优先配置,否则按 `testnet` 推断 `fapi.binance.com` / `testnet.binancefuture.com`)。新增独立 `sign/` 子模块:`sign/binance.rs` 实现 HMAC-SHA256 → hex 编码(`sign_query` + `signed_query` 工厂),`sign/okx.rs` 实现 HMAC-SHA256 → Base64 编码 + 4 头构造(`sign_request` + `build_headers`),隔离两家签名协议,避免在适配器中重复实现。Binance 适配器新增 `fapi_get` / `fapi_post` / `fapi_get_public` 三个私有 helper(独立 `impl BinanceAdapter` 块,不属于 trait),统一处理签名 + 429 Retry-After + ApiError 解析;OKX 适配器新增 `send_okx_signed` / `send_okx_public` / `parse_okx_response` 三个 helper,统一处理 401/429/5xx/`code!="0"` 四类错误路径。`okx::get_leverage_brackets` 与 `okx::get_long_short_ratio` 修复 OKX API 字符串字段(`maxLever: "125"` / `longRatio: "0.6"`)解析失败的隐藏 bug:用 `as_str().parse().or_else(as_u64/as_f64)` 链式 fallback,测试中能正确解析 `"125" → 125` 和 `"0.6" → 0.6`。补齐 19 个 wiremock 集成测试(8 个 Binance + 11 个 OKX)覆盖 8 个 trait 方法 + 4 类响应错误路径(200/401/429/5xx)。
- **Stage 5' workspace 性能基准体系**:50+ Criterion bench 跨 5 个 crate。`benches/core_bench.rs` (axon-core) 28 个 bench:冲击模型(Linear/PowerLaw/Adaptive) + 波动率(EWMA/Rolling/Garman-Klass) + 延迟(Constant/Normal/Queue) + 订单簿(构造/mid/spread) + 订单创建 + 事件(builder 单条/吞吐/router dispatch 5/批量 100/订阅者数量 scaling) + 费用(Taker/Maker/吞吐/funding)。`benches/impact_bench.rs` (axon-backtest) 8 个 bench:`ImpactedMatchingEngine` 在 no-impact / linear / power-law / 深度 scaling(1-50)/ 永久衰减 scaling / 多笔 100 单 / TOML 配置加载 / engine 构造。`crates/axon-data/benches/axon_data_bench.rs` 7 个 group:LRU cache(16/64/256 cap) + Dataset lazy(filter/take/skip/by_time_range 1k-100k) + Mock 生成 + Csv 解析 1k-100k + Parquet 加载/流式 1k-100k + Bar 聚合 + MmapCache put/get/get_zero_copy(feature-gated)。`benches/rl_bench.rs` (axon-rl) 11 个 bench:观测 build(32×3 + 窗口 8-128 scaling) + 奖励(PnL/Sharpe) + TradingEnv step + 500 步 episode + Action 构造/clip/index→action。`benches/phase4_bench.rs` 15 个 bench:风控(check_order/circuit_breaker/pnl/metrics) + OMS(submit/idempotent/update/snapshot) + 监控(counter/gauge/histogram/quantile/alerts)。`Makefile` 新增 `bench` / `bench-cmp` / `bench-one` 三个 target:`bench` 跑全 workspace(`--output-format bencher`)、`bench-cmp` 存 main baseline 用于 PR 对比、`bench-one CRATE=axon-core BENCH=event_builder_tick` 跑单个 bench。`cargo build --workspace --benches` 0 错误 0 警告。CI 不跑(避免 runner 性能噪声);报告 `target/criterion/<group>/report/index.html`。文档同步:`README.md` 性能指标段加"性能基准"小节,`axon-design/PLAN.md` 横向任务-基准测试段补现状说明。

### Fixed
- **`make python-build` 修复**:根因是 maturin 1.14.0 的 `find_bridge` (在 `src/bridge/detection.rs:73`) 要求 `pyo3` 出现在 `cargo metadata` 的依赖图里才能识别为 pyo3 绑定,而 `crates/axon-python/Cargo.toml` 把 `pyo3` 设为 `optional = true`,默认 feature 下 `pyo3` 不在依赖图 → `find_pyo3_bindings` 返回 `None` → 报 "Couldn't detect the binding type";显式 `bindings = "pyo3"`/`--bindings pyo3`/`--config bindings=pyo3` 全部走到同一 `find_pyo3_bindings` 分支,均失败报 "unknown binding type"。叠加问题:`.gitignore` 中 `python/*` 把整个 `python/` 目录从版本控制中排除,导致 `python/axon_quant/{__init__.py, llm.py}` 在工作区缺失,无法做 maturin 二次探测。三处修复:(1) `pyproject.toml` 的 `[tool.maturin]` 加 `features = ["python"]`,激活 axon-python 的 `python` feature 让 pyo3 进依赖图,maturin 自动识别为 pyo3 binding;(2) `python/axon_quant/__init__.py` 与 `llm.py` 重新写回(内容从 `git show HEAD:` 取,与 `b9c7243` 一致);(3) `.gitignore` 的 `python/*` 改为只忽略 `python/axon_quant/{*.so,_native*.so,__pycache__/,*/__pycache__/}` 构建产物,源码恢复可被 git 跟踪。`Makefile` 的 `python-build` / `python-develop` / `python-install` targets 改为强制使用 `.venv/bin/{maturin, pip}` 并设置 `PYO3_PYTHON=$(VENV_PYTHON)`,不再走 miniconda3 下的环境(项目规则)。`maturin build --release` 在 `python-source = "python"` + `manifest-path = "crates/axon-python/Cargo.toml"` + `features = ["python"]` 下稳定输出 `target/wheels/axon_quant-0.1.0a1-cp314-cp314-*.whl`;`python -c "import axon_quant; print(axon_quant.__version__)"` 输出 `0.1.0a1`,7 个 Rust 子模块(rl/hpo/walk_forward/tracker/registry/distributed/llm)与 LLM 顶层 API 全部可访问。
- **axon-llm 测试** 修复 `--features explain` 下 3 个 pre-existing clippy 错误(`clippy::field_reassign_with_default` ×2 + unused variable ×1,在 `explain_integration_test.rs:268` 和 `:232-235`):用 `AgentConfig { max_iterations: N, ..Default::default() }` 替代 `let mut + 字段重赋值`;unused `store` 加 `let _ = store;` 显式抑制。`cargo clippy -p axon-llm --features explain --all-targets -- -D warnings` 0 错误,`cargo test -p axon-llm --features explain` 5 个 explain_integration 测试全通过。
- **axon-llm 测试** 移除 7 处 `unimplemented!()` 占位符(全部在 `tests/` 集成测试 mock struct 的 `explain_action_dimension` 方法)。Grep 验证 `crates/` 下 `unimplemented!()` 0 命中(全工作区生产代码 + 测试代码)。原 stub 改为 `Err(ExplainabilityError::ModelNotLoaded(...))` + 注释说明 0 调用点不会触发。涉及 5 个文件:`decision_recorder_test.rs` / `explainer_bridge_test.rs` / `compute_explanation_tool_test.rs`(2 处) / `e2e_explain_e2e_test.rs` / `explain_integration_test.rs`(2 处)。`cargo test -p axon-llm --features explain` 全通过(decision_recorder 6 / explainer_bridge 4 / compute_explanation_tool 7 / explain_integration 5)。
- **axon-exchange** 修复 `lib.rs` 第 15 行 doctest 在 Stage 4' 添加 `fapi_base_url` 字段后未同步,导致 `cargo test --workspace` 失败(`E0063 missing field fapi_base_url`)。补 `fapi_base_url: None` 即可,zero API 变更。`cargo test --workspace` 现在 370+ 测试 0 失败。
- **axon-monitor** `AlertRule::Missing` 现在基于指标最后一次上报时间与当前时间差判断是否触发告警。新增 `MetricsRegistry::check_missing_alerts()` 与 `AlertRule::check_missing()`，并补齐 3 个测试覆盖。
- **axon-explain** `ReportGenerator::aggregate_risk` 改为 `pub fn`，从 `Explanation` 列表按 `feature_importance` × `confidence` 加权聚合，产出 `var_contribution` / `sharpe_contribution` / `max_drawdown_factors`，并补齐 3 个测试覆盖。
- **axon-exchange/binance** `get_positions()` 改为查询持仓端点（默认 `/fapi/v2/positionRisk`）并解析为 `Vec<Position>`；查询失败时返回空 Vec + warn 日志。`ExchangeConfig` 新增 `position_endpoint` 字段（默认 `/fapi/v2/positionRisk`）。补齐 2 个测试覆盖。
- **axon-exchange/okx** `subscribe()` 在 WebSocket writer 可用时实际调用 `send_subscribe_to_writer` 发送订阅消息；writer 不可用时仍记录到 `subscribed_symbols` 等待重连后补发。补齐 1 个测试覆盖。
- **axon-risk** `RiskEngine::compute_metrics` 中 `var_95` 改为基于 `pnl_history`（滚动 252 样本窗口）调用 `checks::var::calculate_var(history, 0.95)` 计算，历史不足 5 样本时降级为 0.0 且 confidence=0.0。补齐 4 个测试覆盖。
- **axon-inference** `pipeline/collector.rs` 实现 `ObservationSource` trait 与 `ObservationCollector`（多源聚合 + 后台轮询 + 错误隔离 + sink 关闭优雅退出），新增 3 个测试覆盖。`lib.rs` 已 re-export。
- **axon-inference** `CandleBackend` 错误信息更新为指向 TDD 规范路径的明确 "未实现" 文案，模块顶部加注契约桩说明，新增 1 个测试验证错误信息。
- **axon-backtest** `engine.rs` 替换为空壳占位：实现事件驱动的 `BacktestEngine` 主循环（`BacktestEngineConfig` + `BacktestEngine::run/step` + `RunResult` 完整字段），处理 `OrderAction` 与 `FillEvent`，累计 events/orders/fills/PnL/drawdown/Nav/duration 指标；新增 8 个测试覆盖空队列、提交/拒绝、撮合、时钟推进、FillEvent、取消/修改/拒绝、step 单步、最大回撤。
- **axon-llm** 修复 `backends::cost::tests` 并行测试 flakiness：删除全局状态污染源 `reset_for_test()`，`register_pricing_overrides` 改为增量测试（默认表 + `custom-model` 共存并显式断言默认表未受影响），新增 `register_pricing_idempotent` 验证 `HashMap::insert` 语义；`PRICING` 静态变量与 `register_pricing` 函数加 doc 注释明确禁止 reset 模式。零核心 API 改动 + 零新增依赖，`cargo test --features python --lib` 并行 50/50 稳定通过（重复 5 次验证）。
- **axon-python** 修复 `cargo build --workspace` 链接 `libpython` 失败的根因：`Cargo.toml` 把 `pyo3` 与各上游 crate（`axon-rl` / `axon-tracker` / `axon-registry` / `axon-hpo` / `axon-walk-forward` / `axon-distributed` / `axon-llm`）从无条件依赖改为 `optional`，新增 opt-in `python` feature；`crate-type` 从 `["cdylib"]` 改为 `["rlib"]`（Python 扩展产物改为通过 `maturin` / `setuptools-rust` 在 `pyproject.toml` 阶段构建，不再污染 `cargo build`）；`src/lib.rs` 顶部加 `#![cfg(feature = "python")]`。`cargo build --workspace`、`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 全部通过。需要 Python 绑定时执行 `cargo build -p axon-python --features python` 并设置 `PYO3_PYTHON`。
- **交易 + Phase 4 完整路线图** `docs/superpowers/plans/2026-06-17-trading-tools-roadmap.md`(12 个 stage 细粒度 TDD 草稿):覆盖 trading tools 后续(A Exchange/OMS/Backtest adapter、B cancel/replace、C max_position_abs)、Phase 4 production 集成(D Prometheus exporter、E risk circuit breaker 集成、F monitor 集成)、横向任务(G Python 绑定补齐、H 文档 + runbook)。包含 1 个 Graphviz 依赖图、每个 stage 独立的 API 草图 + 关键类型 + 测试要点 + 验收标准 + 风险章节、整图验证策略、风险与缓解表、决策记录。不写时间预估,每个 stage 后续单开 plan。

### Added
- **axon-llm `ExchangeTradingBackend` 适配**(Stage A,方案 2 feature flag):在 `axon-llm::trading` 下新增 `trading-exchange` opt-in feature(默认禁用,需 `cargo build -p axon-llm --features trading-exchange`)+ meta `trading-all` feature。新增 `ExchangeTradingBackend` 包装 `Arc<RwLock<Box<dyn ExchangeAdapter>>>`(Box 满足 `RwLock::new` 的 `T: Sized` 要求 + `&mut **guard` 解 Box 拿到 `&mut dyn`),提供 3 个 `TradingBackend` 方法(`place_order` / `get_balance` / `get_positions`);`SymbolMap` 由使用方显式 `register`(`BTC-USDT <-> BTCUSDT` 等),不自动推断(跨交易所命名差异大)。Free function 转换覆盖 `PlaceOrderArgs -> ExOrder` / `AccountBalance -> CurrencyBalance` / `HashMap<asset, AccountBalance> -> BalanceSnapshot` / `Position -> PositionSnapshot`(用 free function 而非 `TryFrom` 是为避开 Rust 孤儿规则,axon-llm 不能为外部类型实现外部 trait)。`map_exchange_error` 集中映射 7 类 `ExchangeError` 变体到 `TradingError::Backend`(鉴权失败脱敏 + warn 日志,业务错误带前缀)。`Order::meta` 白名单透传 `extras` 中的 `leverage` / `margin_type` / `reduce_only` / `stop_loss` / `take_profit`(`client_order_id` 走 `Order::client_order_id` 字段,不进 meta)。`Cargo.toml` 加 `axon-exchange = { workspace = true, optional = true }` + `rust_decimal = { version = "1", features = ["serde-with-str"], optional = true }`(lib 显式用 f64 <-> Decimal 转换需直接访问)。**关键约束**:`cargo tree -p axon-llm`(默认 feature)零传递依赖新增,不引入 `tokio-tungstenite` / `hmac` / `rust_decimal`。补齐 45 个测试(36 单元 + 8 wiremock 集成 `trading_exchange_integration.rs` + 1 testnet `@ignore` E2E `trading_exchange_testnet.rs`)。配套 `docs/superpowers/specs/2026-06-17-axon-llm-exchange-adapter-design.md` + `docs/superpowers/plans/2026-06-17-axon-llm-exchange-adapter.md`(15 个 task ~ 45 个步骤的执行计划)。

### Fixed
- **路线图状态盘点偏差修正** `docs/superpowers/plans/2026-06-17-trading-tools-roadmap.md` v2:首次入仓的路线图误把 Stage G (`axon-inference::CandleBackend`) 与 Stage I (`axon-exchange` leverage/futures) 列为"未交付",但两者在 2026-06-17 之前 commit 已交付:`CandleBackend` 真实实现见 `059c543`(feat(axon-inference): implement CandleBackend single-layer Linear MLP,12 个 candle 单元测试全过);`leverage/futures` 完整实现见 `6463543` + `9be2704`(Stage 4' D,19 个 wiremock 集成测试全过)。本次修正同步:§1.3/1.4 现状盘点表标注 ✅ 已交付、§3 依赖图 G/I 节点标绿、§3.1 推荐实施顺序剔除 G/I、新增 §3.2 已完成 stage 速览表、§4 Stage G/I 整段改为"已交付 + 实际实现摘要 + 遗留工作"、§2.1 战略目标更新"inference/exchange 已 MVP 收口"、§6 风险与缓解新增"路线图盘点偏差"行、§7 决策记录追加 2 条盘点修正条目、§8 文档关系表补充 G/I 配套 spec/plan 链接、§9 后续步骤强调"盘点校验纪律"。`cargo test -p axon-inference --features candle-backend candle` 12/12 通过验证 Stage G 真实现。

### Tests
- 全工作区验证:`cargo build --workspace`、`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 全部通过(0 失败)。Python 绑定相关 (`axon-rl` cdylib / `axon-python`) 默认不再参与 `cargo build`,改用 `cargo build -p <crate> --features python` + `PYO3_PYTHON` 显式构建。新增覆盖各修复点的 ≥ 25 个测试。
- **axon-llm 交易工具 ReAct 闭环集成测试**(`crates/axon-llm/tests/trading_integration.rs`,3 个):本地 `ScriptedMock`(LLMBackend 简单实现)按预定义响应序列消费,避免依赖 `backends` feature;覆盖 `agent_place_order_dry_run_observation`(DryRun 不真发,backend 订单数=0)、`agent_query_portfolio_in_observation`(Observation 含 USDT 余额 + BTC 持仓 JSON)、`agent_two_phase_full_cycle`(TwoPhase 预生成 token → tc1 首次提交 → tc2 二次确认 → backend 订单数=1,iterations=3)。
- **axon-llm Python 绑定测试**(5 个 lib 单元测试 + 4 个 contract test + 19 个 pytest):覆盖 `PyMessage` 角色映射(4 个 role + 未知 role 降级)、`tool_calls` JSON 合法/非法往返、`tool_call_id` 透传、`__repr__` 包含 role+content、`type_name` 覆盖所有 serde_json variant;contract test 覆盖 `LlmConfig::from_dict` 完整 payload、`OpenAICompatConfig::from_llm_config` 多 backend 索引 / 越界、`Message` 字段对齐;pytest 覆盖模块可见性 / `LLMConfig` 序列化 / `LlmMessage` repr / `make_backend` 校验成功/失败 / `load_config_from_toml` 加载 + 错误路径 / 端到端 TOML→backend 串联。`tests/python/test_llm_python_api.py` 与 `crates/axon-llm/tests/python_binding_test.rs`。
- **axon-exchange 杠杆/合约 wiremock 集成测试**(19 个,8 个 Binance + 11 个 OKX,新增 `dev-dependencies wiremock = "0.6"`):覆盖范围错误(set_leverage 0/200 → `OrderRejected`)、签名端点(set_leverage 验证 4 个 OK-ACCESS-* 头)、公开端点(fundingRate / openInterest / longShortRatio / position-tiers)、杠杆分层 brackets 解析(2 档测试)、账户信息(2 次 REST 合并到 `AccountInfo`)、429 Retry-After(ms 转换正确)、401 AuthenticationFailed、`code != "0"` ApiError 解析(包含 code+msg)。wiremock 0.6 的 `path` matcher 不接受带 `?` 的 URL,所有测试用 `path()` 配 `query_param()` 拆开匹配。

## [0.1.0] - 2026-06-13

### Added

#### Phase 0: 架构与基础设施
- Cargo workspace 初始化，17 个 crate
- CI/CD 验证工作流（GitHub Actions）
- Docker 多阶段构建配置

#### Phase 1: 核心引擎 + RL 环境
- `axon-core`：时间戳、类型、市场数据、订单、事件、队列、投资组合、调度器
- `axon-backtest`：L1/L2/L3 撮合引擎、市场冲击模型、延迟模型
- `axon-rl`：Gymnasium 环境、VecEnv、PyO3 绑定

#### Phase 2: 训练与优化
- `axon-hpo`：超参数优化（TPE/CMA-ES/NSGA-II）
- `axon-walk-forward`：滚动前向验证、Purged 交叉验证
- `axon-tracker`：实验追踪（MLflow/WandB/Local）
- `axon-registry`：模型版本管理
- `axon-distributed`：Ray Actor 分布式训练

#### Phase 3: AI 增强
- `axon-llm`：ReAct 智能体、Tool Calling
- `axon-explain`：SHAP 可解释性、反事实分析
- `axon-ensemble`：投票/堆叠/动态加权集成
- `axon-data`：Arrow IPC、Bar 聚合、Mmap 缓存
- `axon-compliance`：审计日志、合规报表

#### Phase 4: 生产部署
- `axon-risk`：风控引擎（熔断器、VaR、仓位/杠杆/回撤检查）
- `axon-inference`：推理引擎（ONNX/tch/Candle、批推理、热更新）
- `axon-exchange`：交易所对接（WebSocket、限流、订单生命周期）
- `axon-oms`：订单管理（状态机、幂等性、快照恢复）
- `axon-monitor`：监控告警（Counter/Gauge/Histogram、告警规则）

#### Phase 5: 性能深度优化
- `axon-core::simd`：SIMD 加速（AVX2 归一化/VaR/深度计算）
- 零拷贝优化（Symbol/Price into_inner）
- 流式回测引擎（StreamingEngine + PaperTrading）

#### 横向任务
- 并发测试、模糊测试（proptest）、契约测试
- 端到端集成测试（36 个集成测试）
- 性能基准测试（15 个 Criterion 基准）
- 用户指南、架构设计文档、API 文档

### Fixed
- PyO3 0.28 兼容性修复（PyDict::new_bound → PyDict::new）
- VaR 计算修复：全正收益时返回 0（而非负值）
- CI 测试改用 `cargo test --workspace`（避免 libtorch 依赖）

### Changed
- 版本号从 0.0.1 升级到 0.1.0

## [0.0.1] - 2026-06-10

### Added
- 项目初始化：工作区、根 Cargo.toml、统一 lint/profile 配置
