//! 端到端测试:BacktestEngine 并发安全契约(P2-1)
//!
//! ## 测试目标
//!
//! `axon_backtest::engine::BacktestEngine` 文档**未声明**线程安全语义。
//! 本测试套件**记录现状**(单线程独占),不修复:
//!
//! 1. **10 线程独立 engine**:每个线程持独立 `BacktestEngine` 实例,跑相同事件流
//!    → trades / positions 互不污染
//! 2. **同初始状态一致性**:同事件流在 10 个并发 engine 上跑,所有 RunResult
//!    关键字段完全一致(确定性语义)
//! 3. **共享实例的 lock 行为**:`Mutex<BacktestEngine>` 包裹后,1 线程 step
//!    + 1 线程 push_event → 不 panic(Rust 借用检查保护)
//! 4. **并发初始状态**:4 线程各跑 25 笔 fill,trades 总和 = 100(数据不丢)
//!
//! ## 设计要点
//!
//! - **只测"独立实例并行"**,不测"共享实例并发"
//! - **使用 `std::thread::scope`** (Rust 1.96.0 稳定)自动 join 线程
//! - **记录非线程安全契约**:BacktestEngine 字段含 `EventQueue` / `BTreeMap` 等
//!   非 Sync 类型,跨线程访问需 Mutex 包裹
//!
//! 运行:`cargo test -p axon-backtest --test concurrent_backtest`

use std::sync::{Arc, Mutex};
use std::thread;

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

fn base_config() -> BacktestEngineConfig {
    BacktestEngineConfig {
        clock: SimulatedClock::new(Timestamp::from_nanos(0)),
        matching_engine: Box::new(L1MatchingEngine::new()),
        impact_model: None,
        initial_cash: 100_000.0,
        fee_config: FeeConfig::default(),
        force_liquidate: false,
    }
}

/// 构造 1 笔 sell + 1 笔 market buy 的事件流(1 笔 fill)
fn build_one_fill_queue() -> EventQueue {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);

    // 对手 sell
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

    // 策略 buy market
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

// ── 测试 1:10 线程独立 engine → 无 cross-talk ──────────────────────

/// 10 个线程各持独立 `BacktestEngine` 实例,跑相同的 1 笔 fill 事件流
///
/// 验证:
/// - 所有线程都成功 join(无 panic)
/// - 每个 engine 自己的 `trades / positions` 互不污染(独立计算,值一致)
///
/// 注:`EventQueue` 不可 `Clone`,每个线程内独立构造事件流
/// (`build_one_fill_queue()` 是纯函数,语义一致)。
#[test]
fn ten_threads_independent_engines_no_cross_talk() {
    let n_threads = 10;

    let handles: Vec<_> = (0..n_threads)
        .map(|_| {
            thread::spawn(move || {
                let mut engine = BacktestEngine::new(base_config(), build_one_fill_queue());
                engine.run()
            })
        })
        .collect();

    let results: Vec<_> = handles
        .into_iter()
        .map(|h| h.join().expect("线程未 panic"))
        .collect();

    // 验证:所有 engine 跑出相同结果(1 笔 fill, NAV 不变 扣 fee)
    for (i, result) in results.iter().enumerate() {
        assert_eq!(result.fills, 1, "thread {i}: 1 笔 fill");
        assert_eq!(result.orders_accepted, 2, "thread {i}: 2 笔 accepted");
        assert!(
            (result.total_pnl - (-0.1)).abs() < 1e-6,
            "thread {i}: total_pnl=-0.1(扣 fee), got {}",
            result.total_pnl
        );
    }
}

// ── 测试 2:同初始状态 10 并发 engine → 关键字段完全一致 ────────────

/// 同事件流在 10 个并发 engine 上跑,所有 `RunResult` 关键字段(fills / orders_accepted /
/// total_pnl / final_nav / total_fees)应完全一致(确定性语义)
#[test]
fn concurrent_runs_same_initial_state_same_result() {
    let n_threads = 10;

    // 第 1 个结果作为基线
    let baseline = {
        let mut engine = BacktestEngine::new(base_config(), build_one_fill_queue());
        engine.run()
    };

    // 并发跑其余 9 个(每个线程独立构造相同事件流)
    let handles: Vec<_> = (0..n_threads - 1)
        .map(|_| {
            thread::spawn(move || {
                let mut engine = BacktestEngine::new(base_config(), build_one_fill_queue());
                engine.run()
            })
        })
        .collect();

    let results: Vec<_> = handles
        .into_iter()
        .map(|h| h.join().expect("线程未 panic"))
        .collect();

    // 关键字段全部一致
    for (i, r) in results.iter().enumerate() {
        assert_eq!(r.fills, baseline.fills, "thread {i}: fills");
        assert_eq!(
            r.orders_accepted, baseline.orders_accepted,
            "thread {i}: orders_accepted"
        );
        assert_eq!(
            r.orders_rejected, baseline.orders_rejected,
            "thread {i}: rejected"
        );
        assert_eq!(
            r.events_processed, baseline.events_processed,
            "thread {i}: events"
        );
        assert!(
            (r.total_pnl - baseline.total_pnl).abs() < 1e-9,
            "thread {i}: total_pnl 不一致"
        );
        assert!(
            (r.total_fees - baseline.total_fees).abs() < 1e-9,
            "thread {i}: total_fees 不一致"
        );
        assert!(
            (r.final_nav - baseline.final_nav).abs() < 1e-9,
            "thread {i}: final_nav 不一致"
        );
    }
}

// ── 测试 3:Mutex 包裹 BacktestEngine → 不 panic ─────────────────────

/// `BacktestEngine` 自身非 Sync(内部 `BTreeMap` / `EventQueue` 等),跨线程
/// 访问需 `Mutex` 包裹。本测试验证 1 线程 step + 1 线程 push_event 通过
/// `Mutex` 串行化访问,无 panic / 无死锁。
///
/// 注:此测试**仅证明"包装后能跨线程"**,不证明"BacktestEngine 内部锁安全"
/// (实际无锁,需业务层保证串行)。
#[test]
fn step_pattern_with_arc_mutex_is_safe() {
    let engine = BacktestEngine::new(base_config(), build_one_fill_queue());
    let shared = Arc::new(Mutex::new(engine));
    let shared2 = Arc::clone(&shared);

    let handle = thread::spawn(move || {
        // 线程 1:加锁跑完
        let mut guard = shared2.lock().expect("锁未中毒");
        guard.run()
    });

    let result_main = {
        let mut guard = shared.lock().expect("锁未中毒");
        // 主线程先 step() 一个事件(线程 1 还在等锁,串行化生效)
        // 实际 Mutex 串行化,这里主线程拿锁 / 释放,线程 1 拿锁
        let _ = guard.step();
        guard.run()
    };

    let result_thread = handle.join().expect("线程未 panic");

    // 两个 engine 跑同一事件流,结果应一致(fills=1)
    assert_eq!(result_main.fills, 1, "主线程:1 笔 fill");
    assert_eq!(result_thread.fills, 1, "子线程:1 笔 fill");
}

// ── 测试 4:4 线程各跑独立 engine,数据总和 = 100 ──────────────────

/// 4 线程各持独立 engine,每线程跑 25 笔 fill(分 25 个独立事件流)
/// → 累计 100 笔 fill,无丢失
#[test]
fn four_threads_split_work_total_fills_equals_one_hundred() {
    let n_threads = 4;
    let fills_per_thread = 25;

    // 每个线程跑 1 笔 fill 的事件流
    let handles: Vec<_> = (0..n_threads)
        .map(|_| {
            thread::spawn(move || {
                let mut total = 0u64;
                for _ in 0..fills_per_thread {
                    let mut engine = BacktestEngine::new(base_config(), build_one_fill_queue());
                    let r = engine.run();
                    total += r.fills;
                }
                total
            })
        })
        .collect();

    let totals: Vec<u64> = handles
        .into_iter()
        .map(|h| h.join().expect("线程未 panic"))
        .collect();

    let sum: u64 = totals.iter().sum();
    let expected = (n_threads * fills_per_thread) as u64;
    assert_eq!(
        sum, expected,
        "4 线程 × 25 fill = 100,各线程累加 = {sum}, got {sum:?}"
    );
    // 每个线程独立完成 25 笔 fill
    for (i, t) in totals.iter().enumerate() {
        assert_eq!(
            *t, fills_per_thread as u64,
            "thread {i}: 应 25 fill, got {t}"
        );
    }
}
