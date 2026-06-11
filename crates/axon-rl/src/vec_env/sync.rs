//! 同步向量化环境
//!
//! 在 **单线程** 中顺序管理 N 个 `TradingEnv` 实例，行为确定、易于测试。
//! 真正并行版本见 [`super::AsyncVecEnv`]。

use crate::action::types::Action;
use crate::env::EnvError;
use crate::env::trading_env::TradingEnv;
use crate::env::types::EnvInfo;
use crate::observation::types::Observation;
use crate::vec_env::error::{VecEnvError, VecEnvResult};
use crate::vec_env::factory::EnvFactory;
use crate::vec_env::stats::VecEnvStatistics;

/// 单个环境的批量 step 结果
pub type StepItem = (Observation, f64, bool, EnvInfo);

/// 同步向量化环境
///
/// 内部保存 `Vec<TradingEnv>`，所有 `step_batch` / `reset_all` 都在当前线程顺序执行。
/// 适合：
/// - 单元测试与调试（确定性、无线程切换）
/// - 环境非常轻量、并行开销不值得的场景
///
/// 并行场景请使用 [`super::AsyncVecEnv`]。
pub struct SyncVecEnv {
    /// 环境数量
    num_envs: usize,
    /// N 个独立的环境实例
    envs: Vec<TradingEnv>,
    /// 构造时使用的工厂（用于 `close` / `reset` 时按需重建）
    factory: Option<Box<dyn EnvFactory>>,
    /// 每个环境的 done 标志
    dones: Vec<bool>,
    /// 每个环境的累计奖励
    total_rewards: Vec<f64>,
    /// 每个环境的 step 计数
    step_counts: Vec<usize>,
    /// 每个环境的 episode 计数
    episode_counts: Vec<usize>,
}

impl std::fmt::Debug for SyncVecEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncVecEnv")
            .field("num_envs", &self.num_envs)
            .field("dones", &self.dones)
            .field("step_counts", &self.step_counts)
            .field("episode_counts", &self.episode_counts)
            .finish()
    }
}

impl SyncVecEnv {
    /// 通过工厂构造 `SyncVecEnv`，**立即**构建 N 个独立环境实例
    pub fn new<F: EnvFactory + 'static>(factory: F, num_envs: usize) -> VecEnvResult<Self> {
        if num_envs == 0 {
            return Err(VecEnvError::ZeroEnvs);
        }
        let mut envs = Vec::with_capacity(num_envs);
        for i in 0..num_envs {
            envs.push(
                factory
                    .build_env(i)
                    .map_err(|e| VecEnvError::Env(i, e.to_string()))?,
            );
        }
        Ok(Self {
            num_envs,
            envs,
            factory: Some(Box::new(factory)),
            dones: vec![false; num_envs],
            total_rewards: vec![0.0; num_envs],
            step_counts: vec![0; num_envs],
            episode_counts: vec![0; num_envs],
        })
    }

    /// 环境数量
    pub fn num_envs(&self) -> usize {
        self.num_envs
    }

    /// 第 `i` 个环境是否已 done
    pub fn is_done(&self, i: usize) -> bool {
        self.dones.get(i).copied().unwrap_or(false)
    }

    /// 第 `i` 个环境已完成的 episode 数
    pub fn episode_count(&self, i: usize) -> usize {
        self.episode_counts.get(i).copied().unwrap_or(0)
    }

    /// 第 `i` 个环境的累计奖励
    pub fn total_reward(&self, i: usize) -> f64 {
        self.total_rewards.get(i).copied().unwrap_or(0.0)
    }

    /// 第 `i` 个环境的当前 step
    pub fn step_count(&self, i: usize) -> usize {
        self.step_counts.get(i).copied().unwrap_or(0)
    }

    /// 已 done 的环境数
    pub fn done_count(&self) -> usize {
        self.dones.iter().filter(|d| **d).count()
    }

    /// 全部 done？
    pub fn all_done(&self) -> bool {
        self.dones.iter().all(|d| *d)
    }

    /// 并行（顺序）重置所有环境
    ///
    /// 返回 N 个初始观测。若某环境已 done，会先 `episode_counts[i] += 1`。
    pub fn reset_all(&mut self) -> VecEnvResult<Vec<Observation>> {
        let mut observations = Vec::with_capacity(self.num_envs);
        for i in 0..self.num_envs {
            let obs = self.envs[i]
                .reset()
                .map_err(|e| VecEnvError::Env(i, e.to_string()))?;
            observations.push(obs);
            if self.dones[i] {
                self.episode_counts[i] += 1;
            }
            self.dones[i] = false;
            self.total_rewards[i] = 0.0;
            self.step_counts[i] = 0;
        }
        Ok(observations)
    }

    /// 重置第 `i` 个环境
    pub fn reset_one(&mut self, i: usize) -> VecEnvResult<Observation> {
        let obs = self.envs[i]
            .reset()
            .map_err(|e| VecEnvError::Env(i, e.to_string()))?;
        if self.dones[i] {
            self.episode_counts[i] += 1;
        }
        self.dones[i] = false;
        self.total_rewards[i] = 0.0;
        self.step_counts[i] = 0;
        Ok(obs)
    }

    /// 批量执行一步
    ///
    /// - 自动重置：若 `dones[i]`，先 `reset_one(i)` 再应用动作（zero-start episode）
    /// - 任一环境错误立即返回（保留已收集的部分结果）
    pub fn step_batch(&mut self, actions: Vec<Action>) -> VecEnvResult<Vec<StepItem>> {
        if actions.len() != self.num_envs {
            return Err(VecEnvError::DimensionMismatch {
                expected: self.num_envs,
                got: actions.len(),
            });
        }

        let mut results = Vec::with_capacity(self.num_envs);
        for (i, action) in actions.into_iter().enumerate() {
            // 自动重置：done 环境零起步
            if self.dones[i] {
                self.envs[i]
                    .reset()
                    .map_err(|e| VecEnvError::Env(i, e.to_string()))?;
                self.episode_counts[i] += 1;
                self.total_rewards[i] = 0.0;
                self.step_counts[i] = 0;
                self.dones[i] = false;
            }

            let (obs, reward, done, info) = self.envs[i]
                .step(&action)
                .map_err(|e| VecEnvError::Env(i, e.to_string()))?;

            self.dones[i] = done;
            self.total_rewards[i] += reward;
            self.step_counts[i] += 1;
            results.push((obs, reward, done, info));
        }
        Ok(results)
    }

    /// 单独对第 `i` 个环境执行一步（不做自动重置）
    pub fn step_one(&mut self, i: usize, action: &Action) -> VecEnvResult<StepItem> {
        if i >= self.num_envs {
            return Err(VecEnvError::DimensionMismatch {
                expected: self.num_envs,
                got: i + 1,
            });
        }
        if self.dones[i] {
            return Err(VecEnvError::Env(
                i,
                EnvError::EpisodeAlreadyDone(0).to_string(),
            ));
        }
        let (obs, reward, done, info) = self.envs[i]
            .step(action)
            .map_err(|e| VecEnvError::Env(i, e.to_string()))?;
        self.dones[i] = done;
        self.total_rewards[i] += reward;
        self.step_counts[i] += 1;
        Ok((obs, reward, done, info))
    }

    /// 获取只读引用到第 `i` 个环境
    pub fn env(&self, i: usize) -> Option<&TradingEnv> {
        self.envs.get(i)
    }

    /// 获取可变引用到第 `i` 个环境
    pub fn env_mut(&mut self, i: usize) -> Option<&mut TradingEnv> {
        self.envs.get_mut(i)
    }

    /// 收集统计信息
    pub fn statistics(&self) -> VecEnvStatistics {
        VecEnvStatistics {
            num_envs: self.num_envs,
            total_rewards: self.total_rewards.clone(),
            step_counts: self.step_counts.clone(),
            done_count: self.done_count(),
            all_done: self.all_done(),
        }
    }
}

impl Drop for SyncVecEnv {
    fn drop(&mut self) {
        // 释放工厂；env 自身在 `Vec<TradingEnv>` drop 时自动清理
        self.factory = None;
    }
}
