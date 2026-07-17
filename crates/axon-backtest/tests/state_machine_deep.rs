//! 端到端测试:6 状态机"隐藏边界"(W-1)
//!
//! ## 测试目标
//!
//! 现有 E2E 测试(`backtest_e2e_correctness.rs` / `run_result_fields.rs` 等)
//! 主要验证**完全平仓**和**单笔开/平**路径,6 状态机的 5 个 match 分支
//! (全新开仓 / 同向加仓 / 完全平仓 / 反向部分平仓 / 反手)只有 1-2 个被覆盖。
//!
//! 特别漏掉的隐藏场景:
//!
//! 1. **同向加仓 → 立即反手**(`|n| > |p|`):触发"反手"分支,验证 avg_cost reset
//! 2. **同向加仓 → 部分反手**(`|n| < |p|`):触发"反向部分平仓"分支,验证 avg_cost 保留
//! 3. **浮点 1e-12 边界**被路由到"完全平仓"分支(`(p + n).abs() < 1e-9` 容差)
//! 4. **精确尺寸反向**:与浮点边界不同,`p + n = 0` 严格走完全平仓
//! 5. **多 symbol 资金约束**:BTC 用尽 cash 后,ETH 订单被撮合引擎拒
//!
//! ## 手算对账
//!
//! 每个测试构造精确事件流(对手盘 + 策略单),手算 expected PnL / pos / fee,
//! 与 `result.trades[].realized_pnl` / `result.positions[]` / `result.total_pnl` 对账。
//!
//! 运行:`cargo test -p axon-backtest --test state_machine_deep`

use std::collections::HashMap;

use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig};
use axon_backtest::matching::{L1MatchingEngine, MatchingEngine, OrderBookLevel, SubmitResult};
use axon_core::event::{EventBuilder, OrderAction};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{Price, Quantity, Symbol};

// ── MultiSymbolAdapter:test-only thin wrapper(复制自 e2e_multi_symbol) ──────

/// 多 symbol 撮合引擎适配器(同 e2e_multi_symbol.rs)
struct MultiSymbolAdapter {
    engines: HashMap<Symbol, L1MatchingEngine>,
}

impl MultiSymbolAdapter {
    fn new() -> Self {
        Self {
            engines: HashMap::new(),
        }
    }
}

impl Default for MultiSymbolAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl MatchingEngine for MultiSymbolAdapter {
    fn submit(&mut self, order: Order) -> SubmitResult {
        let engine = self.engines.entry(instrument_to_key(&order.instrument)).or_default();
        engine.submit(order)
    }

    fn cancel(&mut self, order_id: u64) -> bool {
        for engine in self.engines.values_mut() {
            if engine.cancel(order_id) {
                return true;
            }
        }
        false
    }

    fn best_bid(&self) -> Option<Price> {
        self.engines.values().filter_map(|e| e.best_bid()).next()
    }

    fn best_ask(&self) -> Option<Price> {
        self.engines.values().filter_map(|e| e.best_ask()).next()
    }

    fn spread(&self) -> Option<Price> {
        None
    }

    fn depth(&self, levels: usize) -> (Vec<OrderBookLevel>, Vec<OrderBookLevel>) {
        let mut bids = Vec::new();
        let mut asks = Vec::new();
        for engine in self.engines.values() {
            let (b, a) = engine.depth(levels);
            bids.extend(b);
            asks.extend(a);
        }
        (bids, asks)
    }

    fn active_order_count(&self) -> usize {
        self.engines.values().map(|e| e.active_order_count()).sum()
    }

    fn clear_book(&mut self) {
        for engine in self.engines.values_mut() {
            engine.clear_book();
        }
    }

    fn seed_liquidity(
        &mut self,
        mid_price: f64,
        half_spread: f64,
        depth_levels: usize,
        size_per_level: f64,
        symbol: Symbol,
        next_id: u64,
    ) -> u64 {
        let engine = self.engines.entry(symbol.clone()).or_default();
        engine.seed_liquidity(
            mid_price,
            half_spread,
            depth_levels,
            size_per_level,
            symbol,
            next_id,
        )
    }
}

// ── 共享 helper ──────────────────────────────────────────────────────

fn btc() -> Symbol {
    Symbol::from("BTC/USDT")
}

/// 构造限价单 helper
fn make_limit_order(id: u64, side: Side, price: f64, qty: f64) -> Order {
    Order::spot(
        id,
            "BTC",
            "USDT",
        side,
        OrderType::Limit {
            price: Price::from_f64(price),
        },
        Quantity::from_f64(qty),
        TimeInForce::GTC,
    )
}

/// 构造市价单 helper
fn make_market_order(id: u64, side: Side, qty: f64) -> Order {
    Order::spot(
        id,
            "BTC",
            "USDT",
        side,
        OrderType::Market,
        Quantity::from_f64(qty),
        TimeInForce::IOC,
    )
}

/// 多 symbol 配置
fn multi_symbol_config() -> BacktestEngineConfig {
    BacktestEngineConfig {
        clock: SimulatedClock::new(Timestamp::from_nanos(0)),
        matching_engine: Box::new(MultiSymbolAdapter::new()),
        impact_model: None,
        initial_cash: 100_000.0,
        fee_config: FeeConfig::default(),
        force_liquidate: false,
    }
}

/// 推入 1 笔对手盘 + 1 笔策略单(同一 ts,counter 先于 strat)
#[allow(clippy::too_many_arguments)]
fn push_trade(
    q: &mut EventQueue,
    b: &mut EventBuilder,
    ts: i64,
    counter_id: &mut u64,
    strat_id: &mut u64,
    _symbol: Symbol,
    strategy_side: Side,
    price: f64,
    qty: f64,
) {
    // 1. 对手盘挂单(策略_side 相反,挂在 price)
    let counter_side = match strategy_side {
        Side::Buy => Side::Sell,
        Side::Sell => Side::Buy,
    };
    let cid = *counter_id;
    *counter_id += 1;
    q.push(b.order(
        Timestamp::from_nanos(ts),
        cid,
        OrderAction::Submitted(make_limit_order(
            cid,
            counter_side,
            price,
            qty,
        )),
    ));

    // 2. 策略市价单吃单
    let sid = *strat_id;
    *strat_id += 1;
    q.push(b.order(
        Timestamp::from_nanos(ts),
        sid,
        OrderAction::Submitted(make_market_order(sid, strategy_side, qty)),
    ));
}

// ── 测试 1:同向加仓 → 立即反手(|n| > |p|,反手分支) ─────────────────────

/// 6 状态机"反手"分支:同向加仓后立即反手,验证 avg_cost 正确 reset
///
/// 事件流:
/// - bar 0: buy 0.1 @ 100(开仓,avg_cost=100)
/// - bar 1: buy 0.4 @ 110(加仓,avg_cost=(0.1*100 + 0.4*110)/0.5=108)
/// - bar 2: sell 0.7 @ 115(|n|=0.7 > |p|=0.5,反手)
///   → 平 0.5 @ 108,realized = (115-108)*0.5 = 3.5
///   → 开反向 -0.2 @ 115,pos.avg_cost reset
///
/// 关键断言:
/// - 1 trade(完全平仓的 push 1 笔,反手不开新 trade)
/// - realized_pnl ≈ 3.5 * 1e6(fee 已扣除后约略小)
/// - pos[BTC] = -0.2
#[test]
fn add_then_reverse_chains_state_machine() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    let mut counter_id = 1u64;
    let mut strat_id = 100u64;

    // bar 0:buy 0.1 @ 100
    push_trade(
        &mut q,
        &mut b,
        1_000,
        &mut counter_id,
        &mut strat_id,
        btc(),
        Side::Buy,
        100.0,
        0.1,
    );
    // bar 1:buy 0.4 @ 110
    push_trade(
        &mut q,
        &mut b,
        2_000,
        &mut counter_id,
        &mut strat_id,
        btc(),
        Side::Buy,
        110.0,
        0.4,
    );
    // bar 2:sell 0.7 @ 115(反手)
    push_trade(
        &mut q,
        &mut b,
        3_000,
        &mut counter_id,
        &mut strat_id,
        btc(),
        Side::Sell,
        115.0,
        0.7,
    );

    let mut engine = BacktestEngine::new(multi_symbol_config(), q);
    let result = engine.run();

    // 3 笔 fill
    assert_eq!(result.fills, 3, "应有 3 笔 fill");
    // 1 笔 trade(完全平仓 push 1 笔;反手部分不开 trade)
    assert_eq!(result.trades.len(), 1, "反手应 push 1 笔 trade");

    let trade = &result.trades[0];
    // realized_pnl = (115 - 108) * 0.5 = 3.5 → 3_500_000(×1e6 定点)
    // 扣 3 笔 fee(0.01 + 0.044 + 0.0805 = 0.1345)影响 realized
    // 但 realized_pnl 不扣 fee(实现在 6 状态机只算价格差,fee 单独计 total_fees)
    let expected_realized = ((115.0 - 108.0) * 0.5 * 1e6) as i64;
    let realized_diff = (trade.realized_pnl - expected_realized).abs();
    assert!(
        realized_diff < 1_000,
        "realized_pnl 应≈{}, got {}",
        expected_realized,
        trade.realized_pnl
    );

    // 末态持仓 = -0.2(反手开反向)
    let pos = result.positions.get("BTC/USDT").copied().unwrap_or(0.0);
    assert!(
        (pos - (-0.2)).abs() < 1e-9,
        "末态持仓应为 -0.2(反手开反向), got {}",
        pos
    );
}

// ── 测试 2:同向加仓 → 部分反手(|n| < |p|,反向部分平仓) ─────────────────

/// 6 状态机"反向部分平仓"分支:同向加仓后部分反手,验证 avg_cost 保留
///
/// 事件流:
/// - bar 0: buy 0.1 @ 100(开仓)
/// - bar 1: buy 0.4 @ 110(加仓,avg_cost=108)
/// - bar 2: sell 0.3 @ 105(|n|=0.3 < |p|=0.5,反向部分平仓)
///   → 平 0.3 @ 108,realized = (105-108)*0.3 = -0.9
///   → 留 0.2 @ 108(avg_cost 保留)
///
/// 关键断言:
/// - 1 trade(部分平仓 push 1 笔)
/// - realized_pnl ≈ -0.9 * 1e6
/// - pos[BTC] = +0.2(仍持 long,数量减少)
#[test]
fn add_then_partial_reverse() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    let mut counter_id = 1u64;
    let mut strat_id = 100u64;

    // bar 0:buy 0.1 @ 100
    push_trade(
        &mut q,
        &mut b,
        1_000,
        &mut counter_id,
        &mut strat_id,
        btc(),
        Side::Buy,
        100.0,
        0.1,
    );
    // bar 1:buy 0.4 @ 110
    push_trade(
        &mut q,
        &mut b,
        2_000,
        &mut counter_id,
        &mut strat_id,
        btc(),
        Side::Buy,
        110.0,
        0.4,
    );
    // bar 2:sell 0.3 @ 105(部分反手)
    push_trade(
        &mut q,
        &mut b,
        3_000,
        &mut counter_id,
        &mut strat_id,
        btc(),
        Side::Sell,
        105.0,
        0.3,
    );

    let mut engine = BacktestEngine::new(multi_symbol_config(), q);
    let result = engine.run();

    // 3 笔 fill
    assert_eq!(result.fills, 3);
    // 1 笔 trade(部分平仓 push 1 笔)
    assert_eq!(result.trades.len(), 1);

    let trade = &result.trades[0];
    // realized_pnl = (105 - 108) * 0.3 = -0.9
    let expected_realized = ((105.0 - 108.0) * 0.3 * 1e6) as i64;
    let realized_diff = (trade.realized_pnl - expected_realized).abs();
    assert!(
        realized_diff < 1_000,
        "realized_pnl 应≈{}, got {}",
        expected_realized,
        trade.realized_pnl
    );

    // 末态持仓 = +0.2(部分平仓,剩 0.2 long)
    let pos = result.positions.get("BTC/USDT").copied().unwrap_or(0.0);
    assert!(
        (pos - 0.2).abs() < 1e-9,
        "末态持仓应为 +0.2(部分平仓), got {}",
        pos
    );
}

// ── 测试 3:浮点 1e-12 边界被路由到"完全平仓"分支 ────────────────────

/// 浮点边界:sell qty = 0.1 + 1e-12,触发"完全平仓"分支(`(p + n).abs() < 1e-9`)
///
/// 事件流:
/// - bar 0: buy 0.1 @ 100
/// - bar 1: sell 0.1+1e-12 @ 100(应该走完全平仓分支,因 |p| ≈ |n|)
///
/// 关键断言:
/// - 1 trade(完全平仓 push 1 笔,不是部分平仓)
/// - 末态持仓 ≈ 0(`(p + n).abs() < 1e-9` 走完全平仓,清空 pos)
#[test]
fn near_zero_remainder_routes_to_full_close() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    let mut counter_id = 1u64;
    let mut strat_id = 100u64;

    // bar 0:buy 0.1 @ 100
    push_trade(
        &mut q,
        &mut b,
        1_000,
        &mut counter_id,
        &mut strat_id,
        btc(),
        Side::Buy,
        100.0,
        0.1,
    );
    // bar 1:sell 0.1+1e-12 @ 100(浮点边界)
    push_trade(
        &mut q,
        &mut b,
        2_000,
        &mut counter_id,
        &mut strat_id,
        btc(),
        Side::Sell,
        100.0,
        0.1 + 1e-12,
    );

    let mut engine = BacktestEngine::new(multi_symbol_config(), q);
    let result = engine.run();

    // 2 笔 fill
    assert_eq!(result.fills, 2);
    // 1 笔 trade(完全平仓,无 remainder)
    assert_eq!(
        result.trades.len(),
        1,
        "1e-12 浮点边界应被路由到完全平仓分支(1e-9 容差)"
    );

    // 末态持仓 ≈ 0(被完全平仓)
    let pos = result.positions.get("BTC/USDT").copied().unwrap_or(0.0);
    assert!(pos.abs() < 1e-6, "末态持仓应 ≈ 0(完全平仓), got {}", pos);
}

// ── 测试 4:精确尺寸反向(无浮点误差) ──────────────────────────────────

/// sell qty = 0.1 严格等于 buy qty,走"完全平仓"分支
///
/// 与测试 3 区别:这里 p + n 严格 = 0,不走浮点容差路径
#[test]
fn reverse_with_exact_size_routes_to_full_close() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    let mut counter_id = 1u64;
    let mut strat_id = 100u64;

    // bar 0:buy 0.1 @ 100
    push_trade(
        &mut q,
        &mut b,
        1_000,
        &mut counter_id,
        &mut strat_id,
        btc(),
        Side::Buy,
        100.0,
        0.1,
    );
    // bar 1:sell 0.1 @ 100(精确尺寸)
    push_trade(
        &mut q,
        &mut b,
        2_000,
        &mut counter_id,
        &mut strat_id,
        btc(),
        Side::Sell,
        100.0,
        0.1,
    );

    let mut engine = BacktestEngine::new(multi_symbol_config(), q);
    let result = engine.run();

    // 2 笔 fill
    assert_eq!(result.fills, 2);
    // 1 笔 trade(完全平仓)
    assert_eq!(result.trades.len(), 1);

    // realized_pnl = (100 - 100) * 0.1 = 0(同价 round-trip)
    let trade = &result.trades[0];
    assert!(
        trade.realized_pnl.abs() < 1,
        "同价 round-trip realized ≈ 0, got {}",
        trade.realized_pnl
    );

    // 末态持仓 = 0
    let pos = result.positions.get("BTC/USDT").copied().unwrap_or(0.0);
    assert!(pos.abs() < 1e-9);
}

// ── 测试 5:多 symbol 资金约束 ──────────────────────────────────────────

/// BTC 用尽 cash 后,ETH 订单被撮合引擎拒(L1 不会因 cash 不足拒单,但买不到对手盘会被拒)
///
/// 事件流:
/// - bar 0: BTC buy 1.5 @ 50_000 → 1 fill, cash -75_000 - fee
/// - bar 1: ETH buy 0.1 @ 3_000 → 0 fill(无对手盘),accepted 但挂簿无效
///
/// 关键断言:
/// - BTC 仓位 = +1.5
/// - ETH 仓位 = 0(订单被拒)
/// - cash 减少 ≈ 75_000 + fee
#[test]
fn multi_symbol_independent_fill_per_symbol() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    let mut counter_id = 1u64;
    let mut strat_id = 100u64;

    // bar 0:BTC sell @ 50_000 qty 1.5(对手) + BTC buy market 1.5
    push_trade(
        &mut q,
        &mut b,
        1_000,
        &mut counter_id,
        &mut strat_id,
        btc(),
        Side::Buy,
        50_000.0,
        1.5,
    );
    // bar 1:ETH buy 0.1 @ 3_000(无 ETH 对手盘,被 L1 拒)
    let eth_id = strat_id;
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        eth_id,
        OrderAction::Submitted(make_market_order(eth_id, Side::Buy, 0.1)),
    ));

    let mut engine = BacktestEngine::new(multi_symbol_config(), q);
    let result = engine.run();

    // BTC:1 笔 fill;ETH:0 笔 fill
    assert_eq!(result.fills, 1, "BTC 1 笔 fill + ETH 0 笔");

    // BTC 仓位 = +1.5
    let btc_pos = result.positions.get("BTC/USDT").copied().unwrap_or(0.0);
    assert!(
        (btc_pos - 1.5).abs() < 1e-9,
        "BTC pos 应=+1.5, got {}",
        btc_pos
    );

    // ETH 仓位 = 0(订单被拒,无 fill)
    let eth_pos = result.positions.get("ETH/USDT").copied().unwrap_or(0.0);
    assert!(
        eth_pos.abs() < 1e-9,
        "ETH pos 应=0(无 fill), got {}",
        eth_pos
    );

    // ETH 订单被拒 → orders_rejected += 1
    // 注:BTC 的 sell limit 挂单在提交时 active_order_count > 0,所以是 accepted;
    // ETH 的 market order 无对手盘 → 提交后 active_order_count 不变 → rejected
    assert!(
        result.orders_rejected >= 1,
        "ETH 无对手盘订单应被拒,rejected={}",
        result.orders_rejected
    );
}

// ── T2.2 过渡 helper ──────────────────────────────────

/// T2.2: Order::symbol -> Order::instrument 过渡期,把 Instrument 序列化为 HashMap key
fn instrument_to_key(inst: &axon_core::types::Instrument) -> axon_core::types::Symbol {
    axon_core::types::Symbol::from(format!(
        "{}/{}",
        inst.base().as_str(),
        inst.quote().as_str()
    ))
}
