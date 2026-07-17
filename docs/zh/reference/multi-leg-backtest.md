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

## 已知限制(0.6.0 路线图)

0.5.0 只验证**结构层**正确性,以下能力**尚未**实装:

| 能力 | 状态 | 计划版本 |
|------|------|---------|
| Funding 结算(perp 收/付 funding rate) | ❌ | 0.6.0 |
| 自动 leg 平衡(`set_target_position` 后自动发单推仓位) | ❌ | 0.6.0 |
| `Position` / `RiskEngine` 全面迁 `HashMap<Instrument, _>` | ❌(暂用 `Symbol` 桥接) | 0.6.0 |
| Mark-to-market 未实现 PnL(只缓存 mark 价) | ❌ | 0.6.0 |

完整 funding 结算 + 自动 leg 平衡 + 多 leg 净值曲线,见 CHANGELOG.md 0.5.0 节"Known Limitations (0.6.0 Roadmap)"。

## 完整测试覆盖

- **Rust 集成测试**:`crates/axon-integration-tests/src/delta_neutral_arb.rs`(5 测试)
  - `two_legs_spot_match_only_spot_fills` — spot fill 不影响 perp
  - `two_legs_orders_route_to_independent_books` — 两 leg 独立挂单
  - `leg_target_position_independent_per_instrument` — 跨 leg 隔离
  - `leg_marks_independent_and_last_wins` — mark 缓存隔离
  - `delta_neutral_entry_orders_isolated` — 真实 delta 中性入场
- **Python E2E**:`python/tests/test_backtest_e2e.py`(6+ 0.5.0 新增测试)
  - `spot_instrument_factory` / `swap_instrument_factory`
  - `begin_bar` per-instrument 独立播种
  - `set_and_get_target_position` 跨 leg 隔离
  - `two_legs_isolated_positions` — spot long + perp short = delta neutral
  - `leg_targets_persist` in `RunResult`
