//! PyO3 桥接层
//!
//! 将 Rust 端 `WalkForwardConfig` / `TimeSeriesSplitter` / `aggregate_folds` / `deflated_sharpe`
//! 暴露给 Python。

#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::useless_conversion)]
#![allow(deprecated)]

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::config::{WalkForwardConfig, WindowType};
use crate::evaluation::{aggregate_folds, compute_deflated_sharpe};
use crate::metrics::{FoldResult, ISMetrics, OOSMetrics};
use crate::purge::{detect_leakage, embargo_indices, purge_overlapping_labels};
use crate::split::TimeSeriesSplitter;

/// Walk-Forward 运行器（PyO3 接口）
#[pyclass(name = "WalkForwardRunner")]
pub struct WalkForwardRunner {
    config: WalkForwardConfig,
}

#[pymethods]
impl WalkForwardRunner {
    /// 从 Python dict 创建 runner
    #[new]
    fn new(config: &Bound<'_, PyDict>) -> PyResult<Self> {
        let json_str: String = Python::with_gil(|py| {
            let json_module = py.import("json")?;
            let dumped = json_module.call_method1("dumps", (config,))?;
            dumped.extract::<String>()
        })?;
        let cfg: WalkForwardConfig = serde_json::from_str(&json_str)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("invalid config: {e}")))?;
        cfg.validate()
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e))?;
        Ok(Self { config: cfg })
    }

    /// 分割 n_samples 个数据点
    fn split<'py>(&self, py: Python<'py>, n_samples: usize) -> PyResult<Bound<'py, PyList>> {
        let folds = TimeSeriesSplitter::new(self.config.clone()).split(n_samples);
        let list = PyList::empty_bound(py);
        for f in folds {
            let dict = PyDict::new_bound(py);
            dict.set_item("fold_id", f.fold_id)?;
            dict.set_item("train_start", f.train_start)?;
            dict.set_item("train_end", f.train_end)?;
            dict.set_item("validation_start", f.validation_start)?;
            dict.set_item("validation_end", f.validation_end)?;
            dict.set_item("test_start", f.test_start)?;
            dict.set_item("test_end", f.test_end)?;
            list.append(dict)?;
        }
        Ok(list)
    }

    /// 配置摘要
    fn __repr__(&self) -> String {
        format!(
            "WalkForwardRunner(train={}, test={}, step={}, type={:?})",
            self.config.train_size,
            self.config.test_size,
            self.config.step_size,
            self.config.window_type
        )
    }
}

/// 便捷函数：聚合 fold 结果（接收 Python dict 列表）
#[pyfunction]
fn py_aggregate_folds<'py>(
    py: Python<'py>,
    folds: Vec<std::collections::HashMap<String, f64>>,
) -> PyResult<Bound<'py, PyDict>> {
    let mut fold_results = Vec::with_capacity(folds.len());
    for (i, t) in folds.into_iter().enumerate() {
        let is_m = ISMetrics {
            total_return: t.get("train_return").copied().unwrap_or(0.0),
            ..ISMetrics::default()
        };
        let oos_m = OOSMetrics {
            total_return: t.get("test_return").copied().unwrap_or(0.0),
            sharpe_ratio: t.get("test_sharpe").copied().unwrap_or(0.0),
            max_drawdown: t.get("test_max_drawdown").copied().unwrap_or(0.0),
            ..OOSMetrics::default()
        };
        let split = crate::split::FoldSplit {
            fold_id: i,
            train_start: 0,
            train_end: 0,
            validation_start: 0,
            validation_end: 0,
            test_start: 0,
            test_end: 0,
        };
        fold_results.push(FoldResult::new(i, split, is_m, oos_m));
    }
    let (agg, stab) = aggregate_folds(&fold_results);
    let dict = PyDict::new_bound(py);
    dict.set_item("mean_oos_return", agg.mean_oos_return)?;
    dict.set_item("std_oos_return", agg.std_oos_return)?;
    dict.set_item("mean_oos_sharpe", agg.mean_oos_sharpe)?;
    dict.set_item("std_oos_sharpe", agg.std_oos_sharpe)?;
    dict.set_item("median_oos_return", agg.median_oos_return)?;
    dict.set_item("worst_fold_return", agg.worst_fold_return)?;
    dict.set_item("best_fold_return", agg.best_fold_return)?;
    dict.set_item("pct_profitable_folds", agg.pct_profitable_folds)?;
    dict.set_item("sharpe_of_sharpe", stab.sharpe_of_sharpe)?;
    dict.set_item("return_autocorrelation", stab.return_autocorrelation)?;
    dict.set_item("deflated_sharpe", stab.deflated_sharpe)?;
    dict.set_item("probability_of_loss", stab.probability_of_loss)?;
    Ok(dict)
}

/// 便捷函数：Deflated Sharpe Ratio
#[pyfunction]
fn py_deflated_sharpe(observed_sharpe: f64, n_trials: usize, sharpe_std: f64) -> f64 {
    compute_deflated_sharpe(observed_sharpe, n_trials, sharpe_std)
}

/// 便捷函数：泄漏检测
#[pyfunction]
fn py_detect_leakage(
    train_idx: Vec<usize>,
    test_idx: Vec<usize>,
    feature_lag: usize,
) -> (bool, Vec<(usize, usize)>) {
    detect_leakage(&train_idx, &test_idx, feature_lag)
}

/// 便捷函数：purge
#[pyfunction]
fn py_purge_overlapping_labels(
    train_idx: Vec<usize>,
    test_idx: Vec<usize>,
    label_horizon: usize,
) -> Vec<usize> {
    purge_overlapping_labels(&train_idx, &test_idx, label_horizon)
}

/// 便捷函数：embargo
#[pyfunction]
fn py_embargo_indices(test_idx: Vec<usize>, embargo_pct: f64, n_total: usize) -> Vec<usize> {
    embargo_indices(&test_idx, embargo_pct, n_total)
}

/// axon_walk_forward Python 模块入口
pub fn register_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<WalkForwardRunner>()?;
    m.add_function(wrap_pyfunction!(py_aggregate_folds, m)?)?;
    m.add_function(wrap_pyfunction!(py_deflated_sharpe, m)?)?;
    m.add_function(wrap_pyfunction!(py_detect_leakage, m)?)?;
    m.add_function(wrap_pyfunction!(py_purge_overlapping_labels, m)?)?;
    m.add_function(wrap_pyfunction!(py_embargo_indices, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}

// 避免未使用导入警告
#[allow(dead_code)]
fn _ensure_window_type_used() -> WindowType {
    WindowType::Expanding
}
