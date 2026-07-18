# 多 Leg 回测(spot + perp delta-neutral 套利)

> 0.5.0 起,`BacktestEngine` 支持多 leg(spot + perp)回测,这是 delta-neutral 套利策略的基础。

## 为什么需要多 Leg?

传统单 leg 回测假设策略只在 **一种** 品种上交易(如仅 spot),无法表达:

- **Delta-neutral 套利**:同时持 spot + 反向 perp,赚 funding rate(永续合约资金费率)
- **跨市场套利**:CEX ↔ DEX 同标的价差
- **统计套利**:价差回归(pair trading / 跨品种 spread)

0.5.0 引入 `Instrument` 抽象(详见 [Python 绑定](python-bindings.md) → 0.5.0 多 leg API),把"symbol 字符串"升级为"品种 enum",每个 leg 独立路由、独立持仓、独立 mark 缓存。

## Instrument 抽象

```rust
// axon-core/src/types/instrument.rs
pub enum Instrument {
    Spot(SpotInstrument { base: Symbol, quote: Symbol }),
    Swap(SwapInstrument {
        base: Symbol,
        quote: Symbol,
        settle: SwapSettle,    // UsdMargin | CoinMargin
        contract_size: f64,    // 合约乘数(Binance BTCUSDT 永续 = 1.0)
    }),
}
```

**`Instrument` 用作 `HashMap` 键**:手写 `Hash` / `Eq`(因 `f64` 不实现 trait,
`contract_size` 用 `f64::to_bits()` bitwise 比较)。

## Python 工厂函数

```python
from axon_quant.backtest import spot_instrument, swap_instrument

# Spot instrument dict
btc_spot = spot_instrument("BTC", "USDT")
# {"kind": "spot", "base": "BTC", "quote": "USDT"}

# Swap instrument dict(永续合约)
btc_perp = swap_instrument("BTC", "USDT", settle="usd_margin", contract_size=1.0)
# {"kind": "swap", "base": "BTC", "quote": "USDT",
#  "settle": "usd_margin", "contract_size": 1.0}
```

`swap_instrument` 的 `settle` 接受 `"usd_margin"`(USD 保证金,Binance 默认) /
`"coin_margin"`(币本位),大小写不敏感;`contract_size` 默认 1.0。

## Order API(`Order::spot` / `Order::swap`)

0.5.0 移除 `Order::new`,改用显式工厂:

```rust
// 旧(0.4.x):Order::new(1, "BTC/USDT", Side::Buy, order_type, qty, tif)
// 新 spot:Order::spot(id, base, quote, side, order_type, qty, tif)
let spot_order = Order::spot(1, "BTC", "USDT", Side::Buy,
    OrderType::Limit { price: Price::from_f64(50_001.0) },
    Quantity::from_f64(0.1),
    TimeInForce::GTC,
);

// 新 swap:Order::swap(id, base, quote, settle, contract_size, side, ...)
let perp_order = Order::swap(2, "BTC", "USDT", SwapSettle::UsdMargin, 1.0,
    Side::Sell,
    OrderType::Limit { price: Price::from_f64(50_001.0) },
    Quantity::from_f64(0.1),
    TimeInForce::GTC,
);
```

Python 端通过 `limit_order(id, instrument, side, price, qty)` 工厂:

```python
order = limit_order(1, btc_spot, "Buy", 50_001.0, 0.1)
# {"id": 1, "instrument": {"kind": "spot", "base": "BTC", "quote": "USDT"},
#  "side": "Buy", "type": "limit", "price": 50001.0, "quantity": 0.1, "tif": "GTC"}
```

## BacktestEngine 多 Leg API

| 方法 | 用途 |
|------|------|
| `set_target_position(instrument, target)` | 记录该 leg 的策略目标仓位(纯记录,不发单) |
| `get_target_position(instrument) -> Optional[float]` | 读目标位(未设置过返回 `None`) |
| `get_position(instrument) -> float` | 读当前实际仓位(默认 0.0) |
| `push_mark(instrument, price, ts_ns)` | 写入 mark 价(后到覆盖先到) |
| `begin_bar(price, instrument)` | 在该 leg 上种虚拟对手盘(用 `with_seed_liquidity(...)` 启用) |

`RunResult` 增 3 个 per-instrument dict:

- `positions: dict[instrument, float]` — 终态仓位
- `leg_targets: dict[instrument, float]` — 目标位快照
- `marks: dict[instrument, float]` — 最新 mark 价

## 端到端示例:Delta-Neutral 入场(Funding > 0)

**策略逻辑**:`funding > 0` 时,perp 空头收 funding(资金费率由多头付给空头),
所以策略同时持 spot long + perp short,吃 funding。

```python
from axon_quant.backtest import (
    BacktestEngine, limit_order, spot_instrument, swap_instrument,
)

spot = spot_instrument("BTC", "USDT")
perp = swap_instrument("BTC", "USDT", settle="usd_margin", contract_size=1.0)

bt = BacktestEngine(initial_cash=100_000.0).with_seed_liquidity(
    half_spread=0.5, depth_levels=2, size_per_level=2.0,
)
# 每根 bar 各自触发 seed 流动性(spot / perp 各自独立)
bt.begin_bar(50_000.0, spot)
bt.begin_bar(50_000.0, perp)
# 设置 leg 目标位:spot long +1,perp short -1(delta 中性,吃 funding > 0)
bt.set_target_position(spot, 1.0)
bt.set_target_position(perp, -1.0)
# 策略下单
bt.push_event({
    "type": "order_submitted", "timestamp_ns": 1_000,
    "order": limit_order(1, spot, "Buy", 50_001.0, 0.5),
})
bt.push_event({
    "type": "order_submitted", "timestamp_ns": 1_500,
    "order": limit_order(2, perp, "Sell", 50_001.0, 0.5),
})
# 推 mark 价(供 0.6.0 funding 结算 / 未实现 PnL 估值)
bt.push_mark(spot, 50_000.0, timestamp_ns=1_000_000)
bt.push_mark(perp, 50_100.0, timestamp_ns=1_500_000)

result = bt.run()
# spot long = +0.5,perp short = -0.5,净额 0(delta 中性)
assert result.positions[spot] == 0.5
assert result.positions[perp] == -0.5
assert bt.get_target_position(spot) == 1.0
assert bt.get_target_position(perp) == -1.0
assert result.marks[spot] == 50_000.0
assert result.marks[perp] == 50_100.0
```

## L1MatchingEngine 多 Instrument 路由

`L1MatchingEngine` 内部从单 book 升级为 `HashMap<Instrument, L1Book>`:

```text
┌─ L1MatchingEngine ─────────────────────┐
│  books: HashMap<Instrument, L1Book>     │
│  ┌─ L1Book(spot BTC/USDT) ─────────┐    │
│  │  bids: BTreeMap<Price, ...>     │    │
│  │  asks: BTreeMap<Price, ...>     │    │
│  │  order_index: HashMap<u64, ...> │    │
│  └─────────────────────────────────┘    │
│  ┌─ L1Book(perp BTC/USDT) ─────────┐    │
│  │  ...(独立 book,撮合互不影响)     │    │
│  └─────────────────────────────────┘    │
│  trade_sequence: AtomicU64(跨 book 共享)│
└─────────────────────────────────────────┘
```

`submit(order)` 路由逻辑:按 `order.instrument` 取对应 `L1Book`(`HashMap::entry().or_default()` 自动建 book),撮合只在 book 内部进行。spot 撮合**不会**触碰 perp book,反之亦然。

## 0.6.0 收口(全栈 Instrument 化 + 跨 leg 风险约束)

0.5.0 路线图上的全部 7 项 ✅ 完成,**0.6.0 一次性发布**。新增能力:

| 能力 | 关键 API |
|------|---------|
| L3 `MultiAssetMatchingEngine.engines` / `dark_orders` 全面 `Instrument` 化 | `register_instrument` / `engine(&Instrument)` / `best_bid(&Instrument)` |
| L3 `CrossPair.leg1` / `leg2` 改用 `LegPair` | `LegPair::new(spot, perp)` / `with_ratio(...)`,`execute_arbitrage(&LegPair, ...)` |
| `axon_backtest::streaming::engine` 全面 `Instrument` 化 | `MarketDataEvent::Tick.instrument`,`StreamDataSource::subscribe(&[Instrument])`,`StreamingStrategy::on_tick(&Instrument, f64)`,`register_instrument` |
| `axon_oms::Order` / `Fill` 加 `instrument: Option<Instrument>` | `Order::with_instrument(Instrument)`,`OmsSnapshot.version = OMS_SNAPSHOT_VERSION_CURRENT = 2`,0.5.0 snapshot 兼容(`#[serde(default)]` 兜底) |
| `begin_bar` 收尾自动 rebalance(Phase 1) | `BacktestEngine::begin_bar` 收尾自动触发,`with_auto_rebalance(threshold)` builder |
| 自适应 funding 调度 8h 整点(Phase 2) | `FundingSchedule { fixed_rate, interval, start_ts }` + `with_funding_schedule(builder)`,`begin_bar` 自动 dispatch |
| 跨 leg 风险约束(净敞口 / VaR / 压力测试,Phase 6) | `axon_risk::checks::leg_pair::check_leg_pair_net_exposure` / `per_leg_var`,`axon_risk::checks::stress::stress_pair` / `stress_portfolio`,`RiskEngine::check_leg_pair(portfolio, &LegPair)` |

### 跨 leg 风险约束(0.6.0 Phase 6)

新增 API 把"delta 中性"从"策略层约定"升级到"引擎层硬约束":

```rust
use axon_core::types::{
    Instrument, LegPair, Price, Quantity, SpotInstrument, SwapInstrument, SwapSettle,
};
use axon_core::market::Side;
use axon_core::portfolio::{Currency, Portfolio, Position};
use axon_risk::{DefaultRiskEngine, RiskConfig, RiskEngine, RiskResult};

// 构造 spot + perp 1:1 对冲对
let btc_spot = Instrument::Spot(SpotInstrument { base: "BTC".into(), quote: "USDT".into() });
let btc_perp = Instrument::Swap(SwapInstrument {
    base: "BTC".into(),
    quote: "USDT".into(),
    settle: SwapSettle::UsdMargin,
    contract_size: 1.0,
});
let pair = LegPair::new(btc_spot.clone(), btc_perp.clone());

// 构造 portfolio:spot long 1.0,perp short 1.0(delta 中性)
let mut pf = Portfolio::new(Currency::USD, 0.0);
pf.add_position(Position::with_instrument(
    btc_spot,
    Quantity::from_f64(1.0),
    Price::from_f64(50_000.0),
));
pf.add_position(Position::with_instrument(
    btc_perp,
    Quantity::from_f64(-1.0),
    Price::from_f64(50_000.0),
));

// 1. 净暴露检查:net = spot_qty + perp_qty * hedge_ratio
//    默认 max_leg_pair_net_exposure = 0.0(严格 delta 中性)
let engine = DefaultRiskEngine::new(RiskConfig::default());
assert_eq!(engine.check_leg_pair(&pf, &pair), RiskResult::Allow);

// 2. 压力测试:价格冲击 ±5% 下 delta 中性对的 PnL 影响
use axon_risk::checks::stress;
let pnl_impact = stress::stress_pair(&pf, &pair, 0.05);
// delta 中性时 spot 与 perp 涨跌相抵,pnl_impact ≈ 0

// 3. Per-leg VaR:用历史收益序列算 VaR(95% 置信)
use axon_risk::checks::leg_pair::per_leg_var;
let var = per_leg_var(&[-0.05, -0.03, -0.01, 0.01, 0.02, 0.03, 0.04], 0.95);
```

新增 reason 变体 `RiskReason::LegPairNetExposureExceeded { pair, current, limit }`,配合 `RiskConfig.max_leg_pair_net_exposure` / `max_leg_pair_var_95` 做硬阈值。

### `axon_oms::Order` / `Fill` Instrument 化(0.6.0 Phase 5)

`Order` 新增结构化 instrument 字段(`Option<Instrument>`),`Fill` 同理。`#[serde(default)]` 让 0.5.0 序列化的 snapshot 仍可在 0.6.0 进程里 recover:

```rust
use axon_oms::{Decimal, Order, OrderType, Side};
use axon_core::types::{Instrument, SpotInstrument};

let order = Order::new(
    "BTC-USDT".to_string(),   // instrument_id:OMS 字符串路径仍保留(0.5.0 兼容)
    Side::Buy,
    OrderType::Limit { price: Decimal::from(50_000) },
    Decimal::from(1),
    Decimal::from(50_000),
)
.with_instrument(Instrument::Spot(SpotInstrument {
    base: "BTC".into(),
    quote: "USDT".into(),
}));
```

snapshot schema 升级:`OmsSnapshot.version` 固定 `OMS_SNAPSHOT_VERSION_CURRENT = 2`(0.5.0 之前是自增计数器)。0.5.0 之前的 v1 snapshot 仍可在 0.6.0 进程里 recover(`#[serde(default)]` 兜底)。

> 📌 **Python 绑定状态**:`check_leg_pair` 是 0.6.0 phase 6 在 Rust `RiskEngine` 上新增的 trait method,但 `axon-risk` Python 绑定 **暂未** 暴露(`crates/axon-risk/src/python/engine.rs` 只有 `check_order` 等 0.5.0 之前的 API)。Phase 6 跨 leg 风险约束目前 **仅 Rust 可用**,Python 用户需要等待 0.6.0 之后的 binding 收口 PR 才能用。完整 funding 结算 + 自动 rebalance 流程(均已有 Python 绑定)见下面两节。

## Funding 结算(0.5.0 Phase C 新增)

永续合约的资金费率由数据源在每个结算点(典型 8h)推送。引擎按
`position_qty × funding_rate × mark_price` 累计到 cash 并写入
`RunResult.total_funding_pnl`,**spot instrument 会被忽略**。

```python
# delta-neutral 套利:spot long 1.0 + perp short 1.0
bt.set_target_position(spot, 1.0)
bt.set_target_position(perp, -1.0)
bt.rebalance_to_target(threshold=1e-6)  # 入场

# 8h 后推 funding
bt.push_funding(perp, funding_rate=0.0001, mark_price=50_100.0,
                timestamp_ns=8 * 3600 * 1_000_000_000)
result = bt.run()
# 持仓:spot long 1.0,perp short 1.0
# perp short 收 1.0 × 0.0001 × 50_100 = +5.01
assert result.total_funding_pnl == pytest.approx(5.01, abs=1e-6)
```

**符号约定**(业内标准):
- `funding_rate > 0`:perp 高于 spot,long 付 / short 收
- `funding_rate < 0`:perp 低于 spot,short 付 / long 收

**调度**:引擎不内置 8h 时钟,需数据源 / 调度器按需调
`push_funding`;`axon-data` / quantcell 应用层可挂 cron 推送。

## 自动 Leg 平衡(0.5.0 Phase D 新增)

`set_target_position` 仅记录策略意图,不主动发单。Phase D 新增
`rebalance_to_target()` 手动触发,或 `with_auto_rebalance(threshold)`
+ 在每根 bar 末调用 rebalance。

```python
# 1) 启用自动 rebalance(阈值 = 1e-6 避免抖动)
bt.with_auto_rebalance(1e-6)

# 2) 设置 leg 目标位
bt.set_target_position(spot, 1.0)   # spot long 1.0
bt.set_target_position(perp, -1.0)  # perp short 1.0(delta 中性)

# 3) 每根 bar 末(策略主循环)手动触发
#    — 0.5.0 由调用方在 bar 末显式调
#    — 0.6.0 计划在 begin_bar 收尾自动触发
bt.begin_bar(50_000.0, spot)         # 种 spot 流动性格子
bt.begin_bar(50_000.0, perp)         # 种 perp 流动性格子
# ... 策略信号生成 ...
triggered = bt.rebalance_to_target()  # None → 用配置的 1e-6 阈值
# triggered:实际发出去的 rebalance 单数

result = bt.run()
print(f"本次回测 rebalance 触发 {result.rebalances_triggered} 次")
```

**API 摘要**:

| 方法 | 用途 |
|------|------|
| `set_target_position(inst, target)` | 记录 leg 目标(0.5.0) |
| `get_target_position(inst) -> Optional[float]` | 读目标(0.5.0) |
| `rebalance_to_target(threshold=None) -> int` | **手动**触发 rebalance(0.5.0 Phase D),返回实际 fill 数 |
| `with_auto_rebalance(threshold)` | 配置默认阈值(0.5.0 Phase D) |
| `with_auto_rebalance_disable()` | 关闭(0.5.0 Phase D) |
| `RunResult.rebalances_triggered` | 累计 rebalance fill 数(0.5.0 Phase D) |

**设计约束**:
- rebalance 单 id 起点 `3_000_000_000`,避开策略(0..1e9) /
  seed 流动性(1e9..2e9) / EOD 平仓(2e9..3e9)区间
- `threshold` 过滤抖动:`|target - current| <= threshold` 不发单
- 未设置 `target` 的 leg 不参与 rebalance(只对显式调过
  `set_target_position` 的 instrument 起作用)
- delta-neutral 入场:`spot long +1 + perp short -1` 各填 1 笔,
  净敞口 0,后续吃 funding

## 完整测试覆盖

- **Rust 单元测试**(`crates/axon-backtest/src/engine.rs` Phase C/D):
  - `test_funding_long_pays_cash_decreases` / `test_funding_short_receives_cash_increases`
  - `test_funding_multiple_accumulate` / `test_funding_spot_instrument_ignored`
  - `test_rebalance_long_target_from_zero` / `test_rebalance_to_zero_position`
  - `test_rebalance_threshold_filters_jitter` / `test_rebalance_only_set_target_legs`
  - `test_rebalance_multiple_legs_delta_neutral`
  - `test_rebalances_triggered_accumulate_across_calls`
- **Rust 集成测试**:`crates/axon-integration-tests/src/delta_neutral_arb.rs`(11 测试)
  - 两腿路由:`two_legs_spot_match_only_spot_fills` / `two_legs_orders_route_to_independent_books`
  - 跨 leg 隔离:`leg_target_position_independent_per_instrument` / `leg_marks_independent_and_last_wins`
  - delta 中性入场:`delta_neutral_entry_orders_isolated`
  - **Phase C funding 端到端**:`funding_settle_end_to_end_delta_neutral` / `funding_multiple_settlements_accumulate` / `mark_funding_combined_unrealized_pnl`
  - **Phase D rebalance 端到端**:`rebalance_end_to_end_pnl_aware` / `rebalance_two_legs_delta_neutral`
  - **完整生命周期**:`delta_neutral_full_lifecycle_funding_and_rebalance`(入场 → rebalance → funding 结算 → delta 中性保持)
- **Python E2E**:`python/tests/test_backtest_e2e.py`(0.5.0 新增)
  - `spot_instrument_factory` / `swap_instrument_factory`
  - `begin_bar` per-instrument 独立播种
  - `set_and_get_target_position` 跨 leg 隔离
  - `two_legs_isolated_positions` — spot long + perp short = delta neutral
  - `leg_targets_persist` in `RunResult`
  - `push_funding_long_pays_cash_decreases` / `push_funding_short_receives_cash_increases`
  - `push_funding_spot_instrument_ignored` / `push_funding_with_zero_position`
  - `rebalance_to_target_fills_position` / `rebalance_to_target_disabled_returns_zero`
  - `rebalance_threshold_filters_jitter` / `rebalance_two_legs_delta_neutral`
