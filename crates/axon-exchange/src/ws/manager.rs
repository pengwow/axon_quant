use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, watch};

use crate::types::{ReconnectConfig, WsMessage};

pub struct WebSocketManager {
    connected: Arc<AtomicBool>,
    consecutive_failures: Arc<AtomicU32>,
    circuit_open: Arc<AtomicBool>,
    config: ReconnectConfig,
    _data_tx: mpsc::Sender<WsMessage>,
    reconnect_tx: watch::Sender<bool>,
}

impl WebSocketManager {
    pub fn new(
        config: ReconnectConfig,
        data_tx: mpsc::Sender<WsMessage>,
    ) -> (Self, watch::Receiver<bool>) {
        let (reconnect_tx, reconnect_rx) = watch::channel(false);
        (
            Self {
                connected: Arc::new(AtomicBool::new(false)),
                consecutive_failures: Arc::new(AtomicU32::new(0)),
                circuit_open: Arc::new(AtomicBool::new(false)),
                config,
                _data_tx: data_tx,
                reconnect_tx,
            },
            reconnect_rx,
        )
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn is_circuit_open(&self) -> bool {
        self.circuit_open.load(Ordering::Relaxed)
    }

    pub fn on_connect_success(&self) {
        self.connected.store(true, Ordering::Relaxed);
        self.consecutive_failures.store(0, Ordering::Relaxed);
        self.circuit_open.store(false, Ordering::Relaxed);
        let _ = self.reconnect_tx.send(true);
    }

    pub fn on_connect_failure(&self) {
        self.connected.store(false, Ordering::Relaxed);
        let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        if failures >= self.config.circuit_breaker_threshold {
            self.circuit_open.store(true, Ordering::Relaxed);
        }
    }

    pub fn calculate_backoff(&self, attempt: u32) -> Duration {
        let base = self.config.initial_backoff.as_secs_f64();
        let max = self.config.max_backoff.as_secs_f64();
        let multiplier = self.config.backoff_multiplier;
        let backoff = (base * multiplier.powi(attempt as i32 - 1)).min(max);
        Duration::from_secs_f64(backoff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_reconnect_config() -> ReconnectConfig {
        ReconnectConfig {
            max_retries: 10,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            backoff_multiplier: 2.0,
            circuit_breaker_threshold: 5,
            circuit_breaker_reset: Duration::from_secs(60),
        }
    }

    #[test]
    fn test_initial_state() {
        let (tx, _rx) = mpsc::channel(10);
        let (manager, _) = WebSocketManager::new(default_reconnect_config(), tx);
        assert!(!manager.is_connected());
        assert!(!manager.is_circuit_open());
    }

    #[test]
    fn test_connect_success() {
        let (tx, _rx) = mpsc::channel(10);
        let (manager, _) = WebSocketManager::new(default_reconnect_config(), tx);
        manager.on_connect_success();
        assert!(manager.is_connected());
    }

    #[test]
    fn test_circuit_breaker_triggers() {
        let (tx, _rx) = mpsc::channel(10);
        let config = ReconnectConfig {
            circuit_breaker_threshold: 2,
            ..default_reconnect_config()
        };
        let (manager, _) = WebSocketManager::new(config, tx);
        manager.on_connect_failure();
        assert!(!manager.is_circuit_open());
        manager.on_connect_failure();
        assert!(manager.is_circuit_open());
    }

    #[test]
    fn test_backoff_calculation() {
        let (tx, _rx) = mpsc::channel(10);
        let (manager, _) = WebSocketManager::new(default_reconnect_config(), tx);
        let backoff1 = manager.calculate_backoff(1);
        let backoff2 = manager.calculate_backoff(2);
        assert!(backoff2 > backoff1);
    }
}
