# axon-backtest E2E 测试场景矩阵

> 维护者: axon-backtest team
> 更新日期: 2026-07-11
> 对应计划: `axon-backtest测试优化方案.md` (v1) + `axon-backtest测试薄弱场景优化v2-收尾与增量方案.md` (v2)

## 1. 测试组织原则(5 维度)

| 维度 | 命名约定 | 覆盖目标 |
|------|----------|----------|
| **A 端到端** | `e2e_*.rs` | 数据 → 策略 → 撮合 → 结算 全链路 + 多场景 |
| **B 状态机** | `state_machine_*.rs` | 6 状态机的所有 match 分支 + 浮点边界 |
| **C 边界** | `*_boundaries.rs` / `negative_validation.rs` | FeeConfig 极值 / 负向输入 / 异常路径 |
| **D 集成** | `*_integration.rs` | L1 / L2 / L3 / Impacted / streaming 各模块的 E2E |
| **E 性能** | `*_perf*.rs` / `impact_breakdown.rs` | perf gate + 基准测量 |

## 2. 场景矩阵(20 个文件 / 99 个集成测试)

| # | 文件 | 行数 | 测试 | 维度 | 触发命令 | 验证目标 |
|---|------|------|------|------|----------|----------|
| 1 | `backtest_e2e_correctness.rs` | 614 | 5 | A | `cargo test -p axon-backtest --test backtest_e2e_correctness` | SMA crossover E2E + PnL 手算对账 |
| 2 | `concurrent_backtest.rs` | 262 | 4 | C | `cargo test -p axon-backtest --test concurrent_backtest` | 多线程独立 backtest 安全性 |
| 3 | `e2e_cancel_amend.rs` | 217 | 4 | A | `cargo test -p axon-backtest --test e2e_cancel_amend` | 撤单 / 改单 E2E 策略 |
| 4 | `e2e_force_liquidate.rs` | 455 | 4 | A | `cargo test -p axon-backtest --test e2e_force_liquidate` | EOD 强制平仓语义 |
| 5 | `e2e_impact_integration.rs` | 584 | 4 | A+D | `cargo test -p axon-backtest --test e2e_impact_integration` | ImpactedMatchingEngine E2E |
| 6 | `e2e_multi_symbol.rs` | 429 | 4 | A | `cargo test -p axon-backtest --test e2e_multi_symbol` | 多 symbol 联合回测 |
| 7 | `e2e_order_type_matrix.rs` | 227 | 7 | A | `cargo test -p axon-backtest --test e2e_order_type_matrix` | Market/Limit/IOC/FOK/Stop/StopLimit/Iceberg E2E |
| 8 | `e2e_seed_liquidity.rs` | 203 | 4 | A | `cargo test -p axon-backtest --test e2e_seed_liquidity` | `with_seed_liquidity` + `begin_bar` E2E |
| 9 | `edge_events_validation.rs` | 264 | 5 | C | `cargo test -p axon-backtest --test edge_events_validation` | Cancel/Modify/Reject 事件路径边界 |
| 10 | `fee_config_boundaries.rs` | 215 | 4 | C | `cargo test -p axon-backtest --test fee_config_boundaries` | FeeConfig 极值 (0 / 1 / NaN / 负) |
| 11 | `impact_breakdown.rs` | 187 | 1 | E | `cargo test -p axon-backtest --test impact_breakdown --release` | 冲击模型各路径 ns/iter 性能 |
| 12 | `l2_engine_e2e.rs` | 404 | 5 | D | `cargo test -p axon-backtest --test l2_engine_e2e` | L2MatchingEngine 独有方法 (modify/from_entries/stats) |
| 13 | `l3_integration.rs` | 410 | 6 | D | `cargo test -p axon-backtest --test l3_integration` | L3 多资产/批量拍卖/暗池/套利检测 |
| 14 | `l3_snapshot_restore.rs` | 279 | 5 | D | `cargo test -p axon-backtest --test l3_snapshot_restore` | L3 snapshot/restore 保留语义 |
| 15 | `nav_dd_consistency.rs` | 345 | 6 | B+C | `cargo test -p axon-backtest --test nav_dd_consistency` | max_drawdown 含恢复 + 多个边界 |
| 16 | `negative_validation.rs` | 283 | 4 | C | `cargo test -p axon-backtest --test negative_validation` | 非法订单(qty=0/price<0/超 capacity) |
| 17 | `perf_1000_bar_replay.rs` | 318 | 4 (2 ignored) | E | `cargo test -p axon-backtest --test perf_1000_bar_replay --release -- --include-ignored` | 1000 根 bar 性能门(1000 < 1s) |
| 18 | `python_matching_engine_trait.rs` | 214 | 0 | D | `cargo test -p axon-backtest --test python_matching_engine_trait` | Python 桥接 stub(预留) |
| 19 | `replace_engine_e2e.rs` | 326 | 3 | D | `cargo test -p axon-backtest --test replace_engine_e2e` | 运行时替换撮合引擎 |
| 20 | `run_result_fields.rs` | 629 | 10 | B | `cargo test -p axon-backtest --test run_result_fields` | RunResult Stage 3 字段(原 0.3.0 引入) |
| 21 | `sharpe_winrate_edge.rs` | 280 | 5 | C | `cargo test -p axon-backtest --test sharpe_winrate_edge` | TradingMetrics 边界(0/1/2 trade / 负均值) |
| 22 | `state_machine_deep.rs` | 479 | 5 | B | `cargo test -p axon-backtest --test state_machine_deep` | 6 状态机隐藏边界(浮点 1e-12 / 多次反转) |

**总测试数**: 99 集成测试 + 179 单元测试 = **278 个测试**(`cargo test -p axon-backtest --tests --release -- --include-ignored`)

## 3. 快速验证命令

```bash
# 1) 默认验证(跳过 #[ignore] perf gate, < 1s)
make test-fast

# 2) Release 模式全套(含 perf gate, ~25s)
make test-release

# 3) 单独跑某个文件
cargo test -p axon-backtest --test e2e_impact_integration

# 4) 跑特定测试(带打印)
cargo test -p axon-backtest --test e2e_force_liquidate -- --nocapture

# 5) clippy 静态检查
cargo clippy -p axon-backtest --all-targets -- -D warnings
```

## 4. 新增测试 checklist

往 `tests/` 添加新文件前,先回答:

- [ ] **属于哪个维度**(A/B/C/D/E)?命名是否遵循约定?
- [ ] **手算对账**: 是否包含 1 个"手算 vs result 对账"的测试?无手算 = 不可信。
- [ ] **确定性数据**: 是否用闭式公式 / 硬编码事件?避免外部 CSV 依赖 CI 稳定。
- [ ] **撮合对手盘**: 策略发 market order 之前,是否先 push 1 个 limit @ bar.close 做对手方?
- [ ] **不修改源码**: 是否用 thin adapter 桥接(同 `ImpactedAdapter` / `L3Adapter` 模式)?不要改源码。
- [ ] **clippy 合规**: 跑 `cargo clippy -p axon-backtest --all-targets -- -D warnings` 通过?
- [ ] **README 更新**: 在本文件"场景矩阵"追加 1 行 + 编号。

## 5. 已知遗留 / 未覆盖

- **B.2** `e2e_streaming_paper.rs`: 阻塞于 [streaming/data_source.rs:91-94](../src/streaming/data_source.rs) `next_event` stub
- **C.4** `replay_source_integration.rs`: 阻塞于 `ReplayStreamSource::next_event` stub
- **P2-7**: 真实 CSV/Parquet 加载 + 10000 bar 性能门
- **P2-6** 增强: L3 snapshot 恢复后的 full L2 簿重建(目前只恢复资产注册,深度需要 L2.from_entries)

## 6. 性能基线(release 模式,MacBook M-series)

| 测试 | 耗时 | 备注 |
|------|------|------|
| `impact_breakdown` | 22.34s | 1 个 bench,100k iter 测 ns/iter(必需 warmup) |
| `perf_1000_bar_replay` | 0.14s | 1000 根 bar SMA crossover,42 fills(perf gate) |
| 其余 20 个文件 | < 0.01s each | 即时返回,无 I/O 阻塞 |
| **总 release 耗时** | ~22.5s | 瓶颈在 `impact_breakdown` 的 warmup |

> **`impact_breakdown` 不拆分**: 它是 ns/iter 测量,需要 1000 warmup + 100k 测时,拆分会破坏测量精度。

## 7. CI 集成建议

```yaml
# .github/workflows/ci.yml(片段)
- name: axon-backtest fast tests
  run: make test-fast

- name: axon-backtest release + perf gate
  run: make test-release
  # 慢,但 25s 可接受,放 daily build 而非 PR check
```
