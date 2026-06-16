# Changelog

All notable changes to AXON will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **axon-llm** 统一 `LlmConfig` 类型(`config.rs`):支持 TOML 配置文件 + 字典构造 + 字段级 `merged_override` + 5 级 fallback 解析。涵盖 `BackendConfig` / `RetryConfig` / `ExplainConfig` 子结构与 `ConfigError` 错误类型。`resolve_with_fallback` 5 级优先级:显式路径 > `config.local.toml` > `config.toml` > 仓库内置 demo 配置 > 默认值(validate 失败,要求显式填 api_key)。`to_template_toml(include_secrets)` 支持生成不含敏感字段的模板。补齐 16 个测试覆盖解析/验证/override/fallback/模板。
- **axon-llm** `OpenAICompatConfig::from_llm_config(&LlmConfig, index)` 工厂方法(在 `backends/openai_compat.rs`):从统一配置构造 backend,索引越界返回 `BackendInitError`;补齐 2 个测试覆盖字段映射与索引越界。
- **axon-llm** `live_trading_demo` 重构:移除硬编码 `DEEPSEEK_API_KEY` 环境变量读取,改为 `--config <path>` / `AXON_LLM_CONFIG` 环境变量 + 5 级 fallback 解析,任意 OpenAI 兼容厂商(DeepSeek / OpenAI / Mimo / 本地 Ollama)均可使用。
- **axon-llm** `demo/bin/config.toml` 升级:从单 `[backend]` 表改为 `[[backends]]` 数组 + `[retry]` + `[explain]` 子段,支持多厂商与可解释性集成配置;附带详细注释说明复制/编辑/运行流程。
- **axon-llm** 新增 `integrated_trading_demo` example:三阶段演示(多 backend 串行对话 → ensemble `HardVoteStrategy` 投票 → `ReportGenerator` 渲染 Markdown 决策报告)。`axon-ensemble` 与 `axon-explain` 作为 `demo` feature 的可选依赖引入(`Cargo.toml`)。
- **axon-llm** PyO3 绑定(`src/python/{mod,backend}.rs`,`python` feature 隐含启用 `backends`):暴露 `make_backend(config_dict)` / `LlmBackend` / `LlmMessage` 给 Python;`make_backend` 内部用 `LlmConfig::from_dict` + `OpenAICompatConfig::from_llm_config` 校验并构造 backend,`LlmBackend::chat([LlmMessage, ...])` 同步桥接 `tokio::block_on`。`LlmMessage` 用 `#[pyclass(from_py_object)]` 显式 opt-in FromPyObject(pyo3 0.28 强制要求);`LlmBackend` 字段为 `pub(crate)` 供 `mod.rs` 内部构造。
- **axon-python** `lib.rs` 在 `_native` 下挂载 `llm` 子模块(通过 `axon_llm::python::axon_llm` 注册);`Cargo.toml` 的 `axon-llm` 依赖启用 `["python", "backends"]` features。Workspace `Cargo.toml` 添加 `axon-llm` workspace 依赖。
- **axon_quant(顶层 Python API)** `python/axon_quant/llm.py`:Python 端 `LLMConfig` dataclass(`backends` / `retry` / `explain` 字段)+ `make_backend(config)`(接受 dataclass 或 dict)+ `load_config_from_toml(path)`(从 TOML 文件加载)+ `LlmBackend` / `LlmMessage` 类型别名。`python/axon_quant/__init__.py` 顶层 re-export `LLMConfig` / `LlmBackend` / `LlmMessage` / `make_backend` / `load_config_from_toml`,并把 `llm` 加入 `__all__`。注意:`_native` 是 cdylib 单文件(不是 Python package 目录),所以 `from ._native.llm import ...` 不可用,改用 `from axon_quant._native import llm` 取子模块对象后再属性访问。

### Fixed
- **axon-monitor** `AlertRule::Missing` 现在基于指标最后一次上报时间与当前时间差判断是否触发告警。新增 `MetricsRegistry::check_missing_alerts()` 与 `AlertRule::check_missing()`，并补齐 3 个测试覆盖。
- **axon-explain** `ReportGenerator::aggregate_risk` 改为 `pub fn`，从 `Explanation` 列表按 `feature_importance` × `confidence` 加权聚合，产出 `var_contribution` / `sharpe_contribution` / `max_drawdown_factors`，并补齐 3 个测试覆盖。
- **axon-exchange/binance** `get_positions()` 改为查询持仓端点（默认 `/fapi/v2/positionRisk`）并解析为 `Vec<Position>`；查询失败时返回空 Vec + warn 日志。`ExchangeConfig` 新增 `position_endpoint` 字段（默认 `/fapi/v2/positionRisk`）。补齐 2 个测试覆盖。
- **axon-exchange/okx** `subscribe()` 在 WebSocket writer 可用时实际调用 `send_subscribe_to_writer` 发送订阅消息；writer 不可用时仍记录到 `subscribed_symbols` 等待重连后补发。补齐 1 个测试覆盖。
- **axon-risk** `RiskEngine::compute_metrics` 中 `var_95` 改为基于 `pnl_history`（滚动 252 样本窗口）调用 `checks::var::calculate_var(history, 0.95)` 计算，历史不足 5 样本时降级为 0.0 且 confidence=0.0。补齐 4 个测试覆盖。
- **axon-inference** `pipeline/collector.rs` 实现 `ObservationSource` trait 与 `ObservationCollector`（多源聚合 + 后台轮询 + 错误隔离 + sink 关闭优雅退出），新增 3 个测试覆盖。`lib.rs` 已 re-export。
- **axon-inference** `CandleBackend` 错误信息更新为指向 TDD 规范路径的明确 "未实现" 文案，模块顶部加注契约桩说明，新增 1 个测试验证错误信息。
- **axon-backtest** `engine.rs` 替换为空壳占位：实现事件驱动的 `BacktestEngine` 主循环（`BacktestEngineConfig` + `BacktestEngine::run/step` + `RunResult` 完整字段），处理 `OrderAction` 与 `FillEvent`，累计 events/orders/fills/PnL/drawdown/Nav/duration 指标；新增 8 个测试覆盖空队列、提交/拒绝、撮合、时钟推进、FillEvent、取消/修改/拒绝、step 单步、最大回撤。

### Tests
- 全工作区验证(除 `axon-rl` cdylib 在 macOS 上需要 PYTHON 库链接的环境问题外):`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test -p <crate>` 全部通过。新增覆盖各修复点的 ≥ 25 个测试。
- **axon-llm Python 绑定测试**(5 个 lib 单元测试 + 4 个 contract test + 19 个 pytest):覆盖 `PyMessage` 角色映射(4 个 role + 未知 role 降级)、`tool_calls` JSON 合法/非法往返、`tool_call_id` 透传、`__repr__` 包含 role+content、`type_name` 覆盖所有 serde_json variant;contract test 覆盖 `LlmConfig::from_dict` 完整 payload、`OpenAICompatConfig::from_llm_config` 多 backend 索引 / 越界、`Message` 字段对齐;pytest 覆盖模块可见性 / `LLMConfig` 序列化 / `LlmMessage` repr / `make_backend` 校验成功/失败 / `load_config_from_toml` 加载 + 错误路径 / 端到端 TOML→backend 串联。`tests/python/test_llm_python_api.py` 与 `crates/axon-llm/tests/python_binding_test.rs`。

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
- `axon-hpo`：超参优化（TPE/CMA-ES/NSGA-II）
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
