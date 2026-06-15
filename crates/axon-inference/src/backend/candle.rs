//! Candle 后端契约桩
//!
//! 当前实现仅返回明确的"未实现"错误，完整实现需参考 TDD 规范：
//! `axon-design/01-tdd/05-phase4-production/01-inference.md`。
//!
//! # 设计说明
//!
//! - 该模块参与编译需启用 `candle-backend` feature（由上层 Cargo.toml 控制）。
//! - 所有方法均返回 `InferenceError`，不会 panic，也不会改变现有 feature gate 行为。
//! - 错误信息明确指向 TDD 规范路径与待实现子任务，便于后续替换为真实实现。

use std::any::Any;
use std::path::Path;

use crate::engine::InferenceEngine;
use crate::error::{Action, InferenceError, ModelConfig, Observation};

/// Candle 后端（纯 Rust 推理）
///
/// 当前为契约桩实现：所有方法返回明确的 `InferenceError`，告知调用方
/// "Candle backend requires model architecture to be specified; current implementation is a stub"。
///
/// `config` 字段保留供真实实现使用（按模型架构加载 safetensors、绑定设备），
/// 当前桩实现不读取该字段，标注 `#[allow(dead_code)]` 避免误报。
#[allow(dead_code)]
pub struct CandleBackend {
    config: ModelConfig,
}

impl CandleBackend {
    /// 创建 Candle 后端实例
    pub fn new(config: ModelConfig) -> Self {
        Self { config }
    }
}

impl InferenceEngine for CandleBackend {
    fn load(&mut self, _path: &Path) -> Result<(), InferenceError> {
        // Candle 模型加载需要按架构读取 safetensors；
        // 真实实现需参考 `axon-design/01-tdd/05-phase4-production/01-inference.md` 中的 "Candle Backend" 章节。
        Err(InferenceError::ModelLoadFailed {
            reason: "Candle backend requires model architecture to be specified; \
                     current implementation is a stub. \
                     TODO: load safetensors via candle-core; \
                     see `axon-design/01-tdd/05-phase4-production/01-inference.md`"
                .into(),
        })
    }

    fn infer(&self, _observation: &Observation) -> Result<Action, InferenceError> {
        Err(InferenceError::InferenceFailed {
            reason: "Candle backend requires model architecture to be specified; \
                     current implementation is a stub. \
                     TODO: forward tensor to candle-core model; \
                     see `axon-design/01-tdd/05-phase4-production/01-inference.md`"
                .into(),
        })
    }

    fn infer_batch(&self, _observations: &[Observation]) -> Result<Vec<Action>, InferenceError> {
        Err(InferenceError::InferenceFailed {
            reason: "Candle backend requires model architecture to be specified; \
                     current implementation is a stub. \
                     TODO: implement batched candle-core inference; \
                     see `axon-design/01-tdd/05-phase4-production/01-inference.md`"
                .into(),
        })
    }

    fn replace_session(&mut self, _new_session: Box<dyn Any>) -> Result<(), InferenceError> {
        Err(InferenceError::HotReloadFailed {
            reason: "Candle backend requires model architecture to be specified; \
                     current implementation is a stub. \
                     TODO: implement atomic session replacement for candle-core; \
                     see `axon-design/01-tdd/05-phase4-production/01-inference.md`"
                .into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ActionType;
    use std::path::PathBuf;
    fn sample_config() -> ModelConfig {
        ModelConfig {
            path: PathBuf::from("/tmp/model.safetensors"),
            backend: crate::error::InferenceBackend::Candle,
            device: crate::error::Device::Cpu,
            input_shape: [1, 64, 128],
            output_dim: 3,
            fp16: false,
            num_threads: 4,
        }
    }

    /// 验证 load 错误信息明确指向 TDD 规范
    #[test]
    fn test_candle_backend_load_returns_descriptive_error() {
        let mut backend = CandleBackend::new(sample_config());
        let err = backend.load(Path::new("/tmp/m.safetensors")).unwrap_err();
        match err {
            InferenceError::ModelLoadFailed { reason } => {
                assert!(reason.contains("candle"), "错误信息应包含 'candle'");
                assert!(
                    reason.contains("axon-design/01-tdd/05-phase4-production/01-inference.md"),
                    "错误信息应指向 TDD 规范路径"
                );
            }
            other => panic!("期望 ModelLoadFailed，实际 {other:?}"),
        }
    }

    /// 验证 infer 错误信息同样明确
    #[test]
    fn test_candle_backend_infer_returns_descriptive_error() {
        let backend = CandleBackend::new(sample_config());
        let obs = Observation {
            symbol: "BTC-USDT".into(),
            timestamp_ns: 1_000_000_000,
            features: vec![0.0; 128],
        };
        let err = backend.infer(&obs).unwrap_err();
        match err {
            InferenceError::InferenceFailed { reason } => {
                assert!(reason.contains("candle"));
                assert!(reason.contains("01-inference.md"));
            }
            other => panic!("期望 InferenceFailed，实际 {other:?}"),
        }
    }

    /// 验证 infer_batch 错误信息明确
    #[test]
    fn test_candle_backend_infer_batch_returns_descriptive_error() {
        let backend = CandleBackend::new(sample_config());
        let obs = Observation {
            symbol: "BTC-USDT".into(),
            timestamp_ns: 1_000_000_000,
            features: vec![0.0; 128],
        };
        let err = backend.infer_batch(&[obs]).unwrap_err();
        match err {
            InferenceError::InferenceFailed { reason } => {
                assert!(reason.contains("batch"));
                assert!(reason.contains("01-inference.md"));
            }
            other => panic!("期望 InferenceFailed，实际 {other:?}"),
        }
    }

    /// 验证 replace_session 错误信息明确
    #[test]
    fn test_candle_backend_replace_session_returns_descriptive_error() {
        let mut backend = CandleBackend::new(sample_config());
        let err = backend.replace_session(Box::new(())).unwrap_err();
        match err {
            InferenceError::HotReloadFailed { reason } => {
                assert!(reason.contains("candle"));
                assert!(reason.contains("01-inference.md"));
            }
            other => panic!("期望 HotReloadFailed，实际 {other:?}"),
        }
    }

    /// 验证 ActionType 的存在（避免 dead_code 警告）
    #[test]
    fn test_action_type_used() {
        let _ = ActionType::Hold;
    }
}
