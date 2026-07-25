//! PyO3 trajectory 绑定入口(Task 9)

#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::useless_conversion)]

use pyo3::prelude::*;
use pyo3::BoundObject;
use pyo3::types::{PyDict, PyList};
use std::sync::Arc as StdArc;

use crate::trading::trajectory::{ToolCall, TrajectoryBar, TrajectoryRecorder};

#[pyclass(name = "TrajectoryRecorder")]
pub struct PyTrajectoryRecorder {
    pub(crate) recorder: StdArc<std::sync::Mutex<TrajectoryRecorder>>,
}

#[pymethods]
impl PyTrajectoryRecorder {
    #[new]
    fn new(seed: u64, instrument: &str, provider: &str, model: &str) -> Self {
        let recorder = TrajectoryRecorder::new(seed, instrument.to_string(), provider.to_string(), model.to_string());
        Self {
            recorder: StdArc::new(std::sync::Mutex::new(recorder)),
        }
    }

    fn get_run_id(&self) -> String {
        self.recorder.lock().unwrap().trajectory().run_id.clone()
    }

    fn set_run_id(&self, run_id: &str) {
        let mut recorder = self.recorder.lock().unwrap();
        recorder.trajectory_mut().run_id = run_id.to_string();
    }

    #[pyo3(signature = (bar_id, ts, thought, action=None, observation=None, reward=0.0, cum_pnl=0.0))]
    fn record(
        &self,
        bar_id: u64,
        ts: i64,
        thought: &str,
        action: Option<&Bound<'_, PyDict>>,
        observation: Option<&str>,
        reward: f64,
        cum_pnl: f64,
    ) -> PyResult<()> {
        let tool_call = if let Some(action_dict) = action {
            let tool: String = match action_dict.get_item("tool") {
                Ok(Some(v)) => v.extract().unwrap_or_else(|_| "unknown".to_string()),
                _ => "unknown".to_string(),
            };
            let args_value = super::helpers::pythonize(action_dict.py(), action_dict.as_any())?;
            Some(ToolCall { tool, args: args_value })
        } else {
            None
        };

        let bar = TrajectoryBar {
            bar_id,
            ts,
            thought: thought.to_string(),
            action: tool_call,
            observation: observation.map(|s| s.to_string()),
            reward,
            cum_pnl,
        };

        self.recorder.lock().unwrap().record(bar);
        Ok(())
    }

    fn flush(&self, path: &str) -> PyResult<()> {
        let path_buf = std::path::PathBuf::from(path);
        self.recorder.lock().unwrap().flush(&path_buf).map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("flush failed: {}", e))
        })
    }

    fn trajectory<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let recorder = self.recorder.lock().unwrap();
        let traj = recorder.trajectory();

        let d = PyDict::new(py);
        d.set_item("version", &traj.version)?;
        d.set_item("run_id", &traj.run_id)?;
        d.set_item("instrument", &traj.instrument)?;
        d.set_item("provider", &traj.provider)?;
        d.set_item("model", &traj.model)?;
        d.set_item("seed", traj.seed)?;

        let bars_list = PyList::empty(py);
        for bar in &traj.bars {
            let bar_dict = PyDict::new(py);
            bar_dict.set_item("bar_id", bar.bar_id)?;
            bar_dict.set_item("ts", bar.ts)?;
            bar_dict.set_item("thought", &bar.thought)?;
            bar_dict.set_item("reward", bar.reward)?;
            bar_dict.set_item("cum_pnl", bar.cum_pnl)?;

            if let Some(action) = &bar.action {
                let action_dict = PyDict::new(py);
                action_dict.set_item("tool", &action.tool)?;
                let args_obj = super::trading::json_to_py(py, &action.args)?;
                action_dict.set_item("args", args_obj)?;
                bar_dict.set_item("action", action_dict)?;
            } else {
                bar_dict.set_item("action", py.None())?;
            }

            if let Some(observation) = &bar.observation {
                bar_dict.set_item("observation", observation)?;
            } else {
                bar_dict.set_item("observation", py.None())?;
            }

            bars_list.append(bar_dict)?;
        }
        d.set_item("bars", bars_list)?;

        if let Some(summary) = &traj.summary {
            let summary_dict = PyDict::new(py);
            summary_dict.set_item("total_pnl", summary.total_pnl)?;
            summary_dict.set_item("trades", summary.trades)?;
            summary_dict.set_item("final_position", summary.final_position)?;
            summary_dict.set_item("wall_time_s", summary.wall_time_s)?;
            summary_dict.set_item("cost_usd", summary.cost_usd)?;
            d.set_item("summary", summary_dict)?;
        } else {
            d.set_item("summary", py.None())?;
        }

        Ok(d.into_bound())
    }

    fn bar_count(&self) -> usize {
        self.recorder.lock().unwrap().trajectory().bars.len()
    }

    fn __repr__(&self) -> String {
        let recorder = self.recorder.lock().unwrap();
        let traj = recorder.trajectory();
        format!("TrajectoryRecorder(run_id={}, bars={})", traj.run_id, traj.bars.len())
    }
}

pub fn register_trajectory_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyTrajectoryRecorder>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::types::PyDict;

    fn run<F, R>(f: F) -> R
    where
        F: FnOnce(Python<'_>) -> R,
    {
        Python::attach(f)
    }

    #[test]
    fn trajectory_recorder_constructs() {
        let recorder = PyTrajectoryRecorder::new(42, "BTC-USDT", "mock", "test");
        assert_eq!(recorder.bar_count(), 0);
    }

    #[test]
    fn trajectory_recorder_record_adds_bar() {
        run(|py| {
            let recorder = PyTrajectoryRecorder::new(42, "BTC-USDT", "mock", "test");
            recorder.record(0, 1234567890, "bar 0", None, None, 0.0, 0.0).unwrap();
            assert_eq!(recorder.bar_count(), 1);
        });
    }

    #[test]
    fn trajectory_recorder_record_with_action() {
        run(|py| {
            let recorder = PyTrajectoryRecorder::new(42, "BTC-USDT", "mock", "test");
            let action = PyDict::new(py).into_bound();
            action.set_item("tool", "place_order").unwrap();
            action.set_item("args", PyDict::new(py).into_bound()).unwrap();
            recorder.record(0, 1234567890, "bar 0", Some(&action), None, 1.0, 0.5).unwrap();
            assert_eq!(recorder.bar_count(), 1);
        });
    }

    #[test]
    fn trajectory_recorder_run_id_getter() {
        let recorder = PyTrajectoryRecorder::new(42, "BTC-USDT", "mock", "test");
        let run_id = recorder.get_run_id();
        assert!(!run_id.is_empty());
    }

    #[test]
    fn trajectory_recorder_run_id_setter() {
        let recorder = PyTrajectoryRecorder::new(42, "BTC-USDT", "mock", "test");
        recorder.set_run_id("custom-run-id");
        assert_eq!(recorder.get_run_id(), "custom-run-id");
    }

    #[test]
    fn trajectory_recorder_flush_writes_file() {
        run(|_py| {
            let recorder = PyTrajectoryRecorder::new(42, "BTC-USDT", "mock", "test");
            recorder.record(0, 1234567890, "bar 0", None, None, 0.0, 0.0).unwrap();
            let path = "/tmp/test_py_trajectory.json";
            recorder.flush(path).unwrap();
            assert!(std::fs::exists(path).unwrap_or(false));
            std::fs::remove_file(path).ok();
        });
    }

    #[test]
    fn trajectory_recorder_trajectory_returns_dict() {
        run(|py| {
            let recorder = PyTrajectoryRecorder::new(42, "BTC-USDT", "mock", "test");
            recorder.record(0, 1234567890, "bar 0", None, None, 0.0, 0.0).unwrap();
            let traj = recorder.trajectory(py).unwrap();
            let version: String = traj.get_item("version").unwrap().unwrap().extract().unwrap();
            assert_eq!(version, "0.10.0");
            let bars = traj.get_item("bars").unwrap().unwrap();
            let l = bars.cast::<PyList>().unwrap();
            assert_eq!(l.len(), 1);
        });
    }
}
