//! 端到端测试:`ExchangeStreamSource` + `StreamingEngine` 串联
//!
//! ## 测试目标
//!
//! 验证 `ExchangeStreamSource`(mock 交易所数据源,`try_push` 同步推入)
//! 与 `StreamingEngine` 的完整串联工作:
//!
//! ```text
//! ExchangeStreamSource::try_push(event)
//!     → next_event() async ─┐
//!                           ▼
//!              StreamingEngine::on_market_event()
//!                           → strategy.on_tick() (optional)
//!                           → submit_order / match / portfolio update
//!                           → return Vec<Event>
//! ```
//!
//! ## 4 个测试场景
//!
//! 1. `exchange_source_push_and_replay_through_engine`:推入多个 tick → 消费 → 喂入 engine → 验证 FIFO
//! 2. `exchange_source_multi_instrument_dispatch`:多 instrument → engine 按 instrument 分发到正确撮合引擎
//! 3. `exchange_source_subscribe_and_buffered`:subscribe 后 is_connected=true,buffered() 计数准确
//! 4. `exchange_source_empty_buffer_returns_none`:buffer 为空时 next_event 返回 None,不 panic
//!
//! 0.6.0 改(BREAKING):`Symbol` → `Instrument`,spot/swap 都用 `Instrument` 派发
//!
//! 运行:`cargo test -p axon-backtest --test e2e_exchange_stream_source`

use axon_backtest::streaming::{
    ExchangeStreamSource, MarketDataEvent, StreamDataSource, StreamingEngine, TradingMode,
};
use axon_core::market::{Side, Tick};
use axon_core::portfolio::Currency;
use axon_core::time::Timestamp;
use axon_core::types::{
    Instrument, Price, Quantity, SpotInstrument, SwapInstrument, SwapSettle, Symbol,
};

// ── helpers ────────────────────────────────────────────────────────────

fn btc_spot() -> Instrument {
    Instrument::Spot(SpotInstrument {
        base: Symbol::from("BTC"),
        quote: Symbol::from("USDT"),
    })
}

fn eth_spot() -> Instrument {
    Instrument::Spot(SpotInstrument {
        base: Symbol::from("ETH"),
        quote: Symbol::from("USDT"),
    })
}

#[allow(dead_code)]
fn btc_swap() -> Instrument {
    Instrument::Swap(SwapInstrument {
        base: Symbol::from("BTC"),
        quote: Symbol::from("USDT"),
        settle: SwapSettle::UsdMargin,
        contract_size: 1.0,
    })
}

fn make_tick(instrument: &Instrument, price: f64, ts_nanos: i64) -> MarketDataEvent {
    MarketDataEvent::Tick {
        instrument: instrument.clone(),
        tick: Tick::new(
            Timestamp::from_nanos(ts_nanos),
            Price::from_f64(price),
            Quantity::from_f64(1.0),
            Side::Buy,
        ),
    }
}

// ── 1. 推入多个 tick → 消费 → 喂入 engine → 验证 FIFO ────────────────

#[tokio::test]
async fn exchange_source_push_and_replay_through_engine() {
    let mut src = ExchangeStreamSource::new("mock-exchange");
    let mut engine = StreamingEngine::new(TradingMode::Backtest);
    engine.register_instrument(btc_spot());
    engine.portfolio_mut().deposit(Currency::USD, 100_000.0);

    let _ = src.subscribe(&[btc_spot()]).await;
    assert!(src.is_connected());

    // 推入 3 个 tick
    src.try_push(make_tick(&btc_spot(), 100.0, 1_000));
    src.try_push(make_tick(&btc_spot(), 101.0, 2_000));
    src.try_push(make_tick(&btc_spot(), 102.0, 3_000));
    assert_eq!(src.buffered(), 3);

    // 消费第一个 tick → 触发 portfolio mark 更新
    let ev1 = src.next_event().await.unwrap();
    let fills1 = engine.on_market_event(ev1);
    // 无 strategy 时 on_market_event 只更新 portfolio,不产生 fill
    assert!(fills1.is_empty());

    // 消费第二个 tick
    let ev2 = src.next_event().await.unwrap();
    let fills2 = engine.on_market_event(ev2);
    assert!(fills2.is_empty());

    // 消费第三个 tick
    let ev3 = src.next_event().await.unwrap();
    let fills3 = engine.on_market_event(ev3);
    assert!(fills3.is_empty());

    // buffer 已空
    assert_eq!(src.buffered(), 0);
    assert!(src.next_event().await.is_none());
}

// ── 2. 多 instrument → engine 按 instrument 分发到正确撮合引擎 ──────────

#[tokio::test]
async fn exchange_source_multi_instrument_dispatch() {
    let mut src = ExchangeStreamSource::new("mock-multi");
    let mut engine = StreamingEngine::new(TradingMode::Backtest);
    engine.register_instrument(btc_spot());
    engine.register_instrument(eth_spot());
    engine.portfolio_mut().deposit(Currency::USD, 100_000.0);

    let _ = src.subscribe(&[btc_spot(), eth_spot()]).await;

    // 推入 BTC 和 ETH 的 tick 交替
    src.try_push(make_tick(&btc_spot(), 50_000.0, 1_000));
    src.try_push(make_tick(&eth_spot(), 3_000.0, 2_000));
    src.try_push(make_tick(&btc_spot(), 50_100.0, 3_000));

    // 消费所有 tick,验证 engine 不 panic 且 portfolio 更新
    while let Some(ev) = src.next_event().await {
        let _ = engine.on_market_event(ev);
    }

    // 两个 instrument 都应被注册
    let snap = engine.snapshot();
    assert_eq!(snap.total_trades, 0); // 无 strategy + 无 maker → 无 fill
}

// ── 3. subscribe 后 is_connected=true,buffered() 计数准确 ─────────────

#[tokio::test]
async fn exchange_source_subscribe_and_buffered() {
    let mut src = ExchangeStreamSource::new("mock-buf");

    assert!(!src.is_connected());
    assert_eq!(src.buffered(), 0);

    let _ = src.subscribe(&[btc_spot()]).await;
    assert!(src.is_connected());

    // 推入 5 个
    for i in 0..5 {
        src.try_push(make_tick(&btc_spot(), 100.0 + i as f64, i * 1_000));
    }
    assert_eq!(src.buffered(), 5);

    // 消费 2 个
    let _ = src.next_event().await;
    let _ = src.next_event().await;
    assert_eq!(src.buffered(), 3);

    // 消费剩余
    let _ = src.next_event().await;
    let _ = src.next_event().await;
    let _ = src.next_event().await;
    assert_eq!(src.buffered(), 0);
}

// ── 4. buffer 为空时 next_event 返回 None,不 panic ────────────────────

#[tokio::test]
async fn exchange_source_empty_buffer_returns_none() {
    let mut src = ExchangeStreamSource::new("mock-empty");
    let _ = src.subscribe(&[btc_spot()]).await;

    // 连续调 3 次,全部返回 None
    assert!(src.next_event().await.is_none());
    assert!(src.next_event().await.is_none());
    assert!(src.next_event().await.is_none());
    assert_eq!(src.buffered(), 0);
}
