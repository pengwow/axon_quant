//! PyO3 桥接层

#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::useless_conversion)]
#![allow(deprecated)]

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::backends::{LocalTracker, MemoryTracker};
use crate::tracker::ExperimentTracker;
use crate::types::{ParamValue, RunStatus};

/// Tracker Python 接口
#[pyclass(name = "MemoryTracker")]
pub struct PyMemoryTracker {
    inner: MemoryTracker,
}

#[pymethods]
impl PyMemoryTracker {
    #[new]
    fn new() -> Self {
        Self {
            inner: MemoryTracker::new(),
        }
    }

    fn log_param(&self, key: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
        let pv = python_to_param_value(value)?;
        self.inner
            .log_param(key, &pv)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:?}")))
    }

    fn log_metric(&self, key: &str, value: f64, step: usize) -> PyResult<()> {
        self.inner
            .log_metric(key, value, step)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:?}")))
    }

    fn set_tag(&self, key: &str, value: &str) -> PyResult<()> {
        self.inner
            .set_tag(key, value)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:?}")))
    }

    fn finish(&self, status: &str) -> PyResult<()> {
        let s = match status {
            "running" => RunStatus::Running,
            "completed" => RunStatus::Completed,
            "failed" => RunStatus::Failed,
            "killed" => RunStatus::Killed,
            _ => return Err(pyo3::exceptions::PyValueError::new_err("invalid status")),
        };
        self.inner
            .finish(s)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:?}")))
    }

    fn get_metrics<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        for entry in self.inner.get_metrics() {
            let key = format!("{}/{}", entry.key, entry.step);
            if let crate::types::MetricValue::Scalar(v) = entry.value {
                dict.set_item(key, v)?;
            }
        }
        Ok(dict)
    }

    fn __repr__(&self) -> String {
        format!("MemoryTracker(run_id={:?})", self.inner.run_id().0)
    }
}

/// LocalTracker Python 接口
#[pyclass(name = "LocalTracker")]
pub struct PyLocalTracker {
    inner: LocalTracker,
}

#[pymethods]
impl PyLocalTracker {
    #[new]
    fn new(base_dir: &str) -> PyResult<Self> {
        let t = LocalTracker::new(std::path::PathBuf::from(base_dir))
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:?}")))?;
        Ok(Self { inner: t })
    }

    fn log_param(&self, key: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
        let pv = python_to_param_value(value)?;
        self.inner
            .log_param(key, &pv)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:?}")))
    }

    fn log_metric(&self, key: &str, value: f64, step: usize) -> PyResult<()> {
        self.inner
            .log_metric(key, value, step)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:?}")))
    }

    fn flush(&self) -> PyResult<()> {
        self.inner
            .flush()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:?}")))
    }

    fn finish(&self, status: &str) -> PyResult<()> {
        let s = match status {
            "running" => RunStatus::Running,
            "completed" => RunStatus::Completed,
            "failed" => RunStatus::Failed,
            "killed" => RunStatus::Killed,
            _ => return Err(pyo3::exceptions::PyValueError::new_err("invalid status")),
        };
        self.inner
            .finish(s)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:?}")))
    }

    fn __repr__(&self) -> String {
        format!("LocalTracker(run_id={:?})", self.inner.run_id().0)
    }
}

/// 便捷函数：获取运行 ID
#[pyfunction]
fn py_memory_tracker_run_id(tracker: &PyMemoryTracker) -> String {
    tracker.inner.run_id().0
}

fn python_to_param_value(obj: &Bound<'_, PyAny>) -> PyResult<ParamValue> {
    if let Ok(b) = obj.extract::<bool>() {
        Ok(ParamValue::Bool(b))
    } else if let Ok(i) = obj.extract::<i64>() {
        Ok(ParamValue::Int(i))
    } else if let Ok(f) = obj.extract::<f64>() {
        Ok(ParamValue::Float(f))
    } else if let Ok(s) = obj.extract::<String>() {
        Ok(ParamValue::String(s))
    } else {
        Err(pyo3::exceptions::PyTypeError::new_err(
            "unsupported param value type (must be bool, int, float, or str)",
        ))
    }
}

/// Python 模块入口
pub fn register_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyMemoryTracker>()?;
    m.add_class::<PyLocalTracker>()?;
    m.add_function(wrap_pyfunction!(py_memory_tracker_run_id, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
