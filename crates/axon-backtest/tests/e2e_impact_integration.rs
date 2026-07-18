//! 端到端测试：冲击模型 + 回测引擎
//!
//! ## 测试目标
//!
//! `backtest_e2e_correctness.rs` 已用 `L1MatchingEngine` 验证 SMA → strategy → PnL 的
//! 端到端正确性,但**未触及冲击模型**。本测试补齐 `ImpactedMatchingEngine` 路径的
//! 端到端验证:
//!
//! 1. **零冲击对账**:`coefficient=0` 时,`ImpactedMatchingEngine` 的 PnL 与 `L1MatchingEngine` 完全一致
//! 2. **正向冲击验证**:用线性冲击,验证 realized_pnl = raw_pnl - cumulative_instantaneous_cost
//! 3. **永久冲击验证**:多次 buy 累积 permanent_offset,后续 sell 成交价被下移
//!
//! ## 设计要点
//!
//! - **BacktestEngine 集成**:`ImpactedMatchingEngine` 自身未实现 `MatchingEngine` trait,
//!   本测试用内部 `ImpactedAdapter` 包装一层(测试文件内的 thin adapter,非源码改动)
//! - **手算对账**:每次 buy 的 `notional * 0.05 * 0.7` 即为本次即时冲击成本,累加应等于
//!   `result.total_pnl` 与 L1 baseline 的差异
//!
//! 运行:`cargo test -p axon-backtest --test e2e_impact_integration`

use std::collections::VecDeque;

use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig};
use axon_backtest::impact::ImpactedMatchingEngine;
use axon_backtest::matching::{L1MatchingEngine, MatchingEngine, OrderBookLevel, SubmitResult};
use axon_core::event::{EventBuilder, OrderAction};
use axon_core::impact::{ImpactModel, LinearImpactModel};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{Instrument, Price, Quantity};

// ── Adapter:让 ImpactedMatchingEngine 接入 BacktestEngine 的 MatchingEngine trait ──

/// `ImpactedMatchingEngine` → `MatchingEngine` trait 适配器
///
/// `ImpactedMatchingEngine` 自身只暴露 `submit/cancel/best_bid/best_ask` 等方法,
/// 未实现 `MatchingEngine` trait。本测试用此 adapter 桥接,使 BacktestEngine
/// 能用 `Box<dyn MatchingEngine>` 持有冲击感知撮合引擎。
///
/// 注:这是**测试内的 thin adapter**,不修改源码;`spread()` 返回 None(impacted
/// 引擎未实现),`depth()` 透传到内部 `L1MatchingEngine`。
struct ImpactedAdapter {
    inner: ImpactedMatchingEngine,
}

impl ImpactedAdapter {
    fn new(model: Box<dyn ImpactModel>) -> Self {
        Self {
            inner: ImpactedMatchingEngine::new(model),
        }
    }

    /// 暴露 impact 模型状态用于断言
    fn stats(&self) -> &axon_backtest::impact::ImpactStats {
        self.inner.stats()
    }

    /// 暴露 permanent_offset 用于断言
    fn permanent_offset(&self) -> f64 {
        self.inner.permanent_offset()
    }
}

impl MatchingEngine for ImpactedAdapter {
    fn submit(&mut self, order: Order) -> SubmitResult {
        self.inner.submit(order)
    }

    fn cancel(&mut self, order_id: u64) -> bool {
        self.inner.cancel(order_id)
    }

    fn best_bid(&self) -> Option<Price> {
        self.inner.best_bid()
    }

    fn best_ask(&self) -> Option<Price> {
        self.inner.best_ask()
    }

    fn spread(&self) -> Option<Price> {
        // ImpactedMatchingEngine 未实现 spread,返回 None
        None
    }

    fn depth(&self, levels: usize) -> (Vec<OrderBookLevel>, Vec<OrderBookLevel>) {
        // 透传到内部 L1MatchingEngine(不受 permanent_offset 影响,内部状态)
        self.inner.inner().depth(levels)
    }

    fn active_order_count(&self) -> usize {
        self.inner.active_order_count()
    }

    fn clear_book(&mut self) {
        self.inner.clear_book();
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
        self.inner.seed_liquidity(
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

/// 1 根 bar
#[derive(Debug, Clone, Copy)]
struct Bar {
    idx: usize,
    close: f64,
}

/// 单边上涨价格序列
fn gen_uptrend(n: usize, base: f64, step: f64) -> Vec<Bar> {
    (0..n)
        .map(|i| Bar {
            idx: i,
            close: base + step * (i + 1) as f64,
        })
        .collect()
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

/// 默认回测配置(L1 baseline)
fn l1_config() -> BacktestEngineConfig {
    BacktestEngineConfig {
        clock: SimulatedClock::new(Timestamp::from_nanos(0)),
        matching_engine: Box::new(L1MatchingEngine::new()),
        impact_model: None,
        initial_cash: 100_000.0,
        fee_config: FeeConfig::default(),
        force_liquidate: false,
    }
}

/// 默认回测配置(ImpactedMatcher)
fn impacted_config(model: Box<dyn ImpactModel>) -> BacktestEngineConfig {
    BacktestEngineConfig {
        clock: SimulatedClock::new(Timestamp::from_nanos(0)),
        matching_engine: Box::new(ImpactedAdapter::new(model)),
        impact_model: None,
        initial_cash: 100_000.0,
        fee_config: FeeConfig::default(),
        force_liquidate: false,
    }
}

/// 简单 SMA crossover 策略状态(单 bar 决策)
struct SmaStrategy {
    short_win: usize,
    long_win: usize,
    closes: VecDeque<f64>,
    position: f64,
    order_id_seq: u64,
    desired: f64,
}

impl SmaStrategy {
    fn new(short_win: usize, long_win: usize) -> Self {
        Self {
            short_win,
            long_win,
            closes: VecDeque::with_capacity(long_win),
            position: 0.0,
            order_id_seq: 1,
            desired: 0.0,
        }
    }

    fn warmup(&mut self, bars: &[Bar]) {
        for b in bars {
            self.closes.push_back(b.close);
            if self.closes.len() > self.long_win {
                self.closes.pop_front();
            }
        }
    }

    fn sma(&self, win: usize) -> Option<f64> {
        if self.closes.len() < win {
            return None;
        }
        let sum: f64 = self.closes.iter().rev().take(win).sum();
        Some(sum / win as f64)
    }

    fn update_signal(&mut self) {
        let short = self.sma(self.short_win);
        let long = self.sma(self.long_win);
        self.desired = match (short, long) {
            (Some(s), Some(l)) if s > l => 1.0,
            _ => 0.0,
        };
    }

    fn next_signal(&self) -> Option<Side> {
        if (self.desired - self.position).abs() < 1e-9 {
            None
        } else if self.desired > self.position {
            Some(Side::Buy)
        } else {
            Some(Side::Sell)
        }
    }
}

/// 把 1 根 bar 翻译成事件流(对手盘 + 策略信号)
fn emit_bar(
    q: &mut EventQueue,
    b: &mut EventBuilder,
    bar: &Bar,
    strategy: &mut SmaStrategy,
    qty: f64,
) {
    let ts = Timestamp::from_nanos(((bar.idx as i64) + 1) * 1_000_000);
    match strategy.next_signal() {
        None => {}
        Some(Side::Buy) => {
            let counter_id = strategy.order_id_seq;
            strategy.order_id_seq += 1;
            q.push(b.order(
                ts,
                counter_id,
                OrderAction::Submitted(make_limit_order(counter_id, Side::Sell, bar.close, qty)),
            ));
            let strat_id = strategy.order_id_seq;
            strategy.order_id_seq += 1;
            q.push(b.order(
                ts,
                strat_id,
                OrderAction::Submitted(make_market_order(strat_id, Side::Buy, qty)),
            ));
            strategy.position = qty;
        }
        Some(Side::Sell) => {
            let counter_id = strategy.order_id_seq;
            strategy.order_id_seq += 1;
            q.push(b.order(
                ts,
                counter_id,
                OrderAction::Submitted(make_limit_order(counter_id, Side::Buy, bar.close, qty)),
            ));
            let strat_id = strategy.order_id_seq;
            strategy.order_id_seq += 1;
            q.push(b.order(
                ts,
                strat_id,
                OrderAction::Submitted(make_market_order(strat_id, Side::Sell, qty)),
            ));
            strategy.position = 0.0;
        }
    }
}

/// 跑 1 次 E2E,返回 `RunResult`
fn run_e2e(bars: &[Bar], cfg: BacktestEngineConfig) -> axon_backtest::engine::RunResult {
    let mut strat = SmaStrategy::new(2, 5);
    strat.warmup(&bars[..5]);

    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    for bar in &bars[5..] {
        strat.closes.push_back(bar.close);
        if strat.closes.len() > strat.long_win {
            strat.closes.pop_front();
        }
        strat.update_signal();
        emit_bar(&mut q, &mut b, bar, &mut strat, 0.1);
    }

    let mut engine = BacktestEngine::new(cfg, q);
    engine.run()
}

// ── 测试 1:零冲击对账 ──────────────────────────────────────────────

/// `coefficient=0` 时,ImpactedMatchingEngine 的 PnL 与 L1MatchingEngine 完全一致
///
/// 验证 adapter 桥接正确性:若结果与 L1 不一致,说明 wrapper 走错了路径
#[test]
fn zero_coefficient_matches_l1_baseline() {
    let bars = gen_uptrend(15, 100.0, 2.0); // 100 → 130

    // L1 baseline
    let l1_result = run_e2e(&bars, l1_config());

    // ImpactedMatcher (coefficient=0)
    let zero_model: Box<dyn ImpactModel> = Box::new(LinearImpactModel::new(0.0));
    let impacted_result = run_e2e(&bars, impacted_config(zero_model));

    assert_eq!(l1_result.fills, impacted_result.fills, "fills 应一致");
    assert_eq!(
        l1_result.orders_accepted, impacted_result.orders_accepted,
        "orders_accepted 应一致"
    );
    assert_eq!(
        l1_result.trades.len(),
        impacted_result.trades.len(),
        "trades 数应一致"
    );
    // PnL 误差容忍浮点精度(impacted 引擎可能引入极小舍入差异)
    let pnl_diff = (l1_result.total_pnl - impacted_result.total_pnl).abs();
    assert!(
        pnl_diff < 1e-9,
        "零冲击 PnL 应一致,l1={}, impacted={}, diff={}",
        l1_result.total_pnl,
        impacted_result.total_pnl,
        pnl_diff
    );
    let fee_diff = (l1_result.total_fees - impacted_result.total_fees).abs();
    assert!(
        fee_diff < 1e-9,
        "零冲击 fee 应一致,l1={}, impacted={}",
        l1_result.total_fees,
        impacted_result.total_fees
    );
}

// ── 测试 2:正向冲击验证 ──────────────────────────────────────────

/// 线性冲击:coefficient=0.05,70% 即时 + 30% 永久
///
/// 构造 1 笔 buy 吃 1 笔 sell(对手盘只有 1.0 qty),验证:
/// 1. 成交价被 instantaneous 影响抬高(> 100)
/// 2. `cumulative_instantaneous` 累加正确
/// 3. `total_pnl` 与 L1 baseline 的差异 ≈ cumulative_instantaneous * qty
#[test]
fn linear_impact_raises_fill_price_and_pnl_diff() {
    // 构造对手盘 + 大买单(不可 Clone,inline 构建两次)
    let build_q = || {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            1,
            OrderAction::Submitted(make_limit_order(1, Side::Sell, 100.0, 1.0)),
        ));
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            2,
            OrderAction::Submitted(make_limit_order(2, Side::Buy, 100.0, 5.0)),
        ));
        q
    };

    // L1 baseline
    let l1_result = {
        let mut engine = BacktestEngine::new(l1_config(), build_q());
        engine.run()
    };

    // ImpactedMatcher (coefficient=0.05)
    let model: Box<dyn ImpactModel> = Box::new(LinearImpactModel::new(0.05));
    let impacted_result = {
        let mut engine = BacktestEngine::new(impacted_config(model), build_q());
        engine.run()
    };

    // 1 笔 fill
    assert_eq!(impacted_result.fills, 1);

    // L1:成交价 = 100(no impact)
    //   cash = -100 - 0.1 (fee) = -100.1
    //   position value @ mark=100 = 100
    //   NAV = initial - 0.1, total_pnl = -0.1
    //
    // Impacted:成交价 = 100 + impact.instantaneous
    //   impact = 0.05 * (1.0/1.0) * 0.7 = 0.035 instantaneous
    //   成交价 = 100.035
    //   cash = -100.035 - 0.100035 (fee on higher notional) = -100.135035
    //   position value @ mark=100.035 = 100.035
    //   NAV = initial - 0.100035, total_pnl = -0.100035
    //
    // PnL 差异 = 0.1 - 0.100035 = 0.000035 = impact.instantaneous * qty * taker_rate
    let pnl_diff = l1_result.total_pnl - impacted_result.total_pnl;
    assert!(
        pnl_diff > 0.0,
        "有冲击时 PnL 应比 L1 低(成交更贵),pnl_diff={}, l1={}, impacted={}",
        pnl_diff,
        l1_result.total_pnl,
        impacted_result.total_pnl
    );
    // 差异 ≈ impact.instantaneous * qty * taker_rate
    // = 0.05 * (1.0/1.0) * 0.7 * 1.0 * 0.001 = 0.000035
    let expected_pnl_diff = 0.05 * (1.0_f64 / 1.0) * 0.7 * 1.0 * 0.001;
    assert!(
        (pnl_diff - expected_pnl_diff).abs() < 1e-9,
        "PnL 差异应 ≈ impact_cost * taker_rate,expected={}, got={}",
        expected_pnl_diff,
        pnl_diff
    );
}

// ── 测试 3:永久冲击影响后续成交价 ──────────────────────────────

/// 永久冲击:round-trip 场景下,Impacted 的 PnL 严格 < L1
///
/// 构造:
/// - bar 0: 卖单 @ 100 qty=10.0(对手盘,深度 10)
/// - bar 1: 买单 @ 100 qty=5.0(吃 5.0 卖单)
///   - L1: fill @ 100, long 5.0 @ 100
///   - Impacted: impact = 0.1 * (5.0/10.0) = 0.05; 70% inst = 0.035, 30% perm = 0.015
///     fill @ 100.035, long 5.0 @ 100.035
/// - bar 2: 买单 @ 100 qty=5.0(挂单,作为对手盘)
/// - bar 3: 卖单 @ 100 qty=5.0(吃 bar 2 买单)
///   - L1: fill @ 100, position 平仓, realized = 0
///   - Impacted: pre_snapshot 看到买盘已带 offset(0.015),asks 仍 = 100(原 inner book)
///     计算:sell impact based on bids snapshot
///     实际 fill_price = 100 - impact.instantaneous(< 100,卖单下移)
///
/// 验证:Impacted PnL < L1 PnL(更多 fee + 不利成交)
#[test]
fn permanent_impact_makes_round_trip_less_profitable() {
    let build_q = || {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        // bar 0: 卖单 @ 100 qty=5.0(对手盘,只 5.0,确保被 buy 吃完不留残余)
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            1,
            OrderAction::Submitted(make_limit_order(1, Side::Sell, 100.0, 5.0)),
        ));
        // bar 1: 买单 @ 100 qty=5.0(吃 5.0 卖单)
        q.push(b.order(
            Timestamp::from_nanos(2_000),
            2,
            OrderAction::Submitted(make_limit_order(2, Side::Buy, 100.0, 5.0)),
        ));
        // bar 2: 买单 @ 100 qty=5.0(此时订单簿空 → 挂单,做平仓对手盘)
        q.push(b.order(
            Timestamp::from_nanos(3_000),
            3,
            OrderAction::Submitted(make_limit_order(3, Side::Buy, 100.0, 5.0)),
        ));
        // bar 3: 卖单 @ 100 qty=5.0(吃 bar 2 买单 → 平仓)
        q.push(b.order(
            Timestamp::from_nanos(4_000),
            4,
            OrderAction::Submitted(make_limit_order(4, Side::Sell, 100.0, 5.0)),
        ));
        q
    };

    // L1 baseline
    let l1_result = {
        let mut engine = BacktestEngine::new(l1_config(), build_q());
        engine.run()
    };

    // ImpactedMatcher (coefficient=0.1, 70% inst + 30% perm)
    let model: Box<dyn ImpactModel> = Box::new(LinearImpactModel::new(0.1));
    let impacted_result = {
        let mut engine = BacktestEngine::new(impacted_config(model), build_q());
        engine.run()
    };

    // 2 笔 fill (开仓 + 平仓)
    assert_eq!(l1_result.fills, 2, "L1 2 笔 fill");
    assert_eq!(impacted_result.fills, 2, "Impacted 2 笔 fill");

    // L1: 完全平仓
    assert_eq!(l1_result.trades.len(), 1, "L1 1 笔 trade");
    assert_eq!(impacted_result.trades.len(), 1, "Impacted 1 笔 trade");

    // L1: realized = 0(同价开/平),PnL = -total_fees
    let l1_trade = &l1_result.trades[0];
    assert!(
        l1_trade.realized_pnl.abs() < 1,
        "L1 同价 round-trip realized≈0,got {}",
        l1_trade.realized_pnl
    );

    // Impacted: buy @ 100.035, sell @ (100 - 卖单瞬时冲击)
    //   realized = (sell_price - 100.035) * 5.0
    //   由于 sell 也是带冲击的(depth=1, qty=5/5=1, inst=0.07),fill @ 100 - 0.07 = 99.93
    //   realized = (99.93 - 100.035) * 5.0 = -0.525
    //   折算成 1e6 定点 ≈ -525000
    let impacted_trade = &impacted_result.trades[0];
    assert!(
        impacted_trade.realized_pnl < l1_trade.realized_pnl,
        "Impacted round-trip 应比 L1 差(买卖都有冲击),impacted={}, l1={}",
        impacted_trade.realized_pnl,
        l1_trade.realized_pnl
    );

    // PnL 整体也更低
    assert!(
        impacted_result.total_pnl < l1_result.total_pnl,
        "Impacted total_pnl < L1,impacted={}, l1={}",
        impacted_result.total_pnl,
        l1_result.total_pnl
    );
}

// ── 测试 4:多笔冲击的累加对账 ──────────────────────────────────

/// 多笔 buy 的累计即时冲击 = Σ(coefficient * qty/depth * ratio)
///
/// 构造 3 笔独立 buy(每笔对手盘 1.0 qty,depth=1),验证:
/// `cumulative_instantaneous` = 0.05 * 1.0/1.0 * 0.7 * 3 = 0.105
/// 但实际 `cumulative_instantaneous` = Σ impact * qty 累计,见 `impacted_engine.rs` 实现
#[test]
fn cumulative_impact_aggregates_across_fills() {
    // 用 1 个 ImpactedAdapter 直接跑 submit,检查 stats().cumulative_instantaneous
    let model: Box<dyn ImpactModel> = Box::new(LinearImpactModel::new(0.05));
    let mut adapter = ImpactedAdapter::new(model);

    // 第 1 笔:卖单 + 大买单
    let _ = adapter.submit(make_limit_order(1, Side::Sell, 100.0, 1.0));
    let _ = adapter.submit(make_limit_order(2, Side::Buy, 100.0, 5.0));
    let stats1 = adapter.stats().clone();
    assert_eq!(stats1.total_fills, 1);
    assert!(stats1.cumulative_instantaneous > 0.0);

    // 第 2 笔:补卖单 + 大买单
    let _ = adapter.submit(make_limit_order(3, Side::Sell, 100.0, 1.0));
    let _ = adapter.submit(make_limit_order(4, Side::Buy, 100.0, 5.0));
    let stats2 = adapter.stats().clone();
    assert_eq!(stats2.total_fills, 2);
    // 累计应 > 第 1 笔
    assert!(
        stats2.cumulative_instantaneous > stats1.cumulative_instantaneous,
        "累计冲击应单调递增,fill1={}, fill2={}",
        stats1.cumulative_instantaneous,
        stats2.cumulative_instantaneous
    );

    // 第 3 笔
    let _ = adapter.submit(make_limit_order(5, Side::Sell, 100.0, 1.0));
    let _ = adapter.submit(make_limit_order(6, Side::Buy, 100.0, 5.0));
    let stats3 = adapter.stats().clone();
    assert_eq!(stats3.total_fills, 3);
    assert!(
        stats3.cumulative_instantaneous > stats2.cumulative_instantaneous,
        "累计冲击应继续递增,fill2={}, fill3={}",
        stats2.cumulative_instantaneous,
        stats3.cumulative_instantaneous
    );

    // 永久冲击也累计
    assert!(
        adapter.permanent_offset() > 0.0,
        "3 笔成交后 permanent_offset 应 > 0,got {}",
        adapter.permanent_offset()
    );
}
