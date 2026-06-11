//! 向量化环境单元测试
//!
//! 覆盖以下场景：
//! - 批量 reset / step
//! - 维度校验（action 数 != num_envs）
//! - 自动重置（done 后下一步 zero-start）
//! - 零环境数 / 越界
//! - 统计信息
//! - 工厂 trait 派生
//! - AsyncVecEnv 并行正确性

use crate::action::types::{Action, ActionSpace, ContinuousActionSpace};
use crate::env::config::EnvConfig;
use crate::env::types::MarketBar;
use crate::vec_env::{BasicEnvFactory, EnvFactory, SyncVecEnv, VecEnvError};

/// 构造 N 根递增 K 线
fn make_market_data(n: usize) -> Vec<MarketBar> {
    (0..n)
        .map(|i| {
            let price = 100.0 + (i as f64) * 0.1;
            MarketBar::new(i as u64, price, price + 0.5, price - 0.5, price, 1000.0)
        })
        .collect()
}

/// 构造测试工厂
///
/// `max_steps` 控制 episode 长度；`market_data_len` 远大于 `max_steps`，
/// 确保 episode 由 `max_steps` 触发而非数据耗尽。
fn make_factory(max_steps: usize) -> BasicEnvFactory {
    let market_data_len = max_steps * 10;
    let config = EnvConfig {
        max_steps,
        return_window: 20,
        ..Default::default()
    };
    let action_space = ActionSpace::Continuous(ContinuousActionSpace::new(-1.0, 1.0));
    BasicEnvFactory::new(config, action_space, make_market_data(market_data_len))
}

// ── SyncVecEnv ──────────────────────────────────────────────

#[test]
fn test_sync_reset_all_returns_n_observations() {
    let factory = make_factory(50);
    let mut envs = SyncVecEnv::new(factory, 4).unwrap();
    let obs = envs.reset_all().unwrap();
    assert_eq!(obs.len(), 4);
    for o in &obs {
        assert!(!o.features.is_empty());
    }
    // 初始状态：所有 done 为 false，step_count = 0
    for i in 0..4 {
        assert!(!envs.is_done(i));
        assert_eq!(envs.step_count(i), 0);
    }
}

#[test]
fn test_sync_step_batch_returns_n_results() {
    let factory = make_factory(50);
    let mut envs = SyncVecEnv::new(factory, 3).unwrap();
    envs.reset_all().unwrap();
    let actions: Vec<Action> = (0..3).map(|_| Action::continuous(vec![0.0])).collect();
    let results = envs.step_batch(actions).unwrap();
    assert_eq!(results.len(), 3);
    for (i, (_obs, reward, done, info)) in results.iter().enumerate() {
        assert_eq!(info.current_step, 1);
        assert!(!done);
        // reward 是 f64，NaN/Inf 检查
        assert!(reward.is_finite(), "reward should be finite");
        // step_count 累加
        assert_eq!(envs.step_count(i), 1);
    }
}

#[test]
fn test_sync_step_batch_dimension_mismatch() {
    let factory = make_factory(10);
    let mut envs = SyncVecEnv::new(factory, 3).unwrap();
    envs.reset_all().unwrap();
    // 给 2 个动作，但 num_envs = 3
    let actions: Vec<Action> = (0..2).map(|_| Action::continuous(vec![0.0])).collect();
    let err = envs.step_batch(actions).unwrap_err();
    assert!(matches!(
        err,
        VecEnvError::DimensionMismatch {
            expected: 3,
            got: 2
        }
    ));
}

#[test]
fn test_sync_zero_envs_returns_error() {
    let factory = make_factory(10);
    let result = SyncVecEnv::new(factory, 0);
    assert_eq!(result.err(), Some(VecEnvError::ZeroEnvs));
}

#[test]
fn test_sync_step_one_out_of_bounds() {
    let factory = make_factory(10);
    let mut envs = SyncVecEnv::new(factory, 2).unwrap();
    let action = Action::continuous(vec![0.0]);
    let err = envs.step_one(5, &action).unwrap_err();
    assert!(matches!(err, VecEnvError::DimensionMismatch { .. }));
}

#[test]
fn test_sync_auto_reset_after_done() {
    // max_steps = 5，market_data = 50 根（充裕）
    // 跑 4 步不 done；第 5 步 done；第 6 步自动重置
    let factory = make_factory(5);
    let mut envs = SyncVecEnv::new(factory, 1).unwrap();
    envs.reset_all().unwrap();

    let hold = Action::continuous(vec![0.0]);
    for step in 1..=4 {
        let r = envs.step_batch(vec![hold.clone()]).unwrap();
        let (_, _, done, info) = &r[0];
        assert!(!done, "step {step} should not be done");
        assert_eq!(info.current_step, step);
    }
    // 第 5 步：current_step 从 4 变 5，5 >= max_steps(5) → done
    let r = envs.step_batch(vec![hold.clone()]).unwrap();
    let (_, _, done, _info) = &r[0];
    assert!(done, "episode should terminate at step 5");
    assert!(envs.is_done(0));
    assert_eq!(
        envs.episode_count(0),
        0,
        "episode_count not yet incremented"
    );

    // 第 6 步：自动重置
    let r = envs.step_batch(vec![hold.clone()]).unwrap();
    let (_, _, done, info) = &r[0];
    assert!(!done, "auto-reset should yield a fresh episode");
    assert_eq!(info.current_step, 1);
    assert_eq!(
        envs.episode_count(0),
        1,
        "episode_count should be 1 after auto-reset"
    );
}

#[test]
fn test_sync_statistics_tracks_rewards_and_steps() {
    let factory = make_factory(20);
    let mut envs = SyncVecEnv::new(factory, 3).unwrap();
    envs.reset_all().unwrap();
    let hold = Action::continuous(vec![0.0]);
    for _ in 0..5 {
        envs.step_batch(vec![hold.clone(); 3]).unwrap();
    }
    let stats = envs.statistics();
    assert_eq!(stats.num_envs, 3);
    assert_eq!(stats.step_counts, vec![5, 5, 5]);
    assert!(stats.total_rewards.iter().all(|r| r.is_finite()));
    assert_eq!(stats.mean_steps(), 5.0);
    assert!(!stats.all_done);
    assert_eq!(stats.done_count, 0);
}

#[test]
fn test_sync_reset_one_resets_episode_count() {
    let factory = make_factory(5);
    let mut envs = SyncVecEnv::new(factory, 1).unwrap();
    envs.reset_all().unwrap();
    let hold = Action::continuous(vec![0.0]);
    // 跑完 episode
    for _ in 0..10 {
        envs.step_batch(vec![hold.clone()]).unwrap();
        if envs.is_done(0) {
            break;
        }
    }
    assert!(envs.is_done(0));
    assert_eq!(
        envs.episode_count(0),
        0,
        "not incremented until next auto-reset"
    );

    let _obs = envs.reset_one(0).unwrap();
    assert_eq!(
        envs.episode_count(0),
        1,
        "reset_one on done env increments count"
    );
    assert!(!envs.is_done(0));
}

#[test]
fn test_sync_env_accessor() {
    let factory = make_factory(10);
    let envs = SyncVecEnv::new(factory, 2).unwrap();
    let _e0 = envs.env(0).expect("env 0 exists");
    let _e1 = envs.env(1).expect("env 1 exists");

    assert!(envs.env(2).is_none(), "out-of-bounds env should be None");

    // 验证两个 env 独立：使用 env_mut 各自推进后 step_count 不同
    let mut envs = envs;
    let a = Action::continuous(vec![0.0]);
    envs.step_one(0, &a).unwrap();
    envs.step_one(0, &a).unwrap();
    envs.step_one(1, &a).unwrap();
    assert_eq!(envs.step_count(0), 2);
    assert_eq!(envs.step_count(1), 1);
    assert_eq!(envs.total_reward(0), envs.total_reward(1));
}

#[test]
fn test_sync_env_with_different_data_per_factory_call() {
    // 验证：工厂 build_env(i) 确实被调用了 N 次，每个环境独立
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    #[derive(Clone)]
    struct CountingFactory {
        inner: BasicEnvFactory,
    }
    impl EnvFactory for CountingFactory {
        fn build_env(
            &self,
            env_id: usize,
        ) -> crate::env::EnvResult<crate::env::trading_env::TradingEnv> {
            COUNTER.fetch_add(1, Ordering::SeqCst);
            self.inner.build_env(env_id)
        }
    }

    COUNTER.store(0, Ordering::SeqCst);
    let factory = CountingFactory {
        inner: make_factory(10),
    };
    let _envs = SyncVecEnv::new(factory, 5).unwrap();
    assert_eq!(COUNTER.load(Ordering::SeqCst), 5);
}

// ── AsyncVecEnv ─────────────────────────────────────────────

#[test]
fn test_async_reset_all_returns_n_observations() {
    use crate::vec_env::AsyncVecEnv;
    let factory = make_factory(50);
    let mut envs = AsyncVecEnv::new(factory, 4).unwrap();
    let obs = envs.reset_all().unwrap();
    assert_eq!(obs.len(), 4);
    for o in &obs {
        assert!(!o.features.is_empty());
    }
}

#[test]
fn test_async_step_batch_returns_n_results() {
    use crate::vec_env::AsyncVecEnv;
    let factory = make_factory(50);
    let mut envs = AsyncVecEnv::new(factory, 3).unwrap();
    envs.reset_all().unwrap();
    let actions: Vec<Action> = (0..3).map(|_| Action::continuous(vec![0.0])).collect();
    let results = envs.step_batch(actions).unwrap();
    assert_eq!(results.len(), 3);
    for r in &results {
        assert!(r.1.is_finite());
    }
    for i in 0..3 {
        assert_eq!(envs.statistics().step_counts[i], 1);
    }
}

#[test]
fn test_async_step_batch_dimension_mismatch() {
    use crate::vec_env::AsyncVecEnv;
    let factory = make_factory(10);
    let mut envs = AsyncVecEnv::new(factory, 3).unwrap();
    envs.reset_all().unwrap();
    let actions: Vec<Action> = (0..2).map(|_| Action::continuous(vec![0.0])).collect();
    let err = envs.step_batch(actions).unwrap_err();
    assert!(matches!(err, VecEnvError::DimensionMismatch { .. }));
}

#[test]
fn test_async_zero_envs_returns_error() {
    use crate::vec_env::AsyncVecEnv;
    let factory = make_factory(10);
    let result = AsyncVecEnv::new(factory, 0);
    assert_eq!(result.err(), Some(VecEnvError::ZeroEnvs));
}

#[test]
fn test_async_auto_reset_after_done() {
    use crate::vec_env::AsyncVecEnv;
    let factory = make_factory(5);
    let mut envs = AsyncVecEnv::new(factory, 1).unwrap();
    envs.reset_all().unwrap();
    let hold = Action::continuous(vec![0.0]);

    for _ in 1..=4 {
        let r = envs.step_batch(vec![hold.clone()]).unwrap();
        assert!(!r[0].2, "steps 1..=4 should not be done");
    }
    let r = envs.step_batch(vec![hold.clone()]).unwrap();
    assert!(r[0].2, "step 5 should be done");
    assert!(envs.is_done(0));

    let r = envs.step_batch(vec![hold.clone()]).unwrap();
    assert!(!r[0].2, "auto-reset produced fresh episode");
}

#[test]
fn test_async_close_joins_workers() {
    // 验证 close 不会 panic / deadlock
    use crate::vec_env::AsyncVecEnv;
    let factory = make_factory(50);
    let envs = AsyncVecEnv::new(factory, 4).unwrap();
    envs.close();
    // drop 完成
}

// ── VecEnvError ─────────────────────────────────────────────

#[test]
fn test_vec_env_error_env_index() {
    let e1 = VecEnvError::Env(3, "boom".to_string());
    assert_eq!(e1.env_index(), Some(3));

    let e2 = VecEnvError::WorkerPanic(7);
    assert_eq!(e2.env_index(), Some(7));

    let e3 = VecEnvError::AllFailed;
    assert_eq!(e3.env_index(), None);

    let e4 = VecEnvError::ZeroEnvs;
    assert_eq!(e4.env_index(), None);
}

#[test]
fn test_vec_env_error_display() {
    let e = VecEnvError::DimensionMismatch {
        expected: 8,
        got: 4,
    };
    let s = e.to_string();
    assert!(s.contains("8"));
    assert!(s.contains("4"));
}

#[test]
fn test_vec_env_error_from_env_error() {
    let env_err = crate::env::error::EnvError::EmptyMarketData;
    let vec_err: VecEnvError = env_err.into();
    assert!(matches!(vec_err, VecEnvError::Env(0, _)));
}

// ── VecEnvStatistics ────────────────────────────────────────

#[test]
fn test_stats_mean_reward_and_steps() {
    use crate::vec_env::stats::VecEnvStatistics;
    let s = VecEnvStatistics {
        num_envs: 3,
        total_rewards: vec![1.0, 2.0, 3.0],
        step_counts: vec![10, 20, 30],
        done_count: 0,
        all_done: false,
    };
    assert!((s.mean_reward() - 2.0).abs() < 1e-9);
    assert!((s.mean_steps() - 20.0).abs() < 1e-9);
}

#[test]
fn test_stats_empty_inputs_return_zero() {
    use crate::vec_env::stats::VecEnvStatistics;
    let s = VecEnvStatistics {
        num_envs: 0,
        total_rewards: vec![],
        step_counts: vec![],
        done_count: 0,
        all_done: false,
    };
    assert_eq!(s.mean_reward(), 0.0);
    assert_eq!(s.mean_steps(), 0.0);
}
