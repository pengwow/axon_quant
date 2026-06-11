//! PyO3 桥接层
//!
//! 将 Rust 端 `DistributedConfig` / `TrainingCheckpoint` 暴露给 Python。

#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::useless_conversion)]
#![allow(deprecated)]

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::checkpoint::{StepMetrics, TrainingCheckpoint};
use crate::config::DistributedConfig;

/// 分布式训练运行器
#[pyclass(name = "DistributedRunner")]
pub struct DistributedRunner {
    config: DistributedConfig,
}

#[pymethods]
impl DistributedRunner {
    /// 从 Python dict 创建 runner
    #[new]
    fn new(config_dict: &Bound<'_, PyDict>) -> PyResult<Self> {
        let json_str: String = Python::attach(|py| {
            let json_module = py.import("json")?;
            let dumped = json_module.call_method1("dumps", (config_dict,))?;
            dumped.extract::<String>()
        })?;
        let cfg: DistributedConfig = serde_json::from_str(&json_str)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("invalid config: {e}")))?;
        cfg.validate()
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e))?;
        Ok(Self { config: cfg })
    }

    /// 从 TOML 文件加载
    #[staticmethod]
    fn from_toml_file(path: String) -> PyResult<Self> {
        let cfg = DistributedConfig::from_toml_file(std::path::Path::new(&path))
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("{e:?}")))?;
        Ok(Self { config: cfg })
    }

    /// 获取摘要
    fn __repr__(&self) -> String {
        format!(
            "DistributedRunner(workers={}, algo={}, batch={})",
            self.config.cluster.num_workers,
            self.config.algorithm.algorithm,
            self.config.resources.train_batch_size
        )
    }
}

/// 便捷函数：序列化 TrainingCheckpoint
#[pyfunction]
fn py_save_checkpoint(
    iteration: usize,
    policy_state: Vec<u8>,
    optimizer_state: Vec<u8>,
    rng_state: Vec<u8>,
) -> String {
    let ckpt = TrainingCheckpoint::new(iteration, policy_state, optimizer_state, rng_state);
    ckpt.to_json()
        .unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
}

/// 便捷函数：反序列化 TrainingCheckpoint
#[pyfunction]
fn py_load_checkpoint(json: &str) -> PyResult<(usize, Vec<u8>)> {
    let ckpt = TrainingCheckpoint::from_json(json)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("{e}")))?;
    Ok((ckpt.iteration, ckpt.policy_state))
}

/// 便捷函数：序列化 StepMetrics
#[pyfunction]
fn py_serialize_metrics(
    step: usize,
    reward: f64,
    policy_loss: f64,
    value_loss: f64,
    entropy: f64,
    fps: f64,
) -> String {
    let m = StepMetrics {
        step,
        episode_reward_mean: reward,
        episode_len_mean: 0.0,
        policy_loss,
        value_loss,
        entropy,
        fps,
    };
    serde_json::to_string(&m).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
}

/// Python 模块入口
pub fn register_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<DistributedRunner>()?;
    m.add_function(wrap_pyfunction!(py_save_checkpoint, m)?)?;
    m.add_function(wrap_pyfunction!(py_load_checkpoint, m)?)?;
    m.add_function(wrap_pyfunction!(py_serialize_metrics, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
