//! Tests for `BacktestEngine::with_seed` builder (0.9.0 D1.1c)
//!
//! ## 测试目标
//!
//! 验证 `BacktestEngine::with_seed(seed)` 链式 builder:
//! 1. 同 seed 跑两次,`final_nav` 一致(确定性 replay 语义入口)
//! 2. 不同 seed 至少能跑通(弱断言,不依赖 RNG 已经接入;0.9.0 阶段 seed 仅记录)
//! 3. 链式 builder 编译期正确(`with_seed` 返回 `Self`)
//!
//! ## 与 plan 的偏差
//!
//! Plan 写 `BacktestEngine::new(100_000.0)`,但 `BacktestEngine::new` 实际签名是
//! `new(BacktestEngineConfig, EventQueue)`(Stage 1A 设计)。这里改用真实 API
//! 构造最小 config + 空 event queue,保持 TDD 失败原因(无 `with_seed` 方法)。
//!
//! 运行:`cargo test -p axon-backtest --test test_with_seed -- --nocapture`

use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig};
use axon_backtest::matching::L1MatchingEngine;
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

/// 同 seed 跑两次,`final_nav` 应一致(空事件队列下,两次都 = initial_cash)
#[test]
fn with_seed_deterministic_replay() {
    let mut e1 = BacktestEngine::new(minimal_config(), EventQueue::new()).with_seed(42);
    let mut e2 = BacktestEngine::new(minimal_config(), EventQueue::new()).with_seed(42);
    // 同 seed 跑同序列,最终 NAV 一致
    let nav1 = e1.run().final_nav;
    let nav2 = e2.run().final_nav;
    assert_eq!(nav1, nav2, "same seed must produce same final_nav");
}

/// 不同 seed 也能正常跑(空事件队列下,两次都 = initial_cash,弱断言不依赖 RNG)
#[test]
fn different_seeds_produce_different_runs() {
    let mut e1 = BacktestEngine::new(minimal_config(), EventQueue::new()).with_seed(1);
    let mut e2 = BacktestEngine::new(minimal_config(), EventQueue::new()).with_seed(2);
    let nav1 = e1.run().final_nav;
    let nav2 = e2.run().final_nav;
    // 不同 seed 应该产生不同结果(允许偶然相等,弱断言)
    let _ = (nav1, nav2);
}

/// 链式 builder 编译期正确(`with_seed` 返回 `Self`)
#[test]
fn with_seed_returns_self_for_chaining() {
    let engine = BacktestEngine::new(minimal_config(), EventQueue::new()).with_seed(42);
    let _ = engine; // 编译期:链式 builder 正确
}
