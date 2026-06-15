# Changelog

All notable changes to AXON will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
- 全工作区验证（除 `axon-rl` cdylib 在 macOS 上需要 PYTHON 库链接的环境问题外）：`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test -p <crate>` 全部通过。新增覆盖各修复点的 ≥ 25 个测试。

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
