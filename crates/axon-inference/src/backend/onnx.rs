use std::any::Any;
use std::path::Path;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::engine::InferenceEngine;
use crate::error::{Action, ActionType, InferenceError, ModelConfig, Observation};
use crate::types::MultiLegAction;

pub struct OnnxBackend {
    session: Option<Arc<RwLock<ort::session::Session>>>,
    config: ModelConfig,
}

impl OnnxBackend {
    pub fn new(config: ModelConfig) -> Self {
        Self {
            session: None,
            config,
        }
    }

    fn decode_action(&self, probs: &[f32], _obs: &Observation) -> Action {
        let (action_type, confidence) = if probs.len() >= 3 {
            let max_idx = probs
                .iter()
                .take(3)
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(i, _)| i)
                .unwrap_or(2);
            let action_type = match max_idx {
                0 => ActionType::Buy,
                1 => ActionType::Sell,
                _ => ActionType::Hold,
            };
            (action_type, probs[max_idx])
        } else {
            let target = probs.first().copied().unwrap_or(0.0).clamp(-1.0, 1.0);
            let action_type = if target > 0.1 {
                ActionType::Buy
            } else if target < -0.1 {
                ActionType::Sell
            } else {
                ActionType::Hold
            };
            (action_type, target.abs())
        };

        Action {
            action_type,
            confidence,
            target_position: probs.first().copied().unwrap_or(0.0),
            model_id: String::new(),
            inference_time_us: 0,
        }
    }

    /// 0.9.0 D1.4b 新增:多 leg 推理,返回 `MultiLegAction`
    ///
    /// 与 `infer` 区别:`infer` 返回 5 类离散 `Action`,`infer_multi_leg` 返回连续多 leg 目标仓位。
    /// 假设 ONNX 输出 shape = (batch, n_legs)。
    pub fn infer_multi_leg(
        &self,
        observation: &Observation,
        n_legs: usize,
    ) -> Result<MultiLegAction, InferenceError> {
        let session = self
            .session
            .as_ref()
            .ok_or(InferenceError::ModelNotLoaded)?;

        let [batch, seq, features] = self.config.input_shape;
        if observation.features.len() != features {
            return Err(InferenceError::DimensionMismatch {
                expected: features,
                actual: observation.features.len(),
            });
        }

        let data = observation.features.clone();
        let input_tensor =
            ort::value::Tensor::from_array(([batch, seq, features], data.into_boxed_slice()))
                .map_err(|e| InferenceError::Onnx(e.to_string()))?;

        let start = std::time::Instant::now();
        let mut session_guard = session.write();
        let outputs = session_guard
            .run(ort::inputs![input_tensor])
            .map_err(|e| InferenceError::Onnx(e.to_string()))?;
        let inference_time_us = start.elapsed().as_micros() as u64;

        let output = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| InferenceError::Onnx(e.to_string()))?;
        let (shape, data) = output;

        // 期望 shape = (batch, n_legs)
        if shape.len() < 2 || shape[1] as usize != n_legs {
            return Err(InferenceError::DimensionMismatch {
                expected: n_legs,
                actual: shape.get(1).copied().unwrap_or(0) as usize,
            });
        }

        // model_id 派生:优先用 path 的 file_stem,fallback 到空字符串
        let model_id = self
            .config
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        Ok(MultiLegAction {
            target_positions: data.to_vec(),
            model_id,
            inference_time_us,
        })
    }
}

impl InferenceEngine for OnnxBackend {
    fn load(&mut self, path: &Path) -> Result<(), InferenceError> {
        if !path.exists() {
            return Err(InferenceError::ModelNotFound {
                path: path.to_path_buf(),
            });
        }

        let session = ort::session::Session::builder()
            .map_err(|e| InferenceError::Onnx(e.to_string()))?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
            .map_err(|e| InferenceError::Onnx(e.to_string()))?
            .with_intra_threads(self.config.num_threads)
            .map_err(|e| InferenceError::Onnx(e.to_string()))?
            .commit_from_file(path)
            .map_err(|e| InferenceError::Onnx(e.to_string()))?;

        self.session = Some(Arc::new(RwLock::new(session)));
        Ok(())
    }

    fn infer(&self, observation: &Observation) -> Result<Action, InferenceError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| InferenceError::InferenceFailed {
                reason: "model not loaded".into(),
            })?;

        let [batch, seq, features] = self.config.input_shape;
        if observation.features.len() != features {
            return Err(InferenceError::DimensionMismatch {
                expected: features,
                actual: observation.features.len(),
            });
        }

        let data = observation.features.clone();
        let input_tensor =
            ort::value::Tensor::from_array(([batch, seq, features], data.into_boxed_slice()))
                .map_err(|e| InferenceError::Onnx(e.to_string()))?;

        let mut session_guard = session.write();
        let outputs = session_guard
            .run(ort::inputs![input_tensor])
            .map_err(|e| InferenceError::Onnx(e.to_string()))?;

        let output = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| InferenceError::Onnx(e.to_string()))?;
        let probs = output.1;

        Ok(self.decode_action(probs, observation))
    }

    fn infer_batch(&self, observations: &[Observation]) -> Result<Vec<Action>, InferenceError> {
        let start = std::time::Instant::now();
        let results: Result<Vec<_>, _> = observations.iter().map(|obs| self.infer(obs)).collect();
        let elapsed = start.elapsed().as_micros() as u64;

        let mut actions = results?;
        for action in &mut actions {
            action.inference_time_us = elapsed / observations.len().max(1) as u64;
        }
        Ok(actions)
    }

    fn build_session(&self, path: &Path) -> Result<Box<dyn Any + Send + Sync>, InferenceError> {
        if !path.exists() {
            return Err(InferenceError::ModelNotFound {
                path: path.to_path_buf(),
            });
        }
        let session = ort::session::Session::builder()
            .map_err(|e| InferenceError::Onnx(e.to_string()))?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
            .map_err(|e| InferenceError::Onnx(e.to_string()))?
            .with_intra_threads(self.config.num_threads)
            .map_err(|e| InferenceError::Onnx(e.to_string()))?
            .commit_from_file(path)
            .map_err(|e| InferenceError::Onnx(e.to_string()))?;
        Ok(Box::new(session))
    }

    fn replace_session(
        &mut self,
        new_session: Box<dyn Any + Send + Sync>,
    ) -> Result<(), InferenceError> {
        let session = new_session
            .downcast::<ort::session::Session>()
            .map_err(|_| {
                InferenceError::Onnx("replace_session: expected Box<ort::session::Session>".into())
            })?;
        self.session = Some(Arc::new(RwLock::new(*session)));
        Ok(())
    }
}
