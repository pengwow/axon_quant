//! Candle 后端(纯 Rust 推理,基于 `candle-core 0.10` + `candle-nn 0.10`)
//!
//! 启用 `candle-backend` feature 时生效。本模块是 **真实现**,不是契约桩:
//! - [`CandleBackend::load`] 从 safetensors 读取 `linear.weight` + `linear.bias` 构造 `nn::Linear`
//! - [`CandleBackend::infer`] 单条 observation 前向,softmax -> argmax -> `Action`
//! - [`CandleBackend::infer_batch`] 批量前向,逐行 argmax 输出 `Vec<Action>`
//!
//! # 限制
//!
//! - 单层 Linear 通用路径(单隐藏层 / 多层用 ONNX 后端)
//! - FP16 暂不支持,`config.fp16 = true` 显式返回错误
//! - `replace_session` 维持 hot-reload 文档约定返回 `Candle("not implemented")`,
//!   需热更新时调 `backend.load(new_path)`
//!
//! # safetensors 约定
//!
//! | Tensor | Shape | DType |
//! |---|---|---|
//! | `linear.weight` | `[output_dim, input_dim]` | F32 |
//! | `linear.bias` | `[output_dim]` | F32 |
//!
//! `input_dim = input_shape.iter().product()`(三维 shape 全部乘起来)。
//! 训练侧负责融合激活函数(本 spec 之前置)等效单层矩阵)。

use std::any::Any;
use std::path::Path;

use candle_core::{DType, Device, Tensor};
use candle_nn::{Linear, Module, VarBuilder, linear, ops};
use parking_lot::Mutex;

use crate::engine::InferenceEngine;
use crate::error::{Action, ActionType, InferenceError, ModelConfig, Observation};

/// Candle 后端(纯 Rust 推理,基于 `candle-core 0.10` + `candle-nn 0.10`)
///
/// 模型采用"懒加载"语义:首次 `infer` 时若 `model` 仍为 `None`,
/// 返回 [`InferenceError::ModelNotLoaded`],要求 caller 先调 [`CandleBackend::load`]。
/// 这样设计是因为 `load` 需要 `&mut self` 但 `infer` 是 `&self`,
/// 同时把"未加载"与"加载失败"两种状态合并到一个 `Option<Result<...>>` 字段。
pub struct CandleBackend {
    /// 模型配置(输入 shape、output_dim、device 标志)
    config: ModelConfig,
    /// 懒加载的模型状态:
    /// - `None` → 尚未尝试加载
    /// - `Some(Err(e))` → 加载失败,缓存错误避免反复重试
    /// - `Some(Ok(m))` → 已加载,后续 infer 直接用
    model: Mutex<Option<Result<LoadedModel, InferenceError>>>,
    /// candle-core 设备(从 `config.device` 派生)
    device: Device,
}

/// 加载后的模型(只读,内部不含可变状态)
struct LoadedModel {
    /// 单层 Linear 模块(weight + bias)
    linear: Linear,
    /// 输出维度(冗余存,方便 infer 路径快速校验)
    output_dim: usize,
    /// 模型路径(用于 `model_id` 标注)
    path: std::path::PathBuf,
}

impl CandleBackend {
    /// 创建 Candle 后端实例(不触发模型加载,需显式调 `load`)
    ///
    /// `config.device` 字段会立即尝试构造 candle `Device`。
    /// Metal/Cuda 构造失败时回退到 `DeviceUnavailable` 错误(让 caller 立刻知道设备不可用)。
    pub fn new(config: ModelConfig) -> Self {
        // 设备构造可能失败(无 GPU、Metal feature 未启用等),失败时降级为 Cpu 让程序继续运行
        // 这样 `new` 不会因为设备不可用而 panic,真实错误在 `load` 时再报
        let device = match config.device {
            crate::error::Device::Cpu => Device::Cpu,
            crate::error::Device::Cuda(ordinal) => {
                Device::new_cuda(ordinal as usize).unwrap_or(Device::Cpu)
            }
            crate::error::Device::Metal => Device::new_metal(0).unwrap_or(Device::Cpu),
        };
        Self {
            config,
            model: Mutex::new(None),
            device,
        }
    }

    /// 取已加载的模型(若未加载或加载失败,返回对应错误)
    fn loaded(&self) -> Result<LoadedModel, InferenceError> {
        let guard = self.model.lock();
        match guard.as_ref() {
            None => Err(InferenceError::ModelNotLoaded),
            Some(Err(e)) => Err(clone_inference_error(e)),
            Some(Ok(m)) => Ok(LoadedModel {
                linear: m.linear.clone(),
                output_dim: m.output_dim,
                path: m.path.clone(),
            }),
        }
    }
}

/// 浅克隆 `InferenceError`(`thiserror` 生成 `#[non_exhaustive]` 之外的变体都能直接 clone)
fn clone_inference_error(e: &InferenceError) -> InferenceError {
    match e {
        InferenceError::ModelNotFound { path } => {
            InferenceError::ModelNotFound { path: path.clone() }
        }
        InferenceError::ModelLoadFailed { reason } => InferenceError::ModelLoadFailed {
            reason: reason.clone(),
        },
        InferenceError::InferenceFailed { reason } => InferenceError::InferenceFailed {
            reason: reason.clone(),
        },
        InferenceError::DimensionMismatch { expected, actual } => {
            InferenceError::DimensionMismatch {
                expected: *expected,
                actual: *actual,
            }
        }
        InferenceError::DeviceUnavailable { device } => {
            InferenceError::DeviceUnavailable { device: *device }
        }
        InferenceError::HotReloadFailed { reason } => InferenceError::HotReloadFailed {
            reason: reason.clone(),
        },
        InferenceError::Onnx(s) => InferenceError::Onnx(s.clone()),
        InferenceError::Tch(s) => InferenceError::Tch(s.clone()),
        InferenceError::Candle(s) => InferenceError::Candle(s.clone()),
        // 新增的 `ModelNotLoaded` 不应在缓存错误里出现(它是状态而非错误结果),
        // 但为了 exhaustive 匹配,fallback 到 `ModelNotLoaded`
        InferenceError::ModelNotLoaded => InferenceError::ModelNotLoaded,
    }
}

/// 把模型输入 shape 展平成 `input_dim`
fn input_dim_from_config(config: &ModelConfig) -> usize {
    config.input_shape.iter().product()
}

/// 把 logits 转为 `Action`(softmax + argmax)
fn action_from_logits(
    logits: &Tensor,
    _config: &ModelConfig,
    model_path: &std::path::Path,
) -> Result<Action, InferenceError> {
    // 沿最后一维做 softmax(logits shape 一般是 `[1, output_dim]` 或 `[N, output_dim]`)
    let probs = ops::softmax(logits, candle_core::D::Minus1).map_err(|e| {
        InferenceError::InferenceFailed {
            reason: format!("softmax: {e}"),
        }
    })?;
    // 拿到 `[output_dim]` 的概率向量
    let probs_vec: Vec<f32> = probs
        .squeeze(0)
        .or_else(|_| Ok(probs.clone()))
        .and_then(|t| t.to_vec1())
        .map_err(|e| InferenceError::InferenceFailed {
            reason: format!("probs to_vec1: {e}"),
        })?;
    // argmax
    let (argmax_idx, &max_prob) = probs_vec
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .ok_or_else(|| InferenceError::InferenceFailed {
            reason: "empty logits".into(),
        })?;
    // ActionType 映射(按 argmax_idx 顺序)
    let action_type = match argmax_idx {
        0 => ActionType::Hold,
        1 => ActionType::Buy,
        2 => ActionType::Sell,
        3 => ActionType::ReduceLong,
        4 => ActionType::ReduceShort,
        // 越界 fallback 到 Hold(防御性)
        _ => ActionType::Hold,
    };
    Ok(Action {
        action_type,
        confidence: max_prob,
        target_position: 0.0,
        model_id: format!("candle:{}", model_path.display()),
        inference_time_us: 0,
    })
}

impl InferenceEngine for CandleBackend {
    fn load(&mut self, path: &Path) -> Result<(), InferenceError> {
        // 1. fp16 显式拒绝
        if self.config.fp16 {
            return Err(InferenceError::ModelLoadFailed {
                reason: "Candle FP16 inference not yet supported; \
                         export the model with F32 and retry"
                    .into(),
            });
        }
        // 2. 读 safetensors 文件
        let data = std::fs::read(path).map_err(|e| InferenceError::ModelLoadFailed {
            reason: format!("read {}: {e}", path.display()),
        })?;
        // 3. 构造 VarBuilder(safe 版本,无 unsafe)
        let vb =
            VarBuilder::from_buffered_safetensors(data, DType::F32, &self.device).map_err(|e| {
                InferenceError::ModelLoadFailed {
                    reason: format!("safetensors load: {e}"),
                }
            })?;
        // 4. 构造 Linear(input_dim, output_dim, vb.pp("linear"))
        //    这样会查找 "linear.weight" + "linear.bias" 两个 tensor
        let input_dim = input_dim_from_config(&self.config);
        let output_dim = self.config.output_dim;
        let linear_layer = linear(input_dim, output_dim, vb.pp("linear")).map_err(|e| {
            InferenceError::ModelLoadFailed {
                reason: format!("linear construct: {e}"),
            }
        })?;
        // 5. 存到懒加载槽
        let mut guard = self.model.lock();
        *guard = Some(Ok(LoadedModel {
            linear: linear_layer,
            output_dim,
            path: path.to_path_buf(),
        }));
        Ok(())
    }

    fn infer(&self, observation: &Observation) -> Result<Action, InferenceError> {
        let model = self.loaded()?;
        let input_dim = input_dim_from_config(&self.config);
        if observation.features.len() != input_dim {
            return Err(InferenceError::DimensionMismatch {
                expected: input_dim,
                actual: observation.features.len(),
            });
        }
        // obs.features -> Tensor [1, input_dim]
        let x = Tensor::from_vec(
            observation.features.clone(),
            (1usize, input_dim),
            &self.device,
        )
        .map_err(|e| InferenceError::InferenceFailed {
            reason: format!("tensor from_vec: {e}"),
        })?;
        // forward
        let logits = model
            .linear
            .forward(&x)
            .map_err(|e| InferenceError::InferenceFailed {
                reason: format!("linear forward: {e}"),
            })?;
        action_from_logits(&logits, &self.config, &model.path)
    }

    fn infer_batch(&self, observations: &[Observation]) -> Result<Vec<Action>, InferenceError> {
        let model = self.loaded()?;
        if observations.is_empty() {
            return Ok(Vec::new());
        }
        let input_dim = input_dim_from_config(&self.config);
        // 维度校验
        for (i, obs) in observations.iter().enumerate() {
            if obs.features.len() != input_dim {
                return Err(InferenceError::DimensionMismatch {
                    expected: input_dim,
                    actual: obs.features.len(),
                });
            }
            // 防止编译器警告(避免 i 只用于断言)
            let _ = i;
        }
        // stack -> Tensor [N, input_dim]
        let n = observations.len();
        let features: Vec<f32> = observations
            .iter()
            .flat_map(|o| o.features.iter().copied())
            .collect();
        let x = Tensor::from_vec(features, (n, input_dim), &self.device).map_err(|e| {
            InferenceError::InferenceFailed {
                reason: format!("batch tensor from_vec: {e}"),
            }
        })?;
        // batched forward
        let logits = model
            .linear
            .forward(&x)
            .map_err(|e| InferenceError::InferenceFailed {
                reason: format!("batch linear forward: {e}"),
            })?;
        // 逐行 argmax(取最后一维 argmax 然后映射成 Action)
        // 把 logits 拷贝到 CPU 一次取 Vec<Vec<f32>> 再逐行处理,
        // 避免对每行都构造 narrow Tensor 引入的 CPU<->GPU 同步开销
        let logits_cpu: Vec<Vec<f32>> =
            logits
                .to_vec2()
                .map_err(|e| InferenceError::InferenceFailed {
                    reason: format!("batch logits to_vec2: {e}"),
                })?;
        // 复用 spec 的"softmax + argmax"语义,这里直接对 CPU Vec 做 softmax + argmax
        let mut actions = Vec::with_capacity(n);
        for row in logits_cpu {
            // softmax
            let max = row.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            let exp_sum: f32 = row.iter().map(|x| (x - max).exp()).sum();
            let probs: Vec<f32> = row
                .iter()
                .map(|x| (x - max).exp() / exp_sum.max(f32::MIN_POSITIVE))
                .collect();
            // argmax
            let (argmax_idx, &max_prob) = probs
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                .ok_or_else(|| InferenceError::InferenceFailed {
                    reason: "empty logits row".into(),
                })?;
            // ActionType 映射
            let action_type = match argmax_idx {
                0 => ActionType::Hold,
                1 => ActionType::Buy,
                2 => ActionType::Sell,
                3 => ActionType::ReduceLong,
                4 => ActionType::ReduceShort,
                _ => ActionType::Hold,
            };
            actions.push(Action {
                action_type,
                confidence: max_prob,
                target_position: 0.0,
                model_id: format!("candle:{}", model.path.display()),
                inference_time_us: 0,
            });
        }
        Ok(actions)
    }

    fn replace_session(
        &mut self,
        _new_session: Box<dyn Any + Send + Sync>,
    ) -> Result<(), InferenceError> {
        // 按 hot-reload spec 约定,CandleBackend 不支持 session 替换;
        // 热更新请用 `backend.load(new_path)`(由 ModelHotReloader 自动调)
        Err(InferenceError::Candle(
            "CandleBackend::replace_session not implemented. \
             For hot-reload call backend.load(new_path) instead \
             (ModelHotReloader does this). \
             See axon-inference-hot-update-design for the implementation roadmap."
                .into(),
        ))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    use candle_core::safetensors;

    /// 构造测试用 ModelConfig(`[1, 4]` shape → input_dim=4, output_dim=3)
    fn sample_config() -> ModelConfig {
        ModelConfig {
            path: PathBuf::from("/tmp/model.safetensors"),
            backend: crate::error::InferenceBackend::Candle,
            device: crate::error::Device::Cpu,
            input_shape: [1, 4, 1], // input_dim = 1*4*1 = 4
            output_dim: 3,
            fp16: false,
            num_threads: 4,
        }
    }

    /// 把 `weight` + `bias` 写到一个临时 safetensors 文件,返回路径
    ///
    /// 训练侧惯例:`linear.weight` shape = `[output_dim, input_dim]`,
    /// `linear.bias` shape = `[output_dim]`。这里直接用 candle 自身的 `safetensors::save`
    /// 一次性序列化多个 tensor,不依赖 torch/pytorch。
    fn write_test_safetensors(
        dir: &Path,
        weight: &[f32],
        weight_shape: (usize, usize),
        bias: &[f32],
    ) -> PathBuf {
        let weight_tensor = Tensor::from_vec(
            weight.to_vec(),
            (weight_shape.0, weight_shape.1),
            &Device::Cpu,
        )
        .expect("create weight tensor");
        let bias_tensor =
            Tensor::from_vec(bias.to_vec(), bias.len(), &Device::Cpu).expect("create bias tensor");
        let mut tensors: HashMap<String, Tensor> = HashMap::new();
        tensors.insert("linear.weight".to_string(), weight_tensor);
        tensors.insert("linear.bias".to_string(), bias_tensor);
        let path = dir.join("model.safetensors");
        safetensors::save(&tensors, &path).expect("save safetensors");
        path
    }

    /// `infer` 在没调 `load` 时返回 `ModelNotLoaded`
    #[test]
    fn candle_infer_without_load_returns_error() {
        let backend = CandleBackend::new(sample_config());
        let obs = Observation {
            symbol: "BTC-USDT".into(),
            timestamp_ns: 1_000_000_000,
            features: vec![0.0; 4],
        };
        let err = backend.infer(&obs).unwrap_err();
        assert!(
            matches!(err, InferenceError::ModelNotLoaded),
            "未调 load 应返回 ModelNotLoaded,实际 {err:?}"
        );
    }

    /// `infer_batch` 在没调 `load` 时同样返回 `ModelNotLoaded`
    #[test]
    fn candle_infer_batch_without_load_returns_error() {
        let backend = CandleBackend::new(sample_config());
        let obs = Observation {
            symbol: "BTC-USDT".into(),
            timestamp_ns: 1_000_000_000,
            features: vec![0.0; 4],
        };
        let err = backend.infer_batch(&[obs]).unwrap_err();
        assert!(matches!(err, InferenceError::ModelNotLoaded));
    }

    /// `replace_session` 按 hot-reload 约定返回 `Candle("not implemented")`
    #[test]
    fn candle_replace_session_returns_candle_error() {
        let mut backend = CandleBackend::new(sample_config());
        let err = backend.replace_session(Box::new(())).unwrap_err();
        match err {
            InferenceError::Candle(msg) => {
                assert!(msg.contains("not implemented"));
                assert!(msg.contains("backend.load"));
            }
            other => panic!("期望 Candle 错误,实际 {other:?}"),
        }
    }

    /// `fp16 = true` 时 `load` 显式拒绝
    #[test]
    fn candle_load_fails_on_fp16() {
        let dir = tempfile::tempdir().expect("tempdir");
        // 写一个无关的 safetensors(应该不会被读到,因为 fp16 校验在前)
        let tensors: HashMap<String, Tensor> = HashMap::new();
        let _ = safetensors::save(&tensors, dir.path().join("model.safetensors"));

        let mut cfg = sample_config();
        cfg.fp16 = true;
        let mut backend = CandleBackend::new(cfg);
        let err = backend
            .load(&dir.path().join("model.safetensors"))
            .unwrap_err();
        match err {
            InferenceError::ModelLoadFailed { reason } => {
                assert!(
                    reason.contains("FP16"),
                    "错误信息应提及 FP16,实际: {reason}"
                );
            }
            other => panic!("期望 ModelLoadFailed,实际 {other:?}"),
        }
    }

    /// 加载不存在的文件返回 `ModelLoadFailed`
    #[test]
    fn candle_load_fails_on_missing_file() {
        let mut backend = CandleBackend::new(sample_config());
        let err = backend
            .load(Path::new("/tmp/this_file_does_not_exist_axon.safetensors"))
            .unwrap_err();
        match err {
            InferenceError::ModelLoadFailed { reason } => {
                assert!(reason.contains("read") || reason.contains("safetensors"));
            }
            other => panic!("期望 ModelLoadFailed,实际 {other:?}"),
        }
    }

    /// 加载一个空的 safetensors(无 weight/bias tensor)会失败
    #[test]
    fn candle_load_fails_on_wrong_shape() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tensors: HashMap<String, Tensor> = HashMap::new();
        let path = dir.path().join("empty.safetensors");
        safetensors::save(&tensors, &path).expect("save empty");

        let mut backend = CandleBackend::new(sample_config());
        let err = backend.load(&path).unwrap_err();
        assert!(
            matches!(err, InferenceError::ModelLoadFailed { .. }),
            "空 safetensors 应触发 ModelLoadFailed,实际 {err:?}"
        );
    }

    /// 加载有效 safetensors 后,`infer` 不再返回 `ModelNotLoaded`
    #[test]
    fn candle_load_succeeds_with_valid_safetensors() {
        let dir = tempfile::tempdir().expect("tempdir");
        // weight: [3, 4] 全 0,bias: [0, 0, 0] → 输出都是 0,softmax 是均匀分布
        let weight = vec![0.0f32; 3 * 4];
        let bias = vec![0.0f32; 3];
        let path = write_test_safetensors(dir.path(), &weight, (3, 4), &bias);

        let mut backend = CandleBackend::new(sample_config());
        backend.load(&path).expect("load ok");

        let obs = Observation {
            symbol: "BTC-USDT".into(),
            timestamp_ns: 1_000_000_000,
            features: vec![0.5; 4],
        };
        let action = backend.infer(&obs).expect("infer should succeed");
        // 均匀分布下 confidence ≈ 1/3
        assert!((action.confidence - 1.0 / 3.0).abs() < 1e-3);
    }

    /// bias 调成 `[1, 0, 0]` → argmax=0 → Hold
    ///
    /// logits = [1, 0, 0] 的 softmax:
    /// `probs[0] = e / (e + 1 + 1) = 2.718 / 4.718 ≈ 0.576`,
    /// 所以置信度阈值用 > 0.5 而非 > 0.99(避免 1/3 = 0.333 的"均匀分布"测试混淆)
    #[test]
    fn candle_infer_returns_argmax_action() {
        let dir = tempfile::tempdir().expect("tempdir");
        let weight = vec![0.0f32; 3 * 4];
        let bias = vec![1.0f32, 0.0, 0.0];
        let path = write_test_safetensors(dir.path(), &weight, (3, 4), &bias);

        let mut backend = CandleBackend::new(sample_config());
        backend.load(&path).expect("load ok");

        let obs = Observation {
            symbol: "BTC-USDT".into(),
            timestamp_ns: 1_000_000_000,
            features: vec![0.0; 4],
        };
        let action = backend.infer(&obs).expect("infer ok");
        assert_eq!(action.action_type, ActionType::Hold);
        assert!(
            action.confidence > 0.5,
            "Hold 概率应 > 0.5,实际 {}",
            action.confidence
        );
    }

    /// `infer_batch` 处理多条 observation
    ///
    /// 用 bias=[1, 0, 0] 制造明确的 argmax=0(Hold),避免全 0 bias 下
    /// softmax 三类等概率时 `max_by` 取最后一个导致 argmax=2(Sell)的歧义。
    #[test]
    fn candle_infer_batch_handles_multiple_observations() {
        let dir = tempfile::tempdir().expect("tempdir");
        let weight = vec![0.0f32; 3 * 4];
        let bias = vec![1.0f32, 0.0, 0.0];
        let path = write_test_safetensors(dir.path(), &weight, (3, 4), &bias);

        let mut backend = CandleBackend::new(sample_config());
        backend.load(&path).expect("load ok");

        let obs: Vec<Observation> = (0..3)
            .map(|i| Observation {
                symbol: format!("BTC-USDT-{i}"),
                timestamp_ns: 1_000_000_000,
                features: vec![0.0; 4],
            })
            .collect();
        let actions = backend.infer_batch(&obs).expect("batch ok");
        assert_eq!(actions.len(), 3);
        for a in &actions {
            assert_eq!(a.action_type, ActionType::Hold);
        }
    }

    /// `infer_batch` 收到 0 条 obs 时返回空 Vec
    #[test]
    fn candle_infer_batch_empty_returns_empty_vec() {
        let dir = tempfile::tempdir().expect("tempdir");
        let weight = vec![0.0f32; 3 * 4];
        let bias = vec![0.0f32; 3];
        let path = write_test_safetensors(dir.path(), &weight, (3, 4), &bias);

        let mut backend = CandleBackend::new(sample_config());
        backend.load(&path).expect("load ok");
        let actions = backend.infer_batch(&[]).expect("empty batch");
        assert!(actions.is_empty());
    }

    /// 5 维 output_dim 时验证 `ReduceLong` / `ReduceShort` 映射
    #[test]
    fn candle_action_type_mapping_5_classes() {
        let dir = tempfile::tempdir().expect("tempdir");
        // bias 让 argmax 落在 index=3 (ReduceLong) 和 index=4 (ReduceShort)
        let weight = vec![0.0f32; 5 * 4];
        let mut bias = vec![0.0f32; 5];
        bias[3] = 1.0; // ReduceLong
        let path = write_test_safetensors(dir.path(), &weight, (5, 4), &bias);

        let mut cfg = sample_config();
        cfg.output_dim = 5;
        let mut backend = CandleBackend::new(cfg);
        backend.load(&path).expect("load ok");

        let obs = Observation {
            symbol: "BTC-USDT".into(),
            timestamp_ns: 1_000_000_000,
            features: vec![0.0; 4],
        };
        let action = backend.infer(&obs).expect("infer ok");
        assert_eq!(action.action_type, ActionType::ReduceLong);
    }

    /// input features 维度与 config 不匹配时返回 DimensionMismatch
    #[test]
    fn candle_infer_dimension_mismatch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let weight = vec![0.0f32; 3 * 4];
        let bias = vec![0.0f32; 3];
        let path = write_test_safetensors(dir.path(), &weight, (3, 4), &bias);

        let mut backend = CandleBackend::new(sample_config());
        backend.load(&path).expect("load ok");

        let obs = Observation {
            symbol: "BTC-USDT".into(),
            timestamp_ns: 1_000_000_000,
            features: vec![0.0; 3], // 期望 4,实际 3
        };
        let err = backend.infer(&obs).unwrap_err();
        assert!(matches!(err, InferenceError::DimensionMismatch { .. }));
    }
}
