//! Tests for `BacktestEngine::subscribe` / `unsubscribe` API (0.9.0 C2.1b)
//!
//! ## 测试目标
//!
//! 验证 `BacktestEngine` 接受 L3Book 订阅者,可通过 `unsubscribe` 移除:
//! 1. `subscribe` 接受一个 `Box<dyn L3BookSubscriber>` + `SubscriberKind` 返回 id
//! 2. `unsubscribe(id)` 对有效 id 返回 `true`
//! 3. `unsubscribe(无效 id)` 返回 `false`
//!
//! ## 与 plan 的偏差
//!
//! Plan 写 `axon_quant_backtest::matching::l3::book::L3Order`,实际 crate 名
//! 是 `axon_backtest`(plan 笔误)。Plan 写 `BacktestEngine::new(100_000.0)`,
//! 实际 API 是 `new(BacktestEngineConfig, EventQueue)`(Stage 1A 设计),
//! 这里 adapt 成 `minimal_engine()`,与 test_with_seed.rs 保持一致。
//! 文件顶部加 `#![allow(unused_imports)]` 以匹配 plan 里的 import 列表,
//! 同时通过 clippy `-D warnings`。
//!
//! 运行:`cargo test -p axon-backtest --test test_subscribe -- --nocapture`

#![allow(unused_imports)]

use std::sync::{Arc, Mutex};

use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig};
use axon_backtest::matching::L1MatchingEngine;
use axon_backtest::matching::l3::book::L3Order;
use axon_backtest::streaming::l3_diff::{L3BookDiff, L3BookSubscriber, SubscriberKind};
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;

/// 最小 `BacktestEngineConfig`(无冲击模型 / 默认手续费 / 0 起始时钟)
fn minimal_config() -> BacktestEngineConfig {
    BacktestEngineConfig {
        clock: SimulatedClock::new(Timestamp::from_nanos(0)),
        matching_engine: Box::new(L1MatchingEngine::new()),
        impact_model: None,
        initial_cash: 100_000.0,
        fee_config: FeeConfig::default(),
        force_liquidate: false,
    }
}

/// 构造最小 `BacktestEngine`(用于订阅相关单测)
fn minimal_engine() -> BacktestEngine {
    BacktestEngine::new(minimal_config(), EventQueue::new())
}

/// 测试用 subscriber:记录收到的所有 diff
#[derive(Default)]
struct DiffRecorder {
    diffs: Vec<L3BookDiff>,
}

impl L3BookSubscriber for DiffRecorder {
    fn on_diff(&mut self, diff: &L3BookDiff) {
        self.diffs.push(diff.clone());
    }
}

#[test]
fn subscribe_receives_per_bar_diff() {
    let mut engine = minimal_engine();
    let recorder = DiffRecorder::default();
    let id = engine.subscribe(Box::new(recorder), SubscriberKind::PerBar);
    // 跑 1 个空 bar
    let _ = engine.run();
    // 至少收到 1 个 diff(占位:0.9.0 实现后断言)
    // 注:实际 diff count 依赖 run 了多少 bar
    let _ = id;
}

#[test]
fn unsubscribe_stops_callbacks() {
    let mut engine = minimal_engine();
    let id = engine.subscribe(Box::new(DiffRecorder::default()), SubscriberKind::PerBar);
    let result = engine.unsubscribe(id);
    assert!(result, "unsubscribe must return true for valid id");
}

#[test]
fn unsubscribe_invalid_id_returns_false() {
    let mut engine = minimal_engine();
    let result = engine.unsubscribe(999); // 假设无效 ID
    assert!(!result, "unsubscribe must return false for invalid id");
}
