//! 端到端集成测试:ReplayStreamSource
//!
//! ## 测试目标
//!
//! 验证 `ReplayStreamSource` 与 `StreamingEngine` 的串联工作:
//!
//! ```text
//! ReplayStreamSource (Vec<Tick>)
//!     → next_event() async ─┐
//!                           ▼
//!              StreamingEngine::on_market_event()
//!                           → strategy.on_tick() (optional)
//!                           → submit_order / match / portfolio update
//!                           → return Vec<Event>
//! ```
//!
//! 涵盖 4 个集成点:
//! 1. tick 按 FIFO 顺序通过引擎处理
//! 2. 全部消费后 `next_event` 返回 `None`
//! 3. 与自定义 strategy 串联产生 fill event
//! 4. `remaining()` / `consumed()` 计数在消费过程中正确变化
//!
//! 运行:`cargo test -p axon-backtest --test replay_source_integration`

use std::collections::VecDeque;

use axon_backtest::streaming::{
    MarketDataEvent, ReplayStreamSource, StrategyAction, StreamDataSource, StreamingEngine,
    StreamingStrategy, TradingMode,
};
use axon_core::event::Event;
use axon_core::market::{Side, Tick};
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::portfolio::Currency;
use axon_core::time::Timestamp;
use axon_core::types::{Price, Quantity, Symbol};

// ── helpers ────────────────────────────────────────────────────────────

fn btc() -> Symbol {
    Symbol::from("BTC/USDT")
}

fn make_tick(price: f64) -> Tick {
    Tick::new(
        Timestamp::from_nanos(1_000),
        Price::from_f64(price),
        Quantity::from_f64(1.0),
        Side::Buy,
    )
}

fn make_market(id: u64, side: Side, qty: f64) -> Order {
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

fn make_limit(id: u64, side: Side, price: f64, qty: f64) -> Order {
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

/// 固定动作策略:每次 `on_tick` 弹出一个预设 action
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

// ── 1. tick 通过 ReplayStreamSource → StreamingEngine FIFO 处理 ────────

#[tokio::test]
async fn replay_source_replays_ticks_through_streaming_engine_in_fifo_order() {
    // 准备 5 个 tick
    let prices = vec![100.0, 101.5, 103.0, 102.0, 105.0];
    let ticks: Vec<Tick> = prices.iter().copied().map(make_tick).collect();

    let mut source = ReplayStreamSource::new("/tmp/nonexistent.csv").with_ticks(btc(), ticks);
    source.subscribe(&[btc()]).await.expect("subscribe ok");
    assert!(source.is_connected());
    assert_eq!(source.remaining(), 5);
    assert_eq!(source.consumed(), 0);

    // 引擎只跟踪 portfolio mark,无 strategy → 不产生 fill
    let mut engine = StreamingEngine::new(TradingMode::Backtest);
    engine.register_symbol(btc());
    engine.portfolio_mut().deposit(Currency::USD, 100_000.0);

    let mut observed: Vec<f64> = Vec::new();
    while let Some(event) = source.next_event().await {
        if let MarketDataEvent::Tick { tick, .. } = &event {
            observed.push(tick.price.as_f64());
        }
        let events = engine.on_market_event(event);
        // 无 strategy 路径,不应产生任何 fill
        assert!(
            events.is_empty(),
            "无 strategy 时 on_market_event 不应产生 fill,实为 {events:?}"
        );
    }

    // 验证价格按入队顺序消费
    assert_eq!(observed, prices);
    // 验证 source 全部消费
    assert_eq!(source.remaining(), 0);
    assert_eq!(source.consumed(), 5);
    // portfolio 仍只有初始现金(无 fill)
    assert_eq!(engine.snapshot().total_trades, 0);
    assert!((engine.portfolio().base_cash() - 100_000.0).abs() < 1e-9);
}

// ── 2. tick 用尽后 next_event 返回 None ────────────────────────────────

#[tokio::test]
async fn replay_source_drains_to_none_after_all_ticks_consumed() {
    let mut source = ReplayStreamSource::new("/tmp/nonexistent.csv")
        .with_ticks(btc(), vec![make_tick(100.0), make_tick(101.0)]);
    source.subscribe(&[btc()]).await.expect("subscribe ok");

    // 消费 2 个
    let e1 = source.next_event().await;
    let e2 = source.next_event().await;
    assert!(e1.is_some());
    assert!(e2.is_some());

    // 第三次应返回 None
    let e3 = source.next_event().await;
    assert!(e3.is_none(), "tick 用尽后应返回 None,实为 {e3:?}");

    // 反复调 None 应持续返回 None(不会 panic / 不会循环)
    for _ in 0..3 {
        let again = source.next_event().await;
        assert!(again.is_none());
    }
    assert_eq!(source.remaining(), 0);
    assert_eq!(source.consumed(), 2);
}

// ── 3. ReplayStreamSource + 自定义 strategy → 端到端 fill ──────────────

#[tokio::test]
async fn replay_source_with_strategy_drives_fills_end_to_end() {
    // 单边上涨 tick 序列:SMA(2) > SMA(3) → strategy 发 Buy Market
    // 但 Market Buy 需要对手机 → 我们在 setup 阶段预挂一个对手机 Sell
    let prices: Vec<f64> = (0..6).map(|i| 100.0 + i as f64 * 1.0).collect();
    let ticks: Vec<Tick> = prices.iter().copied().map(make_tick).collect();

    let mut source = ReplayStreamSource::new("/tmp/nonexistent.csv").with_ticks(btc(), ticks);
    source.subscribe(&[btc()]).await.expect("subscribe ok");

    // 引擎:Backtest 模式,挂对手机 Sell @很低(全吃)
    let mut engine = StreamingEngine::new(TradingMode::Backtest);
    engine.register_symbol(btc());
    engine.portfolio_mut().deposit(Currency::USD, 100_000.0);

    // 预挂 1 个对手机 Sell @2000 qty=1(数量足够,任何 Buy 都能吃)
    let maker = make_limit(900, Side::Sell, 2000.0, 1.0);
    engine.submit_order(maker).expect("submit maker");

    // 策略:把所有 on_tick 都映射为 Market Buy qty=0.1(只触发 1 次,后续 asks 空就不成交)
    let strategy = FixedStrategy::new(vec![StrategyAction::Submit(make_market(1, Side::Buy, 0.1))]);
    let mut engine = engine.with_strategy(Box::new(strategy));

    // 跑完 source 全部 tick
    let mut total_fills = 0;
    let mut consumed = 0;
    while let Some(event) = source.next_event().await {
        consumed += 1;
        let events = engine.on_market_event(event);
        for ev in &events {
            if let Event::Fill(_) = ev {
                total_fills += 1;
            }
        }
    }

    // 期望:strategy 在第 1 个 tick 时返回 Market Buy → 撮合 maker @2000 → 1 笔 fill
    // 后续 tick 走 strategy.actions 已空 → 无新 fill
    assert!(total_fills >= 1, "应至少 1 笔 fill,实为 {total_fills}");
    assert_eq!(consumed, 6, "应消费 6 个 tick");

    // 验证 portfolio:买入 0.1 @2000 → 持仓 0.1 BTC,cash 减少 200
    // 0.5.0 起 Portfolio 用 Instrument key
    let inst = axon_core::types::Instrument::from_symbol(&btc());
    let pos = engine
        .portfolio()
        .position_by_instrument(&inst)
        .expect("应有持仓");
    assert!(
        (pos.quantity.as_f64() - 0.1).abs() < 1e-9,
        "持仓 0.1 BTC,实为 {}",
        pos.quantity.as_f64()
    );
}

// ── 4. remaining() / consumed() 在消费过程中逐步变化 ─────────────────

#[tokio::test]
async fn replay_source_remaining_and_consumed_track_progress() {
    // 注入 4 个 tick
    let mut source = ReplayStreamSource::new("/tmp/nonexistent.csv").with_ticks(
        btc(),
        vec![
            make_tick(100.0),
            make_tick(101.0),
            make_tick(102.0),
            make_tick(103.0),
        ],
    );
    source.subscribe(&[btc()]).await.expect("subscribe ok");

    // 初始:remaining=4, consumed=0
    assert_eq!(source.remaining(), 4);
    assert_eq!(source.consumed(), 0);

    // 消费 1:remaining=3, consumed=1
    let _ = source.next_event().await;
    assert_eq!(source.remaining(), 3);
    assert_eq!(source.consumed(), 1);

    // 消费 2:remaining=2, consumed=2
    let _ = source.next_event().await;
    assert_eq!(source.remaining(), 2);
    assert_eq!(source.consumed(), 2);

    // 消费剩余 2 个:remaining=0, consumed=4
    let _ = source.next_event().await;
    let _ = source.next_event().await;
    assert_eq!(source.remaining(), 0);
    assert_eq!(source.consumed(), 4);

    // 已耗尽:再 next_event 不影响 remaining/consumed 计数
    let none = source.next_event().await;
    assert!(none.is_none());
    assert_eq!(source.remaining(), 0);
    assert_eq!(source.consumed(), 4);
}
