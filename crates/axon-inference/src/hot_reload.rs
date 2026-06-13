use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;
use tokio::sync::watch;

use crate::engine::InferenceEngine;
use crate::error::{InferenceError, ModelConfig};

pub struct ModelHotReloader {
    version_tx: watch::Sender<u64>,
    version_rx: watch::Receiver<u64>,
    backend: Arc<RwLock<dyn InferenceEngine>>,
    config: ModelConfig,
    current_version: AtomicU64,
}

impl ModelHotReloader {
    pub fn new(backend: Arc<RwLock<dyn InferenceEngine>>, config: ModelConfig) -> Self {
        let (tx, rx) = watch::channel(0u64);
        Self {
            version_tx: tx,
            version_rx: rx,
            backend,
            config,
            current_version: AtomicU64::new(0),
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.version_rx.clone()
    }

    pub fn version(&self) -> u64 {
        self.current_version.load(Ordering::Relaxed)
    }

    pub async fn reload(&self) -> Result<u64, InferenceError> {
        let path = &self.config.path;

        if !path.exists() {
            return Err(InferenceError::ModelNotFound { path: path.clone() });
        }

        let new_checksum = compute_sha256(path)?;

        {
            let mut backend = self.backend.write();
            backend.load(path)?;
        }

        let v = self.current_version.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.version_tx.send(v);

        tracing::info!(
            model_path = %path.display(),
            version = v,
            checksum = %new_checksum,
            "model reloaded"
        );

        Ok(v)
    }

    #[cfg(feature = "hot-reload")]
    pub fn spawn_watcher(&self) -> Result<tokio::task::JoinHandle<()>, InferenceError> {
        use notify::{RecommendedWatcher, RecursiveMode, Watcher};
        use tokio::sync::mpsc;

        let (fs_tx, mut fs_rx) = mpsc::channel::<()>(4);

        let watch_path = self
            .config
            .path
            .parent()
            .unwrap_or(&self.config.path)
            .to_path_buf();

        let mut watcher: RecommendedWatcher = Watcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    match event.kind {
                        notify::EventKind::Modify(_) | notify::EventKind::Create(_) => {
                            let _ = fs_tx.blocking_send(());
                        }
                        _ => {}
                    }
                }
            },
            notify::Config::default(),
        )
        .map_err(|e| InferenceError::HotReloadFailed {
            reason: e.to_string(),
        })?;

        watcher
            .watch(&watch_path, RecursiveMode::Recursive)
            .map_err(|e| InferenceError::HotReloadFailed {
                reason: e.to_string(),
            })?;

        let backend = self.backend.clone();
        let config = self.config.clone();
        let version_tx = self.version_tx.clone();
        let current_version = AtomicU64::new(self.current_version.load(Ordering::Relaxed));

        let handle = tokio::spawn(async move {
            while fs_rx.recv().await.is_some() {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                while fs_rx.try_recv().is_ok() {}

                match Self::try_reload_static(&backend, &config).await {
                    Ok(checksum) => {
                        let v = current_version.fetch_add(1, Ordering::SeqCst) + 1;
                        let _ = version_tx.send(v);
                        tracing::info!(
                            model_path = %config.path.display(),
                            version = v,
                            checksum = %checksum,
                            "model hot reloaded"
                        );
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "hot reload failed, keeping current version");
                    }
                }
            }
        });

        Ok(handle)
    }

    async fn try_reload_static(
        backend: &Arc<RwLock<dyn InferenceEngine>>,
        config: &ModelConfig,
    ) -> Result<String, InferenceError> {
        let checksum = compute_sha256(&config.path)?;
        let mut guard = backend.write();
        guard.load(&config.path)?;
        Ok(checksum)
    }
}

fn compute_sha256(path: &std::path::Path) -> Result<String, InferenceError> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).map_err(|e| InferenceError::ModelLoadFailed {
        reason: e.to_string(),
    })?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}
