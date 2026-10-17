//! StrategyManager：策略注册 / BacktestRunner 工厂 / 生命周期管理
//!
//! 单例（OnceLock + Mutex），供 Python 层通过 axon-python 绑定调用。

use std::sync::Arc;

use tokio::sync::Mutex;

use axon_strategy::trait_::Strategy;

// ponytail: std::sync::LazyLock 在 Rust 1.80+ 稳定，省掉 once_cell 依赖。
static REGISTRY: std::sync::LazyLock<Mutex<Vec<Arc<dyn Strategy>>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

/// StrategyManager 公开 API
pub struct StrategyManager;

impl StrategyManager {
    pub async fn register(s: Arc<dyn Strategy>) {
        let mut r = REGISTRY.lock().await;
        r.push(s);
    }

    pub async fn list() -> Vec<(String, String)> {
        let r = REGISTRY.lock().await;
        r.iter()
            .map(|s| (s.name().into(), s.symbol().into()))
            .collect()
    }

    pub async fn clear() {
        let mut r = REGISTRY.lock().await;
        r.clear();
    }
}
