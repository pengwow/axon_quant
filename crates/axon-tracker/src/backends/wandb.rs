//! WandB Tracker（GraphQL API 客户端）

#![cfg(feature = "http")]

use std::path::Path;
use std::sync::Mutex;

use base64::Engine;
use reqwest::blocking::Client;
use serde_json::json;
use url::Url;

use crate::config::MetricBuffer;
use crate::error::TrackerError;
use crate::retry::RetryPolicy;
use crate::tracker::ExperimentTracker;
use crate::types::{ImageFormat, ParamValue, RunStatus};

/// WandB Tracker
pub struct WandbTracker {
    client: Client,
    run_id: String,
    project: String,
    _entity: Option<String>,
    api_key: String,
    metric_buffer: Mutex<MetricBuffer>,
    retry: RetryPolicy,
}

impl WandbTracker {
    /// 创建 WandB tracker
    pub fn new(project: &str, entity: Option<&str>, api_key: &str) -> Result<Self, TrackerError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| TrackerError::Network(e.to_string()))?;

        let base_url = Url::parse("https://api.wandb.ai/graphql")
            .map_err(|e| TrackerError::Parse(e.to_string()))?;

        let init_resp = client
            .post(base_url)
            .bearer_auth(api_key)
            .json(&json!({
                "query": "mutation CreateRun($input: CreateRunInput!) { createRun(input: $input) { run { id displayName } } }",
                "variables": {
                    "input": {
                        "project": project,
                        "entity": entity,
                        "name": format!("train_{}", chrono::Utc::now().format("%Y%m%d_%H%M%S")),
                    }
                }
            }))
            .send()
            .map_err(|e| TrackerError::Network(e.to_string()))?;
        let parsed: serde_json::Value = init_resp
            .json()
            .map_err(|e| TrackerError::Parse(e.to_string()))?;
        let run_id = parsed["data"]["createRun"]["run"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        Ok(Self {
            client,
            run_id,
            project: project.to_string(),
            _entity: entity.map(String::from),
            api_key: api_key.to_string(),
            metric_buffer: Mutex::new(MetricBuffer::new(1000, std::time::Duration::from_secs(30))),
            retry: RetryPolicy::default(),
        })
    }

    fn log_to_wandb(&self, op: &str, variables: serde_json::Value) -> Result<(), TrackerError> {
        let url = Url::parse("https://api.wandb.ai/graphql")
            .map_err(|e| TrackerError::Parse(e.to_string()))?;
        self.retry.execute(|| {
            self.client
                .post(url.clone())
                .bearer_auth(&self.api_key)
                .json(&json!({
                    "query": format!("mutation Log($input: LogInput!) {{ {op}(input: $input) {{ success }} }}"),
                    "variables": { "input": variables.clone() }
                }))
                .send()
                .map_err(|e| TrackerError::Network(e.to_string()))?;
            Ok(())
        })
    }
}

impl ExperimentTracker for WandbTracker {
    fn log_param(&self, key: &str, value: &ParamValue) -> Result<(), TrackerError> {
        self.log_to_wandb(
            "logArtifacts",
            json!({
                "runId": self.run_id,
                "projectId": self.project,
                "data": [{ "key": key, "value": value.to_string(), "type": "config" }],
            }),
        )
    }

    fn log_params(&self, params: &[(String, ParamValue)]) -> Result<(), TrackerError> {
        for (k, v) in params {
            self.log_param(k, v)?;
        }
        Ok(())
    }

    fn log_metric(&self, key: &str, value: f64, step: usize) -> Result<(), TrackerError> {
        let entry = crate::types::MetricEntry {
            key: key.to_string(),
            value: crate::types::MetricValue::Scalar(value),
            step,
            timestamp: std::time::SystemTime::now(),
        };
        let mut buffer = self.metric_buffer.lock().unwrap();
        buffer.push(entry);
        if buffer.should_flush() {
            self.flush()?;
        }
        Ok(())
    }

    fn log_histogram(&self, key: &str, values: &[f64], step: usize) -> Result<(), TrackerError> {
        let entry = crate::types::MetricEntry {
            key: key.to_string(),
            value: crate::types::MetricValue::Histogram {
                values: values.to_vec(),
                bins: vec![],
            },
            step,
            timestamp: std::time::SystemTime::now(),
        };
        self.metric_buffer.lock().unwrap().push(entry);
        Ok(())
    }

    fn log_image(
        &self,
        key: &str,
        image: &[u8],
        format: ImageFormat,
        step: usize,
    ) -> Result<(), TrackerError> {
        let encoded = base64::engine::general_purpose::STANDARD.encode(image);
        self.log_to_wandb(
            "logMedia",
            json!({
                "runId": self.run_id,
                "projectId": self.project,
                "data": [{
                    "key": key,
                    "value": format!("data:image/{format:?};base64,{encoded}"),
                    "step": step,
                }],
            }),
        )
    }

    fn log_artifact(&self, name: &str, path: &Path) -> Result<(), TrackerError> {
        let file_bytes =
            std::fs::read(path).map_err(|e| TrackerError::Io(format!("{path:?}: {e}")))?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&file_bytes);
        self.log_to_wandb(
            "logArtifacts",
            json!({
                "runId": self.run_id,
                "projectId": self.project,
                "data": [{
                    "key": name,
                    "value": encoded,
                    "type": "file",
                }],
            }),
        )
    }

    fn set_tag(&self, key: &str, value: &str) -> Result<(), TrackerError> {
        self.log_to_wandb(
            "updateRun",
            json!({
                "runId": self.run_id,
                "projectId": self.project,
                "tags": { key: value },
            }),
        )
    }

    fn finish(&self, status: RunStatus) -> Result<(), TrackerError> {
        self.set_tag("status", status.as_mlflow_str())
    }

    fn flush(&self) -> Result<(), TrackerError> {
        let entries = {
            let mut buffer = self.metric_buffer.lock().unwrap();
            buffer.drain()
        };
        for entry in entries {
            if let crate::types::MetricValue::Scalar(v) = &entry.value {
                self.log_to_wandb(
                    "logMetrics",
                    json!({
                        "runId": self.run_id,
                        "projectId": self.project,
                        "data": [{ "key": entry.key, "value": v, "step": entry.step }],
                    }),
                )?;
            }
        }
        Ok(())
    }
}
