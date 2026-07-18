# Spot + Perp Two-Leg Backtest Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `Instrument` (Spot/Swap) abstraction and `set_target_position` leg API to `axon-backtest` so strategies can backtest spot+perp delta-neutral arbitrage (e.g. funding-rate arbitrage with two legs).

**Architecture:** Extend `BacktestEngine` in place. Replace `Order.symbol: Symbol` with `Order.instrument: Instrument`. Convert `L1MatchingEngine` from single-book to per-instrument book routing via `HashMap<Instrument, L1Book>`. Position state keyed by `Instrument`. New `MarkEvent` plumbed through but not acted on (placeholder for future funding).

**Tech Stack:** Rust 1.96, `axon-core`, `axon-backtest`, PyO3, `serde`, `clippy -D warnings`.

**Branch:** `0.5.0` (newly created, target version 0.5.0 semver breaking)
**Spec:** `docs/superpowers/specs/2026-07-17-spot-perp-two-leg-backtest-design.md`

**Spec → Plan Mapping:**

| Spec Section | Plan Tasks |
|---|---|
| §4.1 `Instrument` enum | T1.1, T1.2 |
| §4.4 `MarkEvent` + `Event::Mark` | T1.3, T1.4 |
| §4.2 `Order` 改造 | T2.1, T2.2, T2.3 |
| §4.6 `TradeRecord` 扩展 | T2.4 |
| §5.1 `L1MatchingEngine` 多 book | T3.1, T3.2, T3.3, T3.4 |
| §5.2-5.3 `BacktestEngine` key 改 Instrument | T3.5, T3.6, T3.7 |
| §5.4 `set_target_position` API | T3.8, T3.9 |
| §5.5 `RunResult` 扩展 | T3.10 |
| §6.2 Python 协议 | T4.1, T4.2, T4.3 |
| §7.1 单元测试 (per task 同步) | 见各 Task |
| §7.2 集成测试 | T4.4 |
| §7.3 Python 端测试 | T4.5 |
| §8.5 文档 | T5.1, T5.2 |
| §8.6 版本对齐 | T6.1, T6.2, T6.3 |

**Conventions used in this plan:**
- All `cargo` commands run from repo root unless noted.
- All tests use `cargo test -p <crate>` and must pass with `cargo clippy --workspace --all-targets -- -D warnings`.
- Commits use Conventional Commits; English commit messages per workspace rule.
- All Python examples use the existing `from axon_quant.backtest import ...` style.

---

## File Structure

**New files:**
- `crates/axon-core/src/types/instrument.rs` — `Instrument` enum + sub-structs
- `crates/axon-integration-tests/src/delta_neutral_arb.rs` — integration test
- `python/tests/test_backtest_multi_leg.py` — Python end-to-end test
- `docs/superpowers/plans/2026-07-17-spot-perp-two-leg-backtest.md` — this plan

**Modified files:**
- `crates/axon-core/src/types/mod.rs` — export `Instrument`
- `crates/axon-core/src/event.rs` — add `MarkEvent`, `Event::Mark`
- `crates/axon-core/src/order/core.rs` — `Order.instrument: Instrument`, replace `Order::new` with `Order::spot` / `Order::swap`
- `crates/axon-core/src/portfolio/trade_record.rs` — add `instrument` field
- `crates/axon-backtest/src/matching/engine.rs` — refactor L1 to per-instrument books
- `crates/axon-backtest/src/matching/types.rs` — `MatchFill` unchanged
- `crates/axon-backtest/src/engine.rs` — `position_states: HashMap<Instrument, _>`, add leg API, handle `Event::Mark`
- `crates/axon-backtest/src/python/*.rs` — update `OrderDict` protocol, add PyO3 methods
- `python/axon_quant/backtest.py` — add `spot_instrument` / `swap_instrument` factories
- `CHANGELOG.md` — 0.5.0 BREAKING entry
- `docs/zh/reference/backtest.md` — multi-leg section
- `docs/en/reference/backtest.md` — multi-leg section
- `Cargo.toml` — version bump 0.4.1 → 0.5.0
- `pyproject.toml` — version bump 0.4.1 → 0.5.0

**Unchanged files (rationale):**
- `crates/axon-backtest/src/matching/types.rs` — `MatchFill` keeps 56 bytes Copy; `instrument` passed separately to `apply_fill`
- `crates/axon-core/src/market/trade.rs` — `Trade` keeps 40-byte `#[repr(C)]` contract
- `crates/axon-core/src/types/symbol.rs` — `Symbol` unchanged

---

## Phase 1: 类型 + 序列化 (无破坏)

### Task 1.1: Add `Instrument` enum file

**Files:**
- Create: `crates/axon-core/src/types/instrument.rs`
- Modify: `crates/axon-core/src/types/mod.rs`

- [ ] **Step 1: Create instrument.rs with enum and structs**

Create `crates/axon-core/src/types/instrument.rs`:

```rust
//! 交易品种抽象(Spot / Swap)
//!
//! 区分 spot(现货)与 swap(永续合约),为 spot+perp 双 leg 套利提供
//! 类型安全基础。`Instrument` 是策略与撮合引擎之间共同语言,无歧义地
//! 标识"在哪个品种上交易"。
//!
//! 序列化用 `tag = "kind"` 模式,Python 端 dict 协议简洁:
//! ```json
//! {"kind": "spot",  "details": {"base": "BTC", "quote": "USDT"}}
//! {"kind": "swap",  "details": {"base": "BTC", "quote": "USDT",
//!                               "settle": "UsdMargin", "contract_size": 1.0}}
//! ```

use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

use super::Symbol;

/// 交易品种
///
/// `Clone` 而非 `Copy`:因为 `SpotInstrument` / `SwapInstrument` 内含
/// `Symbol(String)`,是堆分配。详见 spec §4.1.
///
/// `Hash` / `Eq` 手动实现:`SwapInstrument.contract_size: f64` 不可派生
/// `Hash` / `Eq`(`f64` 含 NaN,无法满足 `Eq` 律)。我们对 `f64` 用
/// `to_bits()` 转成 `u64` 后再比较和 hash,语义上"位级相等即相等",
/// NaN 与 NaN 比较也会相等(因为位相同),这在 HashMap key 场景下
/// 是合理选择(不期望不同 NaN 表示"不同的 instrument")。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "details", rename_all = "lowercase")]
pub enum Instrument {
    /// 现货
    Spot(SpotInstrument),
    /// 永续合约
    Swap(SwapInstrument),
}

/// 现货交易品种
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpotInstrument {
    /// 基础币种(如 `BTC` 表示一个 `BTC`)
    pub base: Symbol,
    /// 计价币种(如 `USDT` 表示价格以 USDT 计价)
    pub quote: Symbol,
}

/// 永续合约交易品种
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwapInstrument {
    /// 基础币种(如 `BTC`)
    pub base: Symbol,
    /// 计价币种(如 `USDT`)
    pub quote: Symbol,
    /// 结算方式(USD 保证金 / 币本位)
    pub settle: SwapSettle,
    /// 合约乘数(每张合约代表多少基础币种)
    pub contract_size: f64,
}

/// 永续合约结算方式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SwapSettle {
    /// USD 保证金合约(quote 币种作为保证金)
    UsdMargin,
    /// 币本位合约(base 币种作为保证金)
    CoinMargin,
}

impl Eq for SwapInstrument {}

impl Hash for SwapInstrument {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.base.hash(state);
        self.quote.hash(state);
        self.settle.hash(state);
        self.contract_size.to_bits().hash(state);
    }
}

// 手动实现 `Instrument` 的 `Hash`:同 `SwapInstrument` 的考量 ——
// 直接 derive `Hash` 在含有 `f64` 变体时无法编译,而我们已经为
// `SwapInstrument` 提供了位级 Hash,因此 enum 也需要手动实现以保持一致。
impl Hash for Instrument {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // 先用变体判别符区分,再委托给各变体的 Hash 实现
        match self {
            Instrument::Spot(s) => {
                0u8.hash(state);
                s.hash(state);
            }
            Instrument::Swap(s) => {
                1u8.hash(state);
                s.hash(state);
            }
        }
    }
}

impl Instrument {
    /// 基础币种
    pub fn base(&self) -> &Symbol {
        match self {
            Instrument::Spot(s) => &s.base,
            Instrument::Swap(s) => &s.base,
        }
    }

    /// 计价币种
    pub fn quote(&self) -> &Symbol {
        match self {
            Instrument::Spot(s) => &s.quote,
            Instrument::Swap(s) => &s.quote,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_spot_instrument_creation() {
        let inst = Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        });
        assert_eq!(inst.base().as_str(), "BTC");
        assert_eq!(inst.quote().as_str(), "USDT");
    }

    #[test]
    fn test_swap_instrument_creation() {
        let inst = Instrument::Swap(SwapInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
            settle: SwapSettle::UsdMargin,
            contract_size: 1.0,
        });
        assert_eq!(inst.base().as_str(), "BTC");
        assert_eq!(inst.quote().as_str(), "USDT");
    }

    #[test]
    fn test_instrument_equality_and_hash() {
        let a = Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        });
        let b = a.clone();
        let c = Instrument::Spot(SpotInstrument {
            base: Symbol::from("ETH"),
            quote: Symbol::from("USDT"),
        });
        assert_eq!(a, b);
        assert_ne!(a, c);
        let mut set = HashSet::new();
        set.insert(a);
        set.insert(b);
        set.insert(c);
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn test_swap_instrument_hash_via_bits() {
        // contract_size = 1.0 和 1.0(位相同)应当相等
        let a = Instrument::Swap(SwapInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
            settle: SwapSettle::UsdMargin,
            contract_size: 1.0,
        });
        let b = Instrument::Swap(SwapInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
            settle: SwapSettle::UsdMargin,
            contract_size: 1.0,
        });
        assert_eq!(a, b);
        let mut set = HashSet::new();
        set.insert(a);
        set.insert(b);
        assert_eq!(set.len(), 1, "相同 contract_size bits 应 hash 到同一 slot");
    }

    #[test]
    fn test_instrument_serde_json_spot() {
        let inst = Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        });
        let json = serde_json::to_string(&inst).unwrap();
        assert!(json.contains("\"kind\":\"spot\""));
        let parsed: Instrument = serde_json::from_str(&json).unwrap();
        assert_eq!(inst, parsed);
    }

    #[test]
    fn test_instrument_serde_json_swap() {
        let inst = Instrument::Swap(SwapInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
            settle: SwapSettle::UsdMargin,
            contract_size: 1.0,
        });
        let json = serde_json::to_string(&inst).unwrap();
        assert!(json.contains("\"kind\":\"swap\""));
        assert!(json.contains("\"settle\":\"UsdMargin\""));
        let parsed: Instrument = serde_json::from_str(&json).unwrap();
        assert_eq!(inst, parsed);
    }
}
```

- [ ] **Step 2: Wire it into mod.rs**

Edit `crates/axon-core/src/types/mod.rs`:

```rust
//! 通用数据类型（Price、Quantity、Symbol、Instrument）

pub mod instrument;
pub mod price;
pub mod quantity;
pub mod symbol;

pub use instrument::{Instrument, SpotInstrument, SwapInstrument, SwapSettle};
pub use price::Price;
pub use quantity::Quantity;
pub use symbol::Symbol;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p axon-core instrument::`
Expected: 6 tests pass.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy -p axon-core --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/axon-core/src/types/instrument.rs crates/axon-core/src/types/mod.rs
git commit -m "feat(core): add Instrument enum (Spot/Swap) for type-safe multi-instrument routing"
```

---

### Task 1.2: Verify Instrument compiles in workspace

**Files:** (no changes — just verification)

- [ ] **Step 1: Build entire workspace**

Run: `cargo build --workspace`
Expected: compiles, all crates OK. (No consumer of `Instrument` yet, so should be a no-op.)

- [ ] **Step 2: Verify with clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

---

### Task 1.3: Add `MarkEvent` to `axon-core::event`

**Files:**
- Create: `crates/axon-core/src/event/mark.rs` (new file, parallel to `fill.rs` / `system.rs`)
- Modify: `crates/axon-core/src/event/mod.rs` (register `mark` module + re-export)
- Modify: `crates/axon-core/src/event/types.rs` (add `Mark` variant to `Event` enum + `MARK` bit to `EventType`)
- Modify: `crates/axon-core/src/lib.rs` (re-export `MarkEvent`)

> **Path correction vs. original plan**: the original plan said `event.rs`, but
> `axon-core`'s event module is actually a directory `event/` with submodules
> (`fill.rs`, `order.rs`, `system.rs`, ...). `Event` enum lives in `event/types.rs`.
> Use the actual file structure.

- [x] **Step 1: Locate existing event definitions**

Run: `grep -rn "FillEvent\|pub enum Event" crates/axon-core/src/event/ | head -20`
Read surrounding context to understand where to add `MarkEvent`.

- [x] **Step 2: Create `event/mark.rs` with `MarkEvent` struct**

Create `crates/axon-core/src/event/mark.rs` (follows same pattern as `fill.rs` / `system.rs`):

```rust
//! Mark price 事件(标记价格更新)
//!
//! 由外部数据源推入,引擎在 `dispatch` 时写入 `mark_cache`,
//! 供未来 funding 结算 / unrealized PnL 计算使用。
//!
//! 本次 spec 范围:仅写缓存,不触 NAV 重采样。
//! 详见 spec §4.4。

use serde::{Deserialize, Serialize};

use crate::time::Timestamp;
use crate::types::{Instrument, Price};

/// Mark price 事件(标记价格更新)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkEvent {
    /// 品种
    pub instrument: Instrument,
    /// 标记价格
    pub mark_price: Price,
    /// 时间戳
    pub timestamp: Timestamp,
}

impl MarkEvent {
    /// 创建 Mark 事件
    pub fn new(instrument: Instrument, mark_price: Price, timestamp: Timestamp) -> Self {
        Self { instrument, mark_price, timestamp }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{SpotInstrument, SwapInstrument, SwapSettle, Symbol};

    #[test]
    fn test_mark_event_creation() {
        let evt = MarkEvent::new(
            Instrument::Spot(SpotInstrument {
                base: Symbol::from("BTC"),
                quote: Symbol::from("USDT"),
            }),
            Price::from_f64(50_000.0),
            Timestamp::from_nanos(1_700_000_000_000_000_000),
        );
        assert_eq!(evt.mark_price.as_f64(), 50_000.0);
    }

    #[test]
    fn test_mark_event_serde() {
        let evt = MarkEvent::new(
            Instrument::Swap(SwapInstrument {
                base: Symbol::from("ETH"),
                quote: Symbol::from("USDT"),
                settle: SwapSettle::UsdMargin,
                contract_size: 1.0,
            }),
            Price::from_f64(3_000.0),
            Timestamp::from_nanos(0),
        );
        let json = serde_json::to_string(&evt).unwrap();
        let parsed: MarkEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(evt, parsed);
    }
}
```

> **Note**: Added `Eq` to derive (the parent `Event` enum derives `Eq`).

- [x] **Step 3: Add `Mark` variant to `Event` enum in `event/types.rs`**

`pub enum Event` already has `#[non_exhaustive]`, so the new variant is non-breaking.
The `EventType` bit mask gets a new `MARK` bit (`0b10000`, slot 5) — leaves the
existing 4 bits intact so all previous bit-pattern callers keep working.
`EventType::ALL` becomes `0b11111`.

Also updated:
- `Event::timestamp` / `Event::seq` / `Event::event_type` to include the new arm.
  For `seq()`, `MarkEvent` returns `0` since it carries no seq (externally sourced).
- `Display` impls for both `Event` and `EventType` to print `MARK` and the new variant.
  `Instrument` is printed via `{:?}` (no `Display` impl exists for it).
- `EventType` test strings (`ALL` display + `ALL.bits()`).

- [x] **Step 4: Re-export in `event/mod.rs` and `lib.rs`**

`event/mod.rs`:
- `pub mod mark;`
- `pub use mark::MarkEvent;`
- Module-level doc list adds `[`mark`]：标记价格事件`.

`lib.rs` (root):
- `MarkEvent` added to the `pub use event::{...}` line.

- [x] **Step 5: Run tests**

Run: `cargo test -p axon-core mark`
Expected: tests pass (2 in `event::mark::tests`, plus 2 new ones in
`event::types::tests` covering the new variant & bit).

- [x] **Step 6: Run clippy**

Run: `cargo clippy -p axon-core --all-targets -- -D warnings`
Expected: no warnings.

- [x] **Step 7: Commit**

```bash
git add crates/axon-core/src/event/mark.rs crates/axon-core/src/event/mod.rs         crates/axon-core/src/event/types.rs crates/axon-core/src/lib.rs
git commit -m "feat(core): add MarkEvent and Event::Mark variant for future mark-price plumbing"
```

Commit hash: `2a32825` (this repo).

---

### Task 1.4: Verify Phase 1 builds workspace-wide

**Files:** (verification only)

- [ ] **Step 1: Build & test workspace**

Run: `cargo build --workspace && cargo test -p axon-core`
Expected: all green.

- [ ] **Step 2: Verify clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

---

## Phase 2: Order 改造 (breaking)

### Task 2.1: Refactor `Order` to carry `Instrument` (failing test first)

**Files:**
- Modify: `crates/axon-core/src/order/core.rs`

- [x] **Step 1: Write the failing test**

In `crates/axon-core/src/order/core.rs` `tests` module, add:

```rust
#[test]
fn test_order_spot_creation() {
    let order = Order::spot(
        100,
        "BTC",
        "USDT",
        Side::Buy,
        OrderType::Limit { price: Price::from_f64(100.0) },
        Quantity::from_f64(1.0),
        TimeInForce::GTC,
    );
    assert_eq!(order.id, 100);
    assert!(matches!(order.instrument, Instrument::Spot(_)));
    assert_eq!(order.side, Side::Buy);
    assert_eq!(order.filled_quantity, Quantity::default());
}

#[test]
fn test_order_swap_creation() {
    let order = Order::swap(
        101,
        "ETH",
        "USDT",
        SwapSettle::CoinMargin,
        0.01,
        Side::Sell,
        OrderType::Market,
        Quantity::from_f64(10.0),
        TimeInForce::IOC,
    );
    assert!(matches!(order.instrument, Instrument::Swap(_)));
    if let Instrument::Swap(s) = &order.instrument {
        assert_eq!(s.contract_size, 0.01);
        assert_eq!(s.settle, SwapSettle::CoinMargin);
    }
}
```

- [x] **Step 2: Run test, verify it fails**

Run: `cargo test -p axon-core order::core::test_order_spot_creation`
Expected: compile error (no `Order::spot`).

- [x] **Step 3: Modify `Order` struct definition**

Edit `crates/axon-core/src/order/core.rs`:

Replace the `pub struct Order` block:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Order {
    /// 订单 ID
    pub id: OrderId,
    /// 交易品种(spot/swap)
    pub instrument: Instrument,   // 改: 原 symbol: Symbol
    /// 买卖方向
    pub side: Side,
    /// 订单类型
    pub order_type: OrderType,
    /// 订单总数量
    pub quantity: Quantity,
    /// 已成交数量
    pub filled_quantity: Quantity,
    /// 有效期
    pub time_in_force: TimeInForce,
    /// 当前状态
    pub status: OrderStatus,
    /// 创建时间
    pub created_at: Timestamp,
    /// 最近更新时间
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<Timestamp>,
    /// 拒绝原因
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reject_reason: Option<RejectReason>,
    /// 用户自定义标签
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_order_id: Option<String>,
}
```

Update the `use` block at top of file to import `Instrument`:

```rust
use crate::types::{Instrument, Quantity, SpotInstrument, SwapInstrument, SwapSettle};
```

- [x] **Step 4: Replace `Order::new` with `Order::spot` / `Order::swap`**

Replace the `Order::new` method block with:

```rust
impl Order {
    /// 构造现货订单(替代旧 `Order::new`)
    pub fn spot(
        id: OrderId,
        base: impl Into<Symbol>,
        quote: impl Into<Symbol>,
        side: Side,
        order_type: OrderType,
        quantity: Quantity,
        time_in_force: TimeInForce,
    ) -> Self {
        Self {
            id,
            instrument: Instrument::Spot(SpotInstrument {
                base: base.into(),
                quote: quote.into(),
            }),
            side,
            order_type,
            quantity,
            filled_quantity: Quantity::default(),
            time_in_force,
            status: OrderStatus::Created,
            created_at: Timestamp::now(),
            updated_at: None,
            reject_reason: None,
            client_order_id: None,
        }
    }

    /// 构造永续订单
    pub fn swap(
        id: OrderId,
        base: impl Into<Symbol>,
        quote: impl Into<Symbol>,
        settle: SwapSettle,
        contract_size: f64,
        side: Side,
        order_type: OrderType,
        quantity: Quantity,
        time_in_force: TimeInForce,
    ) -> Self {
        Self {
            id,
            instrument: Instrument::Swap(SwapInstrument {
                base: base.into(),
                quote: quote.into(),
                settle,
                contract_size,
            }),
            side,
            order_type,
            quantity,
            filled_quantity: Quantity::default(),
            time_in_force,
            status: OrderStatus::Created,
            created_at: Timestamp::now(),
            updated_at: None,
            reject_reason: None,
            client_order_id: None,
        }
    }

    // ... existing methods (`remaining_quantity`, `is_filled`, `apply_fill`, `cancel`, `reject`, `activate`) unchanged ...
}
```

- [x] **Step 5: Fix existing tests in `core.rs` to use new constructors**

In the existing tests module, replace every `Order::new(id, symbol_str, side, order_type, qty, tif)` with `Order::spot(id, "BTC", "USDT", side, order_type, qty, tif)` (or similar base/quote split). There are about 15+ test sites; use sed or manual edits.

Example transformations:
- `Order::new(1, Symbol::from("BTC-USDT"), Side::Buy, OrderType::Limit { ... }, ...)` → `Order::spot(1, "BTC", "USDT", Side::Buy, OrderType::Limit { ... }, ...)`
- `Order::new(2, Symbol::from("ETH-USDT"), Side::Sell, OrderType::Market, ...)` → `Order::spot(2, "ETH", "USDT", Side::Sell, OrderType::Market, ...)`

(Adjust base/quote split based on the test's symbol name. `"BTC-USDT"` → `("BTC", "USDT")`. `"ETH-USDT"` → `("ETH", "USDT")`. Etc.)

- [x] **Step 6: Run all `axon-core` tests**

Run: `cargo test -p axon-core`
Expected: all tests pass, including the new `test_order_spot_creation` and `test_order_swap_creation`.

**Actual result:** `795 passed; 0 failed; 0 ignored` (47 order-related tests including the 2 new ones). All previous tests still pass under the new `Order::spot` factory.

- [x] **Step 7: Build workspace (expect downstream breakage)**

Run: `cargo build --workspace 2>&1 | head -100`
Expected: `axon-backtest` (and possibly others) fail with "no field `symbol` on `Order`" errors. This is expected — Task 2.2 onward fixes them.

**Actual downstream breakage (as expected):**
- `axon-risk` (lib): 2 errors — `no field 'symbol' on type '&axon_core::Order'` at `crates/axon-risk/src/checks/position.rs:16, 28`
- `axon-backtest` (lib): 15 errors — mix of `no field 'symbol'` and `no associated function 'new'`

These will be resolved in T2.2 (engine) and T2.3 (matching), plus `axon-risk` migration. No further breakage found.

- [x] **Step 8: Commit `Order` change only (downstream broken OK)**

```bash
git add crates/axon-core/src/order/core.rs
git commit -m "feat(core): Order carries Instrument, breaking — replaces Order::new with Order::spot/swap

This is a semver breaking change. Downstream crates using Order::new
will fail to compile until migrated. Follow-up commits will fix them."
```

**Commit hash:** `ffeda88`

**Files in commit (3):**
- `crates/axon-core/src/order/core.rs` — main refactor (struct + 2 factories + migrated tests)
- `crates/axon-core/src/event/order.rs` — same migration in `axon-core`'s internal test
- `benches/core_bench.rs` — top-level Criterion bench for axon-core needed the same migration to keep `cargo clippy -p axon-core --all-targets` green; required because `--all-targets` includes benches

**Deviations from plan:**
- `Order::swap` carries 9 parameters, exceeding clippy's `too_many_arguments` (7) threshold. Added `#[allow(clippy::too_many_arguments)]` on the method. This is a deliberate trade-off: introducing a `SwapOrderParams` builder struct would obscure the simple factory call pattern that downstream tests need.
- A second test file (`event/order.rs`) inside `axon-core` had to be migrated in the same commit to keep `--all-targets` green for clippy. The plan only mentioned `core.rs` tests; the actual scope of "axon-core self-consistent" is wider.
- Removed unused `Symbol` import from `benches/core_bench.rs` (was using `Symbol::from("BTC-USDT")` which is no longer needed).

---

### Task 2.2: Migrate `axon-backtest::engine` to `Order::spot`

**Files:**
- Modify: `crates/axon-backtest/src/engine.rs`

- [ ] **Step 1: Find all `Order::new` call sites in `axon-backtest`**

Run: `grep -rn "Order::new\|order.symbol" crates/axon-backtest/src/`
Expected: ~30+ sites in `engine.rs` and `matching/engine.rs`.

- [ ] **Step 2: Fix `apply_fill` signature and `handle_submit`**

In `crates/axon-backtest/src/engine.rs`:

a) Change `handle_submit` to pass `order.instrument` (not `symbol`):

```rust
fn handle_submit(&mut self, order: Order) {
    let instrument = order.instrument.clone();   // 改: 原 symbol.clone()
    let side = order.side;
    let active_before = self.config.matching_engine.active_order_count();
    let result = self.config.matching_engine.submit(order);
    let active_after = self.config.matching_engine.active_order_count();
    let added_to_book = active_after > active_before;

    match (result.fills.is_empty(), added_to_book) {
        (false, _) => {
            self.stats.orders_accepted += 1;
            self.stats.fills += result.fills.len() as u64;
            for fill in &result.fills {
                let pnl_delta = fill_pnl_delta(fill);
                self.stats.total_pnl += pnl_delta;
                if self.stats.total_pnl > self.stats.pnl_peak {
                    self.stats.pnl_peak = self.stats.total_pnl;
                }
                self.apply_fill(&instrument, side, fill);  // 改: 传 &Instrument
            }
        }
        (true, true) => { self.stats.orders_accepted += 1; }
        (true, false) => { self.stats.orders_rejected += 1; }
    }
}
```

b) Change `apply_fill` signature and key:

```rust
fn apply_fill(
    &mut self,
    instrument: &Instrument,    // 改: 原 symbol: &str
    side: Side,
    fill: &crate::matching::MatchFill,
) {
    // ... existing logic ...
    let pos = self.bt_state
        .position_states
        .entry(instrument.clone())    // 改: 原 entry(symbol.to_string())
        .or_default();
    let p = pos.quantity;
    let n = signed_qty;
    // ... rest of 6-state machine unchanged ...
}
```

- [ ] **Step 3: Fix `liquidate_eod`**

Change `to_liquidate` to use `Instrument`:

```rust
fn liquidate_eod(&mut self) {
    let to_liquidate: Vec<(Instrument, f64)> = self.bt_state
        .position_states
        .iter()
        .filter(|(_, p)| p.quantity.abs() > 1e-9)
        .map(|(inst, p)| (*inst, p.quantity))    // 改: 原 (sym.clone(), p.quantity)
        .collect();
    if to_liquidate.is_empty() {
        return;
    }
    for (idx, (instrument, qty)) in to_liquidate.into_iter().enumerate() {
        let side = if qty > 0.0 { Side::Sell } else { Side::Buy };
        let close_qty = qty.abs();
        let order = Order::spot(    // 改: 原 Order::new
            EOD_LIQUIDATE_ID_BASE + idx as u64,
            "BTC",    // ⚠️ 占位,Phase 3 后用真实 base/quote
            "USDT",
            side,
            OrderType::Market,
            Quantity::from_f64(close_qty),
            TimeInForce::IOC,
        );
        let result = self.config.matching_engine.submit(order);
        for fill in &result.fills {
            self.stats.orders_accepted += 1;
            self.stats.fills += 1;
            self.stats.total_pnl += fill_pnl_delta(fill);
            if self.stats.total_pnl > self.stats.pnl_peak {
                self.stats.pnl_peak = self.stats.total_pnl;
            }
            self.apply_fill(&instrument, side, fill);    // 改: 传 &Instrument
        }
    }
}
```

> ⚠️ **TODO in code**: The `"BTC"` / `"USDT"` placeholders above are wrong — `liquidate_eod` doesn't know base/quote. **Phase 3 will refactor this**: instead of constructing `Order::spot("BTC", "USDT", ...)` blindly, the new approach is to **reconstruct an `Order` from the existing `position_state` or from the original `instrument` already stored in `position_states`**. For now, use the placeholder and add a `#[allow(unused)]` if compile fails. Phase 3 Task 3.7 fixes this properly.

**Better approach for now**: Track base/quote by storing in `position_states` key, and reconstruct via `Order::spot(instrument.base().clone(), instrument.quote().clone(), ...)`:

```rust
let (base, quote) = match &instrument {
    Instrument::Spot(s) => (s.base.clone(), s.quote.clone()),
    Instrument::Swap(_) => continue, // skip swap for now (EOD 暂不处理 swap)
};
let order = Order::spot(
    EOD_LIQUIDATE_ID_BASE + idx as u64,
    base, quote, side, OrderType::Market,
    Quantity::from_f64(close_qty), TimeInForce::IOC,
);
```

This way EOD only handles spot. Perp liquidation is future work.

- [ ] **Step 4: Fix all test sites in `engine.rs`**

Replace every test's `Order::new(...)` and `make_limit_order` helper with `Order::spot(...)`. The helper becomes:

```rust
fn make_limit_order(id: u64, side: Side, price: f64, qty: f64) -> Order {
    Order::spot(
        id, "BTC", "USDT", side,
        OrderType::Limit { price: Price::from_f64(price) },
        Quantity::from_f64(qty), TimeInForce::GTC,
    )
}
```

(About 8+ test sites in `engine.rs`.)

- [ ] **Step 5: Fix imports**

Add to imports at top of `engine.rs`:

```rust
use axon_core::types::Instrument;
```

- [ ] **Step 6: Run axon-backtest tests**

Run: `cargo test -p axon-backtest`
Expected: existing tests pass (after migration). If they don't, the position-states key change is incomplete — see Task 3.5.

- [ ] **Step 7: Commit**

```bash
git add crates/axon-backtest/src/engine.rs
git commit -m "refactor(backtest): migrate engine to Order::spot/Instrument key (transitional)

Position states still keyed by Symbol hash of (base,quote) — full
multi-instrument routing comes in Task 3.5."
```

> **Note**: At this point tests may STILL fail because position_states key is still String. Task 3.5 fixes it. If tests fail with "key type mismatch", continue to Task 3.5 immediately.

---

### Task 2.3: Migrate `L1MatchingEngine::seed_liquidity` and tests

**Files:**
- Modify: `crates/axon-backtest/src/matching/engine.rs`

- [ ] **Step 1: Update `MatchingEngine` trait `seed_liquidity` signature**

Change `_symbol: Symbol` → `_instrument: Instrument` in trait default impl.

```rust
fn seed_liquidity(
    &mut self,
    _mid_price: f64,
    _half_spread: f64,
    _depth_levels: usize,
    _size_per_level: f64,
    _instrument: Instrument,    // 改: 原 _symbol: Symbol
    next_id: u64,
) -> u64 {
    next_id
}
```

- [ ] **Step 2: Update `L1MatchingEngine::seed_liquidity` impl signature**

```rust
pub fn seed_liquidity(
    &mut self,
    mid_price: f64,
    half_spread: f64,
    depth_levels: usize,
    size_per_level: f64,
    instrument: Instrument,    // 改: 原 symbol: Symbol
    next_id: u64,
) -> u64 {
    if mid_price <= 0.0 || half_spread <= 0.0 || depth_levels == 0 || size_per_level <= 0.0 {
        return next_id;
    }
    let mut id = next_id;
    for level in 1..=depth_levels {
        let ask_price = mid_price + half_spread * level as f64;
        if ask_price <= 0.0 { continue; }
        let order = Order::spot(    // 改: 原 Order::new
            id, "BTC", "USDT",    // ⚠️ placeholder, Phase 3 修复
            Side::Sell,
            OrderType::Limit { price: Price::from_f64(ask_price) },
            Quantity::from_f64(size_per_level),
            TimeInForce::GTC,
        );
        // ... rest unchanged ...
    }
    // ... similar for bids ...
    id
}
```

> ⚠️ Same `("BTC", "USDT")` placeholder issue. **Phase 3 Task 3.2** will fix this by routing to the correct book based on `instrument`.

- [ ] **Step 3: Update all test sites in `matching/engine.rs`**

Replace `Order::new(...)` with `Order::spot(id, "BTC", "USDT", ...)`.

(About 20+ test sites in this file.)

- [ ] **Step 4: Run all axon-backtest tests**

Run: `cargo test -p axon-backtest`
Expected: PASS (with placeholders still in seed_liquidity; will be cleaned in Phase 3).

- [ ] **Step 5: Commit**

```bash
git add crates/axon-backtest/src/matching/engine.rs
git commit -m "refactor(backtest): migrate L1 matching to Order::spot, Instrument in seed_liquidity"
```

---

### Task 2.4: Add `instrument` field to `TradeRecord`

**Files:**
- Modify: `crates/axon-core/src/portfolio/trade_record.rs`

- [ ] **Step 1: Add `instrument` field**

Edit `crates/axon-core/src/portfolio/trade_record.rs`:

```rust
use crate::types::Instrument;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradeRecord {
    pub trade: Trade,
    pub realized_pnl: i64,
    pub commission: i64,
    pub net_quantity: i64,
    pub instrument: Instrument,    // 新增
}

impl TradeRecord {
    pub fn new(
        trade: Trade,
        realized_pnl: i64,
        commission: i64,
        net_quantity: i64,
        instrument: Instrument,    // 新增
    ) -> Self {
        Self {
            trade,
            realized_pnl,
            commission,
            net_quantity,
            instrument,
        }
    }
}
```

- [ ] **Step 2: Update existing test**

In test module, update:

```rust
#[test]
fn test_trade_record_creation() {
    let trade = Trade::new(
        Timestamp::from_nanos(1_000),
        Price::from_f64(100.0),
        Quantity::from_f64(1.0),
        1, 2,
    );
    let inst = Instrument::Spot(SpotInstrument {
        base: Symbol::from("BTC"),
        quote: Symbol::from("USDT"),
    });
    let rec = TradeRecord::new(trade, 1_000_000, 100_000, 1_000_000, inst);
    assert_eq!(rec.realized_pnl, 1_000_000);
    assert_eq!(rec.commission, 100_000);
    assert!(matches!(rec.instrument, Instrument::Spot(_)));
}
```

Add imports at top of tests:
```rust
use crate::types::{Instrument, SpotInstrument};
```

- [ ] **Step 3: Update `axon-backtest` engine to pass instrument**

In `crates/axon-backtest/src/engine.rs`, every `TradeRecord::new(...)` call site must add the `instrument` argument. There are 3 such sites (in apply_fill 6-state machine branches). Use the `&Instrument` parameter from `apply_fill` signature.

Pattern (for each of the 3 sites):

```rust
self.bt_state.trades.push(TradeRecord::new(
    trade,
    (pnl * 1e6) as i64,
    (fee * 1e6) as i64,
    (n * 1e6) as i64,
    instrument.clone(),    // 新增
));
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p axon-core -p axon-backtest`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/axon-core/src/portfolio/trade_record.rs crates/axon-backtest/src/engine.rs
git commit -m "feat(backtest): TradeRecord carries instrument"
```

---

## Phase 3: 引擎 + matching 升级 (核心)

### Task 3.1: Define `L1Book` struct (extract from `L1MatchingEngine`)

**Files:**
- Modify: `crates/axon-backtest/src/matching/engine.rs`

- [x] **Step 1: Add `L1Book` struct above `L1MatchingEngine`**

> **完成状态(e58a253)**:L1Book 已抽到 `crates/axon-backtest/src/matching/engine.rs`,
> 含 `new` / `clear` / `active_order_count` / `best_bid` / `best_ask` / `insert_passive` 方法。
> L1Book 持有 bids/asks/order_index,`#[derive(Debug, Default)]`。

In `crates/axon-backtest/src/matching/engine.rs`, add (before `pub struct L1MatchingEngine`):

```rust
/// 单品种的订单簿(bids/asks/index)
///
/// 把 L1 撮合引擎的内部分量抽出来,使 L1MatchingEngine 可以持有
/// `HashMap<Instrument, L1Book>`,实现多品种路由。
#[derive(Debug, Default)]
pub struct L1Book {
    /// 买单簿(BTreeMap 升序,最优买价在末尾)
    pub bids: OrderBookSide,
    /// 卖单簿(BTreeMap 升序,最优卖价在开头)
    pub asks: OrderBookSide,
    /// 活跃订单索引:`order_id -> (side, price)` 快速定位
    pub order_index: HashMap<u64, (Side, Price)>,
}

impl L1Book {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.bids.clear();
        self.asks.clear();
        self.order_index.clear();
    }

    pub fn active_order_count(&self) -> usize {
        self.order_index.len()
    }

    pub fn best_bid(&self) -> Option<Price> {
        self.bids.keys().next_back().copied()
    }

    pub fn best_ask(&self) -> Option<Price> {
        self.asks.keys().next().copied()
    }
}
```

- [x] **Step 2: Run tests, expect compile errors**

> **完成状态**:L1MatchingEngine 已持有 `books: HashMap<Instrument, L1Book>`,
> `seed_liquidity` 按 `instrument` 路由,`submit` 按 `order.instrument` 路由,
> `match_against_asks` / `match_against_bids` 迁为 L1Book 关联函数接收
> `trade_sequence: &AtomicU64`,彻底解决 `&mut self` 同时借用 `self.books`
> 和 `self.trade_sequence` 的 borrow-check 冲突。
>
> L1 / L2 保留 `with_symbol(Symbol)` 为 no-op(参数被忽略),供 axon-llm、
> Python `__init__(symbol=...)`、fuzz 测试等历史调用方使用。
>
> `test_engine_with_symbol` 已替换为 `test_engine_multi_book_routing`
> (覆盖 BTC/ETH 路由隔离 + best_bid_for/ask_for 路由)。

---

### Task 3.2: Refactor `L1MatchingEngine` to hold `HashMap<Instrument, L1Book>`

> **完成状态(e58a253)**:本任务所有 step 已完成。L1MatchingEngine 现在持
> 有 `books: HashMap<Instrument, L1Book>`,所有路由走 `order.instrument`。
> 关键设计点:
> - `match_against_asks` / `match_against_bids` 迁为 `L1Book` 关联函数,
>   接收 `trade_sequence: &AtomicU64`,根除 `&mut self` 同时借用
>   `self.books` 和 `self.trade_sequence` 的 borrow-check 冲突。
> - `L1 / L2::with_symbol(Symbol)` 保留为 no-op(参数被忽略)以兼容
>   axon-llm、Python `__init__(symbol=...)`、fuzz 测试。
> - `clear_book` 遍历 `self.books.values_mut()` 调 `L1Book::clear`,
>   books 容器保留(下次 seed 不用重新枚举 instrument)。
> - `best_bid` / `best_ask` / `depth` / `active_order_count` 跨所有 book
>   聚合;`best_bid_for` / `best_ask_for` 按 instrument 路由(inherent 方法)。
>
> 见 commit e58a253 "refactor(backtest): route L1/L2 by Instrument via L1Book map (multi-leg)"。
> 关联 commit 修复 `streaming_report_e2e` 中 BuyStrategy split 旧 bug。

**Files:**
- Modify: `crates/axon-backtest/src/matching/engine.rs`

- [ ] **Step 1: Replace `L1MatchingEngine` struct**

Replace the struct definition:

```rust
/// L1 撮合引擎(多品种路由版)
pub struct L1MatchingEngine {
    /// 每个 instrument 一个 book(用 HashMap 路由)
    books: HashMap<Instrument, L1Book>,
    /// 成交序列号(单调递增,跨 instrument 共享)
    trade_sequence: AtomicU64,
}
```

- [ ] **Step 2: Replace `impl Default` and `new`**

```rust
impl Default for L1MatchingEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl L1MatchingEngine {
    pub fn new() -> Self {
        Self {
            books: HashMap::new(),
            trade_sequence: AtomicU64::new(0),
        }
    }
}
```

(Remove `with_symbol` — single-symbol binding no longer needed; multi-instrument routing replaces it.)

- [ ] **Step 3: Refactor `validate` to use per-book check**

```rust
fn validate(book: &L1Book, order: &Order) -> MatchingResult<()> {
    if let Some(p) = Self::limit_price(order)
        && p.as_f64() <= 0.0
    {
        return Err(MatchingError::InvalidPrice { price: p });
    }
    if order.quantity.as_f64() <= 0.0 {
        return Err(MatchingError::InvalidQuantity {
            quantity: order.quantity,
        });
    }
    // 移除原 self.symbol 单品种绑定校验
    match order.order_type {
        OrderType::Market | OrderType::Limit { .. } => Ok(()),
        _ => Err(MatchingError::UnsupportedOrderType(format!(
            "{:?}",
            order.order_type
        ))),
    }
}
```

- [ ] **Step 4: Refactor `submit` to route via `instrument`**

```rust
pub fn submit(&mut self, order: Order) -> SubmitResult {
    let instrument = order.instrument.clone();
    let book = self.books.entry(instrument).or_insert_with(L1Book::new);
    Self::validate(book, &order).map_err(|e| {
        // ⚠️ 简化:出错时构造一个空 SubmitResult
        // 真实实现应返回 Err 或带 fills 的 SubmitResult
        // 暂时与原语义一致:validation 失败时返回空 fills
        SubmitResult {
            fills: Vec::new(),
            accepted: false,
            reject_reason: Some(format!("{e:?}")),
        }
    }).unwrap_or_else(|sr| sr)
    // ...
}
```

> ⚠️ **Refactor guidance**: The complete refactor of `submit`, `match_against_asks`, `match_against_bids`, `insert_passive` is mechanical — replace `self.bids` with `book.bids`, `self.asks` with `book.asks`, `self.order_index` with `book.order_index`. The match logic is unchanged. This is a ~150-line mechanical refactor; the implementer should make all the substitutions carefully. **Use this pattern** for `submit`:

```rust
pub fn submit(&mut self, order: Order) -> SubmitResult {
    let instrument = order.instrument.clone();
    let book = self.books.entry(instrument).or_insert_with(L1Book::new);
    Self::validate(book, &order)?;
    // ... 原本的撮合逻辑,只是 self.bids -> book.bids 等 ...
}
```

- [ ] **Step 5: Refactor `clear_book` to clear all books**

```rust
pub fn clear_book(&mut self) {
    for book in self.books.values_mut() {
        book.clear();
    }
}
```

- [ ] **Step 6: Refactor `seed_liquidity` to route to correct book**

```rust
pub fn seed_liquidity(
    &mut self,
    mid_price: f64,
    half_spread: f64,
    depth_levels: usize,
    size_per_level: f64,
    instrument: Instrument,
    next_id: u64,
) -> u64 {
    if mid_price <= 0.0 || half_spread <= 0.0 || depth_levels == 0 || size_per_level <= 0.0 {
        return next_id;
    }
    let mut id = next_id;
    let (base, quote) = match &instrument {
        Instrument::Spot(s) => (s.base.clone(), s.quote.clone()),
        Instrument::Swap(s) => (s.base.clone(), s.quote.clone()),
    };

    let book = self.books.entry(instrument).or_insert_with(L1Book::new);

    for level in 1..=depth_levels {
        let ask_price = mid_price + half_spread * level as f64;
        if ask_price > 0.0 {
            let order = Order::spot(
                id, base.clone(), quote.clone(), Side::Sell,
                OrderType::Limit { price: Price::from_f64(ask_price) },
                Quantity::from_f64(size_per_level), TimeInForce::GTC,
            );
            book.insert_passive(order);
            id += 1;
        }
        let bid_price = mid_price - half_spread * level as f64;
        if bid_price > 0.0 {
            let order = Order::spot(
                id, base.clone(), quote.clone(), Side::Buy,
                OrderType::Limit { price: Price::from_f64(bid_price) },
                Quantity::from_f64(size_per_level), TimeInForce::GTC,
            );
            book.insert_passive(order);
            id += 1;
        }
    }
    id
}
```

(Move `insert_passive` and other helpers to be `L1Book` methods, not `L1MatchingEngine` methods, since they operate on a single book.)

- [ ] **Step 7: Update `best_bid` / `best_ask` / `spread` / `depth` / `active_order_count`**

Since the trait requires single global best bid/ask (a limitation — see note), the default impl returns the first non-empty book's best. **Spec §5.1 says "价格撮合不跨 instrument"**, so callers should query a specific instrument. For now, return any book's best (simplest):

```rust
fn best_bid(&self) -> Option<Price> {
    self.books.values().find_map(|b| b.best_bid())
}
fn best_ask(&self) -> Option<Price> {
    self.books.values().find_map(|b| b.best_ask())
}
fn best_bid_for(&self, instrument: &Instrument) -> Option<Price> {
    self.books.get(instrument).and_then(|b| b.best_bid())
}
fn best_ask_for(&self, instrument: &Instrument) -> Option<Price> {
    self.books.get(instrument).and_then(|b| b.best_ask())
}
fn active_order_count(&self) -> usize {
    self.books.values().map(|b| b.active_order_count()).sum()
}
```

(Add `best_bid_for` / `best_ask_for` as inherent methods, not on trait, since the trait isn't ready for multi-instrument queries.)

- [ ] **Step 8: Build and fix any remaining errors**

Run: `cargo build -p axon-backtest 2>&1 | head -50`
Expected: still some errors related to the engine's `position_states` key. Task 3.5 fixes.

- [ ] **Step 9: Commit (mid-refactor OK)**

```bash
git add crates/axon-backtest/src/matching/engine.rs
git commit -m "refactor(backtest): L1MatchingEngine holds per-instrument L1Book"
```

---

### Task 3.3: Add `L1Book` unit tests

> **完成状态(e58a253)**:本任务所有 step 已完成。tests 模块中添加了:
> - `test_engine_multi_book_routing`:BTC/ETH 两个 instrument 路由隔离,
>   `best_bid_for` / `best_ask_for` 路由正确,跨 instrument 价格不串扰。
> - `test_clear_book_clears_all_instruments`:多 instrument 场景下
>   `clear_book` 清空所有 book 内容但保留 books 容器(形状),
>   clear 之后再 seed 不留 ghost entry。

**Files:**
- Modify: `crates/axon-backtest/src/matching/engine.rs`

- [ ] **Step 1: Add multi-instrument routing test**

In tests module:

```rust
#[test]
fn test_l1_multi_instrument_routing() {
    use axon_core::types::{SpotInstrument, SwapInstrument, SwapSettle};
    let mut engine = L1MatchingEngine::new();
    let btc_spot = Instrument::Spot(SpotInstrument {
        base: Symbol::from("BTC"),
        quote: Symbol::from("USDT"),
    });
    let eth_spot = Instrument::Spot(SpotInstrument {
        base: Symbol::from("ETH"),
        quote: Symbol::from("USDT"),
    });

    let sell_btc = Order::spot(1, "BTC", "USDT", Side::Sell,
        OrderType::Limit { price: Price::from_f64(100.0) },
        Quantity::from_f64(1.0), TimeInForce::GTC);
    let sell_eth = Order::spot(2, "ETH", "USDT", Side::Sell,
        OrderType::Limit { price: Price::from_f64(10.0) },
        Quantity::from_f64(2.0), TimeInForce::GTC);

    engine.submit(sell_btc);
    engine.submit(sell_eth);

    // 2 个独立 book
    assert_eq!(engine.books.len(), 2);
    assert_eq!(engine.active_order_count(), 2);

    // BTC best_ask = 100, ETH best_ask = 10
    assert_eq!(engine.best_ask_for(&btc_spot).unwrap().as_f64(), 100.0);
    assert_eq!(engine.best_ask_for(&eth_spot).unwrap().as_f64(), 10.0);
}
```

- [ ] **Step 2: Add clear-book test**

```rust
#[test]
fn test_l1_clear_book_clears_all_instruments() {
    let mut engine = L1MatchingEngine::new();
    engine.submit(Order::spot(1, "BTC", "USDT", Side::Sell,
        OrderType::Limit { price: Price::from_f64(100.0) },
        Quantity::from_f64(1.0), TimeInForce::GTC));
    engine.submit(Order::spot(2, "ETH", "USDT", Side::Sell,
        OrderType::Limit { price: Price::from_f64(10.0) },
        Quantity::from_f64(1.0), TimeInForce::GTC));
    assert_eq!(engine.active_order_count(), 2);
    engine.clear_book();
    assert_eq!(engine.active_order_count(), 0);
    assert_eq!(engine.books.len(), 2);  // books 容器还在,只是 book 内容空
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p axon-backtest matching::engine::tests::test_l1_multi`
Expected: 2 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/axon-backtest/src/matching/engine.rs
git commit -m "test(backtest): L1 multi-instrument routing and clear_book coverage"
```

---

### Task 3.4: Update `BacktestEngine::begin_bar` for Instrument routing

> **完成状态(T2.3)**:本任务所有 step 已在 T2.3 阶段完成。
> `BacktestEngine::begin_bar` 现在签名是 `pub fn begin_bar(&mut self, mid_price: f64, instrument: Instrument)`,
> 参数 `symbol: Symbol` 替换为 `instrument: Instrument`,内部 `clear_book()`
> 清所有 book(语义不变)+ `seed_liquidity(..., instrument, ...)` 按 instrument 路由。
>
> 完整实现见 `crates/axon-backtest/src/engine.rs` 第 455 行 `begin_bar`。
> 见 commit 6d91f99 "refactor(backtest): seed_liquidity and begin_bar take Instrument"。

**Files:**
- Modify: `crates/axon-backtest/src/engine.rs`

- [ ] **Step 1: Change `begin_bar` to take Instrument**

```rust
pub fn begin_bar(&mut self, mid_price: f64, instrument: Instrument) {  // 改: 原 Symbol
    let Some(cfg) = self.seed_liquidity_config else { return; };
    if mid_price <= 0.0 || cfg.half_spread <= 0.0 || cfg.depth_levels == 0 || cfg.size_per_level <= 0.0 {
        return;
    }
    // clear_book 清所有 book(语义不变)
    self.config.matching_engine.clear_book();
    let next_id = self.seed_liquidity_next_id.load(std::sync::atomic::Ordering::Relaxed);
    let new_next_id = self.config.matching_engine.seed_liquidity(
        mid_price, cfg.half_spread, cfg.depth_levels, cfg.size_per_level,
        instrument,    // 改: 原 symbol
        next_id,
    );
    self.seed_liquidity_next_id.store(new_next_id, std::sync::atomic::Ordering::Relaxed);
}
```

- [ ] **Step 2: Build**

Run: `cargo build -p axon-backtest 2>&1 | head -20`

- [ ] **Step 3: Commit**

```bash
git add crates/axon-backtest/src/engine.rs
git commit -m "refactor(backtest): begin_bar takes Instrument for multi-instrument seed"
```

---

### Task 3.5: Update `BacktestEngine::position_states` to Instrument key

**Files:**
- Modify: `crates/axon-backtest/src/engine.rs`

- [ ] **Step 1: Change `BacktestState` field type**

```rust
#[derive(Debug, Default)]
struct BacktestState {
    /// per-instrument 持仓状态(改: 原 HashMap<String, PositionState>)
    position_states: HashMap<Instrument, PositionState>,
    trading_metrics: TradingMetrics,
    cash: f64,
    fee_accumulator: f64,
    nav_peak: f64,
    equity_curve: Vec<(Timestamp, f64)>,
    trades: Vec<TradeRecord>,
    /// 新增: leg 目标仓位
    legs: HashMap<Instrument, LegConfig>,
    /// 新增: mark 价格缓存
    mark_cache: HashMap<Instrument, Price>,
}
```

- [ ] **Step 2: Add `LegConfig` struct**

Above `BacktestState`:

```rust
/// Leg 配置(策略目标仓位)
#[derive(Debug, Clone, Copy)]
pub struct LegConfig {
    pub instrument: Instrument,
    pub target_position: f64,
}

impl Default for LegConfig {
    fn default() -> Self {
        Self {
            instrument: Instrument::Spot(SpotInstrument {
                base: Symbol::default(),
                quote: Symbol::default(),
            }),
            target_position: 0.0,
        }
    }
}
```

- [ ] **Step 3: Add `mark_cache` field initialization**

In `BacktestEngine::new`, ensure new fields default-init (Default does it for HashMap; Price needs to be added).

- [ ] **Step 4: Update `apply_fill` (already done in T2.2; verify)**

- [ ] **Step 5: Update `liquidate_eod` to use Instrument**

(Already done in T2.2.)

- [ ] **Step 6: Update `RunResult.positions`**

```rust
pub struct RunResult {
    // ...
    pub positions: HashMap<Instrument, f64>,    // 改: 原 HashMap<String, f64>
    pub leg_targets: HashMap<Instrument, f64>,  // 新增
    pub marks: HashMap<Instrument, f64>,        // 新增
    // ...
}
```

Update `build_result` to populate them:

```rust
let positions: HashMap<Instrument, f64> = self.bt_state
    .position_states
    .iter()
    .filter(|(_, p)| p.quantity.abs() > 1e-9)
    .map(|(inst, p)| (*inst, p.quantity))
    .collect();

let leg_targets: HashMap<Instrument, f64> = self.bt_state
    .legs
    .iter()
    .map(|(inst, cfg)| (*inst, cfg.target_position))
    .collect();

let marks: HashMap<Instrument, f64> = self.bt_state
    .mark_cache
    .iter()
    .map(|(inst, p)| (*inst, p.as_f64()))
    .collect();
```

- [ ] **Step 7: Run all axon-backtest tests**

Run: `cargo test -p axon-backtest 2>&1 | head -50`
Expected: still failing if `Default` for `BacktestState` is missing some fields. Fix imports.

- [ ] **Step 8: Commit**

```bash
git add crates/axon-backtest/src/engine.rs
git commit -m "feat(backtest): position_states keyed by Instrument, add legs and mark_cache"
```

---

### Task 3.6: Add `Event::Mark` handling to `dispatch`

**Files:**
- Modify: `crates/axon-backtest/src/engine.rs`

- [ ] **Step 1: Add Mark variant to dispatch**

In `BacktestEngine::dispatch`:

```rust
fn dispatch(&mut self, event: Event) {
    self.config.clock.set(event.timestamp());
    self.stats.events_processed += 1;
    match event {
        Event::Order(OrderEvent { action, .. }) => self.handle_order_action(action),
        Event::Fill(fill) => self.handle_fill(fill),
        Event::Mark(mark) => self.handle_mark(mark),   // 新增
        _ => { trace!(...); }
    }
}
```

- [ ] **Step 2: Add `handle_mark` method**

```rust
fn handle_mark(&mut self, mark: MarkEvent) {
    // 写缓存,本次范围不触 NAV 重采样(详见 spec §5.2)
    self.bt_state.mark_cache.insert(mark.instrument, mark.mark_price);
}
```

Add `use axon_core::event::MarkEvent;` to imports.

- [ ] **Step 3: Add unit test for MarkEvent**

In tests:

```rust
#[test]
fn test_mark_event_writes_to_cache() {
    use axon_core::event::{EventBuilder, MarkEvent};
    use axon_core::types::{SpotInstrument, Price};
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    let inst = Instrument::Spot(SpotInstrument {
        base: Symbol::from("BTC"),
        quote: Symbol::from("USDT"),
    });
    q.push(b.mark(Timestamp::from_nanos(1_000), MarkEvent {
        instrument: inst.clone(),
        mark_price: Price::from_f64(50_000.0),
        timestamp: Timestamp::from_nanos(1_000),
    }));
    let mut engine = BacktestEngine::new(simple_config(), q);
    let result = engine.run();
    assert_eq!(result.marks.get(&inst).copied(), Some(50_000.0));
}
```

(May need to add `mark` method to `EventBuilder` — see spec §4.4 if missing.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p axon-backtest test_mark_event`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/axon-backtest/src/engine.rs
git commit -m "feat(backtest): handle MarkEvent, write to mark_cache"
```

---

### Task 3.7: Update `EOD liquidate` for proper instrument reconstruction

**Files:**
- Modify: `crates/axon-backtest/src/engine.rs`

- [ ] **Step 1: Use `Instrument` from `position_states` directly**

The `liquidate_eod` already has `instrument` from the position state. Reconstruct `Order` from it:

```rust
fn liquidate_eod(&mut self) {
    let to_liquidate: Vec<(Instrument, f64)> = self.bt_state
        .position_states
        .iter()
        .filter(|(_, p)| p.quantity.abs() > 1e-9)
        .map(|(inst, p)| (*inst, p.quantity))
        .collect();
    if to_liquidate.is_empty() { return; }

    for (idx, (instrument, qty)) in to_liquidate.into_iter().enumerate() {
        let side = if qty > 0.0 { Side::Sell } else { Side::Buy };
        let close_qty = qty.abs();
        // 用 instrument 自己的 base/quote 构造 Order
        let (base, quote) = match &instrument {
            Instrument::Spot(s) => (s.base.clone(), s.quote.clone()),
            Instrument::Swap(s) => (s.base.clone(), s.quote.clone()),
        };
        let order = Order::spot(
            EOD_LIQUIDATE_ID_BASE + idx as u64,
            base, quote,
            side, OrderType::Market,
            Quantity::from_f64(close_qty), TimeInForce::IOC,
        );
        let result = self.config.matching_engine.submit(order);
        for fill in &result.fills {
            self.stats.orders_accepted += 1;
            self.stats.fills += 1;
            self.stats.total_pnl += fill_pnl_delta(fill);
            if self.stats.total_pnl > self.stats.pnl_peak {
                self.stats.pnl_peak = self.stats.total_pnl;
            }
            self.apply_fill(&instrument, side, fill);
        }
    }
}
```

- [ ] **Step 2: Add multi-instrument EOD test**

```rust
#[test]
fn test_eod_liquidate_handles_multiple_instruments() {
    // 构造 spot BTC long 1.0 + spot ETH long 2.0
    // EOD 后两者都清零
    // (详细代码略,follow 现有 test_run_matched_orders_yield_one_fill 模式)
}
```

- [ ] **Step 3: Run all tests**

Run: `cargo test -p axon-backtest`
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add crates/axon-backtest/src/engine.rs
git commit -m "fix(backtest): EOD liquidate reconstructs Order from stored Instrument"
```

---

### Task 3.8: Add `set_target_position` / `get_target_position` / `get_position` API

**Files:**
- Modify: `crates/axon-backtest/src/engine.rs`

- [ ] **Step 1: Add the three methods**

In `impl BacktestEngine`:

```rust
/// 设置某 leg 的目标仓位(仅记录,不主动下单)
pub fn set_target_position(&mut self, instrument: Instrument, target: f64) {
    self.bt_state.legs
        .entry(instrument)
        .or_insert_with(|| LegConfig { instrument, target_position: 0.0 })
        .target_position = target;
}

/// 查询某 leg 的目标仓位
pub fn get_target_position(&self, instrument: &Instrument) -> Option<f64> {
    self.bt_state.legs.get(instrument).map(|l| l.target_position)
}

/// 查询某 instrument 的当前仓位
pub fn get_position(&self, instrument: &Instrument) -> f64 {
    self.bt_state.position_states
        .get(instrument)
        .map(|p| p.quantity)
        .unwrap_or(0.0)
}
```

- [ ] **Step 2: Add unit tests**

```rust
#[test]
fn test_set_get_target_position() {
    let mut engine = BacktestEngine::new(simple_config(), EventQueue::new());
    let inst = Instrument::Spot(SpotInstrument {
        base: Symbol::from("BTC"), quote: Symbol::from("USDT"),
    });
    assert_eq!(engine.get_target_position(&inst), None);
    engine.set_target_position(inst, 1.5);
    assert_eq!(engine.get_target_position(&inst), Some(1.5));
    engine.set_target_position(inst, -2.0);
    assert_eq!(engine.get_target_position(&inst), Some(-2.0));
}

#[test]
fn test_get_position_returns_zero_for_empty() {
    let engine = BacktestEngine::new(simple_config(), EventQueue::new());
    let inst = Instrument::Spot(SpotInstrument {
        base: Symbol::from("BTC"), quote: Symbol::from("USDT"),
    });
    assert_eq!(engine.get_position(&inst), 0.0);
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p axon-backtest set_get_target`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/axon-backtest/src/engine.rs
git commit -m "feat(backtest): set/get_target_position and get_position API for multi-leg"
```

---

### Task 3.9: Add `apply_fill` keyed-by-Instrument test

**Files:**
- Modify: `crates/axon-backtest/src/engine.rs`

- [ ] **Step 1: Add test**

```rust
#[test]
fn test_apply_fill_keyed_by_instrument() {
    // spot BTC 和 spot ETH 独立持仓
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    let btc = Instrument::Spot(SpotInstrument {
        base: Symbol::from("BTC"), quote: Symbol::from("USDT"),
    });
    let eth = Instrument::Spot(SpotInstrument {
        base: Symbol::from("ETH"), quote: Symbol::from("USDT"),
    });
    // BTC sell @ 100
    q.push(b.order(Timestamp::from_nanos(1_000), 1,
        OrderAction::Submitted(Order::spot(1, "BTC", "USDT", Side::Sell,
            OrderType::Limit { price: Price::from_f64(100.0) },
            Quantity::from_f64(1.0), TimeInForce::GTC))));
    // BTC buy @ 100 (吃 BTC sell)
    q.push(b.order(Timestamp::from_nanos(2_000), 2,
        OrderAction::Submitted(Order::spot(2, "BTC", "USDT", Side::Buy,
            OrderType::Limit { price: Price::from_f64(100.0) },
            Quantity::from_f64(1.0), TimeInForce::GTC))));
    // ETH sell @ 10 (不同 book)
    q.push(b.order(Timestamp::from_nanos(3_000), 3,
        OrderAction::Submitted(Order::spot(3, "ETH", "USDT", Side::Sell,
            OrderType::Limit { price: Price::from_f64(10.0) },
            Quantity::from_f64(2.0), TimeInForce::GTC))));
    // ETH buy @ 10 (吃 ETH sell)
    q.push(b.order(Timestamp::from_nanos(4_000), 4,
        OrderAction::Submitted(Order::spot(4, "ETH", "USDT", Side::Buy,
            OrderType::Limit { price: Price::from_f64(10.0) },
            Quantity::from_f64(2.0), TimeInForce::GTC))));

    let mut engine = BacktestEngine::new(simple_config(), q);
    let result = engine.run();
    assert_eq!(result.fills, 2);
    assert_eq!(result.positions.get(&btc).copied(), Some(1.0));
    assert_eq!(result.positions.get(&eth).copied(), Some(2.0));
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p axon-backtest test_apply_fill_keyed`
Expected: pass.

- [ ] **Step 3: Commit**

```bash
git add crates/axon-backtest/src/engine.rs
git commit -m "test(backtest): per-instrument position isolation"
```

---

### Task 3.10: Verify Phase 3 build clean

- [ ] **Step 1: Run all tests**

Run: `cargo test -p axon-backtest -p axon-core`
Expected: all pass.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy -p axon-backtest --all-targets -- -D warnings`
Expected: no warnings. Fix any that appear.

---

## Phase 4: Python 绑定

### Task 4.1: Update OrderDict protocol in PyO3

**Files:**
- Modify: `crates/axon-backtest/src/python/*.rs` (likely `mod.rs` or `engine.rs`)

- [ ] **Step 1: Find OrderDict dict construction in PyO3 code**

Run: `grep -rn "\"symbol\"\|symbol:\|order_dict" crates/axon-backtest/src/python/ | head -20`
Identify where OrderDict is read from Python.

- [ ] **Step 2: Replace `symbol` extraction with `instrument`**

The PyO3 code reads OrderDict from Python. Replace the symbol extraction with instrument parsing:

```rust
// 旧
let symbol: String = order_dict.get_item("symbol")?.extract()?;

// 新
let instrument_dict: PyObject = order_dict.get_item("instrument")?;
let instrument = parse_instrument(&py, &instrument_dict)?;
```

Where `parse_instrument` is:

```rust
fn parse_instrument(py: Python, dict: &PyObject) -> PyResult<Instrument> {
    let kind: String = dict.get_item(py, "kind")?.extract(py)?;
    match kind.as_str() {
        "spot" => {
            let base: String = dict.get_item(py, "base")?.extract(py)?;
            let quote: String = dict.get_item(py, "quote")?.extract(py)?;
            Ok(Instrument::Spot(SpotInstrument {
                base: Symbol::from(base),
                quote: Symbol::from(quote),
            }))
        }
        "swap" => {
            let base: String = dict.get_item(py, "base")?.extract(py)?;
            let quote: String = dict.get_item(py, "quote")?.extract(py)?;
            let settle: String = dict.get_item(py, "settle")?.extract(py)?;
            let contract_size: f64 = dict.get_item(py, "contract_size")?.extract(py)?;
            let settle_enum = match settle.as_str() {
                "usd_margin" | "UsdMargin" => SwapSettle::UsdMargin,
                "coin_margin" | "CoinMargin" => SwapSettle::CoinMargin,
                _ => return Err(PyValueError::new_err(format!("invalid settle: {settle}"))),
            };
            Ok(Instrument::Swap(SwapInstrument {
                base: Symbol::from(base),
                quote: Symbol::from(quote),
                settle: settle_enum,
                contract_size,
            }))
        }
        _ => Err(PyValueError::new_err(format!("invalid instrument kind: {kind}"))),
    }
}
```

- [ ] **Step 3: Build PyO3**

Run: `cargo build -p axon-backtest --features python`
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
git add crates/axon-backtest/src/python/
git commit -m "feat(python): OrderDict protocol uses Instrument instead of symbol string"
```

---

### Task 4.2: Add PyO3 methods for leg API

**Files:**
- Modify: `crates/axon-backtest/src/python/*.rs`

- [ ] **Step 1: Add `set_target_position` / `get_target_position` / `get_position` PyO3 methods**

In the `#[pymethods]` impl for `PyBacktestEngine`:

```rust
#[pyo3(signature = (instrument, target))]
fn set_target_position(&mut self, py: Python, instrument: &PyAny, target: f64) -> PyResult<()> {
    let inst = parse_instrument(py, &instrument.into())?;
    self.inner.borrow_mut().set_target_position(inst, target);
    Ok(())
}

#[pyo3(signature = (instrument))]
fn get_target_position(&self, py: Python, instrument: &PyAny) -> PyResult<Option<f64>> {
    let inst = parse_instrument(py, &instrument.into())?;
    Ok(self.inner.borrow().get_target_position(&inst))
}

#[pyo3(signature = (instrument))]
fn get_position(&self, py: Python, instrument: &PyAny) -> PyResult<f64> {
    let inst = parse_instrument(py, &instrument.into())?;
    Ok(self.inner.borrow().get_position(&inst))
}
```

- [ ] **Step 2: Add `push_mark` convenience method**

```rust
#[pyo3(signature = (instrument, price, timestamp_ns))]
fn push_mark(&mut self, py: Python, instrument: &PyAny, price: f64, timestamp_ns: i64) -> PyResult<()> {
    use axon_core::event::MarkEvent;
    let inst = parse_instrument(py, &instrument.into())?;
    let mark = MarkEvent {
        instrument: inst,
        mark_price: Price::from_f64(price),
        timestamp: Timestamp::from_nanos(timestamp_ns),
    };
    self.inner.borrow_mut().push_event(Event::Mark(mark));
    Ok(())
}
```

- [ ] **Step 3: Build & commit**

Run: `cargo build -p axon-backtest --features python`
Then commit:

```bash
git add crates/axon-backtest/src/python/
git commit -m "feat(python): PyO3 methods for set/get_target_position, get_position, push_mark"
```

---

### Task 4.3: Update `axon_quant/backtest.py` factories

**Files:**
- Modify: `python/axon_quant/backtest.py`

- [ ] **Step 1: Add `spot_instrument` / `swap_instrument` factory functions**

Append:

```python
def spot_instrument(base: str, quote: str) -> dict:
    """构造现货 instrument dict。

    Args:
        base: 基础币种,如 "BTC"
        quote: 计价币种,如 "USDT"

    Returns:
        dict,字段对应 Rust 端 `Instrument::Spot(SpotInstrument)`
    """
    return {"kind": "spot", "details": {"base": base, "quote": quote}}
    # 注:使用 serde tag = "kind",content = "details" 时,Python 端 dict 应该是
    # `{"kind": "spot", "details": {...}}`。如果 Rust 端 dict 协议用 flat 形式
    # (直接 kind/base/quote),则改为:
    # return {"kind": "spot", "base": base, "quote": quote}


def swap_instrument(
    base: str, quote: str,
    settle: str = "usd_margin",
    contract_size: float = 1.0,
) -> dict:
    """构造永续 instrument dict。

    Args:
        base: 基础币种
        quote: 计价币种
        settle: "usd_margin" 或 "coin_margin"
        contract_size: 合约乘数

    Returns:
        dict,字段对应 Rust 端 `Instrument::Swap(SwapInstrument)`
    """
    return {"kind": "swap", "details": {
        "base": base, "quote": quote,
        "settle": settle, "contract_size": contract_size,
    }}


def limit_order(
    order_id: int,
    instrument: dict,
    side: str,
    price: float,
    quantity: float,
    tif: str = "GTC",
) -> dict:
    """构造限价单 dict(替代旧 symbol: str 接口)。

    Args:
        order_id: 订单 ID
        instrument: `spot_instrument()` 或 `swap_instrument()` 返回的 dict
        side: "Buy" / "Sell"
        price: 限价单价
        quantity: 数量
        tif: 有效期

    Returns:
        dict,字段对应 Rust 端 `Order` 字段
    """
    return {
        "id": int(order_id),
        "instrument": instrument,
        "side": str(side),
        "type": "limit",
        "price": float(price),
        "quantity": float(quantity),
        "tif": str(tif).upper(),
    }


def market_order(
    order_id: int,
    instrument: dict,
    side: str,
    quantity: float,
) -> dict:
    """构造市价单 dict(替代旧 symbol: str 接口)。"""
    return {
        "id": int(order_id),
        "instrument": instrument,
        "side": str(side),
        "type": "market",
        "quantity": float(quantity),
        "tif": "IOC",
    }
```

> ⚠️ **Decide the wire format**: This plan assumes serde `tag = "kind", content = "details"` translates to Python `{"kind": "spot", "details": {...}}`. If the PyO3 dict reader is written to accept flat `{"kind": "spot", "base": ..., "quote": ...}`, use the flat form. **Verify by reading the actual PyO3 dict parsing code in Task 4.1 Step 1** and align Python factory output with the same shape.

- [ ] **Step 2: Verify with quick import test**

Run: `python -c "from axon_quant.backtest import spot_instrument, swap_instrument, limit_order; print(spot_instrument('BTC', 'USDT'))"`

- [ ] **Step 3: Commit**

```bash
git add python/axon_quant/backtest.py
git commit -m "feat(python): spot_instrument / swap_instrument factories, instrument-based limit_order"
```

---

### Task 4.4: Add `delta_neutral_arb` integration test

**Files:**
- Create: `crates/axon-integration-tests/src/delta_neutral_arb.rs`

- [ ] **Step 1: Add new file**

```rust
//! Spot + Perp 双 leg 回测集成测试
//!
//! 验证:
//! - spot 和 swap 各自撮合独立
//! - position_states 按 Instrument 区分
//! - MarkEvent 写入 mark_cache
//! - set_target_position 写入 legs
//! - RunResult 包含 leg_targets / marks

use axon_backtest::engine::BacktestEngine;
use axon_backtest::matching::L1MatchingEngine;
use axon_backtest::{BacktestEngineConfig, EventQueue, FeeConfig};
use axon_core::event::{Event, EventBuilder, MarkEvent, OrderAction};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{
    Instrument, Price, Quantity, SpotInstrument, SwapInstrument, SwapSettle, Symbol,
};

fn make_limit(id: u64, base: &str, quote: &str, side: Side, price: f64, qty: f64) -> Order {
    Order::spot(id, base, quote, side,
        OrderType::Limit { price: Price::from_f64(price) },
        Quantity::from_f64(qty), TimeInForce::GTC)
}

#[test]
fn test_spot_and_swap_isolation() {
    let btc_spot = Instrument::Spot(SpotInstrument {
        base: Symbol::from("BTC"), quote: Symbol::from("USDT"),
    });
    let btc_swap = Instrument::Swap(SwapInstrument {
        base: Symbol::from("BTC"), quote: Symbol::from("USDT"),
        settle: SwapSettle::UsdMargin, contract_size: 1.0,
    });

    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    // spot: sell @ 100
    q.push(b.order(Timestamp::from_nanos(1_000), 1,
        OrderAction::Submitted(make_limit(1, "BTC", "USDT", Side::Sell, 100.0, 1.0))));
    // spot: buy @ 100 (吃 spot sell)
    q.push(b.order(Timestamp::from_nanos(2_000), 2,
        OrderAction::Submitted(make_limit(2, "BTC", "USDT", Side::Buy, 100.0, 1.0))));

    let mut engine = BacktestEngine::new(
        BacktestEngineConfig {
            clock: SimulatedClock::new(Timestamp::from_nanos(0)),
            matching_engine: Box::new(L1MatchingEngine::new()),
            impact_model: None,
            initial_cash: 100_000.0,
            fee_config: FeeConfig::default(),
            force_liquidate: false,
        },
        q,
    );
    engine.set_target_position(btc_spot, 1.0);
    engine.set_target_position(btc_swap, -1.0);

    // Push a MarkEvent
    let mut b2 = EventBuilder::new(2);
    engine.push_event(b2.mark(Timestamp::from_nanos(3_000), MarkEvent {
        instrument: btc_swap.clone(),
        mark_price: Price::from_f64(99.5),
        timestamp: Timestamp::from_nanos(3_000),
    }));

    let result = engine.run();
    assert_eq!(result.fills, 1);
    assert_eq!(result.positions.get(&btc_spot).copied(), Some(1.0));
    assert_eq!(result.positions.get(&btc_swap).copied(), None);  // swap 没交易
    assert_eq!(result.leg_targets.get(&btc_spot).copied(), Some(1.0));
    assert_eq!(result.leg_targets.get(&btc_swap).copied(), Some(-1.0));
    assert_eq!(result.marks.get(&btc_swap).copied(), Some(99.5));
}
```

- [ ] **Step 2: Wire into integration test crate**

Edit `crates/axon-integration-tests/src/lib.rs` (or `main.rs`):

```rust
#[cfg(test)]
#[path = "delta_neutral_arb.rs"]
mod delta_neutral_arb;
```

(Adjust to the actual structure of the integration test crate — it may use `mod` directly.)

- [ ] **Step 3: Check `EventBuilder::mark` exists**

If `EventBuilder` doesn't have a `mark` method, add it (similar to `fill`):

```rust
impl EventBuilder {
    pub fn mark(&mut self, ts: Timestamp, mark: MarkEvent) -> Event {
        Event::new_marked(ts, self.next_seq(), mark)
    }
}
```

(Add `Event::new_marked` constructor if missing; or use the existing `Event::Mark` constructor pattern.)

- [ ] **Step 4: Run test**

Run: `cargo test -p axon-integration-tests delta_neutral`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/axon-integration-tests/src/delta_neutral_arb.rs
git commit -m "test(integration): spot+perp two-leg backtest end-to-end"
```

---

### Task 4.5: Add Python end-to-end test

**Files:**
- Create: `python/tests/test_backtest_multi_leg.py`

- [ ] **Step 1: Create test file**

```python
"""Spot + Perp 双 leg 端到端 Python 测试。"""

from axon_quant.backtest import (
    BacktestEngine,
    limit_order,
    spot_instrument,
    swap_instrument,
)


def test_spot_perp_two_leg_routing():
    bt = BacktestEngine(initial_cash=100_000.0)
    spot = spot_instrument("BTC", "USDT")
    perp = swap_instrument("BTC", "USDT", settle="usd_margin")
    # 推入两腿订单
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_000,
        "order": limit_order(1, spot, "Buy", 100.0, 1.0),
    })
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_000,
        "order": limit_order(2, perp, "Sell", 100.0, 1.0),
    })
    result = bt.run()
    # 由于两边都没有对手盘,没有成交
    assert result.fills == 0
    # 但 legs 和 mark 应有目标位
    bt.set_target_position(spot, 1.0)
    bt.set_target_position(perp, -1.0)
    assert bt.get_target_position(spot) == 1.0
    assert bt.get_target_position(perp) == -1.0


def test_mark_push_writes_cache():
    bt = BacktestEngine(initial_cash=100_000.0)
    spot = spot_instrument("BTC", "USDT")
    bt.push_mark(spot, 50_000.0, timestamp_ns=1_000)
    result = bt.run()
    assert result.marks[spot] == 50_000.0
```

(Adjust the dict format `{"type": "order_submitted", "order": ...}` to match existing Python → Rust dict protocol. May need to check `python/axon_quant/data.py` or similar for the actual wire format.)

- [ ] **Step 2: Run pytest**

Run: `pytest python/tests/test_backtest_multi_leg.py -v`
Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add python/tests/test_backtest_multi_leg.py
git commit -m "test(python): spot+perp two-leg end-to-end test"
```

---

## Phase 5: 文档

### Task 5.1: Update `CHANGELOG.md`

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Add 0.5.0 section at top**

```markdown
## [0.5.0] - 2026-07-XX (BREAKING)

### ⚠️ BREAKING CHANGES

- `Order` field `symbol: Symbol` replaced with `instrument: Instrument`
- `Order::new(id, symbol, ...)` removed; use `Order::spot(...)` or `Order::swap(...)`
- Python `OrderDict` protocol: `symbol: str` replaced with `instrument: dict`
- `BacktestEngine.position_states` and `RunResult.positions` keyed by `Instrument`
- `MatchingEngine::seed_liquidity` parameter `symbol: Symbol` replaced with `instrument: Instrument`
- `BacktestEngine::begin_bar(mid_price, symbol)` parameter changed to `Instrument`

### Added

- `Instrument` enum with `Spot` / `Swap` variants (in `axon-core`)
- `MarkEvent` and `Event::Mark` variant for mark price plumbing (placeholder for future funding)
- `BacktestEngine::set_target_position(instrument, qty)` for leg-level target position tracking
- `BacktestEngine::get_target_position(instrument) -> Option<f64>`
- `BacktestEngine::get_position(instrument) -> f64`
- `BacktestEngine::push_mark(instrument, price, timestamp_ns)` Python convenience method
- `RunResult.leg_targets: HashMap<Instrument, f64>`
- `RunResult.marks: HashMap<Instrument, f64>`
- `TradeRecord.instrument: Instrument` field
- `L1MatchingEngine` now holds `HashMap<Instrument, L1Book>` for multi-instrument routing
- `axon_quant.backtest.spot_instrument(base, quote)` factory
- `axon_quant.backtest.swap_instrument(base, quote, settle, contract_size)` factory

### Out of Scope (this release)

- Funding rate settlement (Python user responsibility)
- Mark-driven NAV resampling
- Automatic rebalance trigger
- Perp margin / leverage / liquidation
- Atomic dual-leg order submission
```

- [ ] **Step 2: Commit**

```bash
git add CHANGELOG.md
git commit -m "docs(changelog): 0.5.0 BREAKING - spot+perp two-leg backtest"
```

---

### Task 5.2: Update reference docs

**Files:**
- Modify: `docs/zh/reference/backtest.md`
- Modify: `docs/en/reference/backtest.md`

- [ ] **Step 1: Add multi-leg section to Chinese docs**

Append to `docs/zh/reference/backtest.md`:

```markdown
## 双 Leg 回测 (Spot + Perp)

`axon_quant 0.5.0+` 支持 spot 和 perp 两腿同时回测,适用于资金费率套利等 delta 中性策略。

### 概念

- `Instrument`: 交易品种抽象,枚举 `Spot` / `Swap` 两种变体
- `Leg`: 一个 instrument 的目标仓位(由策略在 Python 端设置)
- 双 leg 策略: spot leg + perp leg,共用同一 cash 池,delta 中性

### Python 用例

\`\`\`python
from axon_quant.backtest import (
    BacktestEngine, limit_order,
    spot_instrument, swap_instrument,
)

bt = BacktestEngine(initial_cash=100_000.0)
spot = spot_instrument("BTC", "USDT")
perp = swap_instrument("BTC", "USDT", settle="usd_margin")

# 设置两腿目标位
bt.set_target_position(spot, 1.0)   # spot long 1 BTC
bt.set_target_position(perp, -1.0)  # perp short 1 BTC (delta 中性)

# 推入订单
bt.push_event({"type": "order_submitted", "timestamp_ns": 1_000,
               "order": limit_order(1, spot, "Buy", 100.0, 1.0)})
bt.push_event({"type": "order_submitted", "timestamp_ns": 1_000,
               "order": limit_order(2, perp, "Sell", 100.0, 1.0)})

# 推入 mark price (本次不触 NAV,仅缓存)
bt.push_mark(perp, 99.5, timestamp_ns=2_000)

result = bt.run()
print(result.leg_targets)  # {spot: 1.0, perp: -1.0}
print(result.marks)         # {perp: 99.5}
print(result.positions)     # {spot: 1.0}
\`\`\`

### 范围

- ✅ 双 instrument 撮合隔离
- ✅ 双 leg 目标位 API
- ✅ MarkEvent 写缓存
- ❌ funding 结算(留 Python 端,见下文)
- ❌ 自动 rebalance 触发

### Funding 结算(用户自行实现)

`axon_quant` 当前**不**做资金费结算,这是 Python 端策略的责任:

\`\`\`python
# 在每根 funding 周期 (e.g. 8:00 UTC) 之前,策略自己计算并 push mark
if timestamp_ns == next_funding_ts:
    funding_payment = position_size * mark_price * funding_rate
    # 通过自定义 Python 代码记账
\`\`\`

未来版本将内置 `FundingEvent` 支持。
```

- [ ] **Step 2: Mirror to English docs**

Append equivalent section to `docs/en/reference/backtest.md`.

- [ ] **Step 3: Verify docs build**

Run: `mkdocs build --strict` (from repo root, both `mkdocs.yml` and `mkdocs-en.yml`)
Expected: builds clean.

- [ ] **Step 4: Commit**

```bash
git add docs/zh/reference/backtest.md docs/en/reference/backtest.md
git commit -m "docs(backtest): multi-leg spot+perp section in zh and en reference"
```

---

## Phase 6: 版本对齐

### Task 6.1: Bump workspace version in `Cargo.toml`

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Find `[workspace.package]` section**

Run: `grep -A 5 "workspace.package" Cargo.toml`

- [ ] **Step 2: Bump version**

Change `version = "0.4.1"` to `version = "0.5.0"` in `[workspace.package]`.

- [ ] **Step 3: Verify all 22 crates auto-sync via `version.workspace = true`**

Run: `grep -L "version.workspace" crates/*/Cargo.toml`
Expected: empty (all crates use workspace version).

If any crate has hardcoded `version = "0.4.1"`, change to `version.workspace = true`.

- [ ] **Step 4: Build to verify version propagation**

Run: `cargo build --workspace 2>&1 | head -20`

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/*/Cargo.toml
git commit -m "chore: bump version to 0.5.0"
```

---

### Task 6.2: Bump Python package version in `pyproject.toml`

**Files:**
- Modify: `pyproject.toml`

- [ ] **Step 1: Bump version**

Change `version = "0.4.1"` to `version = "0.5.0"`.

- [ ] **Step 2: Verify**

Run: `grep -A 1 "\[project\]" pyproject.toml`
Expected: shows new version.

- [ ] **Step 3: Commit**

```bash
git add pyproject.toml
git commit -m "chore: bump python package version to 0.5.0"
```

---

### Task 6.3: Final verification

- [ ] **Step 1: Run full workspace test suite**

Run: `cargo test --workspace`
Expected: all pass.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 3: Run Python tests**

Run: `pytest python/tests/ -v`
Expected: all pass.

- [ ] **Step 4: Run version-check**

Run: `make version-check`
Expected: confirms 0.5.0 across all 22 crates and Python.

- [ ] **Step 5: Build Python wheel and verify**

Run: `make build-wheel` (or equivalent)
Verify `_native.cpython-313-darwin.so` rebuilds with new version.

- [ ] **Step 6: Run mkdocs build**

Run: `mkdocs build --strict`
Expected: clean.

- [ ] **Step 7: Final commit if any pending changes**

```bash
git status
# If clean, done. Otherwise commit remaining.
```

---

## Self-Review

**1. Spec coverage:**

| Spec Section | Plan Task(s) |
|---|---|
| §4.1 Instrument enum | T1.1, T1.2 |
| §4.2 Order spot/swap | T2.1 |
| §4.3 Position key | T3.5 |
| §4.4 MarkEvent | T1.3, T3.6 |
| §4.5 LegConfig | T3.5, T3.8 |
| §4.6 TradeRecord | T2.4 |
| §4.7 MatchFill unchanged | (verified in T2.3 — Instrument 单独传) |
| §5.1 L1 multi-book | T3.1, T3.2, T3.3, T3.4 |
| §5.2 dispatch Mark | T3.6 |
| §5.3 apply_fill | T2.2, T3.9 |
| §5.4 leg API | T3.8 |
| §5.5 RunResult | T3.5 |
| §5.6 EOD liquidate | T2.2, T3.7 |
| §6.2 Python protocol | T4.1, T4.2, T4.3 |
| §7.1 unit tests | (covered per task) |
| §7.2 integration test | T4.4 |
| §7.3 Python test | T4.5 |
| §8 docs | T5.1, T5.2 |
| §9 risks | (mitigated via TDD, incremental commits) |

Coverage: complete.

**2. Placeholder scan:**

- ⚠️ `("BTC", "USDT")` placeholder in T2.2, T2.3 — explicitly marked as transitional, fixed in T3.7 / T3.2.
- ⚠️ "Decide the wire format" note in T4.3 — explicitly asks implementer to align with PyO3 code.
- All other steps have concrete code or specific test values.

**3. Type consistency:**

- `Instrument` defined T1.1, used everywhere downstream ✓
- `Order::spot(id, base, quote, side, ...)` signature consistent across all uses ✓
- `apply_fill(&self, instrument: &Instrument, side: Side, fill: &MatchFill)` consistent in T2.2, T3.5, T3.7, T3.9 ✓
- `LegConfig { instrument, target_position }` consistent in T3.5, T3.8 ✓
- `MarkEvent { instrument, mark_price, timestamp }` consistent in T1.3, T3.6, T4.4 ✓

**4. Identified issues fixed inline:**

- T2.2 Step 3 "EOD placeholders" → fixed in T3.7 with proper instrument reconstruction
- T2.3 Step 2 "seed_liquidity placeholders" → fixed in T3.2 Step 6 with instrument routing
- T4.3 Step 1 "wire format decision" → added explicit verification step

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-17-spot-perp-two-leg-backtest.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** - I dispatch a fresh subagent per task, review between tasks, fast iteration
2. **Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints

**Which approach?**
