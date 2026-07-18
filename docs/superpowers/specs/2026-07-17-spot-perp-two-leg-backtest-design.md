# Spot + Perp Two-Leg Backtest Design

> Date: 2026-07-17
> Status: Draft (pending user review)
> Version target: 0.5.0 (breaking change)
> Crate: `axon-backtest`, `axon-core`

## 1. Background and Motivation

`axon-backtest` 当前 (`0.4.x`) 是一款 single-instrument, single-leg 的事件驱动回测引擎:

- 一个 `BacktestEngine` 持有一个 `MatchingEngine`
- 持仓按 `Symbol` (`String`) 索引,单 symbol 单向仓位(quantity 字段同时表达方向和数量)
- 6 状态机处理 fill,无 instrument type 概念
- 无 funding rate 事件,无 mark price,无 multi-leg 抽象

虽然代码里有 `MultiAssetMatchingEngine` (L3 层),但其 `CrossPair` / `ArbitrageOpportunity`
是面向**跨交易所价差套利**(如 Binance BTC-USDT vs Coinbase BTC-USD),
**不是** spot-vs-perp delta-neutral 套利。两者是不同模型。

真实做 spot + perp 套利需要:

1. 引擎区分 spot 和 swap(语义不同:perp 需 funding 结算,spot 不需要)
2. 策略可以单独设置 `spot_target_position` 和 `perp_target_position`
3. 两个 leg 的持仓独立记账,但共用同一 cash 池(delta 中性套利的本质)
4. (未来) funding 事件和独立 mark price

本次 spec 覆盖**骨架**:
- Instrument 类型抽象
- 多 instrument 撮合
- 双 leg 目标位 API
- MarkEvent 占位(本次不主动算 unrealized)

**不在范围**:
- Funding 结算逻辑(留给 Python 端 / 未来 spec)
- 自动 rebalance 触发(留给策略层)
- Perp 保证金 / leverage / liquidation(留给未来 spec)
- MarkEvent 主动触 NAV 重采样(本次仅写缓存)

## 2. Decisions Summary

| 决策点 | 选定方案 | 备选 | 理由 |
|---|---|---|---|
| 范围 | 双 leg 骨架 + 引擎支持 spot/perp | 完整 funding 框架 | YAGNI;funding 留 Python 端 |
| Instrument 表达 | 新 `Instrument` enum (Spot/Swap 变体) | 字符串后缀 / Symbol 扩展 | 类型安全 + 表达力 |
| Portfolio 跟踪 | `HashMap<Instrument, PositionState>`,cash 唯一 | Leg 抽象 / ArbPair 装饰 | 与现有架构一致;YAGNI |
| Funding | **不在范围** | FundingEvent 推入 | 用户明确要求留 Python 端 |
| Mark 机制 | 新增 `MarkEvent` (可选推入) | 仅 fill 价 | 为未来 funding 铺路 |
| 引擎整合 | 原地扩展 (`方案 A`) | 多 matching engine / 包装 | 唯一在"长期 + 完整性"上全胜出 |
| 版本号 | `0.5.0` (semver breaking) | `0.4.x` 软升级 | OrderDict 协议破坏性变更 |

## 3. Architecture

### 3.1 Component Diagram

```
┌──────────────────────────────────────────────────────────────┐
│                    BacktestEngine (axon-backtest)             │
│                                                              │
│  ┌──────────────┐    ┌──────────────┐    ┌────────────────┐  │
│  │ EventQueue   │───▶│ Dispatcher   │───▶│ PositionStates │  │
│  │ • OrderEvt   │    │ • Submit     │    │ HashMap<       │  │
│  │ • FillEvt    │    │ • Fill       │    │   Instrument,  │  │
│  │ • MarkEvt ✨ │    │ • Mark  ✨   │    │   PositionState│  │
│  └──────────────┘    └──────┬───────┘    └────────────────┘  │
│                             │                                │
│                             ▼                                │
│                  ┌──────────────────────┐                    │
│                  │ MatchingEngine       │                    │
│                  │ (单实例,内部多 book) │                    │
│                  │                      │                    │
│                  │ HashMap<Instrument,  │                    │
│                  │         L1OrderBook> │                    │
│                  └──────────────────────┘                    │
│                             │                                │
│                             ▼                                │
│                  ┌──────────────────────┐                    │
│                  │ RunResult            │                    │
│                  │ (delta 中性 PnL 合并) │                    │
│                  └──────────────────────┘                    │
└──────────────────────────────────────────────────────────────┘
```

### 3.2 Key Changes

| Element | 旧 | 新 |
|---|---|---|
| `Order.symbol` | `Symbol` (String) | `Order.instrument: Instrument` |
| `PositionState` key | `String` (raw symbol) | `Instrument` |
| `MarkEvent` | 不存在 | 新增,可推入 |
| `MatchingEngine` 内部 | 单一 book | `HashMap<Instrument, L1OrderBook>` 路由 |
| `legs` 配置 | 不存在 | `HashMap<Instrument, LegConfig>` |
| `set_target_position` API | 不存在 | 新增 |

## 4. Data Model

### 4.1 `Instrument` enum (新, `axon-core/src/types/instrument.rs`)

```rust
// 注意:Instrument 实现 Clone(不是 Copy,因为内含 Symbol(String))
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "details")]
pub enum Instrument {
    /// 现货
    Spot(SpotInstrument),
    /// 永续合约
    Swap(SwapInstrument),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpotInstrument {
    pub base: Symbol,   // e.g. "BTC"
    pub quote: Symbol,  // e.g. "USDT"
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SwapInstrument {
    pub base: Symbol,
    pub quote: Symbol,
    pub settle: SwapSettle,  // UsdMargin | CoinMargin
    pub contract_size: f64,  // 合约乘数
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SwapSettle { UsdMargin, CoinMargin }
```

> **设计权衡**:`Symbol(String)` 堆分配,所以 `Instrument` 不能 `Copy`。
> 实际场景:单次回测的 `position_states` 元素数 << 100,`entry(instrument.clone())` 的 clone 开销可忽略。
> 频繁 hot path(`apply_fill` 内部)用 `&Instrument` 借阅,只在 HashMap 插入时 clone 一次。

**理由**:
- 字符串后缀 (`"BTC-USDT-PERP"`) 表达力弱,parse 不可靠
- `enum` 让 match 强制分支(Spot vs Swap 处理路径不同)
- 拆 base/quote 方便报告/对账

### 4.2 `Order` 改造 (`axon-core/src/order.rs`)

```rust
pub struct Order {
    pub id: u64,
    pub instrument: Instrument,   // ← 改: 原 symbol: Symbol
    pub side: Side,
    pub order_type: OrderType,
    pub quantity: Quantity,
    pub tif: TimeInForce,
}

impl Order {
    /// 构造现货订单(替代旧 Order::new)
    pub fn spot(
        id: u64,
        base: impl Into<Symbol>,
        quote: impl Into<Symbol>,
        side: Side,
        order_type: OrderType,
        quantity: Quantity,
        tif: TimeInForce,
    ) -> Order { ... }

    /// 构造永续订单
    pub fn swap(
        id: u64,
        base: impl Into<Symbol>,
        quote: impl Into<Symbol>,
        settle: SwapSettle,
        contract_size: f64,
        side: Side,
        order_type: OrderType,
        quantity: Quantity,
        tif: TimeInForce,
    ) -> Order { ... }
}
```

`Order::new(...)` **删除**(强制迁移,grep 全改)。

### 4.3 `PositionState` 不动结构,只换 key

`BacktestState.position_states: HashMap<Instrument, PositionState>`
(原 `HashMap<String, PositionState>`)

### 4.4 `MarkEvent` (新, `axon-core/src/event.rs`)

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarkEvent {
    pub instrument: Instrument,
    pub mark_price: Price,
    pub timestamp: Timestamp,
}

/// Event 枚举扩展
pub enum Event {
    Order(OrderEvent),
    Fill(FillEvent),
    Mark(MarkEvent),   // 新
    MarketData(...),   // 已有
    System(...),       // 已有
}
```

`#[non_exhaustive]` 已加,新 variant 不破坏外部 match。

### 4.5 `LegConfig` (新, `axon-backtest/src/engine.rs`)

```rust
pub struct LegConfig {
    pub instrument: Instrument,
    /// 该 leg 的目标仓位(由策略调用 set_target_position 更新)
    pub target_position: f64,
}
```

`BacktestEngine.legs: HashMap<Instrument, LegConfig>`(运行时**不**主动 rebalance,仅记录)

### 4.6 `TradeRecord` 扩展

```rust
pub struct TradeRecord {
    pub trade: Trade,
    pub realized_pnl: i64,     // ×1e6 定点
    pub fee: i64,              // ×1e6 定点
    pub net_quantity: i64,     // ×1e6 定点
    pub instrument: Instrument, // 新增
}
```

### 4.7 `MatchFill` 不动 (关键澄清)

**`MatchFill` 不增加 `instrument` 字段**,保持 `Copy` 和 ~56 字节固定大小。

原因:
- `MatchFill` 当前 `#[derive(Copy)]`(~56 字节:5 × u64 + 1 × Side + 1 × Timestamp)
- `Instrument` 因为内含 `Symbol(String)`(24 字节),**不是** `Copy`
- 如果给 `MatchFill` 加 `instrument`,会失去 `Copy` 语义,影响所有 `by-value` 调用点

**替代方案**:`apply_fill` 单独接收 `&Instrument` 参数(从 `Order.instrument` 直接传,不绕道 `MatchFill`):

```rust
fn handle_submit(&mut self, order: Order) {
    let instrument = order.instrument;  // Copy of Instrument
    let side = order.side;
    ...
    for fill in &result.fills {
        self.apply_fill(&instrument, side, fill);  // instrument 单独传
    }
}

fn apply_fill(&mut self, instrument: &Instrument, side: Side, fill: &MatchFill) {
    // ... 用 instrument 做 key,不用 fill.instrument ...
}
```

`Trade` 结构本身(40 字节 `#[repr(C)]` 固定大小合约)也**不动**。

## 5. Engine Behavior

### 5.1 `L1MatchingEngine` 内部从单 book 升级为多 book

```rust
pub struct L1MatchingEngine {
    /// 按 instrument 路由的订单簿
    books: HashMap<Instrument, L1Book>,
    next_order_id: u64,
}

impl L1MatchingEngine {
    pub fn submit(&mut self, order: Order) -> SubmitResult {
        let book = self.books
            .entry(order.instrument)
            .or_insert_with(L1Book::default);
        book.submit(order)
    }

    pub fn clear_book(&mut self) {
        // 清空所有 book(begin_bar 语义不变)
        for book in self.books.values_mut() {
            book.clear();
        }
    }

    pub fn seed_liquidity(...) {
        // 接受 instrument 参数(原 Symbol 路径),路由到对应 book
    }
}
```

**关键语义**:
- 每 instrument 一个独立 order book,价格撮合不跨 instrument
- 撮合优先级 (price-time) 在 book 内部维持
- `clear_book` 仍是一次性清空所有 book

### 5.2 `BacktestEngine::dispatch` 增加 Mark 分支

```rust
fn dispatch(&mut self, event: Event) {
    self.config.clock.set(event.timestamp());
    self.stats.events_processed += 1;

    match event {
        Event::Order(OrderEvent { action, .. }) => self.handle_order_action(action),
        Event::Fill(fill) => self.handle_fill(fill),
        Event::Mark(mark) => self.handle_mark(mark),  // 新
        _ => trace!(...),
    }
}

fn handle_mark(&mut self, mark: MarkEvent) {
    // 仅写缓存,本次范围不触 NAV 重采样
    // mark_cache 用 Price 类型(精确),RunResult 暴露时 as_f64() 转换
    self.bt_state.mark_cache.insert(mark.instrument, mark.mark_price);
}
```

`BacktestState.mark_cache: HashMap<Instrument, Price>`(新字段,Rust 内部用精确 Price 类型;`RunResult.marks` 暴露为 `f64` 用 `Price::as_f64()` 转换)

### 5.3 6 状态机 key 改 `Instrument`

```rust
fn apply_fill(&mut self, instrument: &Instrument, side: Side, fill: &MatchFill) {
    // 现有逻辑不变,只换 key(见 §4.7:instrument 从 Order 直接传,不用 fill)
    let pos = self.bt_state
        .position_states
        .entry(instrument.clone())   // Instrument 是 Clone(非 Copy)
        .or_default();
    // 6 状态机分支不变
}
```

> **注**:`Instrument` 实现 `Clone` 而非 `Copy`(因内含 `Symbol(String)` 堆分配)。
> 实际场景单次回测中 `position_states` 元素数 << 100,clone 开销可忽略。
> `apply_fill` 接受 `&Instrument`(避免不必要的 clone)只在 `entry` 时 clone 一次。

### 5.4 `leg_target_position` API

```rust
impl BacktestEngine {
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
}
```

**重要**: `set_target_position` 只记录,不自动生成订单。引擎保持"机械执行者"语义。
策略在 Python 端自行计算 delta 并下单:

```python
target_spot = 1.0
target_perp = -1.0
bt.set_target_position(BTC_SPOT, target_spot)
bt.set_target_position(BTC_PERP, target_perp)

cur_spot = bt.get_position(BTC_SPOT)
cur_perp = bt.get_position(BTC_PERP)
if cur_spot != target_spot:
    bt.submit(limit_order(..., BTC_SPOT, ...))
```

### 5.5 `RunResult` 新增字段

```rust
pub struct RunResult {
    // ... 已有字段 ...
    /// leg 目标仓位快照(`instrument -> target_position`)
    pub leg_targets: HashMap<Instrument, f64>,
    /// mark 价格缓存快照(`instrument -> mark_price`)
    pub marks: HashMap<Instrument, f64>,
    /// 终态持仓快照 — key 改 Instrument
    pub positions: HashMap<Instrument, f64>,
}
```

### 5.6 `EOD liquidate` 跨 instrument

```rust
fn liquidate_eod(&mut self) {
    // 旧: HashMap<String, f64> 遍历
    // 新: HashMap<Instrument, f64> 遍历
    let to_liquidate: Vec<(Instrument, f64)> = self.bt_state
        .position_states
        .iter()
        .filter(|(_, p)| p.quantity.abs() > 1e-9)
        .map(|(inst, p)| (*inst, p.quantity))
        .collect();
    // 路由到 matching_engine.submit()(其内部按 instrument 路由)
}
```

### 5.7 不变项

- 主循环 while 流程不变
- `handle_submit` / `handle_fill` 6 状态机分支不变
- `fee_config` / `force_liquidate` 语义不变
- `seed_liquidity` 外部 API 不变(参数里 Symbol 改 Instrument)
- `step()` / `run()` / `pending_events()` 不变

## 6. API Surface

### 6.1 Rust 公开 API 变更清单

| Item | 旧 | 新 |
|---|---|---|
| `Order::new(id, symbol, ...)` | ✅ | ❌ 删除 |
| `Order::spot(id, base, quote, side, type, qty, tif)` | ❌ | ✅ 新增 |
| `Order::swap(id, base, quote, settle, side, type, qty, tif)` | ❌ | ✅ 新增 |
| `Order { symbol: Symbol }` | ✅ | ❌ 改 `instrument: Instrument` |
| `BacktestEngine::new(cfg, queue)` | ✅ | ✅ 不变 |
| `BacktestEngine::set_target_position(inst, qty)` | ❌ | ✅ 新增 |
| `BacktestEngine::get_target_position(&inst)` | ❌ | ✅ 新增 |
| `BacktestEngine::get_position(&inst)` | ❌ | ✅ 新增 |
| `BacktestEngine::with_seed_liquidity(...)` | Symbol 参数 | Instrument 参数 |
| `RunResult.positions` | `HashMap<String, f64>` | `HashMap<Instrument, f64>` |
| `RunResult.leg_targets` | ❌ | ✅ 新增 |
| `RunResult.marks` | ❌ | ✅ 新增 |
| `TradeRecord.instrument` | ❌ | ✅ 新增 |

### 6.2 Python 绑定层 (`crates/axon-backtest/src/python/`)

**`OrderDict` 协议**:

```python
# Spot order
order = {
    "id": 1, "side": "Buy", "type": "limit", "price": 100.0,
    "quantity": 1.0, "tif": "GTC",
    "instrument": {"kind": "spot", "base": "BTC", "quote": "USDT"}
}

# Swap order
order = {
    "id": 1, "side": "Sell", "type": "limit", "price": 100.0,
    "quantity": 1.0, "tif": "GTC",
    "instrument": {
        "kind": "swap", "base": "BTC", "quote": "USDT",
        "settle": "usd_margin", "contract_size": 1.0
    }
}
```

**工厂函数** (`python/axon_quant/backtest.py`):

```python
def spot_instrument(base: str, quote: str) -> dict:
    return {"kind": "spot", "base": base, "quote": quote}

def swap_instrument(
    base: str, quote: str,
    settle: str = "usd_margin",
    contract_size: float = 1.0,
) -> dict:
    return {"kind": "swap", "base": base, "quote": quote,
            "settle": settle, "contract_size": contract_size}

def limit_order(order_id, instrument, side, price, quantity, tif="GTC"):
    # instrument 改为必填 dict
    return {"id": order_id, "instrument": instrument, "side": side,
            "type": "limit", "price": price, "quantity": quantity, "tif": tif}
```

**PyO3 新方法** (`PyBacktestEngine`):
- `set_target_position(instrument: dict, target: float)`
- `get_target_position(instrument: dict) -> float`
- `get_position(instrument: dict) -> float`
- `push_mark(instrument: dict, price: float, timestamp_ns: int)` — 推 MarkEvent 便捷方法

### 6.3 错误处理

**新增 `MatchingError::UnknownInstrument { instrument: Instrument }`**
(为 future 严格模式预留;本次不引入严格模式,`or_insert_with` 自动创建空 book 不会触发)

### 6.4 兼容性迁移路径

**Rust 端**:
- `Order::new` 删除,所有调用点 grep + sed 改 `Order::spot` (主要在 backtest 测试,其他下游 crate 用 `Order` 的地方需逐个改)
- 约 30+ 处测试需要迁移

**Python 端**:
- `OrderDict` 协议**破坏性**变更
- 提供 `axon_quant.backtest.spot_instrument()` / `swap_instrument()` 工厂减少样板
- CHANGELOG 显著标注

**版本号**: `0.4.1` → `0.5.0` (semver breaking)

**版本号同步约束** (来自 project memory):
- `Cargo.toml` workspace.package.version
- `pyproject.toml` version
- `CHANGELOG.md` 顶部
- 22 个 `axon-*` crate 通过 `version.workspace = true` 自动同步
- `make version-check` 验证

## 7. Testing Strategy

### 7.1 单元测试 (`crates/axon-backtest/src/`)

| 文件 | 测试名 | 目的 |
|---|---|---|
| `matching/l1/engine_l1.rs` | `test_multi_instrument_submit_routes_to_correct_book` | book 隔离 |
| `matching/l1/engine_l1.rs` | `test_clear_book_clears_all_instruments` | clear 语义 |
| `engine.rs` | `test_apply_fill_keyed_by_instrument` | spot/perp 仓位独立 |
| `engine.rs` | `test_set_get_target_position` | API 行为 |
| `engine.rs` | `test_mark_event_writes_to_cache` | MarkEvent 写缓存,不触 NAV |
| `engine.rs` | `test_mark_event_overrides_previous` | 同 instrument 后推覆盖 |
| `engine.rs` | `test_eod_liquidate_handles_multiple_instruments` | 跨 instrument 强制平仓 |
| `engine.rs` | `test_leg_targets_in_run_result` | RunResult 字段 |

### 7.2 集成测试 (`crates/axon-integration-tests/src/delta_neutral_arb.rs` 新文件)

端到端:
- spot + perp 同 base 配对
- 推入 spot 买单 + perp 卖单
- 验证两个 book 各自撮合,PositionState 独立
- 推入 MarkEvent
- 验证 `marks` 字段反映
- 调 `set_target_position` 验证 `leg_targets` 字段

### 7.3 Python 端测试 (`python/tests/test_backtest_multi_leg.py` 新文件)

```python
def test_spot_perp_two_leg_routing():
    bt = BacktestEngine(initial_cash=100_000.0)
    spot = spot_instrument("BTC", "USDT")
    perp = swap_instrument("BTC", "USDT", settle="usd_margin")
    bt.push_event({
        "type": "order_submitted", "timestamp_ns": 1_000,
        "order": limit_order(1, spot, "Buy", 100.0, 1.0),
    })
    bt.push_event({
        "type": "order_submitted", "timestamp_ns": 1_000,
        "order": limit_order(2, perp, "Sell", 100.0, 1.0),
    })
    result = bt.run()
    assert result.positions[spot] == 1.0
    assert result.positions[perp] == -1.0
```

### 7.4 回归测试范围

所有现有测试必须通过。约 30+ 测试需批量改 `Order::new` → `Order::spot`。
重点关注:
- `test_run_matched_orders_yield_one_fill` (单 instrument,不受影响)
- `test_max_drawdown_tracks_peak` (6 状态机逻辑不变)
- 所有 `test_apply_*` 6 状态机分支

### 7.5 性能基线 (可选)

`benches/` 目录新增 `bench_multi_instrument_routing.rs`:
- 对比单/多 instrument submit 吞吐
- 预期: HashMap 路由常数开销 < 5%

## 8. Implementation Plan (增量 commit)

**Step 1: 类型 + 序列化 (无破坏)**
- 新建 `crates/axon-core/src/types/instrument.rs`
- 在 `axon-core/src/types/mod.rs` 导出
- `axon-core/src/event.rs` 加 `MarkEvent` + `Event::Mark` variant
- 单元测试 `Instrument` 序列化 / 反序列化
- **commit**: `feat(core): add Instrument enum and MarkEvent`

**Step 2: Order 改造 (breaking)**
- `Order` 字段 `symbol: Symbol` → `instrument: Instrument`
- 删除 `Order::new`,加 `Order::spot` / `Order::swap`
- `OrderBuilder` 同步
- 单元测试覆盖
- **commit**: `feat(core): Order carries Instrument, breaking change`

**Step 3: 引擎 + matching 升级**
- `L1MatchingEngine` 内部 `HashMap<Instrument, L1Book>`
- `BacktestEngine` `position_states` key 改 `Instrument`
- `apply_fill` 签名改 `&Instrument`
- `TradeRecord` 加 `instrument` 字段
- 新增 `set/get_target_position` / `get_position` / `legs` / `mark_cache`
- `RunResult` 加 `leg_targets` / `marks`,`positions` key 改
- 单元测试全部迁移 + 新加
- **commit**: `feat(backtest): support multi-instrument spot+perp two-leg backtest`

**Step 4: PyO3 绑定 + Python 包装**
- `axon-backtest/src/python/` 改 `OrderDict` 协议
- 新增 `spot_instrument` / `swap_instrument` 工厂
- 新增 PyO3 方法
- `python/axon_quant/backtest.py` 同步
- `python/tests/test_backtest_multi_leg.py` 新文件
- **commit**: `feat(python): multi-leg spot+perp support with Instrument protocol`

**Step 5: 文档**
- `CHANGELOG.md` 增 `0.5.0` 段,标注 BREAKING
- `docs/zh/reference/backtest.md` 加新章节
- `docs/en/reference/backtest.md` 同步
- `mkdocs build --strict` 验证
- **commit**: `docs: multi-leg spot+perp backtest, 0.5.0 BREAKING`

**Step 6: 版本对齐**
- `Cargo.toml` workspace.package.version `0.4.1` → `0.5.0`
- `pyproject.toml` version 同步
- `make version-check` 验证
- **commit**: `chore: bump version to 0.5.0`

## 9. Risks and Mitigations

| 风险 | 缓解 |
|---|---|
| `Order` 字段重命名下游 crate 全爆 | grep 找全 `Order { symbol` 用法,批量改;CI 全跑 |
| Python 用户现有 OrderDict 协议破坏 | CHANGELOG 显著标注;文档给迁移示例 |
| HashMap<Instrument, ...> 序列化不兼容旧数据 | 不持久化旧数据,只管内存 |
| 6 状态机漏改某分支 | 现有 6 分支测试已覆盖,加 multi-instrument 隔离测试 |
| PyO3 dict 协议 + Instrument 嵌套 | serde `tag = "kind"` 保证 dict 协议简单 |

## 10. Out of Scope (本次明确不做)

- ❌ FundingEvent 结算逻辑 (留 Python 端 / 未来 spec)
- ❌ 自动 rebalance 触发器
- ❌ MarkEvent 主动触 NAV 重采样
- ❌ Perp 保证金 / leverage / liquidation
- ❌ 严格模式 (必须 register instrument 才能 submit)
- ❌ 多 portfolio 子账 (本设计 cash 唯一)
- ❌ 与 `axon-defi` (EVM 链上 perp DEX) 的 integration

## 11. Future Work

下次 spec 可考虑:
1. **FundingEvent** 全套(事件类型 + 结算公式 + PnL 累加)
2. **NAV 自动重采样** on MarkEvent (从 fill 价切换到 mark 价)
3. **自动 rebalance** 触发器 (`rebalance_interval` 配置)
4. **Perp 保证金** / leverage / 强平价格
5. **Order 原子双 leg 提交** (绑定的 spot+perp 订单 pair,部分成交处理)

## 12. References

- 现有 engine 实现: `crates/axon-backtest/src/engine.rs`
- 现有 matching 实现: `crates/axon-backtest/src/matching/`
- Symbol 类型: `crates/axon-core/src/types/symbol.rs`
- Python 绑定: `crates/axon-backtest/src/python/`
- 现有 CHANGELOG: `CHANGELOG.md`
- Project memory: `is_multiple_of`, `version.workspace = true` 等约束
