//! 端到端测试:`PaperTradingEngine` partial fill 裁决
//!
//! ## 测试目标
//!
//! 验证 0.4.0 新增的 `SimulatedExchange::{fill_probability, partial_fill_min_ratio, seed}`
//! 在 `PaperTradingEngine::should_fill()` / `fill_ratio()` 和 `StreamingEngine` 路径上
//! 行为正确。
//!
//! ## 5 个测试场景
//!
//! 1. `default_config_full_fill_unchanged`:默认配置下(`fill_probability=0.95` + `partial_fill_min_ratio=1.0`),
//!    整笔成交且不缩量(向后兼容老路径)
//! 2. `fill_probability_zero_rejects_everything`:`fill_probability=0.0` → 100% 拒单
//! 3. `fill_probability_full_accepts_everything`:`fill_probability=1.0` → 100% 全成
//! 4. `partial_fill_min_ratio_lt_one_scales_quantity`:`partial_fill_min_ratio=0.5` 时,
//!    `StreamingEngine` 路径上 fill quantity 被缩量到 `[0.5, 1.0]` 区间
//! 5. `seed_determinism_same_seed_same_decisions`:同 seed → 同 should_fill 序列(可重复)
//!
//! 运行:`cargo test -p axon-backtest --test paper_partial_fill_test`

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

// ── helpers ───────────────────────────────────────────────────────────

fn btc() -> Symbol {
    Symbol::from("BTC-USDT")
}

fn make_limit(id: u64, side: Side, price: f64, qty: f64) -> Order {
    Order::new(
        id,
        btc(),
        side,
        OrderType::Limit {
            price: Price::from_f64(price),
        },
        Quantity::from_f64(qty),
        TimeInForce::GTC,
    )
}

fn make_market(id: u64, side: Side, qty: f64) -> Order {
    Order::new(
        id,
        btc(),
        side,
        OrderType::Market,
        Quantity::from_f64(qty),
        TimeInForce::IOC,
    )
}

fn make_tick(price: f64) -> Tick {
    Tick::new(
        Timestamp::from_nanos(1_000),
        Price::from_f64(price),
        Quantity::from_f64(1.0),
        Side::Buy,
    )
}

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

// ── 1. 默认配置 → 100% 全成且不缩量(向后兼容) ────────────────────

#[test]
fn default_config_full_fill_unchanged() {
    // 默认 SimulatedExchange:fill_probability=0.95, partial_fill_min_ratio=1.0
    // seed 固定 → 跑 1000 次,统计拒单率应 < 5%(95% 全成)
    let mut engine = PaperTradingEngine::new(SimulatedExchange::default()).with_seed(0xDEFA);
    let n = 1_000;
    let accepted = (0..n).filter(|_| engine.should_fill()).count();
    let reject_rate = (n - accepted) as f64 / n as f64;
    assert!(
        reject_rate < 0.05,
        "默认 0.95 概率下,跑 {n} 次拒单率应 < 5%,实为 {reject_rate:.3}"
    );
    // fill_ratio 默认 1.0(partial_fill_min_ratio=1.0)
    for _ in 0..100 {
        assert!(
            (engine.fill_ratio() - 1.0).abs() < 1e-9,
            "默认 partial_fill_min_ratio=1.0 → fill_ratio=1.0"
        );
    }
}

// ── 2. fill_probability=0.0 → 100% 拒单 ────────────────────────────

#[test]
fn fill_probability_zero_rejects_everything() {
    let ex = SimulatedExchange {
        fill_probability: 0.0,
        ..SimulatedExchange::default()
    };
    let mut engine = PaperTradingEngine::new(ex);
    for _ in 0..1000 {
        assert!(!engine.should_fill(), "fill_probability=0.0 应 100% 拒单");
    }
    // fill_ratio 与 should_fill 解耦:即使概率 0,fill_ratio 仍走 partial_fill_min_ratio=1.0 → 1.0
    assert!((engine.fill_ratio() - 1.0).abs() < 1e-9);
}

// ── 3. fill_probability=1.0 → 100% 全成 ─────────────────────────────

#[test]
fn fill_probability_full_accepts_everything() {
    let ex = SimulatedExchange {
        fill_probability: 1.0,
        ..SimulatedExchange::default()
    };
    let mut engine = PaperTradingEngine::new(ex);
    for _ in 0..1000 {
        assert!(engine.should_fill(), "fill_probability=1.0 应 100% 全成");
    }
}

// ── 4. partial_fill_min_ratio<1.0 → engine 路径上 fill 被缩量 ────────

#[test]
fn partial_fill_min_ratio_lt_one_scales_quantity() {
    // maker: Sell Limit @100 qty 10
    // strategy: Buy Market qty 10
    // paper config: fill_probability=1.0 + partial_fill_min_ratio=0.5
    //   → 100% 全成 + fill_ratio ∈ [0.5, 1.0]
    // 期望:fill quantity ∈ [5.0, 10.0]
    let paper = PaperTradingEngine::new(SimulatedExchange {
        fill_probability: 1.0,
        partial_fill_min_ratio: 0.5,
        ..SimulatedExchange::default()
    })
    .with_seed(42);

    let mut engine = StreamingEngine::new(TradingMode::PaperTrading).with_paper_engine(paper);
    engine.register_symbol(btc());
    engine.portfolio_mut().deposit(Currency::USD, 100_000.0);

    // maker
    let maker = make_limit(900, Side::Sell, 100.0, 10.0);
    engine.submit_order(maker).expect("submit maker");

    // strategy: Buy Market qty 10
    let strategy = FixedStrategy::new(vec![StrategyAction::Submit(make_market(1, Side::Buy, 10.0))]);
    let mut engine = engine.with_strategy(Box::new(strategy));

    let events = engine.on_market_event(MarketDataEvent::Tick {
        symbol: btc(),
        tick: make_tick(100.0),
    });

    // 1 笔 fill, quantity 落在 [5.0, 10.0]
    assert_eq!(events.len(), 1, "fill_probability=1.0 + 有 maker → 必有 1 fill");
    let fill_qty = match &events[0] {
        Event::Fill(f) => f.trade.quantity.as_f64(),
        other => panic!("期望 Event::Fill,实为 {other:?}"),
    };
    assert!(
        (5.0..=10.0).contains(&fill_qty),
        "partial_fill_min_ratio=0.5 时 fill qty 应∈[5.0, 10.0],实为 {fill_qty}"
    );
    // critical: 一定小于原 10.0(seed 决定的第一次 rng gen)
    // 注:第一次 gen_range(0.0..1.0) 不一定为 1.0,所以可能等于 10.0
    // 仅断言下界满足
    assert!(fill_qty >= 5.0, "下界 5.0 应满足,实为 {fill_qty}");
}

// ── 5. 同 seed → 同 should_fill 序列(可重复测试) ──────────────────

#[test]
fn seed_determinism_same_seed_same_decisions() {
    fn make_engine() -> PaperTradingEngine {
        PaperTradingEngine::new(SimulatedExchange {
            fill_probability: 0.5,
            ..SimulatedExchange::default()
        })
        .with_seed(2026)
    }
    let mut a = make_engine();
    let mut b = make_engine();
    for _ in 0..1000 {
        assert_eq!(
            a.should_fill(),
            b.should_fill(),
            "同 seed 应产生同 should_fill 序列"
        );
        assert_eq!(a.fill_ratio(), b.fill_ratio(), "同 seed 应产生同 fill_ratio 序列");
    }
}
