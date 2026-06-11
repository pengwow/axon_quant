//! 异步向量化环境：std::thread + mpsc
//!
//! 内部启动 `num_envs` 个 OS 线程，每个线程拥有独立的 `TradingEnv` 实例。
//! 主线程通过 mpsc channel 发送 [`WorkerCommand`]，worker 处理后返回 [`WorkerResponse`]。
//!
//! ## 同步 vs 异步
//!
//! - `SyncVecEnv`：顺序执行，确定性，无线程开销，适合测试与小规模
//! - `AsyncVecEnv`（本模块）：真并行，主线程串行化命令派发与结果收集
//!
//! 对于环境数量 ≤ 32、`step` 耗时 ≫ 通信开销的场景，AsyncVecEnv 加速比应接近 N。

use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::{self, JoinHandle};

use crate::action::types::Action;
use crate::env::types::EnvInfo;
use crate::observation::types::Observation;
use crate::vec_env::error::{VecEnvError, VecEnvResult};
use crate::vec_env::factory::EnvFactory;
use crate::vec_env::stats::VecEnvStatistics;

// ── Worker 消息 ──────────────────────────────────────────────

/// 主线程 → worker 的命令
#[derive(Debug)]
pub enum WorkerCommand {
    /// 重置环境（可选 seed）
    Reset {
        /// 可选 seed
        seed: Option<u64>,
    },
    /// 执行一步
    Step {
        /// 待执行动作
        action: Action,
    },
    /// 关闭 worker
    Shutdown,
}

/// worker → 主线程的响应
#[derive(Debug)]
pub enum WorkerResponse {
    /// 重置结果
    Reset {
        /// 初始观测
        obs: Observation,
    },
    /// Step 结果
    Step {
        /// 新观测
        obs: Observation,
        /// 奖励
        reward: f64,
        /// 是否终止
        done: bool,
        /// 额外信息
        info: EnvInfo,
    },
    /// 错误响应
    Error {
        /// 错误描述
        msg: String,
    },
}

// ── Worker 句柄 ─────────────────────────────────────────────

/// 单个 worker 线程的通信句柄
pub(super) struct WorkerHandle {
    /// 命令发送端
    tx: Sender<WorkerCommand>,
    /// 响应接收端
    rx: Receiver<WorkerResponse>,
    /// worker 线程 join 句柄
    handle: Option<JoinHandle<()>>,
    /// 环境索引
    env_id: usize,
}

impl std::fmt::Debug for WorkerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerHandle")
            .field("env_id", &self.env_id)
            .finish_non_exhaustive()
    }
}

impl WorkerHandle {
    /// 启动 worker 线程
    fn spawn(factory: Box<dyn EnvFactory>, env_id: usize) -> VecEnvResult<Self> {
        let (cmd_tx, cmd_rx) = channel::<WorkerCommand>();
        let (resp_tx, resp_rx) = channel::<WorkerResponse>();

        let handle = thread::Builder::new()
            .name(format!("axon-vec-env-{env_id}"))
            .spawn(move || Self::worker_loop(factory, env_id, cmd_rx, resp_tx))
            .map_err(|e| VecEnvError::ChannelSend(format!("spawn failed: {e}")))?;

        Ok(Self {
            tx: cmd_tx,
            rx: resp_rx,
            handle: Some(handle),
            env_id,
        })
    }

    /// Worker 线程主循环
    fn worker_loop(
        factory: Box<dyn EnvFactory>,
        env_id: usize,
        cmd_rx: Receiver<WorkerCommand>,
        resp_tx: Sender<WorkerResponse>,
    ) {
        let mut env = match factory.build_env(env_id) {
            Ok(e) => e,
            Err(e) => {
                let _ = resp_tx.send(WorkerResponse::Error {
                    msg: format!("env {env_id} build failed: {e}"),
                });
                return;
            }
        };

        while let Ok(cmd) = cmd_rx.recv() {
            match cmd {
                WorkerCommand::Reset { seed } => {
                    // 当前 TradingEnv::reset 不接 seed，这里预留接口
                    let _ = seed;
                    match env.reset() {
                        Ok(obs) => {
                            if resp_tx.send(WorkerResponse::Reset { obs }).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            if resp_tx
                                .send(WorkerResponse::Error {
                                    msg: format!("env {env_id} reset failed: {e}"),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
                WorkerCommand::Step { action } => match env.step(&action) {
                    Ok((obs, reward, done, info)) => {
                        if resp_tx
                            .send(WorkerResponse::Step {
                                obs,
                                reward,
                                done,
                                info,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(e) => {
                        if resp_tx
                            .send(WorkerResponse::Error {
                                msg: format!("env {env_id} step failed: {e}"),
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                },
                WorkerCommand::Shutdown => break,
            }
        }
    }

    /// 发送 reset 命令并等待响应
    fn send_reset(&self) -> VecEnvResult<Observation> {
        self.tx
            .send(WorkerCommand::Reset { seed: None })
            .map_err(|e| VecEnvError::ChannelSend(e.to_string()))?;
        match self.rx.recv() {
            Ok(WorkerResponse::Reset { obs }) => Ok(obs),
            Ok(WorkerResponse::Error { msg }) => Err(VecEnvError::Env(self.env_id, msg)),
            Ok(_) => Err(VecEnvError::ChannelRecv(
                "unexpected response to reset".into(),
            )),
            Err(e) => Err(VecEnvError::ChannelRecv(e.to_string())),
        }
    }

    /// 发送 step 命令并等待响应
    fn send_step(&self, action: Action) -> VecEnvResult<(Observation, f64, bool, EnvInfo)> {
        self.tx
            .send(WorkerCommand::Step { action })
            .map_err(|e| VecEnvError::ChannelSend(e.to_string()))?;
        match self.rx.recv() {
            Ok(WorkerResponse::Step {
                obs,
                reward,
                done,
                info,
            }) => Ok((obs, reward, done, info)),
            Ok(WorkerResponse::Error { msg }) => Err(VecEnvError::Env(self.env_id, msg)),
            Ok(_) => Err(VecEnvError::ChannelRecv(
                "unexpected response to step".into(),
            )),
            Err(e) => Err(VecEnvError::ChannelRecv(e.to_string())),
        }
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        // 通知 worker 退出并 join
        let _ = self.tx.send(WorkerCommand::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

// ── AsyncVecEnv 主结构 ──────────────────────────────────────

/// 单个环境的批量 step 结果
pub type AsyncStepItem = (Observation, f64, bool, EnvInfo);

/// 异步向量化环境
///
/// 启动 `num_envs` 个独立线程并行执行环境。
pub struct AsyncVecEnv {
    /// 环境数量
    num_envs: usize,
    /// worker 句柄
    workers: Vec<WorkerHandle>,
    /// 每个环境的 done 标志
    dones: Vec<bool>,
    /// 每个环境的累计奖励
    total_rewards: Vec<f64>,
    /// 每个环境的 step 计数
    step_counts: Vec<usize>,
    /// 每个环境的 episode 计数
    episode_counts: Vec<usize>,
}

impl std::fmt::Debug for AsyncVecEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncVecEnv")
            .field("num_envs", &self.num_envs)
            .field("dones", &self.dones)
            .field("step_counts", &self.step_counts)
            .finish_non_exhaustive()
    }
}

impl AsyncVecEnv {
    /// 构造异步向量化环境，立即启动 N 个 worker 线程
    ///
    /// **要求**：`F: EnvFactory + Clone + 'static`，因为每个 worker 线程都需要
    /// 拥有工厂的独立副本（不与主线程共享，避免 `&Factory` 的生命周期跨越线程边界）。
    pub fn new<F: EnvFactory + Clone + 'static>(factory: F, num_envs: usize) -> VecEnvResult<Self> {
        if num_envs == 0 {
            return Err(VecEnvError::ZeroEnvs);
        }
        let mut workers = Vec::with_capacity(num_envs);
        for i in 0..num_envs {
            let worker_factory: Box<dyn EnvFactory> = Box::new(factory.clone());
            let worker = WorkerHandle::spawn(worker_factory, i)?;
            workers.push(worker);
        }
        Ok(Self {
            num_envs,
            workers,
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

    /// 已 done 的环境数
    pub fn done_count(&self) -> usize {
        self.dones.iter().filter(|d| **d).count()
    }

    /// 全部 done？
    pub fn all_done(&self) -> bool {
        self.dones.iter().all(|d| *d)
    }

    /// 并行重置所有环境
    pub fn reset_all(&mut self) -> VecEnvResult<Vec<Observation>> {
        let mut observations = Vec::with_capacity(self.num_envs);
        for (i, worker) in self.workers.iter().enumerate() {
            let obs = worker.send_reset()?;
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

    /// 批量执行一步
    ///
    /// 串行派发 + 串行回收（每个 worker 仍在自己线程里跑 env.step）。
    /// 自动重置：done 环境零起步。
    pub fn step_batch(&mut self, actions: Vec<Action>) -> VecEnvResult<Vec<AsyncStepItem>> {
        if actions.len() != self.num_envs {
            return Err(VecEnvError::DimensionMismatch {
                expected: self.num_envs,
                got: actions.len(),
            });
        }

        let mut results = Vec::with_capacity(self.num_envs);
        for (i, action) in actions.into_iter().enumerate() {
            // 自动重置：done 环境先 reset
            if self.dones[i] {
                self.workers[i].send_reset()?;
                self.episode_counts[i] += 1;
                self.total_rewards[i] = 0.0;
                self.step_counts[i] = 0;
                self.dones[i] = false;
            }

            let (obs, reward, done, info) = self.workers[i].send_step(action)?;
            self.dones[i] = done;
            self.total_rewards[i] += reward;
            self.step_counts[i] += 1;
            results.push((obs, reward, done, info));
        }
        Ok(results)
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

    /// 显式关闭（一般无需调用，`Drop` 会自动 join）
    pub fn close(mut self) {
        // 取出 workers 触发 Drop
        self.workers.clear();
    }
}
