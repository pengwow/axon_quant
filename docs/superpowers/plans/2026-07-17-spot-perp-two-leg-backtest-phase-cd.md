# 0.5.0 Phase A~D 实施计划 — 多 Leg 完整性

> 在 0.5.0 范围内,把原本归到 0.6.0 的 4 项全部实现,避免 spot+perp 持仓碰撞 footgun,
> 并让 delta-neutral 套利回测能直接出 funding PnL。

## 范围(0.5.0 强制收口)

| Phase | 项 | 关键 API | 风险 |
|------|------|------|------|
| A | Portfolio / RiskEngine 加 `Instrument` 区分 | `Position::instrument`、`Portfolio::instrument_positions()` | 中(只加字段,不删字段) |
| B | Mark-to-market unrealized PnL | `BacktestState::nav()` 用 `mark_cache` 计算 | 低(纯计算) |
| C | Funding 结算 | `Event::Funding`、`push_funding()`、`BacktestEngine` 派发 | 中(新事件类型) |
| D | BacktestEngine 自动 leg 平衡 | `with_auto_rebalance()`、`rebalance_to_target()` | 低(用户可选) |

## Phase A:Portfolio / RiskEngine 加 Instrument 区分

**问题根因**:
- `Position::symbol: Symbol` + `Portfolio::positions: HashMap<Symbol, _>`
- Spot `BTC/USDT` 和 Perp `BTC/USDT` 的 base/quote 完全一样
- HashMap key 碰撞 → Portfolio 把两个 leg 净持仓合成一个 → 风险被错误对冲

**最简修复**(不破坏现有 API):
1. `Position` 增加 `instrument: Instrument` 字段(可选,Default → empty Instrument)
2. `Position::new(symbol, qty, cost)` 增加 `instrument` 参数,旧调用方走 `Position::with_instrument(...)` 工厂或迁移
3. `Portfolio::add_position` / `apply_trade` 接收 `instrument: Instrument` 参数
4. 新增 `Portfolio::instrument_positions() -> HashMap<Instrument, &Position>`(disambiguated 视图)
5. 风险检查 `axon-risk/src/checks/position.rs` 和 `concentration.rs` 用 `pos.instrument` 判 spot/perp
6. `axon-oms/src/portfolio.rs` 和 `axon-oms/src/manager.rs` 同步迁移

**回退方案**:保留 `symbol: Symbol` 字段不变(只加 `instrument`),所有现有 `Symbol` API 不动;新代码用 `instrument`。

## Phase B:Mark-to-market unrealized PnL

**问题**:`BacktestState.mark_cache` 只缓存 mark 价,从不参与 NAV 计算;多 leg 持 spot long + perp short 时
`RunResult.total_pnl` 在 fill 之外永远是 0。

**实施**:
1. `BacktestEngine::step()` 每根 bar 末 / 每笔 fill 后:
   - 遍历 `position_states`,用 `mark_cache[instrument]` 计算 `unrealized_pnl`
   - 把 `(Timestamp, nav = cash + sum(mark * qty))` push 到 `equity_curve`
2. `RunResult::final_nav` = `equity_curve` 末帧(已经反映 unrealized)
3. `RunResult::total_pnl` = `final_nav - initial_cash`(口径不变,数值现在反映 mark)
4. 工具:`BacktestState::nav(mark_cache) -> f64`

## Phase C:Funding 结算

**问题**:delta-neutral 收益全部来自 funding;没有 FundingEvent,8h 一次的 funding 收/付永远不入账。

**实施**:
1. `axon_core::event::funding.rs`(新):`FundingEvent { instrument, funding_rate, mark_price, timestamp }`
2. `Event::Funding(FundingEvent)` 新变体
3. `EventBuilder::funding(...)` 工厂
4. `BacktestEngine` 派发:
   - 找 `position_states[instrument]`(必须是 perp)
   - `cash += qty * funding_rate * mark_price`(正 funding → long 收 / short 付)
   - 累计 `total_funding_pnl` 到 `RunResult`
5. `BacktestEngine::push_funding(instrument, rate, mark)` 便捷方法(用户从 8h 调度器调)
6. Python:`engine.push_funding(instrument_dict, rate, mark_price)`

**8h 调度**:不在引擎内强制调度(框架不绑具体交易所时区),由用户/quantcell 在数据驱动下 push FundingEvent;
引擎只负责"收到 FundingEvent 就结算"。

## Phase D:BacktestEngine 自动 leg 平衡

**问题**:`set_target_position` 只记录 target,策略要手写 rebalance loop。

**实施**:
1. `BacktestEngine::with_auto_rebalance(threshold)` builder
   - `threshold: f64` 是最小 delta(|delta| < threshold 不发单,避免抖动)
2. `BacktestEngine::rebalance_to_target()` 方法
   - 遍历 `bt_state.legs`
   - `current = position_states[instrument].quantity`
   - `delta = target - current`
   - 如果 `|delta| > threshold` → 发市价单 `|delta|` 数量,Side 跟 delta 同号
   - 用 `seed_liquidity_next_id` 模式发单(id 从 `3_000_000_000` 起,避免与策略/seed/EOD 冲突)
3. `with_auto_rebalance` 启用后,每根 bar 末自动调 `rebalance_to_target()`(在 `begin_bar` 或 step 收尾)
4. 新增 `RunResult::rebalances_triggered: u64` 统计

## 文件清单

### 新建
- `crates/axon-core/src/event/funding.rs`
- `crates/axon-integration-tests/src/multi_leg_e2e.rs`(完整 funding + mark + rebalance 端到端)

### 修改
- `crates/axon-core/src/portfolio/{position,core,snapshot}.rs`
- `crates/axon-core/src/event/{mod,order,system,...}.rs`(`Event::Funding` 变体)
- `crates/axon-risk/src/checks/{position,concentration,leverage,order_size}.rs`
- `crates/axon-oms/src/{portfolio,manager,types}.rs`
- `crates/axon-backtest/src/engine.rs`(`apply_funding`, `rebalance_to_target`, nav 接入)
- `crates/axon-backtest/src/python/{engine,types}.rs`(`push_funding`, `rebalance_to_target`)
- `python/axon_quant/backtest.py`(`push_funding`, `rebalance_to_target`)
- `python/tests/test_backtest_e2e.py`(新测试)

## 验证

每个 Phase:
1. 写新测试(red)
2. 实施 + 调通(green)
3. `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
4. `cargo test --workspace`(不破其它)
5. 单独 commit

最终:
- `make test-ci` 全过
- `make version-check` 三源对齐
- `mkdocs build -f {mkdocs,mkdocs-en}.yml --strict` clean
- 推 0.5.0 → origin

## 决策记录

1. **不动 `Portfolio` 键类型** — 加 `instrument` 字段 + 新视图 API,避免 axon-oms/axon-llm 大改
2. **Funding 调度不在引擎** — 框架不绑具体交易所(币安/OKX/Bybit 时区不同),由用户数据驱动
3. **Auto rebalance 用阈值抖动过滤** — `threshold = 0.0` 等价"每 tick rebalance",默认 `1e-6`
4. **不删除 Symbol 字段** — Portfolio 仍按 Symbol 聚合(同一 base/quote 合并视图);
   RiskEngine/BacktestEngine 用 Instrument 区分
