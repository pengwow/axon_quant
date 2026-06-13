use std::sync::Arc;

use parking_lot::RwLock;
use rayon::prelude::*;
use tokio::sync::mpsc;

use crate::engine::InferenceEngine;
use crate::error::{Action, BatchConfig, InferenceStats, Observation};

pub struct BatchInferencePipeline {
    stats: Arc<RwLock<InferenceStats>>,
}

impl BatchInferencePipeline {
    pub fn new(
        backend: Arc<RwLock<dyn InferenceEngine>>,
        batch_config: BatchConfig,
    ) -> (Self, mpsc::Sender<Observation>, mpsc::Receiver<Vec<Action>>) {
        let (obs_tx, obs_rx) = mpsc::channel(batch_config.prealloc_buffer_size);
        let (action_tx, action_rx) = mpsc::channel(batch_config.prealloc_buffer_size);
        let stats = Arc::new(RwLock::new(InferenceStats::default()));

        let stats_clone = stats.clone();
        tokio::spawn(Self::batch_loop(
            obs_rx,
            action_tx,
            backend,
            batch_config,
            stats_clone,
        ));

        let pipeline = Self { stats };

        (pipeline, obs_tx, action_rx)
    }

    async fn batch_loop(
        mut obs_rx: mpsc::Receiver<Observation>,
        action_tx: mpsc::Sender<Vec<Action>>,
        backend: Arc<RwLock<dyn InferenceEngine>>,
        config: BatchConfig,
        stats: Arc<RwLock<InferenceStats>>,
    ) {
        let mut buffer: Vec<Observation> = Vec::with_capacity(config.max_batch_size);
        let collect_duration = std::time::Duration::from_micros(config.collect_timeout_us);

        loop {
            if buffer.is_empty() {
                match tokio::time::timeout(collect_duration, obs_rx.recv()).await {
                    Ok(Some(obs)) => buffer.push(obs),
                    Ok(None) => break,
                    Err(_) => continue,
                }
            }

            while buffer.len() < config.max_batch_size {
                match obs_rx.try_recv() {
                    Ok(obs) => buffer.push(obs),
                    Err(_) => break,
                }
            }

            let start = std::time::Instant::now();
            let observations = std::mem::take(&mut buffer);

            let actions = {
                let backend_guard = backend.read();
                let results: Result<Vec<_>, _> = observations
                    .par_iter()
                    .map(|obs| backend_guard.infer(obs))
                    .collect();
                results.unwrap_or_default()
            };

            let elapsed_us = start.elapsed().as_micros() as u64;

            {
                let mut s = stats.write();
                s.total_batch_inferences += 1;
                s.total_inferences += actions.len() as u64;
                let n = s.total_batch_inferences as f64;
                s.avg_latency_us = (s.avg_latency_us * (n - 1.0) + elapsed_us as f64) / n;
            }

            if action_tx.send(actions).await.is_err() {
                break;
            }
        }
    }

    pub fn stats(&self) -> InferenceStats {
        self.stats.read().clone()
    }
}
