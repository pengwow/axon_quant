use std::any::Any;
use std::path::Path;

use crate::error::InferenceError;
use crate::error::{Action, Observation};

pub trait InferenceEngine: Send + Sync {
    fn load(&mut self, path: &Path) -> Result<(), InferenceError>;
    fn infer(&self, observation: &Observation) -> Result<Action, InferenceError>;
    fn infer_batch(&self, observations: &[Observation]) -> Result<Vec<Action>, InferenceError>;
    /// 在 backend 上下文中预构造一个新 session(不替换当前 session)
    ///
    /// 与 `replace_session` 配合,实现**两步原子热更新**:
    /// 1. `let new_session = backend.read().build_session(path)?;`(只读锁)
    /// 2. `backend.write().replace_session(new_session)?;`(写锁替换)
    ///
    /// 这样 `build_session` 阶段可并发推理,只在 `replace_session` 瞬间阻塞。
    /// 具体 backend 决定"session"含义:
    /// - Onnx:返回 `Box<ort::Session>`(已 commit 的 session)
    /// - Candle:返回 `Box<CandleReloadState>`,内含 path,`replace_session` 触发重新 `load`
    /// - Tch:返回 `Box<tch::CModule>`(已 load 的 CModule)
    fn build_session(
        &self,
        path: &Path,
    ) -> Result<Box<dyn Any + Send + Sync>, InferenceError>;
    /// 替换当前 backend 的 session(详见 hot-update spec §3.1)
    ///
    /// - 状态变更:需要独占修改 backend 内部 session,故接收 `&mut self`
    /// - 必须持有 backend 写锁的环境下调用(由调用方保证)
    /// - 实现方 downcast `Box<dyn Any + Send + Sync>` 为具体 session 类型
    /// - 失败不修改 backend 状态
    fn replace_session(
        &mut self,
        new_session: Box<dyn Any + Send + Sync>,
    ) -> Result<(), InferenceError>;
}
