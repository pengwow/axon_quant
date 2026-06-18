use std::any::Any;
use std::path::Path;

use crate::error::InferenceError;
use crate::error::{Action, Observation};

pub trait InferenceEngine: Send + Sync {
    fn load(&mut self, path: &Path) -> Result<(), InferenceError>;
    fn infer(&self, observation: &Observation) -> Result<Action, InferenceError>;
    fn infer_batch(&self, observations: &[Observation]) -> Result<Vec<Action>, InferenceError>;
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
