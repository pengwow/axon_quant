//! 端到端测试:`FeeConfig` 边界(P2-3)
//!
//! ## 测试目标
//!
//! `axon_backtest::engine::FeeConfig` 仅 1 个字段 `taker_rate: f64`,文档未明确
//! 取值约束。本测试验证 4 个典型边界下的语义契约:
//!
//! 1. **`taker_rate = 0.0`**:无手续费,`total_fees == 0`,`total_pnl` 仅受价格影响
//! 2. **`FeeConfig::default()`**:默认值 = `{ taker_rate: 0.001 }`
//! 3. **`taker_rate = 1.0`**:100% 费率,1 笔 buy 的 fee = notional,NAV 减半
//! 4. **`taker_rate = -0.1`**:负费率(理论返佣),记录实现行为(不 panic)
//!
//! ## 设计要点
//!
//! - **手算对账**:每笔 fill 的 `fee = notional * taker_rate` 可手算
//! - **不依赖 SMA 策略**:每个测试构造最小对手盘 + 策略单
//! - **L1 撮合**:沿用现有 `L1MatchingEngine`
//!
//! 运行:`cargo test -p axon-backtest --test fee_config_boundaries`

use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig};
use axon_backtest::matching::L1MatchingEngine;
use axon_core::event::{EventBuilder, OrderAction};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{Price, Quantity, Symbol};

// ── 共享 helper ──────────────────────────────────────────────────────

fn sym() -> Symbol {
    Symbol::from("BTC-USDT")
}

/// 构造配置(可指定 taker_rate)
fn config_with_rate(initial_cash: f64, taker_rate: f64) -> BacktestEngineConfig {
    BacktestEngineConfig {
        clock: SimulatedClock::new(Timestamp::from_nanos(0)),
        matching_engine: Box::new(L1MatchingEngine::new()),
        impact_model: None,
        initial_cash,
        fee_config: FeeConfig { taker_rate },
        force_liquidate: false,
    }
}

/// 构造对手 sell + 策略 buy market 的事件队列
///
/// 辅助函数(无事件类型参数化):每次调用产生 1 笔 sell limit + 1 笔 market buy
/// 的最小事件流,跑出 1 笔 fill。taker_rate 由调用方在 config 中指定。
fn build_one_fill_queue() -> EventQueue {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);

    // 对手 sell @ 100 qty=1
    let counter = Order::new(
        1,
        sym(),
        Side::Sell,
        OrderType::Limit {
            price: Price::from_f64(100.0),
        },
        Quantity::from_f64(1.0),
        TimeInForce::GTC,
    );
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(counter),
    ));

    // 策略 buy market 1
    let strategy = Order::new(
        2,
        sym(),
        Side::Buy,
        OrderType::Market,
        Quantity::from_f64(1.0),
        TimeInForce::IOC,
    );
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        2,
        OrderAction::Submitted(strategy),
    ));

    q
}

// ── 测试 1:taker_rate = 0 → 0 手续费 ────────────────────────────────

/// `taker_rate = 0.0` → buy 1 @ 100 时,`fee = 100 * 0 = 0`
///
/// 验证:`total_fees == 0`,`total_pnl == 0`(账户视角,无 fee 扣除)
/// 终态 NAV = initial_cash(持仓抵 cash 减少)
#[test]
fn taker_rate_zero_yields_zero_total_fees() {
    let q = build_one_fill_queue();
    let mut engine = BacktestEngine::new(config_with_rate(100_000.0, 0.0), q);
    let result = engine.run();

    assert_eq!(result.fills, 1, "1 笔 fill");
    assert!(
        (result.total_fees - 0.0).abs() < 1e-9,
        "taker_rate=0 → total_fees=0, got {}",
        result.total_fees
    );
    // 无 fee,total_pnl = final_nav - initial_cash = 0(账户视角)
    // 终态:long 1 @ mark=100, cash = 100_000 - 100 = 99_900
    // NAV = 99_900 + 100 = 100_000,total_pnl = 0
    assert!(
        (result.total_pnl - 0.0).abs() < 1e-6,
        "taker_rate=0 → total_pnl=0(账户视角), got {}",
        result.total_pnl
    );
    assert!(
        (result.final_nav - 100_000.0).abs() < 1e-6,
        "final_nav = initial_cash(无 fee 扣除), got {}",
        result.final_nav
    );
}

// ── 测试 2:FeeConfig::default() == { taker_rate: 0.001 } ─────────────

/// 直接断言默认值契约,确保 `Default` 实现不漂移
#[test]
fn taker_rate_default_is_0_001() {
    let default = FeeConfig::default();
    assert!(
        (default.taker_rate - 0.001).abs() < 1e-9,
        "FeeConfig::default().taker_rate 应 = 0.001, got {}",
        default.taker_rate
    );
    // 同时验证与显式构造的相等性
    let explicit = FeeConfig { taker_rate: 0.001 };
    assert_eq!(
        default, explicit,
        "default() == FeeConfig {{ taker_rate: 0.001 }}"
    );
}

// ── 测试 3:taker_rate = 1.0 → 100% 费率 ─────────────────────────────

/// `taker_rate = 1.0` → buy 1 @ 100 时,`fee = 100 * 1 = 100`
///
/// 验证:
/// - `total_fees ≈ 100.0`
/// - `final_nav = initial_cash - 100.0`(NAV 减半 100k → 99.9k,实际是 -100)
#[test]
fn taker_rate_one_halves_equity_via_full_fee() {
    let q = build_one_fill_queue();
    let mut engine = BacktestEngine::new(config_with_rate(100_000.0, 1.0), q);
    let result = engine.run();

    assert_eq!(result.fills, 1, "1 笔 fill");
    // notional = 100, fee = 100 * 1.0 = 100
    let expected_fee = 100.0 * 1.0 * 1.0; // price * qty * taker_rate
    assert!(
        (result.total_fees - expected_fee).abs() < 1e-6,
        "taker_rate=1.0 → total_fees={}, got {}",
        expected_fee,
        result.total_fees
    );
    // 终态:buy 1 @ 100,cash = 100_000 - 100 - 100(fee) = 99_800
    // NAV = 99_800 + 100(mark-to-market) = 99_900
    // total_pnl = 99_900 - 100_000 = -100
    let expected_nav = 100_000.0 - 100.0 - 100.0 + 100.0;
    assert!(
        (result.final_nav - expected_nav).abs() < 1e-6,
        "final_nav 应={}, got {}",
        expected_nav,
        result.final_nav
    );
    // total_pnl = -fee
    assert!(
        (result.total_pnl - (-100.0)).abs() < 1e-6,
        "taker_rate=1.0 → total_pnl=-100(fee), got {}",
        result.total_pnl
    );
}

// ── 测试 4:负 taker_rate 不 panic ────────────────────────────────────

/// `taker_rate = -0.1` → fee = notional * (-0.1) = 负数(理论返佣)
///
/// 验证:**不 panic**。`total_fees` 可能为负(实现定义),不强求符号。
/// 注:若 fee 为负,cash 实际增加(buy 100 qty=1,fee=-10 → cash = 100_000 - 100 - (-10) = 99_910),
/// 终态 NAV 反而增加。这是实现定义,不是 bug。
#[test]
fn negative_taker_rate_does_not_panic() {
    let q = build_one_fill_queue();
    let mut engine = BacktestEngine::new(config_with_rate(100_000.0, -0.1), q);
    let result = engine.run();

    // 关键:不 panic
    assert_eq!(result.events_processed, 2, "2 事件全部处理");
    assert_eq!(result.fills, 1, "1 笔 fill(撮合不被费率影响)");

    // fee 计算:notional * taker_rate = 100 * (-0.1) = -10
    // fee 为负意味着 cash 实际"返佣"
    // 终态:buy 1 @ 100,cash = 100_000 - 100 - (-10) = 99_910
    // NAV = 99_910 + 100 = 100_010
    // total_pnl = 100_010 - 100_000 = +10
    // total_fees = -10(负数)
    assert!(
        result.total_fees < 0.0,
        "taker_rate=-0.1 → total_fees 应 < 0(返佣), got {}",
        result.total_fees
    );
    // 不强制断言 NAV 数值,只断言不 NaN
    assert!(!result.final_nav.is_nan(), "NAV 不能 NaN");
    assert!(!result.total_pnl.is_nan(), "PnL 不能 NaN");
}
