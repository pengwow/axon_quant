//! 缓存运维句柄
//!
//! 由 [`DataService::cache_control`](crate::DataService::cache_control) 构造,
//! 与 DataService 共享同一 `DataServiceInner`(通过 `Arc`),
//! 提供 `clear_l1` / `clear_l2` / `resize_l1` 三个 LRU 缓存管理操作。
//!
//! # 设计动机
//!
//! 原 `DataService` 的缓存状态散落在 `DataService` 自身字段中,
//! 外部难以做运维操作(清缓存、调容量)。本模块将"运维句柄"
//! 与"业务服务"解耦,句柄可独立 clone / 跨线程持有,与 DataService
//! 共享同一 L1/L2 视图。
//!
//! # 线程安全
//!
//! `CacheControl` 持 `Arc<DataServiceInner>`,`DataServiceInner`
//! 内部用 `Mutex<LruCache>` 保护 L1 写,`Mutex<MmapCache>` 保护 L2 写,
//! 跨句柄的并发操作安全。

use std::num::NonZeroUsize;
use std::sync::Arc;

#[cfg(feature = "mmap-cache")]
use crate::error::DataResult;
use crate::service::DataServiceInner;

/// 缓存运维句柄
///
/// 持 `DataServiceInner` 的 Arc 引用,与 `DataService` 共享同一缓存状态。
/// `Clone` 仅复制 Arc 指针(无深拷贝)。
#[derive(Clone)]
pub struct CacheControl {
    pub(crate) inner: Arc<DataServiceInner>,
}

impl CacheControl {
    /// 由 `DataService::cache_control()` 构造(包内可见)
    pub(crate) fn new(inner: Arc<DataServiceInner>) -> Self {
        Self { inner }
    }

    /// 清空 L1 LRU 缓存
    ///
    /// 持有引用计数归零的 dataset 将被立即释放(若 L1 是唯一引用)。
    /// L2 mmap 缓存不受影响。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use axon_data::{DataService, DataRequest, Frequency};
    /// use axon_data::sources::MockSource;
    /// use chrono::Utc;
    /// use axon_core::market::{Side, Tick};
    /// use axon_core::time::Timestamp;
    /// use axon_core::types::{Price, Quantity};
    ///
    /// # async fn run() {
    /// let svc = DataService::new()
    ///     .register_source(Box::new(MockSource::with_rows("mock", vec![
    ///         Tick::new(Timestamp::from_nanos(0), Price::from_f64(1.0), Quantity::from(1.0), Side::Buy),
    ///     ])));
    /// let req = DataRequest::new("X", Utc::now(), Utc::now(), Frequency::Tick);
    /// svc.load(&req).await.unwrap(); // 触发 L1 写入
    /// assert!(svc.cache_stats().len > 0);
    /// svc.cache_control().clear_l1();
    /// assert_eq!(svc.cache_stats().len, 0);
    /// # }
    /// ```
    pub fn clear_l1(&self) {
        self.inner
            .cache
            .lock()
            .expect("cache mutex poisoned")
            .clear();
    }

    /// 清空 L2 mmap 缓存(feature = "mmap-cache" 时可用)
    ///
    /// **运维操作**:建议在写入停止后调用。并发写入场景下未保护的数据
    /// 可能被一并清空。
    #[cfg(feature = "mmap-cache")]
    pub fn clear_l2(&self) -> DataResult<()> {
        use crate::error::DataError;
        if let Some(l2) = &self.inner.mmap_cache {
            l2.lock()
                .map_err(|_| DataError::Internal("L2 mutex poisoned".into()))?
                .clear()
                .map_err(|e| DataError::Internal(format!("L2 clear: {e}")))
        } else {
            // L2 未配置(`mmap-cache` feature 启用但实例未创建)
            Err(DataError::Internal("L2 cache not configured".into()))
        }
    }

    /// 调整 L1 LRU 容量
    ///
    /// 若新容量小于当前 L1 长度,会触发 LRU 淘汰直到 L1 长度 ≤ cap。
    /// L2 mmap 缓存不受影响。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::num::NonZeroUsize;
    /// use axon_data::DataService;
    ///
    /// let svc = DataService::new()
    ///     .with_cache_capacity(NonZeroUsize::new(8).unwrap());
    /// assert_eq!(svc.cache_stats().capacity, 8);
    /// ```
    pub fn resize_l1(&self, cap: NonZeroUsize) {
        self.inner
            .cache
            .lock()
            .expect("cache mutex poisoned")
            .resize(cap);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DataService;
    use crate::sources::MockSource;
    use crate::types::{DataRequest, Frequency};
    use axon_core::market::{Side, Tick};
    use axon_core::time::Timestamp;
    use axon_core::types::{Price, Quantity};
    use chrono::Utc;

    fn tick() -> Tick {
        Tick::new(
            Timestamp::from_nanos(0),
            Price::from_f64(1.0),
            Quantity::from(1.0),
            Side::Buy,
        )
    }

    #[tokio::test]
    async fn clear_l1_empties_cache() {
        let svc = DataService::new()
            .register_source(Box::new(MockSource::with_rows("mock", vec![tick()])));
        let req = DataRequest::new("X", Utc::now(), Utc::now(), Frequency::Tick);
        svc.load(&req).await.unwrap();
        assert!(svc.cache_stats().len > 0);

        svc.cache_control().clear_l1();
        assert_eq!(svc.cache_stats().len, 0);
    }

    #[tokio::test]
    async fn resize_l1_takes_effect() {
        let svc = DataService::new();
        svc.cache_control().resize_l1(NonZeroUsize::new(8).unwrap());
        let stats = svc.cache_stats();
        assert_eq!(stats.capacity, 8);
    }

    #[tokio::test]
    async fn clone_shares_underlying_state() {
        let svc = DataService::new()
            .register_source(Box::new(MockSource::with_rows("mock", vec![tick()])));
        let ctrl1 = svc.cache_control();
        let ctrl2 = ctrl1.clone();

        let req = DataRequest::new("X", Utc::now(), Utc::now(), Frequency::Tick);
        svc.load(&req).await.unwrap();
        assert!(svc.cache_stats().len > 0);

        // ctrl1 清 → ctrl2 可见
        ctrl1.clear_l1();
        assert_eq!(svc.cache_stats().len, 0);

        // ctrl2 也能清
        svc.load(&req).await.unwrap();
        assert!(svc.cache_stats().len > 0);
        ctrl2.clear_l1();
        assert_eq!(svc.cache_stats().len, 0);
    }
}
