//! Python 适配器：将 Python callable 适配为 Rust `Policy` trait。

use pyo3::prelude::*;

use crate::traits::Policy;
use crate::types::{Action, ActionType, ModelType, Observation};

/// Python 可调用对象适配 `Policy` trait
pub struct PyPolicy {
    callable: Py<PyAny>,
    name: String,
    model_type: ModelType,
}

impl PyPolicy {
    /// 创建新的适配器
    pub fn new(callable: Py<PyAny>, name: String, model_type: ModelType) -> Self {
        Self {
            callable,
            name,
            model_type,
        }
    }
}

impl Policy for PyPolicy {
    fn predict(&self, observation: &Observation) -> Action {
        Python::attach(|py| {
            // 将 Observation 转为 Python dict
            let obs_dict = match observation_to_py(py, observation) {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!("Failed to convert observation: {}", e);
                    return Action {
                        action_type: ActionType::Hold,
                        symbol: None,
                        quantity: None,
                        confidence: 0.0,
                    };
                }
            };

            // 调用 Python callable
            match self.callable.call1(py, (obs_dict,)) {
                Ok(result) => {
                    // 尝试从返回值提取 Action
                    if let Ok(dict) = result.cast_bound::<pyo3::types::PyDict>(py) {
                        let action_type = dict
                            .get_item("action_type")
                            .ok()
                            .flatten()
                            .and_then(|v| v.extract::<String>().ok())
                            .map(|s| match s.as_str() {
                                "buy" | "Buy" => ActionType::Buy,
                                "sell" | "Sell" => ActionType::Sell,
                                _ => ActionType::Hold,
                            })
                            .unwrap_or(ActionType::Hold);

                        let symbol = dict
                            .get_item("symbol")
                            .ok()
                            .flatten()
                            .and_then(|v| v.extract::<String>().ok());

                        let quantity = dict
                            .get_item("quantity")
                            .ok()
                            .flatten()
                            .and_then(|v| v.extract::<f64>().ok());

                        let confidence = dict
                            .get_item("confidence")
                            .ok()
                            .flatten()
                            .and_then(|v| v.extract::<f64>().ok())
                            .unwrap_or(0.5);

                        Action {
                            action_type,
                            symbol,
                            quantity,
                            confidence,
                        }
                    } else {
                        Action {
                            action_type: ActionType::Hold,
                            symbol: None,
                            quantity: None,
                            confidence: 0.5,
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Python predict call failed: {}", e);
                    Action {
                        action_type: ActionType::Hold,
                        symbol: None,
                        quantity: None,
                        confidence: 0.0,
                    }
                }
            }
        })
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn model_type(&self) -> ModelType {
        self.model_type
    }
}

fn observation_to_py(py: Python, obs: &Observation) -> PyResult<Py<PyAny>> {
    let dict = pyo3::types::PyDict::new(py);
    dict.set_item("market_features", &obs.market_features)?;
    dict.set_item("technical_indicators", &obs.technical_indicators)?;
    dict.set_item("time_features", &obs.time_features)?;
    Ok(dict.into_any().unbind())
}
