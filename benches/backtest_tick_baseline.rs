//! 0.8.0 Phase 3 A3.3:`BacktestEngine::begin_bar` 端到端 tick 延迟基线
//!
//! 运行:`cargo bench -p axon-backtest --bench backtest_tick_baseline`
//!
//! ## 目的
//!
//! A3.0 显示 `MatchingEngine::submit` 已是 0.68µs,远低于 plan 提及的
//! "150µs"(0.7.0 之前乐观目标)。A3.0 决定重规划 A3.x,目标
//! 从"压实 `inner.submit` ≤ 50µs"调整为"`BacktestEngine::begin_bar`
//! 端到端 ≤ 10µs / bar(单 leg,无 fill)"——tick 整体才是真热路径,
//! 不只是单次 submit。
//!
//! ## 场景覆盖
//!
//! | bench | 配置 | 含义 |
//! |-------|------|------|
//! | `begin_bar_minimal` | 单 leg, 无 seed, 无 rebalance, 无 funding | 最小开销 baseline |
//! | `begin_bar_with_seed_5` | 单 leg, `with_seed_liquidity(half=0.5, depth=5)` | 每 bar 挂 10 档 |
//! | `begin_bar_with_seed_50` | 单 leg, `with_seed_liquidity(half=0.5, depth=50)` | 每 bar 挂 100 档 |
//!
//! ## 复现性
//!
//! - `begin_bar` 调 N 次,测单次平均
//! - `black_box()` 防止编译器优化掉 mid_price
//! - 每次 iter 推进 `clock.set()` 到下一分钟,模拟 1 分钟 bar
//!
//! ## Gate
//!
//! `begin_bar_minimal` 应 ≤ 10µs / bar(plan 目标)
//! `begin_bar_with_seed_5` 应 ≤ 30µs / bar(每 bar 10 档 L1 submit)
//! `begin_bar_with_seed_50` 应 ≤ 100µs / bar(每 bar 100 档 L1 submit)
//!
//! ## 验收
//!
//! - `begin_bar_minimal` < 1µs(只有 HashMap lookup + push 1 帧)
//! - `begin_bar_with_seed_5` < 30µs(每 bar 10 档 submit + clear + push 1 帧)
//! - `begin_bar_with_seed_50` < 100µs(每 bar 100 档 submit + clear + push 1 帧)

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};

use axon_backtest::engine::BacktestEngine;
use axon_backtest::engine::BacktestEngineConfig;
use axon_backtest::matching::engine::L1MatchingEngine;
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{Instrument, SpotInstrument};

// ─── 辅助函数 ─────────────────────────────────────────

fn btc_spot() -> Instrument {
    Instrument::Spot(SpotInstrument {
        base: "BTC".into(),
        quote: "USDT".into(),
    })
}

fn minimal_config() -> BacktestEngineConfig {
    BacktestEngineConfig {
        clock: SimulatedClock::new(Timestamp::from_nanos(0)),
        matching_engine: Box::new(L1MatchingEngine::new()),
        impact_model: None,
        initial_cash: 100_000.0,
        fee_config: Default::default(),
        force_liquidate: false,
    }
}

/// 跑 N 根 bar,返回每根平均延迟(ns)
fn run_n_bars(engine: &mut BacktestEngine, inst: &Instrument, n: u64, mid_price: f64) -> u128 {
    let start = std::time::Instant::now();
    for i in 0..n {
        // 1 分钟 bar(i+1 避免与 0 冲突)
        let ts_nanos: i64 = ((i + 1) as i64) * 60 * 1_000_000_000;
        engine.set_clock(Timestamp::from_nanos(ts_nanos));
        engine.begin_bar(mid_price, inst.clone());
    }
    start.elapsed().as_nanos() / n as u128
}

// ─── 基准 ─────────────────────────────────────────────

/// 1. 最小配置:无 seed,无 rebalance target,无 funding,无 position
fn bench_begin_bar_minimal(c: &mut Criterion) {
    let inst = btc_spot();
    let mut engine = BacktestEngine::new(minimal_config(), EventQueue::new());

    // 预热
    engine.begin_bar(100.0, inst.clone());
    black_box(&engine);

    c.bench_function("begin_bar_minimal", |b| {
        b.iter(|| {
            let avg_ns = run_n_bars(&mut engine, &inst, 1000, 100.0);
            black_box(avg_ns);
        })
    });
}

/// 2. seed_liquidity(half=0.5, depth=5)→ 每 bar 挂 5×2=10 档
fn bench_begin_bar_with_seed_5(c: &mut Criterion) {
    let inst = btc_spot();
    let mut engine = BacktestEngine::new(minimal_config(), EventQueue::new());
    engine.with_seed_liquidity(0.5, 5, 1.0);
    // 预热
    engine.begin_bar(100.0, inst.clone());
    black_box(&engine);

    c.bench_function("begin_bar_with_seed_5", |b| {
        b.iter(|| {
            let avg_ns = run_n_bars(&mut engine, &inst, 1000, 100.0);
            black_box(avg_ns);
        })
    });
}

/// 3. seed_liquidity(half=0.5, depth=50)→ 每 bar 挂 50×2=100 档
fn bench_begin_bar_with_seed_50(c: &mut Criterion) {
    let inst = btc_spot();
    let mut engine = BacktestEngine::new(minimal_config(), EventQueue::new());
    engine.with_seed_liquidity(0.5, 50, 1.0);
    // 预热
    engine.begin_bar(100.0, inst.clone());
    black_box(&engine);

    c.bench_function("begin_bar_with_seed_50", |b| {
        b.iter(|| {
            let avg_ns = run_n_bars(&mut engine, &inst, 1000, 100.0);
            black_box(avg_ns);
        })
    });
}

// ─── 入口 ─────────────────────────────────────────────

criterion_group!(
    name = backtest_tick_baseline;
    config = Criterion::default()
        .sample_size(50)  // 50 samples × ~5K iters = ~250K ops/bench
        .measurement_time(std::time::Duration::from_secs(8));
    targets =
        bench_begin_bar_minimal,
        bench_begin_bar_with_seed_5,
        bench_begin_bar_with_seed_50,
);
criterion_main!(backtest_tick_baseline);
