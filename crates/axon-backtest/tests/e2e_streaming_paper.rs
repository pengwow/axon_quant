//! 端到端测试:流式 Paper Trading
//!
//! ## 测试目标
//!
//! 验证 `StreamingEngine` 在 `TradingMode::PaperTrading` 下,自动应用 `PaperTradingEngine`
//! 的 1bps 滑点到限价单,且完整 tick→strategy→order→match→portfolio 链路工作正常。
//!
//! ## 滑点语义
//!
//! `PaperTradingEngine::apply_slippage` 对**限价单**做"激进化"调整:
//! - Buy 限价上浮 1bps(更愿意付更多)
//! - Sell 限价下浮 1bps(更愿意接受更低)
//!
//! 撮合价仍取**对手方(maker)挂单价**(L1 默认语义)。本测试通过精心设计对手价,
//! 让"上浮/下浮后的 taker 价"恰好与 maker 价相等,这样 fill 价就等于滑点后的价,
//! 既证明滑点逻辑被触发,又可被直接断言。
//!
//! ## 5 个测试场景
//!
//! 1. `paper_buy_limit_slippage_makes_taker_more_aggressive`:Buy 上浮 1bps,撮合到 maker @100.01
//! 2. `paper_sell_limit_slippage_makes_taker_more_aggressive`:Sell 下浮 1bps,撮合到 maker @99.99
//! 3. `paper_market_order_not_slipped`:Market 单不被滑点
//! 4. `paper_roundtrip_buy_then_sell_yields_pnl_minus_commission`:Buy 100→Sell 101 净赚价差-手续费
//! 5. `paper_hold_action_emits_no_fill`:Hold 路径不产生 fill event
//!
//! 运行:`cargo test -p axon-backtest --test e2e_streaming_paper`

use std::collections::VecDeque;

use axon_backtest::streaming::{
    MarketDataEvent, PaperTradingEngine, SimulatedExchange, StrategyAction, StreamingEngine,
    StreamingStrategy, TradingMode,
};
use axon_core::event::Event;
use axon_core::market::{Side, Tick};
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::portfolio::Currency;
use axon_core::time::Timestamp;
use axon_core::types::{Price, Quantity, Symbol};

// ── 测试用 strategy ────────────────────────────────────────────────────

/// "固定动作"策略:每次 `on_tick` 弹出一个预设 action,弹完返回空 Vec
struct FixedStrategy {
    actions: VecDeque<StrategyAction>,
}

impl FixedStrategy {
    fn new(actions: Vec<StrategyAction>) -> Self {
        Self {
            actions: actions.into_iter().collect(),
        }
    }
}

impl StreamingStrategy for FixedStrategy {
    fn on_tick(&mut self, _symbol: &Symbol, _price: f64) -> Vec<StrategyAction> {
        self.actions.pop_front().into_iter().collect()
    }
}

// ── helpers ────────────────────────────────────────────────────────────

/// 构造限价单 helper(用作对手盘或策略订单)
fn make_limit(id: u64, side: Side, price: f64, qty: f64) -> Order {
    Order::new(
        id,
        Symbol::from("BTC-USDT"),
        side,
        OrderType::Limit {
            price: Price::from_f64(price),
        },
        Quantity::from_f64(qty),
        TimeInForce::GTC,
    )
}

/// 构造市价单 helper
fn make_market(id: u64, side: Side, qty: f64) -> Order {
    Order::new(
        id,
        Symbol::from("BTC-USDT"),
        side,
        OrderType::Market,
        Quantity::from_f64(qty),
        TimeInForce::IOC,
    )
}

/// 构造 tick
fn make_tick(price: f64) -> Tick {
    Tick::new(
        Timestamp::from_nanos(1_000),
        Price::from_f64(price),
        Quantity::from_f64(1.0),
        Side::Buy,
    )
}

fn btc() -> Symbol {
    Symbol::from("BTC-USDT")
}

/// 0.4.0:在 paper 模式下注入 `fill_probability=1.0` 的 paper engine,
/// 让"是否成交"完全确定(默认 0.95 会引入随机性,让 roundtrip 测试不稳定)
fn deterministic_paper_engine() -> StreamingEngine {
    StreamingEngine::new(TradingMode::PaperTrading).with_paper_engine(
        PaperTradingEngine::new(SimulatedExchange {
            fill_probability: 1.0,
            ..SimulatedExchange::default()
        }),
    )
}

// ── 1. Buy 限价 1bps 上浮 → 撮合到 maker @100.01 ─────────────────────

#[test]
fn paper_buy_limit_slippage_makes_taker_more_aggressive() {
    let mut engine = deterministic_paper_engine();
    engine.register_symbol(btc());

    // 对手盘:Sell Limit @100.01(模拟"市场深度恰好 1bps 之外")
    // 策略 Buy @100(原)上浮 1bps 后变 @100.01,正好够到 maker
    let maker = make_limit(900, Side::Sell, 100.01, 1.0);
    engine.submit_order(maker).expect("submit maker");

    // 策略:Buy Limit @100(挂 100,paper 模式上浮 1bps → 100.01,正好撮合到 maker)
    let taker = make_limit(1, Side::Buy, 100.0, 1.0);
    let strategy = FixedStrategy::new(vec![StrategyAction::Submit(taker)]);
    let mut engine = engine.with_strategy(Box::new(strategy));

    let events = engine.on_market_event(MarketDataEvent::Tick {
        symbol: btc(),
        tick: make_tick(100.0),
    });

    assert_eq!(
        events.len(),
        1,
        "Buy 滑点后应成交 1 笔,实为 {}",
        events.len()
    );
    match &events[0] {
        Event::Fill(fill) => {
            let p = fill.trade.price.as_f64();
            assert!(
                (p - 100.01).abs() < 1e-6,
                "fill 价 = maker 价 = 100.01 (Buy 滑点后正好够到),实为 {p}"
            );
        }
        other => panic!("期望 Event::Fill,实为 {other:?}"),
    }
}

// ── 2. Sell 限价 1bps 下浮 → 撮合到 maker @99.99 ─────────────────────

#[test]
fn paper_sell_limit_slippage_makes_taker_more_aggressive() {
    let mut engine = deterministic_paper_engine();
    engine.register_symbol(btc());

    // 对手盘:Buy Limit @99.99
    // 策略 Sell @100 下浮 1bps → 99.99,正好撮合到 maker
    let maker = make_limit(900, Side::Buy, 99.99, 1.0);
    engine.submit_order(maker).expect("submit maker");

    let taker = make_limit(1, Side::Sell, 100.0, 1.0);
    let strategy = FixedStrategy::new(vec![StrategyAction::Submit(taker)]);
    let mut engine = engine.with_strategy(Box::new(strategy));

    let events = engine.on_market_event(MarketDataEvent::Tick {
        symbol: btc(),
        tick: make_tick(100.0),
    });

    assert_eq!(
        events.len(),
        1,
        "Sell 滑点后应成交 1 笔,实为 {}",
        events.len()
    );
    match &events[0] {
        Event::Fill(fill) => {
            let p = fill.trade.price.as_f64();
            assert!(
                (p - 99.99).abs() < 1e-6,
                "fill 价 = maker 价 = 99.99 (Sell 滑点后正好够到),实为 {p}"
            );
        }
        other => panic!("期望 Event::Fill,实为 {other:?}"),
    }
}

// ── 3. Market 单不应用滑点 ─────────────────────────────────────────────

#[test]
fn paper_market_order_not_slipped() {
    let mut engine = deterministic_paper_engine();
    engine.register_symbol(btc());

    // 对手盘:Sell Limit @100(Market Buy 直接吃 maker)
    let maker = make_limit(900, Side::Sell, 100.0, 1.0);
    engine.submit_order(maker).expect("submit maker");

    // 策略:Market Buy(Market 单无 limit_price,跳过滑点 → 撮合到 maker @100)
    let taker = make_market(1, Side::Buy, 1.0);
    let strategy = FixedStrategy::new(vec![StrategyAction::Submit(taker)]);
    let mut engine = engine.with_strategy(Box::new(strategy));

    let events = engine.on_market_event(MarketDataEvent::Tick {
        symbol: btc(),
        tick: make_tick(100.0),
    });

    assert_eq!(events.len(), 1);
    match &events[0] {
        Event::Fill(fill) => {
            let p = fill.trade.price.as_f64();
            assert!(
                (p - 100.0).abs() < 1e-9,
                "Market 单 fill 价不应被滑点,实为 {p}"
            );
        }
        other => panic!("期望 Event::Fill,实为 {other:?}"),
    }
}

// ── 4. 完整往返 Buy→Sell 净赚价差-手续费 ─────────────────────────────

/// Buy 价 < Sell 价,验证 PnL 数学正确性 + commission 累加 + 仓位闭环
///
/// **设计要点**:
/// - maker1 Sell @100 与 maker2 Buy @200 不能同时存在(会被 L1 撮合掉),
///   所以分两阶段挂单
/// - strategy 用 Market 单避免 paper 滑点路径干扰(滑点路径由测试 1/2 单独验证)
/// - 流程:
///   1. setup:挂 maker1 Sell @100
///   2. tick1:strategy Market Buy → 撮合 maker1 @100,fill=100,持仓 +1 BTC
///   3. mid:挂 maker2 Buy @200(此时 asks 已空,安全)
///   4. tick2:strategy Market Sell → 撮合 maker2 @200,fill=200,平仓
/// - 期望 PnL = (200 - 100) - 2 * commission ≈ 99.97
#[test]
fn paper_roundtrip_buy_then_sell_yields_pnl_minus_commission() {
    let mut engine = deterministic_paper_engine();
    engine.register_symbol(btc());
    // 入金 100k(回测初始资金),保证买得起
    engine.portfolio_mut().deposit(Currency::USD, 100_000.0);

    // 阶段 1:挂 maker1 Sell @100
    let maker1 = make_limit(901, Side::Sell, 100.0, 1.0);
    engine.submit_order(maker1).expect("submit maker1");

    // 阶段 2:绑定 strategy(返回 [Market Buy, Market Sell])
    let strategy = FixedStrategy::new(vec![
        StrategyAction::Submit(make_market(1, Side::Buy, 1.0)),
        StrategyAction::Submit(make_market(2, Side::Sell, 1.0)),
    ]);
    let mut engine = engine.with_strategy(Box::new(strategy));

    // 阶段 3:tick1 触发 strategy → Market Buy 撮合 maker1 @100
    let e1 = engine.on_market_event(MarketDataEvent::Tick {
        symbol: btc(),
        tick: make_tick(100.0),
    });
    assert_eq!(e1.len(), 1, "tick1 应有 1 fill,实为 {e1:?}");

    // 阶段 4:mid 挂 maker2 Buy @200(asks 已空,Buy taker 不撮合 bids,安全)
    let maker2 = make_limit(902, Side::Buy, 200.0, 1.0);
    engine.submit_order(maker2).expect("submit maker2");

    // 阶段 5:tick2 触发 strategy → Market Sell 撮合 maker2 @200,平仓
    let e2 = engine.on_market_event(MarketDataEvent::Tick {
        symbol: btc(),
        tick: make_tick(200.0),
    });
    assert_eq!(e2.len(), 1, "tick2 应有 1 fill,实为 {e2:?}");

    // 验证 fill 价
    let buy_fill_price = match &e1[0] {
        Event::Fill(f) => f.trade.price.as_f64(),
        _ => panic!("tick1 期望 Fill"),
    };
    let sell_fill_price = match &e2[0] {
        Event::Fill(f) => f.trade.price.as_f64(),
        _ => panic!("tick2 期望 Fill"),
    };
    // Market 单无 paper 滑点 → 直接撮合 maker 价
    assert!(
        (buy_fill_price - 100.0).abs() < 1e-9,
        "Buy fill 应为 maker 价 100.0,实为 {buy_fill_price}"
    );
    assert!(
        (sell_fill_price - 200.0).abs() < 1e-9,
        "Sell fill 应为 maker 价 200.0,实为 {sell_fill_price}"
    );

    // 验证 portfolio realized PnL
    // 默认 commission rate = 0.001 (0.1%)
    // realized = (sell - buy) - commission_buy - commission_sell
    //         = 100.0 - 100.0*0.001 - 200.0*0.001
    //         = 100.0 - 0.1 - 0.2 = 99.7
    let realized_f = engine.portfolio().total_realized_pnl() as f64 / 1_000_000.0;
    let expected_raw = 200.0_f64 - 100.0_f64;
    let commission_rate = 0.001_f64; // Portfolio::default() 的 0.1% 费率
    let expected_commission = 100.0_f64 * commission_rate + 200.0_f64 * commission_rate;
    let expected = expected_raw - expected_commission;
    assert!(
        (realized_f - expected).abs() < 1e-4,
        "realized_pnl={realized_f}, expected≈{expected:.6}"
    );
    // 关键不变量:应正(价差 100 > 2 次手续费 0.3)
    assert!(realized_f > 0.0, "价差 100 > 2 次手续费,应净赚");
    // 关闭仓位:应无持仓
    assert!(
        engine.portfolio().positions().is_empty(),
        "roundtrip 后应平仓,实为 {:?}",
        engine.portfolio().positions()
    );
}

// ── 5. Hold action 不产生 fill ───────────────────────────────────────

#[test]
fn paper_hold_action_emits_no_fill() {
    let mut engine = deterministic_paper_engine();
    engine.register_symbol(btc());

    let strategy = FixedStrategy::new(vec![StrategyAction::Hold, StrategyAction::Hold]);
    let mut engine = engine.with_strategy(Box::new(strategy));

    let e1 = engine.on_market_event(MarketDataEvent::Tick {
        symbol: btc(),
        tick: make_tick(100.0),
    });
    let e2 = engine.on_market_event(MarketDataEvent::Tick {
        symbol: btc(),
        tick: make_tick(101.0),
    });

    assert!(e1.is_empty(), "Hold 不应产生 fill,实为 {e1:?}");
    assert!(e2.is_empty(), "Hold 不应产生 fill,实为 {e2:?}");
    assert_eq!(engine.snapshot().total_trades, 0);
    // portfolio 仍为空(无交易)
    assert!(engine.portfolio().positions().is_empty());
}
