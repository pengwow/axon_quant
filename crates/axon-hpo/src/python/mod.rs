//! PyO3 桥接层：通过 Python 调用 Optuna HPO
//!
//! Rust 端负责：配置校验、结果后处理、Pareto 前沿计算（权威实现）
//! Python 端负责：Optuna study 管理、trial 执行、剪枝决策
//!
//! ## 通信流程
//!
//! ```text
//! Rust: HPORunner::run(config, objective_py)
//!   ↓ PyO3 GIL 获取
//! Python: axon_hpo.optuna_runner.OptunaHPO(...)
//!   ↓ Optuna study.optimize
//! Python: trial._objective() → 调用 objective_py(params)
//!   ↓ 收集 TrialResult
//! Rust: 转为 HPOResult，调用 Rust 端 pareto / hypervolume
//! ```

#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::useless_conversion)]
#![allow(deprecated)]

use std::collections::HashMap;
use std::time::Instant;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyTuple};
use serde_json::Value as JsonValue;

use crate::config::HPOConfig;
use crate::pareto::{ParetoPoint, compute_hypervolume_from_points, compute_pareto_front};
use crate::result::HPOResult;
use crate::search_space::SearchSpaceDef;
use crate::trial::{TrialResult, TrialState};

/// 把 Rust `SearchSpaceDef` 序列化为 Python dict（供 Optuna 使用）
fn space_def_to_py<'py>(py: Python<'py>, def: &SearchSpaceDef) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    match def {
        SearchSpaceDef::Uniform { low, high } => {
            dict.set_item("type", "uniform")?;
            dict.set_item("low", *low)?;
            dict.set_item("high", *high)?;
        }
        SearchSpaceDef::LogUniform { low, high } => {
            dict.set_item("type", "log_uniform")?;
            dict.set_item("low", *low)?;
            dict.set_item("high", *high)?;
        }
        SearchSpaceDef::IntUniform { low, high, step } => {
            dict.set_item("type", "int_uniform")?;
            dict.set_item("low", *low)?;
            dict.set_item("high", *high)?;
            dict.set_item("step", *step)?;
        }
        SearchSpaceDef::Discrete { choices } => {
            dict.set_item("type", "discrete")?;
            let list = PyList::new(py, choices.iter().map(|v| *v as f64))?;
            dict.set_item("choices", list)?;
        }
        SearchSpaceDef::Choice { choices } => {
            dict.set_item("type", "choice")?;
            let mut py_choices: Vec<Py<PyAny>> = Vec::with_capacity(choices.len());
            for v in choices {
                py_choices.push(json_to_py(py, v)?);
            }
            let list = PyList::new(py, py_choices.iter())?;
            dict.set_item("choices", list)?;
        }
        SearchSpaceDef::Categorical { choices } => {
            dict.set_item("type", "categorical")?;
            let mut py_choices: Vec<Py<PyAny>> = Vec::with_capacity(choices.len());
            for v in choices {
                py_choices.push(json_to_py(py, v)?);
            }
            let list = PyList::new(py, py_choices.iter())?;
            dict.set_item("choices", list)?;
        }
    }
    Ok(dict)
}

/// JSON value → Python object
fn json_to_py(py: Python<'_>, v: &JsonValue) -> PyResult<Py<PyAny>> {
    match v {
        JsonValue::Null => Ok(py.None()),
        JsonValue::Bool(b) => {
            let obj = b.into_pyobject(py)?;
            Ok(obj.to_owned().into_any().unbind())
        }
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                let obj = i.into_pyobject(py)?;
                Ok(obj.to_owned().into_any().unbind())
            } else if let Some(u) = n.as_u64() {
                let obj = u.into_pyobject(py)?;
                Ok(obj.to_owned().into_any().unbind())
            } else if let Some(f) = n.as_f64() {
                let obj = f.into_pyobject(py)?;
                Ok(obj.to_owned().into_any().unbind())
            } else {
                Ok(py.None())
            }
        }
        JsonValue::String(s) => {
            let obj = s.into_pyobject(py)?;
            Ok(obj.to_owned().into_any().unbind())
        }
        JsonValue::Array(arr) => {
            let list = PyList::empty(py);
            for item in arr {
                let obj = json_to_py(py, item)?;
                list.append(obj)?;
            }
            Ok(list.into_any().unbind())
        }
        JsonValue::Object(map) => {
            let dict = PyDict::new(py);
            for (k, v) in map {
                let obj = json_to_py(py, v)?;
                dict.set_item(k, obj)?;
            }
            Ok(dict.into_any().unbind())
        }
    }
}

/// HPO 运行器（PyO3 接口）
#[pyclass(name = "HPORunner")]
pub struct HPORunner {
    config: HPOConfig,
}

#[pymethods]
impl HPORunner {
    /// 从 Python dict 创建 runner
    #[new]
    fn new(config: &Bound<'_, PyDict>) -> PyResult<Self> {
        // 从 Python dict 反序列化为 HPOConfig
        let json_str: String = python_dict_to_json(config)?;
        let cfg: HPOConfig = serde_json::from_str(&json_str)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("invalid config: {e}")))?;
        Ok(Self { config: cfg })
    }

    /// 运行 HPO，objective_fn 是 Python 可调用对象
    ///
    /// Args:
    /// - objective_fn: `Callable[[dict], list[float]]`
    ///
    /// Returns:
    /// - dict（含 `best_trial` / `all_trials` / `pareto_front` / `param_importances`）
    fn run<'py>(
        &self,
        py: Python<'py>,
        objective_fn: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyDict>> {
        // 1. 校验搜索空间
        for (name, def) in &self.config.search_space {
            def.validate()
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("{name}: {e}")))?;
        }

        // 2. 构建 search_space dict（Python 侧）
        let search_space = PyDict::new(py);
        for (name, def) in &self.config.search_space {
            let py_def = space_def_to_py(py, def)?;
            search_space.set_item(name, py_def)?;
        }

        // 3. 调用 Python axon_hpo.optuna_runner.OptunaHPO
        let module = py.import("axon_hpo.optuna_runner")?;
        let optuna_runner = module.getattr("OptunaHPO")?;

        // 提取 directions
        let directions: Vec<String> = self
            .config
            .objective
            .objective
            .to_optuna_directions()
            .iter()
            .map(|s| s.to_string())
            .collect();
        let directions_py: Vec<&str> = directions.iter().map(|s| s.as_str()).collect();

        // 构造 pruner / sampler config（默认配置即可）
        let types_module = py.import("axon_hpo.types")?;
        let pruner_cfg = types_module.getattr("PrunerConfig")?.call0()?;
        let sampler_cfg = types_module.getattr("SamplerConfig")?.call0()?;

        let runner_obj = optuna_runner.call1((
            search_space,
            objective_fn,
            self.config.study.study_name.clone(),
            PyTuple::new(py, directions_py)?,
            pruner_cfg,
            sampler_cfg,
            self.config
                .study
                .storage
                .clone()
                .unwrap_or_else(|| "None".to_string()),
        ))?;

        // 4. 调用 run() 方法
        let t0 = Instant::now();
        let _ = runner_obj.call_method(
            "run",
            (
                self.config.n_trials(),
                self.config.n_jobs(),
                self.config.timeout_seconds(),
            ),
            None,
        )?;
        let elapsed_ms = t0.elapsed().as_millis() as u64;

        // 5. 收集结果
        let trials_py_bound = runner_obj.call_method0("collect_results")?;
        let trials_py: Vec<Py<PyAny>> = trials_py_bound.extract()?;
        let trials: Vec<TrialResult> = trials_py
            .iter()
            .map(|t| py_to_trial_result(py, t))
            .collect::<PyResult<Vec<_>>>()?;

        // 6. 计算 Pareto 前沿（如果多目标）
        let n_obj = self.config.objective.objective.n_directions();
        let pareto_front = if n_obj > 1 {
            let directions_enum: Vec<crate::config::StudyDirection> =
                match &self.config.objective.objective {
                    crate::config::ObjectiveDef::Single { direction } => {
                        vec![direction.clone()]
                    }
                    crate::config::ObjectiveDef::Multi { directions } => directions.clone(),
                };
            let front = compute_pareto_front(&trials, &directions_enum)
                .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
            Some(front.points)
        } else {
            None
        };

        // 7. 找最佳 trial（单目标）
        let best_trial = if n_obj == 1 {
            trials
                .iter()
                .filter(|t| t.state.is_complete())
                .max_by(|a, b| {
                    let va = a.values.first().copied().unwrap_or(f64::NEG_INFINITY);
                    let vb = b.values.first().copied().unwrap_or(f64::NEG_INFINITY);
                    va.partial_cmp(&vb).unwrap_or(std::cmp::Ordering::Equal)
                })
                .cloned()
        } else {
            None
        };

        // 8. 构造结果
        let hpo_result = HPOResult {
            study_config: self.config.study.clone(),
            best_trial,
            all_trials: trials,
            param_importances: HashMap::new(),
            pareto_front,
            elapsed_ms,
        };

        // 9. 序列化为 Python dict
        result_to_py_dict(py, &hpo_result)
    }

    /// 配置摘要
    fn __repr__(&self) -> String {
        format!(
            "HPORunner(study={}, n_trials={}, n_jobs={})",
            self.config.study.study_name,
            self.config.n_trials(),
            self.config.n_jobs()
        )
    }
}

/// Python dict → JSON string（用于 `HPORunner::new`）
fn python_dict_to_json(dict: &Bound<'_, PyDict>) -> PyResult<String> {
    // 使用 Python 的 json 模块做转换
    Python::attach(|py| {
        let json_module = py.import("json")?;
        let dumped = json_module.call_method1("dumps", (dict,))?;
        dumped.extract::<String>()
    })
}

/// 把 HPOResult 转为 Python dict
fn result_to_py_dict<'py>(py: Python<'py>, result: &HPOResult) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);

    // best_trial
    if let Some(best) = &result.best_trial {
        dict.set_item("best_trial", trial_result_to_py_dict(py, best)?)?;
    } else {
        dict.set_item("best_trial", py.None())?;
    }

    // all_trials
    let trials_list = PyList::empty(py);
    for t in &result.all_trials {
        trials_list.append(trial_result_to_py_dict(py, t)?)?;
    }
    dict.set_item("all_trials", trials_list)?;

    // param_importances（暂未实现）
    dict.set_item("param_importances", PyDict::new(py))?;

    // pareto_front
    if let Some(front) = &result.pareto_front {
        let front_list = PyList::empty(py);
        for p in front {
            front_list.append(pareto_point_to_py_dict(py, p)?)?;
        }
        dict.set_item("pareto_front", front_list)?;
    } else {
        dict.set_item("pareto_front", py.None())?;
    }

    // elapsed_ms
    dict.set_item("elapsed_ms", result.elapsed_ms)?;

    // n_complete
    dict.set_item("n_complete", result.n_complete())?;

    Ok(dict)
}

/// TrialResult → Python dict
fn trial_result_to_py_dict<'py>(py: Python<'py>, t: &TrialResult) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("trial_id", t.trial_id)?;
    dict.set_item(
        "params",
        json_value_to_py_dict(
            py,
            &JsonValue::Object(
                t.params
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ),
        )?,
    )?;
    let values_list = PyList::new(py, t.values.iter())?;
    dict.set_item("values", values_list)?;
    dict.set_item("state", t.state.as_str())?;
    dict.set_item("duration_ms", t.duration_ms)?;
    let intermediate = PyList::empty(py);
    for (step, val) in &t.intermediate_values {
        let pair = PyTuple::new(
            py,
            &[
                (step_value_py(py, *step)?),
                val.into_pyobject(py)?.into_any().unbind(),
            ],
        )?;
        intermediate.append(pair)?;
    }
    dict.set_item("intermediate_values", intermediate)?;
    Ok(dict)
}

/// 把 usize step 转为 Python 整数
fn step_value_py(py: Python<'_>, step: usize) -> PyResult<Py<PyAny>> {
    let obj = step.into_pyobject(py)?;
    Ok(obj.to_owned().into_any().unbind())
}

/// ParetoPoint → Python dict
fn pareto_point_to_py_dict<'py>(py: Python<'py>, p: &ParetoPoint) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("trial_id", p.trial_id)?;
    dict.set_item(
        "params",
        json_value_to_py_dict(
            py,
            &JsonValue::Object(
                p.params
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ),
        )?,
    )?;
    let obj_list = PyList::new(py, p.objectives.iter())?;
    dict.set_item("objectives", obj_list)?;
    Ok(dict)
}

/// JSON Value → Python dict/list（专用于内部参数 dict）
fn json_value_to_py_dict<'py>(py: Python<'py>, v: &JsonValue) -> PyResult<Bound<'py, PyAny>> {
    match v {
        JsonValue::Object(map) => {
            let dict = PyDict::new(py);
            for (k, v) in map {
                let obj = json_to_py(py, v)?;
                dict.set_item(k, obj)?;
            }
            Ok(dict.into_any())
        }
        _ => {
            let obj = json_to_py(py, v)?;
            Ok(obj.into_bound(py))
        }
    }
}

/// 把 Python object 转为 Rust TrialResult（用于 `HPORunner::run` 收集结果）
fn py_to_trial_result(py: Python<'_>, obj: &Py<PyAny>) -> PyResult<TrialResult> {
    let bound = obj.bind(py);
    let trial_id: i32 = bound.get_item("trial_id")?.extract()?;
    let state_str: String = bound.get_item("state")?.extract()?;
    let values: Vec<f64> = bound.get_item("values")?.extract()?;
    let duration_ms: u64 = bound.get_item("duration_ms")?.extract().unwrap_or(0);

    let params_value = bound.get_item("params")?;
    let params_py: &Bound<'_, PyDict> = params_value.cast()?;
    let mut params: HashMap<String, JsonValue> = HashMap::new();
    for (k, v) in params_py.iter() {
        let key: String = k.extract()?;
        let json_val = py_to_json(&v)?;
        params.insert(key, json_val);
    }

    let state = match state_str.as_str() {
        "complete" | "COMPLETE" => TrialState::Complete,
        "pruned" | "PRUNED" => TrialState::Pruned,
        "fail" | "FAIL" => TrialState::Fail,
        "running" | "RUNNING" => TrialState::Running,
        _ => TrialState::Fail,
    };

    Ok(TrialResult {
        trial_id,
        params,
        values,
        state,
        duration_ms,
        intermediate_values: Vec::new(),
    })
}

/// Python object → JSON value
fn py_to_json(obj: &Bound<'_, PyAny>) -> PyResult<JsonValue> {
    if obj.is_none() {
        return Ok(JsonValue::Null);
    }
    if let Ok(b) = obj.extract::<bool>() {
        return Ok(JsonValue::Bool(b));
    }
    if let Ok(i) = obj.extract::<i64>() {
        return Ok(JsonValue::Number(i.into()));
    }
    if let Ok(f) = obj.extract::<f64>() {
        return serde_json::Number::from_f64(f)
            .map(JsonValue::Number)
            .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("non-finite number"));
    }
    if let Ok(s) = obj.extract::<String>() {
        return Ok(JsonValue::String(s));
    }
    if let Ok(list) = obj.cast::<PyList>() {
        let mut arr = Vec::with_capacity(list.len());
        for item in list {
            arr.push(py_to_json(&item)?);
        }
        return Ok(JsonValue::Array(arr));
    }
    if let Ok(dict) = obj.cast::<PyDict>() {
        let mut map = serde_json::Map::new();
        for (k, v) in dict.iter() {
            let key: String = k.extract()?;
            map.insert(key, py_to_json(&v)?);
        }
        return Ok(JsonValue::Object(map));
    }
    Err(pyo3::exceptions::PyTypeError::new_err(format!(
        "unsupported type: {}",
        obj.get_type().name()?
    )))
}

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
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e))?;
    Ok(true)
}

/// axon_hpo Python 模块入口
pub fn register_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<HPORunner>()?;
    m.add_function(wrap_pyfunction!(py_compute_pareto_front, m)?)?;
    m.add_function(wrap_pyfunction!(py_compute_hypervolume, m)?)?;
    m.add_function(wrap_pyfunction!(py_validate_search_space, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
