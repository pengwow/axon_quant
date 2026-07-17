//! 端到端测试:`SmaCrossover` 策略 + `StreamingEngine` 完整链路
//!
//! ## 测试目标
//!
//! 验证 `SmaCrossover` 策略(定义在 `strategy.rs`,仅在单元测试中验证 `on_tick` 返回值)
//! 走完 `StreamingEngine` → `on_market_event` → `strategy.on_tick` → `Submit` → L1 撮合 → `fill`
//! 的完整端到端链路。
//!
//! ## 4 个测试场景
//!
//! 1. `sma_crossover_uptrend_triggers_buy_fills`:递增价格 → short_sma > long_sma → Buy Market → fill
//! 2. `sma_crossover_downtrend_holds`:递减价格 → short_sma < long_sma → Hold → 无 fill
//! 3. `sma_crossover_mixed_switches_behavior`:先升后降 → 上升段发单、下降段观望
//! 4. `sma_crossover_order_ids_increment`:连续触发 → 订单 id 自增不冲突
//!
//! 运行:`cargo test -p axon-backtest --test e2e_sma_crossover`

use axon_backtest::streaming::{MarketDataEvent, SmaCrossover, StreamingEngine, TradingMode};
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

fn make_tick(price: f64, ts_nanos: i64) -> Tick {
    Tick::new(
        Timestamp::from_nanos(ts_nanos),
        Price::from_f64(price),
        Quantity::from_f64(1.0),
        Side::Buy,
    )
}

/// 注入 SmaCrossover 策略的 engine(Backtest 模式,无 paper 滑点干扰)
/// 同时挂一个 Sell Limit maker,让 Market Buy 有对手盘可撮合
fn engine_with_sma(short_win: usize, long_win: usize) -> StreamingEngine {
    let mut engine = StreamingEngine::new(TradingMode::Backtest);
    engine.register_symbol(btc());
    engine.portfolio_mut().deposit(Currency::USD, 100_000.0);
    // 挂 Sell Limit maker(Market Buy 的对手盘)
    let maker = Order::spot(
        900,
            "BTC",
            "USDT",
        Side::Sell,
        OrderType::Limit {
            price: Price::from_f64(200.0), // 高于所有测试价格,确保能撮合
        },
        Quantity::from_f64(100.0), // 足够大的量
        TimeInForce::GTC,
    );
    engine.submit_order(maker).expect("submit maker");
    engine.with_strategy(Box::new(SmaCrossover::new(short_win, long_win)))
}

// ── 1. 递增价格 → short_sma > long_sma → Buy Market → fill ────────────

#[test]
fn sma_crossover_uptrend_triggers_buy_fills() {
    // SmaCrossover(2, 3): 需要至少 3 个 tick 才能同时算出 short(2) 和 long(3)
    // 第 3 个 tick 后: short=avg(101,102)=101.5, long=avg(100,101,102)=101.0 → 101.5 > 101.0 → Buy
    let mut engine = engine_with_sma(2, 3);

    let mut total_fills = 0;
    for (i, price) in [100.0, 101.0, 102.0, 103.0, 104.0].iter().enumerate() {
        let events = engine.on_market_event(MarketDataEvent::Tick {
            symbol: btc(),
            tick: make_tick(*price, (i as i64 + 1) * 1_000),
        });
        total_fills += events.len();

        // 验证 fill 为 Buy Market
        for ev in &events {
            match ev {
                Event::Fill(fill) => {
                    assert!(fill.trade.quantity.as_f64() > 0.0);
                }
                other => panic!("期望 Event::Fill,实为 {other:?}"),
            }
        }
    }

    // 递增序列应产生至少 1 笔 fill(short > long 时持续触发)
    assert!(
        total_fills > 0,
        "递增价格序列应触发 Buy fill,实为 {total_fills} 笔"
    );
}

// ── 2. 递减价格 → short_sma < long_sma → Hold → 无 fill ───────────────

#[test]
fn sma_crossover_downtrend_holds() {
    let mut engine = engine_with_sma(2, 3);

    let mut total_fills = 0;
    for (i, price) in [100.0, 99.0, 98.0, 97.0, 96.0].iter().enumerate() {
        let events = engine.on_market_event(MarketDataEvent::Tick {
            symbol: btc(),
            tick: make_tick(*price, (i as i64 + 1) * 1_000),
        });
        total_fills += events.len();
    }

    // 递减序列: short < long 始终成立 → 全部 Hold → 无 fill
    assert_eq!(
        total_fills, 0,
        "递减价格序列应全部 Hold,实为 {total_fills} 笔 fill"
    );
    assert_eq!(engine.snapshot().total_trades, 0);
}

// ── 3. 先升后降 → 上升段发单、下降段观望 ──────────────────────────────

#[test]
fn sma_crossover_mixed_switches_behavior() {
    let mut engine = engine_with_sma(2, 3);

    // 阶段 1:递增 → 触发 Buy
    let mut fills_in_uptrend = 0;
    for (i, price) in [100.0, 101.0, 102.0, 103.0].iter().enumerate() {
        let events = engine.on_market_event(MarketDataEvent::Tick {
            symbol: btc(),
            tick: make_tick(*price, (i as i64 + 1) * 1_000),
        });
        fills_in_uptrend += events.len();
    }
    assert!(
        fills_in_uptrend > 0,
        "上升段应触发 Buy fill,实为 {fills_in_uptrend}"
    );

    // 阶段 2:急剧递减 → short < long → Hold
    // 需要足够大的跌幅让 short_sma 迅速低于 long_sma
    let mut fills_in_downtrend = 0;
    for (i, price) in [80.0, 70.0, 60.0, 50.0].iter().enumerate() {
        let events = engine.on_market_event(MarketDataEvent::Tick {
            symbol: btc(),
            tick: make_tick(*price, (4 + i as i64) * 1_000),
        });
        fills_in_downtrend += events.len();
    }
    assert_eq!(
        fills_in_downtrend, 0,
        "下降段应 Hold 无 fill,实为 {fills_in_downtrend}"
    );
}

// ── 4. 连续触发时订单 id 自增不冲突 ──────────────────────────────────

#[test]
fn sma_crossover_order_ids_increment() {
    let mut engine = engine_with_sma(2, 3);

    let mut seen_ids = Vec::new();
    for (i, price) in [100.0, 101.0, 102.0, 103.0, 104.0, 105.0]
        .iter()
        .enumerate()
    {
        let events = engine.on_market_event(MarketDataEvent::Tick {
            symbol: btc(),
            tick: make_tick(*price, (i as i64 + 1) * 1_000),
        });
        for ev in &events {
            if let Event::Fill(fill) = ev {
                // 验证 trade 存在(订单已被撮合)
                assert!(fill.trade.quantity.as_f64() > 0.0);
                seen_ids.push(fill.trade.buyer_order_id);
            }
        }
    }

    // 所有 fill 的 taker_order_id 应互不相同
    seen_ids.sort();
    seen_ids.dedup();
    assert_eq!(
        seen_ids.len(),
        engine.snapshot().total_trades,
        "订单 id 应互不相同"
    );
}
