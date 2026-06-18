use std::any::Any;
use std::path::Path;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::engine::InferenceEngine;
use crate::error::{Action, ActionType, InferenceError, ModelConfig, Observation};

pub struct TchBackend {
    model: Option<Arc<RwLock<tch::CModule>>>,
    config: ModelConfig,
}

impl TchBackend {
    pub fn new(config: ModelConfig) -> Self {
        Self {
            model: None,
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
}

impl InferenceEngine for TchBackend {
    fn load(&mut self, path: &Path) -> Result<(), InferenceError> {
        if !path.exists() {
            return Err(InferenceError::ModelNotFound {
                path: path.to_path_buf(),
            });
        }

        let device = match self.config.device {
            crate::error::Device::Cpu => tch::Device::Cpu,
            crate::error::Device::Cuda(id) => tch::Device::Cuda(id as usize),
            crate::error::Device::Metal => tch::Device::Mps,
        };

        let model = tch::CModule::load_on_device(path, device)
            .map_err(|e| InferenceError::Tch(e.to_string()))?;

        self.model = Some(Arc::new(RwLock::new(model)));
        Ok(())
    }

    fn infer(&self, observation: &Observation) -> Result<Action, InferenceError> {
        let model = self
            .model
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

        let input = tch::Tensor::from_slice(&observation.features).reshape(&[
            batch as i64,
            seq as i64,
            features as i64,
        ]);

        let model_guard = model.read();
        let output = model_guard
            .forward_ts(&[input])
            .map_err(|e| InferenceError::Tch(e.to_string()))?;

        let probs: Vec<f32> = output.into();
        Ok(self.decode_action(&probs, observation))
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

    fn replace_session(
        &mut self,
        new_session: Box<dyn Any + Send + Sync>,
    ) -> Result<(), InferenceError> {
        let model = new_session.downcast::<tch::CModule>().map_err(|_| {
            InferenceError::Tch("replace_session: expected Box<tch::CModule>".into())
        })?;
        self.model = Some(Arc::new(RwLock::new(*model)));
        Ok(())
    }
}
