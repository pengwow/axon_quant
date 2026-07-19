# Multi-Leg Backtest (spot + perp delta-neutral arbitrage)

> From 0.5.0, `BacktestEngine` supports multi-leg (spot + perp) backtesting, the foundation for delta-neutral arbitrage strategies.

## Why Multi-Leg?

Traditional single-leg backtest assumes the strategy trades on **one** instrument (e.g., spot only) and cannot express:

- **Delta-neutral arbitrage**: simultaneously hold spot + opposite perp, earn funding rate (perpetual contract funding rate)
- **Cross-market arbitrage**: CEX ↔ DEX same-instrument spread
- **Statistical arbitrage**: spread reversion (pair trading / cross-instrument spread)

0.5.0 introduces the `Instrument` abstraction (see [Python Bindings](python-bindings.md) → 0.5.0 multi-leg API), upgrading "symbol string" to "instrument enum". Each leg has independent routing, position, and mark cache.

## Instrument Abstraction

```rust
// axon-core/src/types/instrument.rs
pub enum Instrument {
    Spot(SpotInstrument { base: Symbol, quote: Symbol }),
    Swap(SwapInstrument {
        base: Symbol,
        quote: Symbol,
        settle: SwapSettle,    // UsdMargin | CoinMargin
        contract_size: f64,    // contract multiplier (Binance BTCUSDT perp = 1.0)
    }),
}
```

**`Instrument` as `HashMap` key**: manually implemented `Hash` / `Eq` (because `f64` doesn't implement these traits; `contract_size` uses `f64::to_bits()` for bitwise comparison).

## Python Factories

```python
from axon_quant.backtest import spot_instrument, swap_instrument

# Spot instrument dict
btc_spot = spot_instrument("BTC", "USDT")
# {"kind": "spot", "base": "BTC", "quote": "USDT"}

# Swap instrument dict (perpetual contract)
btc_perp = swap_instrument("BTC", "USDT", settle="usd_margin", contract_size=1.0)
# {"kind": "swap", "base": "BTC", "quote": "USDT",
#  "settle": "usd_margin", "contract_size": 1.0}
```

`swap_instrument` `settle` accepts `"usd_margin"` (USD margin, Binance default) /
`"coin_margin"` (coin-margined), case-insensitive; `contract_size` defaults to 1.0.

## Order API (`Order::spot` / `Order::swap`)

0.5.0 removes `Order::new` in favor of explicit factories:

```rust
// Old (0.4.x): Order::new(1, "BTC/USDT", Side::Buy, order_type, qty, tif)
// New spot: Order::spot(id, base, quote, side, order_type, qty, tif)
let spot_order = Order::spot(1, "BTC", "USDT", Side::Buy,
    OrderType::Limit { price: Price::from_f64(50_001.0) },
    Quantity::from_f64(0.1),
    TimeInForce::GTC,
);

// New swap: Order::swap(id, base, quote, settle, contract_size, side, ...)
let perp_order = Order::swap(2, "BTC", "USDT", SwapSettle::UsdMargin, 1.0,
    Side::Sell,
    OrderType::Limit { price: Price::from_f64(50_001.0) },
    Quantity::from_f64(0.1),
    TimeInForce::GTC,
);
```

Python side uses `limit_order(id, instrument, side, price, qty)` factory:

```python
order = limit_order(1, btc_spot, "Buy", 50_001.0, 0.1)
# {"id": 1, "instrument": {"kind": "spot", "base": "BTC", "quote": "USDT"},
#  "side": "Buy", "type": "limit", "price": 50001.0, "quantity": 0.1, "tif": "GTC"}
```

## BacktestEngine Multi-Leg API

| Method | Purpose |
|--------|---------|
| `set_target_position(instrument, target)` | Record strategy target position for this leg (record only, no order sent) |
| `get_target_position(instrument) -> Optional[float]` | Read target (returns `None` if not set) |
| `get_position(instrument) -> float` | Read current actual position (default 0.0) |
| `push_mark(instrument, price, ts_ns)` | Write mark price (last-wins) |
| `begin_bar(price, instrument)` | Seed virtual counterparty liquidity on this leg (requires `with_seed_liquidity(...)`) |

`RunResult` adds 3 per-instrument dicts:

- `positions: dict[instrument, float]` — terminal positions
- `leg_targets: dict[instrument, float]` — target snapshot
- `marks: dict[instrument, float]` — latest mark prices

### Chained `with_*` configuration (0.7.1+)

Since 0.7.1, all `BacktestEngine.with_*` methods return `&mut Self` (Python: `PyRefMut<...>`) instead of `()`, so you can chain configuration fluently:

```python
bt = (BacktestEngine(initial_cash=100_000.0)
      .with_seed_liquidity(half_spread=0.5, depth_levels=2, size_per_level=2.0)
      .with_fee_config(0.0005)
      .with_auto_rebalance(threshold=0.01)
      .with_funding_schedule(period_secs=28_800))  # 8h funding
```

Affected methods: `with_matching_engine`, `with_fee_config`, `with_force_liquidate`, `with_seed_liquidity`, `with_seed_liquidity_for`, `with_auto_rebalance`, `with_auto_rebalance_disable`, `with_funding_schedule`, `with_funding_schedule_disable`. **BREAKING (light)**: Python callers that bound the return value to a name will see `engine` instead of `None`; call-and-discard code is unaffected.

### `bar_nav_curve` per-bar NAV curve (0.7.1+)

`RunResult.equity_curve` only samples on `fill / mark / funding` events. For a short backtest with zero fills the last frame is `initial_cash`, which makes Sharpe / max-drawdown calculations meaningless. Since 0.7.1, every `begin_bar` / `begin_bar_multi` call also appends one frame to `bar_nav_curve: list[tuple[ts_ns, nav]]` where `nav = compute_nav(clock.now(), mark_fallback)`.

```python
result = bt.run()
import numpy as np
arr = np.asarray(result.bar_nav_curve, dtype=np.float64)  # shape (N, 2)
ts_s  = arr[:, 0] * 1e-9
nav   = arr[:, 1]
# Annualised Sharpe from per-bar returns (15-min bars → 35_040 bars/year)
log_r = np.diff(np.log(nav))
bar_per_year = 365 * 24 * 4  # 15-min
sharpe = log_r.mean() / log_r.std(ddof=1) * np.sqrt(bar_per_year)
```

Same-`ts` de-dup: if you call `begin_bar` multiple times with the same `clock.now()` (e.g. across multiple legs), the last frame overwrites the previous one — no duplicate points pollute the Sharpe series.

## End-to-End Example: Delta-Neutral Entry (Funding > 0)

**Strategy logic**: when `funding > 0`, perp shorts receive funding (funding rate paid from longs to shorts), so the strategy simultaneously holds spot long + perp short, eating funding.

```python
from axon_quant.backtest import (
    BacktestEngine, limit_order, spot_instrument, swap_instrument,
)

spot = spot_instrument("BTC", "USDT")
perp = swap_instrument("BTC", "USDT", settle="usd_margin", contract_size=1.0)

bt = BacktestEngine(initial_cash=100_000.0).with_seed_liquidity(
    half_spread=0.5, depth_levels=2, size_per_level=2.0,
)
# Each bar triggers independent seed liquidity (spot / perp isolated)
bt.begin_bar(50_000.0, spot)
bt.begin_bar(50_000.0, perp)
# Set leg targets: spot long +1, perp short -1 (delta neutral, eat funding > 0)
bt.set_target_position(spot, 1.0)
bt.set_target_position(perp, -1.0)
# Strategy orders
bt.push_event({
    "type": "order_submitted", "timestamp_ns": 1_000,
    "order": limit_order(1, spot, "Buy", 50_001.0, 0.5),
})
bt.push_event({
    "type": "order_submitted", "timestamp_ns": 1_500,
    "order": limit_order(2, perp, "Sell", 50_001.0, 0.5),
})
# Push mark prices (for 0.6.0 funding settlement / unrealized PnL valuation)
bt.push_mark(spot, 50_000.0, timestamp_ns=1_000_000)
bt.push_mark(perp, 50_100.0, timestamp_ns=1_500_000)

result = bt.run()
# spot long = +0.5, perp short = -0.5, net 0 (delta neutral)
assert result.positions[spot] == 0.5
assert result.positions[perp] == -0.5
assert bt.get_target_position(spot) == 1.0
assert bt.get_target_position(perp) == -1.0
assert result.marks[spot] == 50_000.0
assert result.marks[perp] == 50_100.0
```

## L1MatchingEngine Multi-Instrument Routing

`L1MatchingEngine` internally upgrades from single book to `HashMap<Instrument, L1Book>`:

```text
┌─ L1MatchingEngine ─────────────────────┐
│  books: HashMap<Instrument, L1Book>     │
│  ┌─ L1Book(spot BTC/USDT) ─────────┐    │
│  │  bids: BTreeMap<Price, ...>     │    │
│  │  asks: BTreeMap<Price, ...>     │    │
│  │  order_index: HashMap<u64, ...> │    │
│  └─────────────────────────────────┘    │
│  ┌─ L1Book(perp BTC/USDT) ─────────┐    │
│  │  ...(independent book, matching │    │
│  │  is fully isolated)              │    │
│  └─────────────────────────────────┘    │
│  trade_sequence: AtomicU64(shared)      │
└─────────────────────────────────────────┘
```

`submit(order)` routing logic: fetch corresponding `L1Book` by `order.instrument` (`HashMap::entry().or_default()` auto-creates), matching happens only within the book. Spot matching **does not** touch perp book, and vice versa.

## Known Limitations (0.6.0 Roadmap)

0.5.0 only validates **structural** correctness. The following capabilities are **not yet** implemented:

| Capability | Status | Target Version |
|-----------|--------|----------------|
| Funding settlement (perp receives/pays funding rate) | ❌ | 0.6.0 |
| Automatic leg balancing (auto-send orders after `set_target_position`) | ❌ | 0.6.0 |
| Full `Position` / `RiskEngine` migration to `HashMap<Instrument, _>` | ❌ (uses `Symbol` bridge) | 0.6.0 |
| Mark-to-market unrealized PnL (only caches mark price) | ❌ | 0.6.0 |

Full funding settlement + automatic leg balancing + multi-leg NAV curve, see CHANGELOG.md 0.5.0 section "Known Limitations (0.6.0 Roadmap)".

## Complete Test Coverage

- **Rust integration tests**: `crates/axon-integration-tests/src/delta_neutral_arb.rs` (5 tests)
  - `two_legs_spot_match_only_spot_fills` — spot fill doesn't affect perp
  - `two_legs_orders_route_to_independent_books` — two legs independent passive orders
  - `leg_target_position_independent_per_instrument` — cross-leg isolation
  - `leg_marks_independent_and_last_wins` — mark cache isolation
  - `delta_neutral_entry_orders_isolated` — real delta-neutral entry
- **Python E2E**: `python/tests/test_backtest_e2e.py` (6+ 0.5.0 new tests)
  - `spot_instrument_factory` / `swap_instrument_factory`
  - `begin_bar` per-instrument independent seeding
  - `set_and_get_target_position` cross-leg isolation
  - `two_legs_isolated_positions` — spot long + perp short = delta neutral
  - `leg_targets_persist` in `RunResult`
