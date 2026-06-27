//! MLflow Tracker（使用 reqwest 阻塞 HTTP 客户端）

#![cfg(feature = "http")]

use std::path::Path;
use std::sync::Mutex;

use reqwest::blocking::Client;
use serde_json::json;
use url::Url;

use crate::config::MetricBuffer;
use crate::error::TrackerError;
use crate::retry::RetryPolicy;
use crate::tracker::ExperimentTracker;
use crate::types::{ExperimentId, ImageFormat, ParamValue, RunId, RunStatus};

/// MLflow Tracker
pub struct MlflowTracker {
    client: Client,
    tracking_uri: Url,
    run_id: RunId,
    _experiment_id: ExperimentId,
    metric_buffer: Mutex<MetricBuffer>,
    retry: RetryPolicy,
}

impl MlflowTracker {
    /// 创建 MLflow tracker
    pub fn new(tracking_uri: Url, experiment_name: &str) -> Result<Self, TrackerError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| TrackerError::Network(e.to_string()))?;
        let experiment_id =
            Self::get_or_create_experiment(&client, &tracking_uri, experiment_name)?;
        let run_id = Self::create_run(&client, &tracking_uri, &experiment_id)?;
        Ok(Self {
            client,
            tracking_uri,
            run_id: RunId(run_id),
            _experiment_id: ExperimentId(experiment_id),
            metric_buffer: Mutex::new(MetricBuffer::new(1000, std::time::Duration::from_secs(30))),
            retry: RetryPolicy::default(),
        })
    }

    /// 构造函数（带自定义 retry）
    pub fn with_retry(
        tracking_uri: Url,
        experiment_name: &str,
        retry: RetryPolicy,
    ) -> Result<Self, TrackerError> {
        let mut s = Self::new(tracking_uri, experiment_name)?;
        s.retry = retry;
        Ok(s)
    }

    fn get_or_create_experiment(
        client: &Client,
        base_uri: &Url,
        name: &str,
    ) -> Result<String, TrackerError> {
        let url = base_uri
            .join("api/2.0/mlflow/experiments/search")
            .map_err(|e| TrackerError::Parse(e.to_string()))?;
        let resp = client
            .get(url)
            .query(&[("filter", format!("name = '{name}'"))])
            .send()
            .map_err(|e| TrackerError::Network(e.to_string()))?;
        let parsed: serde_json::Value = resp
            .json()
            .map_err(|e| TrackerError::Parse(e.to_string()))?;
        if let Some(experiments) = parsed["experiments"].as_array() {
            if let Some(exp) = experiments.first() {
                if let Some(id) = exp["experiment_id"].as_str() {
                    return Ok(id.to_string());
                }
            }
        }
        let url = base_uri
            .join("api/2.0/mlflow/experiments/create")
            .map_err(|e| TrackerError::Parse(e.to_string()))?;
        let resp = client
            .post(url)
            .json(&json!({ "name": name }))
            .send()
            .map_err(|e| TrackerError::Network(e.to_string()))?;
        let parsed: serde_json::Value = resp
            .json()
            .map_err(|e| TrackerError::Parse(e.to_string()))?;
        Ok(parsed["experiment_id"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    fn create_run(
        client: &Client,
        base_uri: &Url,
        experiment_id: &str,
    ) -> Result<String, TrackerError> {
        let url = base_uri
            .join("api/2.0/mlflow/runs/create")
            .map_err(|e| TrackerError::Parse(e.to_string()))?;
        let resp = client
            .post(url)
            .json(&json!({
                "experiment_id": experiment_id,
                "run_name": format!("run_{}", chrono::Utc::now().timestamp()),
            }))
            .send()
            .map_err(|e| TrackerError::Network(e.to_string()))?;
        let parsed: serde_json::Value = resp
            .json()
            .map_err(|e| TrackerError::Parse(e.to_string()))?;
        Ok(parsed["run"]["info"]["run_id"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }
}

impl ExperimentTracker for MlflowTracker {
    fn log_param(&self, key: &str, value: &ParamValue) -> Result<(), TrackerError> {
        let url = self
            .tracking_uri
            .join("api/2.0/mlflow/runs/log-parameter")
            .map_err(|e| TrackerError::Parse(e.to_string()))?;
        self.retry.execute(|| {
            self.client
                .post(url.clone())
                .json(&json!({
                    "run_id": self.run_id.0,
                    "key": key,
                    "value": value.to_string(),
                }))
                .send()
                .map_err(|e| TrackerError::Network(e.to_string()))?;
            Ok(())
        })
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
        let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let num_bins = 30.min(values.len());
        let step_size = if num_bins > 0 {
            (max - min) / num_bins as f64
        } else {
            1.0
        };
        let bins: Vec<f64> = (0..=num_bins).map(|i| min + i as f64 * step_size).collect();
        let entry = crate::types::MetricEntry {
            key: key.to_string(),
            value: crate::types::MetricValue::Histogram {
                values: values.to_vec(),
                bins,
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
        let url = self
            .tracking_uri
            .join("api/2.0/mlflow/runs/log-image")
            .map_err(|e| TrackerError::Parse(e.to_string()))?;
        let ext = match format {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Svg => "svg",
        };
        self.retry.execute(|| {
            let form = reqwest::blocking::multipart::Form::new()
                .text("run_id", self.run_id.0.clone())
                .text("key", key.to_string())
                .text("step", step.to_string())
                .part(
                    "image",
                    reqwest::blocking::multipart::Part::bytes(image.to_vec())
                        .file_name(format!("img_{step}.{ext}")),
                );
            self.client
                .post(url.clone())
                .multipart(form)
                .send()
                .map_err(|e| TrackerError::Network(e.to_string()))?;
            Ok(())
        })
    }

    fn log_artifact(&self, name: &str, path: &Path) -> Result<(), TrackerError> {
        let url = self
            .tracking_uri
            .join("api/2.0/mlflow/runs/log-artifact")
            .map_err(|e| TrackerError::Parse(e.to_string()))?;
        let file_bytes =
            std::fs::read(path).map_err(|e| TrackerError::Io(format!("{path:?}: {e}")))?;
        self.retry.execute(|| {
            let form = reqwest::blocking::multipart::Form::new()
                .text("run_id", self.run_id.0.clone())
                .text("path", name.to_string())
                .part(
                    "artifact",
                    reqwest::blocking::multipart::Part::bytes(file_bytes.clone())
                        .file_name(name.to_string()),
                );
            self.client
                .post(url.clone())
                .multipart(form)
                .send()
                .map_err(|e| TrackerError::Network(e.to_string()))?;
            Ok(())
        })
    }

    fn set_tag(&self, key: &str, value: &str) -> Result<(), TrackerError> {
        let url = self
            .tracking_uri
            .join("api/2.0/mlflow/runs/set-tag")
            .map_err(|e| TrackerError::Parse(e.to_string()))?;
        self.retry.execute(|| {
            self.client
                .post(url.clone())
                .json(&json!({
                    "run_id": self.run_id.0,
                    "key": key,
                    "value": value,
                }))
                .send()
                .map_err(|e| TrackerError::Network(e.to_string()))?;
            Ok(())
        })
    }

    fn finish(&self, status: RunStatus) -> Result<(), TrackerError> {
        self.flush()?;
        let url = self
            .tracking_uri
            .join("api/2.0/mlflow/runs/update")
            .map_err(|e| TrackerError::Parse(e.to_string()))?;
        self.retry.execute(|| {
            self.client
                .post(url.clone())
                .json(&json!({
                    "run_id": self.run_id.0,
                    "status": status.as_mlflow_str(),
                    "end_time": chrono::Utc::now().timestamp_millis() as u64,
                }))
                .send()
                .map_err(|e| TrackerError::Network(e.to_string()))?;
            Ok(())
        })
    }

    fn flush(&self) -> Result<(), TrackerError> {
        let entries = {
            let mut buffer = self.metric_buffer.lock().unwrap();
            buffer.drain()
        };
        if entries.is_empty() {
            return Ok(());
        }
        let url = self
            .tracking_uri
            .join("api/2.0/mlflow/runs/log-batch")
            .map_err(|e| TrackerError::Parse(e.to_string()))?;
        let metrics: Vec<_> = entries
            .iter()
            .filter_map(|e| {
                if let crate::types::MetricValue::Scalar(v) = &e.value {
                    Some(json!({
                        "key": e.key,
                        "value": v,
                        "step": e.step,
                    }))
                } else {
                    None
                }
            })
            .collect();
        if !metrics.is_empty() {
            self.retry.execute(|| {
                self.client
                    .post(url.clone())
                    .json(&json!({
                        "run_id": self.run_id.0,
                        "metrics": metrics,
                    }))
                    .send()
                    .map_err(|e| TrackerError::Network(e.to_string()))?;
                Ok(())
            })?;
        }
        Ok(())
    }
}
