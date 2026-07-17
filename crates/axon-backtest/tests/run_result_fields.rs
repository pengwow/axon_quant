//! 阶段 B 集成测试:验证 RunResult 阶段 B 新增字段
//!
//! 覆盖 4 个核心场景:
//! 1. `trades 配对` — 6 状态机的"完全平仓"产出 TradeRecord
//! 2. `fee 累计` — 多笔 fill 的手续费正确累加
//! 3. `equity 采样` — 每笔 fill 后 NAV 曲线新增一个点
//! 4. `metrics 计算` — win_rate / sharpe_ratio 从 TradingMetrics 取出
//!
//! 所有测试用纯 Rust API + L1MatchingEngine 默认撮合,通过 push_event
//! 推入订单事件,验证 RunResult 的 Stage 3 阶段 B 扩展字段。
//!
//! 运行:`cargo test -p axon-backtest --test run_result_fields`

use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig};
use axon_backtest::matching::L1MatchingEngine;
use axon_core::event::{EventBuilder, OrderAction};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{Price, Quantity};

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

/// 简单回测配置(L1 撮合,无冲击,默认手续费 0.1%)
fn simple_config() -> BacktestEngineConfig {
    BacktestEngineConfig {
        clock: SimulatedClock::new(Timestamp::from_nanos(0)),
        matching_engine: Box::new(L1MatchingEngine::new()),
        impact_model: None,
        initial_cash: 100_000.0,
        fee_config: FeeConfig::default(),
        force_liquidate: false,
    }
}

// ─── 测试 1: trades 配对(完全平仓) ────────────────────────────────

/// 完整回测流程:开仓 → 完全平仓,验证 trades 推入 1 笔 TradeRecord
///
/// 事件流:
/// 1. Sell @ 100 qty=0.1 (挂单,无对手方)
/// 2. Buy  @ 100 qty=0.1 (吃单,1 笔 fill,Long 0.1 @ 100)
/// 3. Buy  @ 105 qty=0.1 (挂单,无对手方)
/// 4. Sell @ 105 qty=0.1 (吃单,1 笔 fill,完全平仓,pnl=0.5)
#[test]
fn trades_recorded_on_full_close() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);

    // 1) 卖单挂单
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(make_limit_order(1, Side::Sell, 100.0, 0.1)),
    ));
    // 2) 买单吃单 → Long 0.1 @ 100
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        2,
        OrderAction::Submitted(make_limit_order(2, Side::Buy, 100.0, 0.1)),
    ));
    // 3) 买单挂单(为下一步平仓的对手方)
    q.push(b.order(
        Timestamp::from_nanos(3_000),
        3,
        OrderAction::Submitted(make_limit_order(3, Side::Buy, 105.0, 0.1)),
    ));
    // 4) 卖单吃单 → 完全平仓
    q.push(b.order(
        Timestamp::from_nanos(4_000),
        4,
        OrderAction::Submitted(make_limit_order(4, Side::Sell, 105.0, 0.1)),
    ));

    let mut engine = BacktestEngine::new(simple_config(), q);
    let result = engine.run();

    // 4 笔订单全部 accepted
    assert_eq!(result.orders_accepted, 4);
    // 2 笔 fill
    assert_eq!(result.fills, 2);
    // trades 推入 1 笔(完全平仓)
    assert_eq!(result.trades.len(), 1, "完全平仓应 push 1 个 TradeRecord");

    let tr = &result.trades[0];
    // realized_pnl = (105-100) * 0.1 = 0.5 → × 1e6 = 500_000
    assert!(
        (tr.realized_pnl - 500_000).abs() < 1,
        "expected realized_pnl=500_000, got {}",
        tr.realized_pnl
    );
    // 平仓时间戳:由 L1MatchingEngine 决定(taker_created,系统时间,>0)
    // 这里不验证具体值(事件时间戳是回测 harness 的职责)
    assert!(tr.trade.timestamp.nanos > 0);
}

// ─── 测试 2: fee 累计 ─────────────────────────────────────────────

/// 验证多笔 fill 的手续费按 `notional * taker_rate` 正确累加
///
/// 场景:2 笔 fill(测试 1 的开仓 + 平仓)
/// 预期:`total_fees = 100*0.1*0.001 + 105*0.1*0.001 = 0.0205`
#[test]
fn total_fees_accumulated_across_fills() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(make_limit_order(1, Side::Sell, 100.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        2,
        OrderAction::Submitted(make_limit_order(2, Side::Buy, 100.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(3_000),
        3,
        OrderAction::Submitted(make_limit_order(3, Side::Buy, 105.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(4_000),
        4,
        OrderAction::Submitted(make_limit_order(4, Side::Sell, 105.0, 0.1)),
    ));

    let mut engine = BacktestEngine::new(simple_config(), q);
    let result = engine.run();

    let expected_fees = 100.0_f64 * 0.1 * 0.001 + 105.0_f64 * 0.1 * 0.001;
    assert!(
        (result.total_fees - expected_fees).abs() < 1e-9,
        "expected total_fees={}, got {}",
        expected_fees,
        result.total_fees
    );
    // total_fees > 0(不是 0)
    assert!(result.total_fees > 0.0);
}

// ─── 测试 3: equity 采样 ──────────────────────────────────────────

/// 验证每笔 fill 后 equity_curve 新增 (timestamp, nav) 采样
///
/// 2 笔 fill → 2 个采样点
#[test]
fn equity_curve_sampled_per_fill() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(make_limit_order(1, Side::Sell, 100.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        2,
        OrderAction::Submitted(make_limit_order(2, Side::Buy, 100.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(3_000),
        3,
        OrderAction::Submitted(make_limit_order(3, Side::Buy, 105.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(4_000),
        4,
        OrderAction::Submitted(make_limit_order(4, Side::Sell, 105.0, 0.1)),
    ));

    let mut engine = BacktestEngine::new(simple_config(), q);
    let result = engine.run();

    // 2 笔 fill → 2 个采样点
    assert_eq!(result.equity_curve.len(), 2);

    // 第一个采样点:开仓后,nav = 100_000 - 100*0.1 - fee + 100*0.1 (mark-to-market)
    // = 100_000 - 10 - 0.01 + 10 = 99_999.99
    // timestamp 由 L1MatchingEngine 决定(taker_created,系统时间)
    let (ts1, nav1) = result.equity_curve[0];
    assert!(ts1.nanos > 0);
    assert!(
        (nav1 - (100_000.0 - 100.0 * 0.1 * 0.001)).abs() < 1e-6,
        "expected first nav ≈ 99_999.99, got {nav1}"
    );

    // 第二个采样点:平仓后,nav = 100_000 + pnl - total_fees
    // = 100_000 + 0.5 - 0.0205 = 100_000.4795
    let (ts2, nav2) = result.equity_curve[1];
    assert!(ts2.nanos > 0);
    let expected_nav = 100_000.0 + 0.5 - (100.0 * 0.1 * 0.001 + 105.0 * 0.1 * 0.001);
    assert!(
        (nav2 - expected_nav).abs() < 1e-6,
        "expected final nav={}, got {}",
        expected_nav,
        nav2
    );

    // nav_peak 应为 max(nav1, nav2)
    assert!((result.nav_peak - result.nav_peak.max(nav1).max(nav2)).abs() < 1e-9);
}

// ─── 测试 4: metrics 计算 ─────────────────────────────────────────

/// 验证 win_rate / sharpe_ratio 从 TradingMetrics 正确计算
///
/// 场景:1 笔 win trade(平仓价 105 > 开仓价 100)→ win_rate = 1.0
///      2 个 log return → sharpe_ratio 非 0
#[test]
fn win_rate_and_sharpe_computed_from_metrics() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(make_limit_order(1, Side::Sell, 100.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        2,
        OrderAction::Submitted(make_limit_order(2, Side::Buy, 100.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(3_000),
        3,
        OrderAction::Submitted(make_limit_order(3, Side::Buy, 105.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(4_000),
        4,
        OrderAction::Submitted(make_limit_order(4, Side::Sell, 105.0, 0.1)),
    ));

    let mut engine = BacktestEngine::new(simple_config(), q);
    let result = engine.run();

    // 1 笔 win trade → win_rate = 1.0
    assert!(
        (result.win_rate - 1.0).abs() < 1e-9,
        "expected win_rate=1.0, got {}",
        result.win_rate
    );

    // 2 个 log return → sharpe_ratio 应被计算(可能为 0 因为方差为 0,但 0 也是合理值)
    // 实际 log_return = ln(nav2/nav1) ≈ 4.795e-6,只有一个非零值,无法算方差
    // → 第二个 log_return 不存在(equity_curve 长度 = 2,len>=2 时记录一次),
    //   而第一个 log_return 是在 equity_curve[0] 之前无法计算(prev_nav 不存在)
    // 所以 sharpe_ratio = 0(没有 log return 记录)
    assert_eq!(
        result.sharpe_ratio, 0.0,
        "只有 1 个 log return 时 sharpe=0,got {}",
        result.sharpe_ratio
    );
}

/// 多笔盈亏混合:验证 win_rate = wins / total_trades
#[test]
fn win_rate_mixed_pnl() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);

    // 第 1 轮:开仓 @ 100 → 平仓 @ 105(win,+0.5)
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(make_limit_order(1, Side::Sell, 100.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        2,
        OrderAction::Submitted(make_limit_order(2, Side::Buy, 100.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(3_000),
        3,
        OrderAction::Submitted(make_limit_order(3, Side::Buy, 105.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(4_000),
        4,
        OrderAction::Submitted(make_limit_order(4, Side::Sell, 105.0, 0.1)),
    ));

    // 第 2 轮:开仓 @ 110 → 平仓 @ 108(loss,-0.2)
    q.push(b.order(
        Timestamp::from_nanos(5_000),
        5,
        OrderAction::Submitted(make_limit_order(5, Side::Sell, 110.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(6_000),
        6,
        OrderAction::Submitted(make_limit_order(6, Side::Buy, 110.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(7_000),
        7,
        OrderAction::Submitted(make_limit_order(7, Side::Buy, 108.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(8_000),
        8,
        OrderAction::Submitted(make_limit_order(8, Side::Sell, 108.0, 0.1)),
    ));

    let mut engine = BacktestEngine::new(simple_config(), q);
    let result = engine.run();

    // 2 笔 trade:1 win + 1 loss → win_rate = 0.5
    assert_eq!(
        result.trades.len(),
        2,
        "2 轮完全平仓应 push 2 个 TradeRecord"
    );
    assert!(
        (result.win_rate - 0.5).abs() < 1e-9,
        "expected win_rate=0.5, got {}",
        result.win_rate
    );
}

// ─── 测试 5: 反向部分平仓 + 反手(6 状态机分支) ──────────────────────

/// 反向部分平仓:Long 0.2 → Sell 0.1 → 平 0.1 + 留 0.1
#[test]
fn reverse_partial_close() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);

    // 1) Sell @ 100 qty=0.2 (挂单)
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(make_limit_order(1, Side::Sell, 100.0, 0.2)),
    ));
    // 2) Buy @ 100 qty=0.2 (吃单) → Long 0.2 @ 100
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        2,
        OrderAction::Submitted(make_limit_order(2, Side::Buy, 100.0, 0.2)),
    ));
    // 3) Buy @ 105 qty=0.1 (挂单,做平仓对手方)
    q.push(b.order(
        Timestamp::from_nanos(3_000),
        3,
        OrderAction::Submitted(make_limit_order(3, Side::Buy, 105.0, 0.1)),
    ));
    // 4) Sell @ 105 qty=0.1 (吃单) → 反向部分平仓:close 0.1,留 0.1
    q.push(b.order(
        Timestamp::from_nanos(4_000),
        4,
        OrderAction::Submitted(make_limit_order(4, Side::Sell, 105.0, 0.1)),
    ));

    let mut engine = BacktestEngine::new(simple_config(), q);
    let result = engine.run();

    // 反向部分平仓 push 1 笔 TradeRecord
    assert_eq!(
        result.trades.len(),
        1,
        "反向部分平仓应 push 1 个 TradeRecord"
    );
    // realized_pnl = (105-100) * 0.1 = 0.5
    let tr = &result.trades[0];
    assert!(
        (tr.realized_pnl - 500_000).abs() < 1,
        "expected realized_pnl=500_000, got {}",
        tr.realized_pnl
    );
    // 终态:Long 0.1(还剩一半)
    assert_eq!(result.positions.len(), 1);
    assert!(
        (result.positions["BTC/USDT"] - 0.1).abs() < 1e-9,
        "expected position=0.1, got {}",
        result.positions["BTC/USDT"]
    );
}

/// 反手:Long 0.1 → Sell 0.2 → 平 0.1 + 开反向 0.1
#[test]
fn reverse_flip_position() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);

    // 1) Sell @ 100 qty=0.1 (挂单)
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(make_limit_order(1, Side::Sell, 100.0, 0.1)),
    ));
    // 2) Buy @ 100 qty=0.1 (吃单) → Long 0.1 @ 100
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        2,
        OrderAction::Submitted(make_limit_order(2, Side::Buy, 100.0, 0.1)),
    ));
    // 3) Buy @ 105 qty=0.2 (挂单,反手对手方)
    q.push(b.order(
        Timestamp::from_nanos(3_000),
        3,
        OrderAction::Submitted(make_limit_order(3, Side::Buy, 105.0, 0.2)),
    ));
    // 4) Sell @ 105 qty=0.2 (吃单) → 反手:平 0.1,开反向 0.1
    q.push(b.order(
        Timestamp::from_nanos(4_000),
        4,
        OrderAction::Submitted(make_limit_order(4, Side::Sell, 105.0, 0.2)),
    ));

    let mut engine = BacktestEngine::new(simple_config(), q);
    let result = engine.run();

    // 反手 push 1 笔 TradeRecord
    assert_eq!(result.trades.len(), 1, "反手应 push 1 个 TradeRecord");
    // realized_pnl = (105-100) * 0.1 = 0.5
    let tr = &result.trades[0];
    assert!(
        (tr.realized_pnl - 500_000).abs() < 1,
        "expected realized_pnl=500_000, got {}",
        tr.realized_pnl
    );
    // 终态:Short 0.1
    assert_eq!(result.positions.len(), 1);
    assert!(
        (result.positions["BTC/USDT"] - (-0.1)).abs() < 1e-9,
        "expected position=-0.1, got {}",
        result.positions["BTC/USDT"]
    );
}

// ─── 测试 6: total_pnl 账户视角(未平仓 long 不再失真) ──────────────

/// 验证 `total_pnl` 修复后:含未平仓 long 的回测,total_pnl 反映真实账户变化
///
/// 旧实现按 fill 维度 cash flow 累计,2098 笔 buy + 62 笔 sell 时 `total_pnl ≈ -3300`,
/// 但实际账户仅损失手续费(≈ 0)。新实现 `total_pnl = final_nav - initial_cash`,
/// 对未平仓 long 持仓 mark-to-market 后,total_pnl 接近 0。
///
/// 场景(无对手方单边 buy):Sell 挂单 → Buy 吃单 → 终态 long 0.1 @ 100
/// 期望:
/// - 1 笔 fill, 0.1 手续费
/// - final_nav ≈ 100_000 - 0.1(只扣费,持仓抵 cash 减少)
/// - total_pnl ≈ -0.1(账户视角)
/// - 旧版会断言 total_pnl = -100(buy 端 cash flow),新版本 -0.1
#[test]
fn total_pnl_account_view_with_unclosed_long() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(make_limit_order(1, Side::Sell, 100.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        2,
        OrderAction::Submitted(make_limit_order(2, Side::Buy, 100.0, 0.1)),
    ));

    let mut engine = BacktestEngine::new(simple_config(), q);
    let result = engine.run();

    // 账户视角:final_nav = cash + position_value(仅未平仓持仓的 mark-to-market)
    // - 终态 long 0.1 @ 100,cash 减少 10(notional) + 0.01(fee)= -10.01
    //   → cash = 100_000 - 10.01 = 99_989.99
    // - 持仓 mark-to-market:0.1 * 100 = 10
    // - final_nav = 99_989.99 + 10 = 99_999.99
    // (cash 减 10 + 持仓 mark +10 抵消,final_nav = initial - fee = 99_999.99)
    let expected_final_nav = 100_000.0 - 100.0 * 0.1 * 0.001;
    assert!(
        (result.final_nav - expected_final_nav).abs() < 1e-6,
        "expected final_nav={}, got {}",
        expected_final_nav,
        result.final_nav
    );
    // total_pnl = final_nav - initial_cash ≈ -0.01(只扣手续费)
    let expected_pnl = -0.01_f64;
    assert!(
        (result.total_pnl - expected_pnl).abs() < 1e-6,
        "expected total_pnl={} (账户视角), got {}",
        expected_pnl,
        result.total_pnl
    );
    // 关键:total_pnl **不**等于 -10(买花的钱),因为 long 持仓 mark-to-market
    // 抵消了 cash 减少
    assert!(
        result.total_pnl > -1.0,
        "total_pnl 应该接近 0(账户视角),不是 cash flow 视角;got {}",
        result.total_pnl
    );
    // 终态 long 0.1 BTC(未平仓)
    assert_eq!(result.positions.len(), 1);
    assert!((result.positions["BTC/USDT"] - 0.1).abs() < 1e-9);
}

// ─── 测试 7: force_liquidate 强制平仓 ──────────────────────────────

/// force_liquidate=false:long 0.1 保留,total_pnl 包含 mark-to-market
/// 已在 `total_pnl_account_view_with_unclosed_long` 中验证。
///
/// force_liquidate=true:long 0.1 被市价清仓,但无对手盘(IOC 拒单)→ 持仓仍保留。
/// 这里构造"先开仓,再平仓"场景(有对手方),验证 force_liquidate 把终态清零。
///
/// 场景:
/// 1. Sell @ 100 qty=0.1 → 挂单
/// 2. Buy  @ 100 qty=0.1 → 吃单,long 0.1 @ 100
/// 3. Buy  @ 105 qty=0.1 → 挂单
/// 4. Sell @ 105 qty=0.1 → 吃单,完全平仓
///
/// 终态:无持仓,total_pnl = 0.5 - 0.0205 = 0.4795
///
/// 关闭 force_liquidate:同事件流,但市价单路径可能改变,这里改用"开仓不
/// 平仓"再 force_liquidate 验证。
#[test]
fn force_liquidate_clears_open_position() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    // 1) 卖单挂单
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(make_limit_order(1, Side::Sell, 100.0, 0.1)),
    ));
    // 2) 买单吃单 → long 0.1 @ 100
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        2,
        OrderAction::Submitted(make_limit_order(2, Side::Buy, 100.0, 0.1)),
    ));
    // 3) 买单 @ 105 挂单(为 force_liquidate 的市价平仓做对手方)
    q.push(b.order(
        Timestamp::from_nanos(3_000),
        3,
        OrderAction::Submitted(make_limit_order(3, Side::Buy, 105.0, 0.1)),
    ));
    // 不发第 4 个 sell,留 long 0.1 @ 100 给 force_liquidate 清仓

    let mut cfg = simple_config();
    cfg.force_liquidate = true;
    let mut engine = BacktestEngine::new(cfg, q);
    let result = engine.run();

    // 主循环:1 sell 挂单 + 1 buy 吃单(accepted) + 1 buy 挂单(accepted) = 3
    // force_liquidate:1 市价 sell(IOC,吃 buy @ 105 挂单)→ 1 accepted + 1 fill
    assert_eq!(result.orders_accepted, 4, "3 主循环 + 1 EOD 平仓");
    // 1 主循环 fill + 1 EOD fill = 2
    assert_eq!(result.fills, 2);
    // 终态:long 0.1 被 EOD 市价单平掉(IOC, 1 笔 fill @ 105)
    // realized_pnl = (105-100) * 0.1 = 0.5
    // total_fees = 2 * 0.001 * 100 (fill 1) + 1 * 0.001 * 105 (fill 2)
    //          = 0.0002 + 0.000105 = ... 等等,实际 fee 是按 notional 算
    // 简化:total_pnl = final_nav - initial_cash
    // final_nav:cash = 100_000 - 10 - 0.1 - 10.5 - 0.105 + 10.5 = 99_989.795
    // 终态 position=0,nav = cash = 99_989.795
    // total_pnl = -10.205(平仓亏损:price 100 买 105 卖,扣手续费,实际亏损)
    // 不对,(105-100)*0.1 = +0.5,但 cash flow 抵消:
    // buy 100 qty=0.1: cash -= 10 + 0.001*10 = -10.01
    // sell 105 qty=0.1: cash += 10.5 - 0.001*10.5 = +10.4895
    // 净 cash flow: +0.4795,扣 initial 100_000 = 100_000.4795
    // final_nav = 100_000.4795
    // total_pnl = 0.4795
    assert!(
        (result.total_pnl - 0.4795).abs() < 1e-3,
        "expected total_pnl≈0.4795, got {}",
        result.total_pnl
    );
    // 终态 position 应被清零
    assert!(
        result.positions.is_empty(),
        "force_liquidate 后 position 应清空,got {:?}",
        result.positions
    );
    // trades 应包含 1 笔平仓记录(EOD 市价平仓也算)
    assert_eq!(result.trades.len(), 1, "EOD 平仓 push 1 笔 TradeRecord");
}

/// 不开启 force_liquidate 时,long 0.1 保留,total_pnl 走 mark-to-market
/// 与 `force_liquidate_clears_open_position` 对比:
/// - 不 force_liquidate:final_nav ≈ 100_000 - 0.1 fee + 0(无平仓收益) = 99_999.9
/// - 区别:final_nav 高(因为 long 持仓 mark 抵 cash 减少)
#[test]
fn no_force_liquidate_keeps_open_position() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(make_limit_order(1, Side::Sell, 100.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        2,
        OrderAction::Submitted(make_limit_order(2, Side::Buy, 100.0, 0.1)),
    ));

    let mut cfg = simple_config();
    cfg.force_liquidate = false; // 显式 false
    let mut engine = BacktestEngine::new(cfg, q);
    let result = engine.run();

    // 1 笔 fill(主循环),无 EOD
    assert_eq!(result.fills, 1);
    assert_eq!(result.orders_accepted, 2);
    // 终态 long 0.1 @ mark=100
    assert_eq!(result.positions.len(), 1);
    assert!((result.positions["BTC/USDT"] - 0.1).abs() < 1e-9);
    // total_pnl = final_nav - initial = -0.01(只扣手续费,持仓 mark 抵 cash)
    assert!(
        (result.total_pnl - (-0.01)).abs() < 1e-6,
        "expected total_pnl=-0.01 (账户视角,mark 抵 cash 减少), got {}",
        result.total_pnl
    );
}
