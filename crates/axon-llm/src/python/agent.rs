#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::useless_conversion)]

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};
use std::sync::{Arc, Mutex};

use crate::agent::AgentConfig;
use crate::backend::{LLMBackend, LLMError, ToolDefinition};
use crate::react_agent::ReActAgent;
use crate::tools::Tool;
use crate::types::{LLMResponse, Message, TokenUsage};

use super::helpers::pythonize;

struct PyLLMBackendAdapter {
    py_backend: Arc<Mutex<Py<PyAny>>>,
}

#[async_trait::async_trait]
impl LLMBackend for PyLLMBackendAdapter {
    async fn complete(&self, messages: &[Message]) -> Result<LLMResponse, LLMError> {
        let py_backend = self.py_backend.clone();

        Python::try_attach(|py| {
            let py_backend_obj = py_backend.lock().unwrap();

            let py_msgs = PyList::empty(py);
            for m in messages {
                let msg_dict = PyDict::new(py);
                let role_str = match m.role {
                    crate::types::Role::System => "system",
                    crate::types::Role::User => "user",
                    crate::types::Role::Assistant => "assistant",
                    crate::types::Role::Tool => "tool",
                };
                msg_dict
                    .set_item("role", role_str)
                    .map_err(|e| LLMError::Backend(e.to_string()))?;
                msg_dict
                    .set_item("content", &m.content)
                    .map_err(|e| LLMError::Backend(e.to_string()))?;
                if let Some(id) = &m.tool_call_id {
                    msg_dict
                        .set_item("tool_call_id", id)
                        .map_err(|e| LLMError::Backend(e.to_string()))?;
                }
                py_msgs
                    .append(msg_dict)
                    .map_err(|e| LLMError::Backend(e.to_string()))?;
            }

            let args =
                PyTuple::new(py, &[py_msgs]).map_err(|e| LLMError::Backend(e.to_string()))?;
            let resp = py_backend_obj
                .call_method(py, "chat", args, None)
                .map_err(|e| LLMError::Backend(e.to_string()))?;

            let content: String = resp
                .getattr(py, "content")
                .map_err(|e| LLMError::Backend(e.to_string()))?
                .extract::<String>(py)
                .map_err(|e| LLMError::Backend(e.to_string()))?;
            let prompt_tokens: usize = resp
                .getattr(py, "prompt_tokens")
                .map_err(|e| LLMError::Backend(e.to_string()))?
                .extract::<usize>(py)
                .map_err(|e| LLMError::Backend(e.to_string()))?;
            let completion_tokens: usize = resp
                .getattr(py, "completion_tokens")
                .map_err(|e| LLMError::Backend(e.to_string()))?
                .extract::<usize>(py)
                .map_err(|e| LLMError::Backend(e.to_string()))?;
            let total_tokens: usize = resp
                .getattr(py, "total_tokens")
                .map_err(|e| LLMError::Backend(e.to_string()))?
                .extract::<usize>(py)
                .map_err(|e| LLMError::Backend(e.to_string()))?;

            Ok(LLMResponse {
                content: Some(content),
                tool_calls: None,
                token_usage: TokenUsage {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                },
                finish_reason: crate::types::FinishReason::Stop,
            })
        })
        .ok_or_else(|| LLMError::Backend("Failed to attach to Python interpreter".to_string()))?
    }

    async fn complete_with_tools(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
    ) -> Result<LLMResponse, LLMError> {
        self.complete(messages).await
    }

    fn context_window_size(&self) -> usize {
        8192
    }
}

#[pyclass(name = "ReActAgent")]
pub struct PyReActAgent {
    inner: ReActAgent,
}

#[pymethods]
impl PyReActAgent {
    #[new]
    #[pyo3(signature = (backend, config=None))]
    fn new(
        py: Python<'_>,
        backend: &Bound<'_, PyAny>,
        config: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Self> {
        let py_backend = backend.clone().unbind().into();

        let adapter = Box::new(PyLLMBackendAdapter {
            py_backend: Arc::new(Mutex::new(py_backend)),
        });

        let agent_config = match config {
            Some(d) => {
                let json_value = pythonize(py, d.as_any())?;
                let map: std::collections::HashMap<String, serde_json::Value> = match json_value {
                    serde_json::Value::Object(m) => m.into_iter().collect(),
                    other => {
                        return Err(pyo3::exceptions::PyValueError::new_err(format!(
                            "config must be a dict, got {}",
                            other
                        )));
                    }
                };

                let max_iterations = map
                    .get("max_iterations")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize)
                    .unwrap_or(10);
                let temperature = map
                    .get("temperature")
                    .and_then(|v| v.as_f64())
                    .map(|f| f as f32)
                    .unwrap_or(0.1);
                let max_context_tokens = map
                    .get("max_context_tokens")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize)
                    .unwrap_or(8192);
                let enable_reflection = map
                    .get("enable_reflection")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let allowed_tools: Vec<String> = map
                    .get("allowed_tools")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();

                AgentConfig {
                    max_iterations,
                    temperature,
                    max_context_tokens,
                    enable_reflection,
                    allowed_tools,
                }
            }
            None => AgentConfig::default(),
        };

        Ok(Self {
            inner: ReActAgent::new(adapter, agent_config),
        })
    }

    fn add_tool(&mut self, tool: &Bound<'_, PyAny>) -> PyResult<()> {
        let name: String = tool.getattr("name")?.extract()?;
        let description: String = tool.getattr("description")?.extract()?;
        let parameters_schema_str: String = tool.getattr("parameters_schema")?.extract()?;
        let parameters_schema: serde_json::Value =
            serde_json::from_str(&parameters_schema_str).unwrap_or_default();

        let py_tool = PyToolWrapper {
            name,
            description,
            parameters_schema,
            py_tool: Arc::new(Mutex::new(tool.clone().unbind().into())),
        };

        self.inner.add_tool(Box::new(py_tool));
        Ok(())
    }

    fn reason<'py>(&mut self, py: Python<'py>, query: &str) -> PyResult<Bound<'py, PyDict>> {
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;

        let response = runtime
            .block_on(self.inner.reason(query))
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;

        let dict = PyDict::new(py);
        dict.set_item("answer", response.answer)?;
        dict.set_item("iterations", response.iterations)?;

        let token_usage_dict = PyDict::new(py);
        token_usage_dict.set_item("prompt_tokens", response.token_usage.prompt_tokens)?;
        token_usage_dict.set_item("completion_tokens", response.token_usage.completion_tokens)?;
        token_usage_dict.set_item("total_tokens", response.token_usage.total_tokens)?;
        dict.set_item("token_usage", token_usage_dict)?;

        let reasoning_trace = PyList::empty(py);
        for step in response.reasoning_trace {
            let step_dict = PyDict::new(py);
            step_dict.set_item("step", step.step)?;
            step_dict.set_item("thought", step.thought)?;
            if let Some(action) = step.action {
                let action_dict = PyDict::new(py);
                action_dict.set_item("id", action.id)?;
                action_dict.set_item("function_name", action.function_name)?;
                action_dict.set_item("arguments", action.arguments)?;
                step_dict.set_item("action", action_dict)?;
            }
            if let Some(observation) = step.observation {
                step_dict.set_item("observation", observation)?;
            }
            reasoning_trace.append(step_dict)?;
        }
        dict.set_item("reasoning_trace", reasoning_trace)?;

        Ok(dict)
    }

    fn __repr__(&self) -> String {
        "ReActAgent".to_string()
    }
}

struct PyToolWrapper {
    name: String,
    description: String,
    parameters_schema: serde_json::Value,
    py_tool: Arc<Mutex<Py<PyAny>>>,
}

#[async_trait::async_trait]
impl Tool for PyToolWrapper {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.parameters_schema.clone()
    }

    async fn execute(&self, arguments: &str) -> Result<String, crate::tools::ToolError> {
        let py_tool = self.py_tool.clone();

        let py_result = Python::try_attach(|py| {
            let py_tool_obj = py_tool.lock().unwrap();
            let args = PyTuple::new(py, &[arguments]).map_err(|e| {
                crate::tools::ToolError::ExecutionFailed(format!(
                    "Python tool execution failed: {}",
                    e
                ))
            })?;
            let result = py_tool_obj
                .call_method(py, "execute", args, None)
                .map_err(|e| {
                    crate::tools::ToolError::ExecutionFailed(format!(
                        "Python tool execution failed: {}",
                        e
                    ))
                })?;
            Ok(result.extract::<String>(py).map_err(|e| {
                crate::tools::ToolError::ExecutionFailed(format!(
                    "Python tool result extraction failed: {}",
                    e
                ))
            })?)
        })
        .ok_or_else(|| {
            crate::tools::ToolError::ExecutionFailed(
                "Failed to attach to Python interpreter".to_string(),
            )
        })?;

        py_result
    }
}

#[pyclass(name = "Tool", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyTool {
    #[pyo3(get)]
    pub name: String,
    #[pyo3(get)]
    pub description: String,
    #[pyo3(get)]
    pub parameters_schema: String,
}

#[pymethods]
impl PyTool {
    #[new]
    fn new(name: String, description: String, parameters_schema: String) -> Self {
        Self {
            name,
            description,
            parameters_schema,
        }
    }

    fn execute(&self, _arguments: &str) -> PyResult<String> {
        Err(pyo3::exceptions::PyNotImplementedError::new_err(
            "Base Tool class cannot be executed",
        ))
    }

    fn __repr__(&self) -> String {
        format!("Tool(name={})", self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::MockBackend;

    #[tokio::test]
    async fn py_react_agent_with_mock_backend() {
        let mock = MockBackend::text_only("Hello, world!");
        let config = AgentConfig::default();
        let mut agent = ReActAgent::new(Box::new(mock), config);
        let response = agent.reason("Say hello").await.unwrap();
        assert!(response.answer.contains("Hello"));
    }
}
