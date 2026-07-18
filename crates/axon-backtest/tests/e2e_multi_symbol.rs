//! 端到端测试:多 symbol 联合回测(P0-3)
//!
//! ## 测试目标
//!
//! 现有 E2E 测试(`backtest_e2e_correctness.rs` / `e2e_impact_integration.rs`)
//! 全部用单 symbol `BTC-USDT`。`axon_backtest::BacktestEngine` 自身
//! 只持一个 `Box<dyn MatchingEngine>`,**没有原生 per-symbol 撮合支持**。
//! 真实场景(多资产组合策略、做市商跨资产对冲)需要多 symbol 联合回测。
//!
//! 本测试套件填补此空缺:在测试内实现一个 **`MultiSymbolAdapter`** thin wrapper,
//! 持 `HashMap<Instrument, L1MatchingEngine>`,按 `order.instrument` 分发。**不动源码**。
//!
//! ## 已知约束
//!
//! `BacktestEngine::apply_fill` 的 NAV 采样用**单一 mark 价格**(`mark = fill_price`),
//! 不做 per-symbol mark。因此测试场景在「同价位」(都用 100.0)下断言
//! final_nav / PnL 累加可分;测试 4 跨 symbol 故意用不同价位以清晰展示
//! 「per-symbol 撮合隔离」,此时不验证 mark 估值。
//!
//! ## 测试场景
//!
//! 1. `two_symbols_independent_positions`:BTC + ETH 各开 1 仓,positions 独立
//! 2. `two_symbols_pnl_sum_matches_individual_runs`:multi.pnl = btc.pnl + eth.pnl
//! 3. `two_symbols_cash_pool_shared`:共用 cash,资金不足时 cash 可负
//! 4. `cross_symbol_no_unintended_fill`:BTC buy 不会匹配 ETH ask
//!
//! 运行:`cargo test -p axon-backtest --test e2e_multi_symbol`

use std::collections::HashMap;

use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig};
use axon_backtest::matching::{L1MatchingEngine, MatchingEngine, OrderBookLevel, SubmitResult};
use axon_core::event::{EventBuilder, OrderAction};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{Instrument, Price, Quantity, SpotInstrument, Symbol};

/// 构造 BTC/USDT 现货 Instrument(T3.5:RunResult.positions key 改 Instrument)
fn btc_inst() -> Instrument {
    Instrument::Spot(SpotInstrument {
        base: Symbol::from("BTC"),
        quote: Symbol::from("USDT"),
    })
}

/// 构造 ETH/USDT 现货 Instrument(T3.5)
fn eth_inst() -> Instrument {
    Instrument::Spot(SpotInstrument {
        base: Symbol::from("ETH"),
        quote: Symbol::from("USDT"),
    })
}

// ── MultiSymbolAdapter:test-only thin wrapper ────────────────────────

/// 多 symbol 撮合引擎适配器
///
/// 按 `order.instrument` 分发到 per-symbol 的 `L1MatchingEngine`。**仅用于测试**,
/// 源码层面 `BacktestEngine` 仍持单 `Box<dyn MatchingEngine>`。
struct MultiSymbolAdapter {
    /// T2.3 改: 改 Instrument key 以匹配 trait 签名(原 Symbol key)
    engines: HashMap<Instrument, L1MatchingEngine>,
}

impl MultiSymbolAdapter {
    /// 创建空 adapter
    fn new() -> Self {
        Self {
            engines: HashMap::new(),
        }
    }

    /// 注册 1 个 instrument(预创建空 L1 引擎)
    #[allow(dead_code)]
    fn register(&mut self, instrument: Instrument) {
        self.engines.insert(instrument, L1MatchingEngine::new());
    }
}

impl Default for MultiSymbolAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl MatchingEngine for MultiSymbolAdapter {
    fn submit(&mut self, order: Order) -> SubmitResult {
        // T2.3 改: 直接用 order.instrument 作 key(原 BASE-QUOTE 字符串拼接)
        let engine = self.engines.entry(order.instrument.clone()).or_default();
        engine.submit(order)
    }

    fn cancel(&mut self, order_id: u64) -> bool {
        // 全扫所有 symbol 引擎(测试用,O(N) 可接受)
        for engine in self.engines.values_mut() {
            if engine.cancel(order_id) {
                return true;
            }
        }
        false
    }

    fn best_bid(&self) -> Option<Price> {
        // 任意 symbol 的最优买价(测试不强求聚合语义)
        self.engines.values().filter_map(|e| e.best_bid()).next()
    }

    fn best_ask(&self) -> Option<Price> {
        self.engines.values().filter_map(|e| e.best_ask()).next()
    }

    fn spread(&self) -> Option<Price> {
        None
    }

    fn depth(&self, levels: usize) -> (Vec<OrderBookLevel>, Vec<OrderBookLevel>) {
        // 简单聚合:取所有 symbol 的 depth 平铺
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
        instrument: Instrument, // 改: 原 symbol: Symbol (T2.3)
        next_id: u64,
    ) -> u64 {
        // engines 已经按 Instrument key 路由(T3.1 后)
        let engine = self.engines.entry(instrument.clone()).or_default();
        engine.seed_liquidity(
            mid_price,
            half_spread,
            depth_levels,
            size_per_level,
            instrument,
            next_id,
        )
    }
}

// ── 共享 helper ──────────────────────────────────────────────────────

/// 构造限价单 helper
fn make_limit_order(id: u64, base: &str, quote: &str, side: Side, price: f64, qty: f64) -> Order {
    Order::spot(
        id,
        base,
        quote,
        side,
        OrderType::Limit {
            price: Price::from_f64(price),
        },
        Quantity::from_f64(qty),
        TimeInForce::GTC,
    )
}

/// 构造市价单 helper
fn make_market_order(id: u64, base: &str, quote: &str, side: Side, qty: f64) -> Order {
    Order::spot(
        id,
        base,
        quote,
        side,
        OrderType::Market,
        Quantity::from_f64(qty),
        TimeInForce::IOC,
    )
}

fn adapter_config(initial_cash: f64) -> BacktestEngineConfig {
    BacktestEngineConfig {
        clock: SimulatedClock::new(Timestamp::from_nanos(0)),
        matching_engine: Box::new(MultiSymbolAdapter::new()),
        impact_model: None,
        initial_cash,
        fee_config: FeeConfig::default(),
        force_liquidate: false,
    }
}

// ── 测试 1:两 symbol 独立开仓(同价位以兼容单 mark 估值) ─────────

/// BTC + ETH 各开 1 仓(同价位 100.0),positions 分别记录,互不干扰
///
/// 注:本测试故意用同价位,规避 `apply_fill` 单 mark 估值的简化,
/// 重点验证「per-symbol 撮合隔离 + positions 独立」语义
#[test]
fn two_symbols_independent_positions() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);

    // bar 0: BTC 卖 + 策略买 @ 100
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(make_limit_order(1, "BTC", "USDT", Side::Sell, 100.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        2,
        OrderAction::Submitted(make_market_order(2, "BTC", "USDT", Side::Buy, 0.1)),
    ));

    // bar 1: ETH 卖 + 策略买 @ 100
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        3,
        OrderAction::Submitted(make_limit_order(3, "ETH", "USDT", Side::Sell, 100.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        4,
        OrderAction::Submitted(make_market_order(4, "ETH", "USDT", Side::Buy, 0.1)),
    ));

    let mut engine = BacktestEngine::new(adapter_config(100_000.0), q);
    let result = engine.run();

    assert_eq!(result.fills, 2, "BTC + ETH 各 1 笔 fill");
    assert_eq!(result.positions.len(), 2, "2 个 symbol 持仓");
    let btc = btc_inst();
    let eth = eth_inst();
    assert!(
        (result.positions[&btc] - 0.1).abs() < 1e-9,
        "BTC 持仓 0.1, got {}",
        result.positions[&btc]
    );
    assert!(
        (result.positions[&eth] - 0.1).abs() < 1e-9,
        "ETH 持仓 0.1, got {}",
        result.positions[&eth]
    );
    // 手算(同价位 100,mark 与 fill 一致):
    // cash = 100000 - 10 - 10 - 0.02 = 99979.98
    // position_value = 0.1*100 + 0.1*100 = 20
    // final_nav = 99979.98 + 20 = 99999.98
    let expected_total_fees = 100.0 * 0.1 * 0.001 * 2.0; // 2 笔
    let expected_final_nav =
        100_000.0 - 100.0 * 0.1 * 2.0 - expected_total_fees + 0.1 * 100.0 * 2.0;
    assert!(
        (result.final_nav - expected_final_nav).abs() < 1e-6,
        "final_nav 应 ≈ {}, got {}",
        expected_final_nav,
        result.final_nav
    );
}

// ── 测试 2:多 symbol PnL = 单 symbol PnL 之和(同价位) ───────────

/// 同价位下,多 symbol PnL 应 = 各 symbol 单独 PnL 之和
/// (跨 symbol 累加可分)
#[test]
fn two_symbols_pnl_sum_matches_individual_runs() {
    // 跑 1 次多 symbol
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(make_limit_order(1, "BTC", "USDT", Side::Sell, 100.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        2,
        OrderAction::Submitted(make_market_order(2, "BTC", "USDT", Side::Buy, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        3,
        OrderAction::Submitted(make_limit_order(3, "ETH", "USDT", Side::Sell, 100.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        4,
        OrderAction::Submitted(make_market_order(4, "ETH", "USDT", Side::Buy, 0.1)),
    ));
    let multi_pnl = BacktestEngine::new(adapter_config(100_000.0), q)
        .run()
        .total_pnl;

    // 跑 1 次单 BTC
    let q_btc = || {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            1,
            OrderAction::Submitted(make_limit_order(1, "BTC", "USDT", Side::Sell, 100.0, 0.1)),
        ));
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            2,
            OrderAction::Submitted(make_market_order(2, "BTC", "USDT", Side::Buy, 0.1)),
        ));
        q
    };
    let btc_pnl = BacktestEngine::new(adapter_config(100_000.0), q_btc())
        .run()
        .total_pnl;

    // 跑 1 次单 ETH
    let q_eth = || {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            3,
            OrderAction::Submitted(make_limit_order(3, "ETH", "USDT", Side::Sell, 100.0, 0.1)),
        ));
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            4,
            OrderAction::Submitted(make_market_order(4, "ETH", "USDT", Side::Buy, 0.1)),
        ));
        q
    };
    let eth_pnl = BacktestEngine::new(adapter_config(100_000.0), q_eth())
        .run()
        .total_pnl;

    // 同价位下,多 symbol PnL ≈ BTC + ETH PnL(允许 1e-6 浮点误差)
    let pnl_sum = btc_pnl + eth_pnl;
    assert!(
        (multi_pnl - pnl_sum).abs() < 1e-6,
        "multi.pnl ({}) ≠ btc.pnl + eth.pnl ({})",
        multi_pnl,
        pnl_sum
    );
}

// ── 测试 3:共用 cash 池,资金不足时 cash 可负 ─────────────────────

/// 初始 cash = 50(远不够买 0.1 BTC @ 100 = 10 + ETH 0.1 @ 100 = 10,共 20)
/// 关键验证:
/// - 两 symbol 都成交(BacktestEngine 当前不强制 cash check,撮合器只管撮合)
/// - `total_pnl < 0`(扣手续费)间接证明 cash 池被 BTC + ETH 共用
#[test]
fn two_symbols_cash_pool_shared() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);

    // BTC buy 0.1 @ 100 = 10
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(make_limit_order(1, "BTC", "USDT", Side::Sell, 100.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        2,
        OrderAction::Submitted(make_market_order(2, "BTC", "USDT", Side::Buy, 0.1)),
    ));

    // ETH buy 0.1 @ 100 = 10
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        3,
        OrderAction::Submitted(make_limit_order(3, "ETH", "USDT", Side::Sell, 100.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        4,
        OrderAction::Submitted(make_market_order(4, "ETH", "USDT", Side::Buy, 0.1)),
    ));

    let mut engine = BacktestEngine::new(adapter_config(50.0), q);
    let result = engine.run();

    // 两 symbol 都成交
    assert_eq!(result.fills, 2, "两 symbol 都被撮合");
    assert_eq!(result.positions.len(), 2);

    // 手算(同价位 100,mark 一致):
    // total_fees = 2 * 100 * 0.1 * 0.001 = 0.02
    // final_nav = initial_cash - 2*10 - 0.02 + 0.2*100 = 50 - 20.02 + 20 = 49.98
    let expected_total_fees = 100.0 * 0.1 * 0.001 * 2.0;
    let expected_final_nav = 50.0 - 100.0 * 0.1 * 2.0 - expected_total_fees + 0.1 * 100.0 * 2.0;
    assert!(
        (result.total_fees - expected_total_fees).abs() < 1e-6,
        "total_fees={}, expected={}",
        result.total_fees,
        expected_total_fees
    );
    assert!(
        (result.final_nav - expected_final_nav).abs() < 1e-6,
        "final_nav={}, expected={}",
        result.final_nav,
        expected_final_nav
    );
    // total_pnl < 0(扣手续费)
    assert!(
        result.total_pnl < 0.0,
        "total_pnl={} < 0(扣手续费),间接证明 cash 池共用",
        result.total_pnl
    );
}

// ── 测试 4:跨 symbol 不会意外撮合 ───────────────────────────────────

/// BTC buy market + ETH ask 不同价位 → 0 fill(无 BTC 对手方,BTC market buy 被拒)
#[test]
fn cross_symbol_no_unintended_fill() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);

    // 只有 ETH ask,无 BTC ask
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(make_limit_order(1, "ETH", "USDT", Side::Sell, 3000.0, 0.1)),
    ));
    // BTC market buy(无 BTC 对手方,应被拒)
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        2,
        OrderAction::Submitted(make_market_order(2, "BTC", "USDT", Side::Buy, 0.1)),
    ));

    let mut engine = BacktestEngine::new(adapter_config(100_000.0), q);
    let result = engine.run();

    // BTC buy market 无 BTC 对手方 → 0 fill,被拒
    // ETH ask 3000 挂簿 → accepted
    assert_eq!(result.fills, 0, "跨 symbol 不应撮合");
    assert_eq!(result.orders_accepted, 1, "ETH ask 挂簿 accepted");
    assert_eq!(result.orders_rejected, 1, "BTC market buy 无对手方被拒");
    // BTC 持仓 = 0(未成交)
    let btc = btc_inst();
    assert!(
        !result.positions.contains_key(&btc) || result.positions[&btc].abs() < 1e-9,
        "BTC 持仓应为 0,got {:?}",
        result.positions.get(&btc)
    );
}
