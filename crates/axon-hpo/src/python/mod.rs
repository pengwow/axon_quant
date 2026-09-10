//! PyO3 桥接层：把 Rust 权威数值计算暴露给 Python
//!
//! Rust 端负责：搜索空间校验、Pareto 前沿计算、超体积计算（权威实现）
//!
//! 说明：HPO 的执行器统一走纯 Python 的 `axon_hpo.OptunaHPO`（以及复用它
//! 的 `axon_quant.training.RLHPOSweeper`）。这里只保留不带 Optuna 依赖的、
//! 需要 Rust 加速的纯数值工具函数。

#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::useless_conversion)]
#![allow(deprecated)]

use std::collections::HashMap;

use pyo3::prelude::*;
use pyo3::types::PyAny;

use crate::pareto::{ParetoPoint, compute_hypervolume_from_points, compute_pareto_front};
use crate::search_space::SearchSpaceDef;
use crate::trial::{TrialResult, TrialState};

/// 便捷函数：Rust 端计算 Pareto 前沿（暴露给 Python）
#[pyfunction]
fn py_compute_pareto_front(
    py: Python<'_>,
    trials: Vec<HashMap<String, Py<PyAny>>>,
    directions: Vec<String>,
) -> PyResult<Py<PyAny>> {
    let dirs: Vec<crate::config::StudyDirection> = directions
        .iter()
        .map(|d| match d.as_str() {
            "minimize" => crate::config::StudyDirection::Minimize,
            _ => crate::config::StudyDirection::Maximize,
        })
        .collect();

    let mut trial_results: Vec<TrialResult> = Vec::with_capacity(trials.len());
    for t in trials {
        let trial_id: i32 = t
            .get("trial_id")
            .and_then(|v| v.bind(py).extract::<i32>().ok())
            .unwrap_or(0);
        let values: Vec<f64> = t
            .get("values")
            .and_then(|v| v.bind(py).extract::<Vec<f64>>().ok())
            .unwrap_or_default();
        let state_str: String = t
            .get("state")
            .and_then(|v| v.bind(py).extract::<String>().ok())
            .unwrap_or_else(|| "complete".to_string());
        let state = match state_str.as_str() {
            "pruned" => TrialState::Pruned,
            "fail" => TrialState::Fail,
            "running" => TrialState::Running,
            _ => TrialState::Complete,
        };
        trial_results.push(TrialResult {
            trial_id,
            params: HashMap::new(),
            values,
            state,
            duration_ms: 0,
            intermediate_values: Vec::new(),
        });
    }

    let front = compute_pareto_front(&trial_results, &dirs)
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
    let result_json = serde_json::to_string(&front.points)
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
    let result: Py<PyAny> = py
        .eval(
            &std::ffi::CString::new(result_json).map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("invalid JSON: {e}"))
            })?,
            None,
            None,
        )?
        .into();
    Ok(result)
}

/// 便捷函数：Rust 端计算超体积（暴露给 Python）
#[pyfunction]
fn py_compute_hypervolume(
    py: Python<'_>,
    points: Vec<HashMap<String, Py<PyAny>>>,
    directions: Vec<String>,
    reference_point: Vec<f64>,
) -> PyResult<f64> {
    let dirs: Vec<crate::config::StudyDirection> = directions
        .iter()
        .map(|d| match d.as_str() {
            "minimize" => crate::config::StudyDirection::Minimize,
            _ => crate::config::StudyDirection::Maximize,
        })
        .collect();

    let mut pareto_points: Vec<ParetoPoint> = Vec::with_capacity(points.len());
    for p in points {
        let trial_id: i32 = p
            .get("trial_id")
            .and_then(|v| v.bind(py).extract::<i32>().ok())
            .unwrap_or(0);
        let objectives: Vec<f64> = p
            .get("objectives")
            .or_else(|| p.get("values"))
            .and_then(|v| v.bind(py).extract::<Vec<f64>>().ok())
            .unwrap_or_default();
        pareto_points.push(ParetoPoint {
            params: HashMap::new(),
            objectives,
            trial_id,
        });
    }

    let hv = compute_hypervolume_from_points(&pareto_points, &dirs, &reference_point)
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
    Ok(hv)
}

/// 便捷函数：Rust 端校验搜索空间
#[pyfunction]
fn py_validate_search_space(def_json: String) -> PyResult<bool> {
    let def: SearchSpaceDef = serde_json::from_str(&def_json)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("invalid: {e}")))?;
    def.validate()
        .map_err(pyo3::exceptions::PyValueError::new_err)?;
    Ok(true)
}

/// axon_hpo Python 模块入口
pub fn register_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(py_compute_pareto_front, m)?)?;
    m.add_function(wrap_pyfunction!(py_compute_hypervolume, m)?)?;
    m.add_function(wrap_pyfunction!(py_validate_search_space, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}