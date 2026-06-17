//! 数据服务统一入口
//!
//! 缓存策略:
//! - L1 `Mutex<LruCache>` 内存缓存(默认容量 64,builder 可调)
//! - L2 mmap 共享缓存(feature-gated: mmap-cache)
//!
//! 命中率:`AtomicU64` 计数,无锁并发安全
//!
//! # 内部结构
//!
//! 字段全部封装在 `DataServiceInner` 中,通过 `Arc` 在 `DataService` 与
//! `CacheControl` 之间共享,这样 `cache_control()` 句柄 clone 后
//! 仍能操作同一 L1/L2 缓存,无需引入额外锁。

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use arrow::record_batch::RecordBatch;
use futures::StreamExt;
use futures_core::Stream;
use lru::LruCache;
use std::pin::Pin;

use crate::dataset::Dataset;
use crate::error::{DataError, DataResult};
use crate::traits::DataSource;
use crate::types::DataRequest;

/// 内部共享状态(DataService 与 CacheControl 通过 Arc 共享)
pub(crate) struct DataServiceInner {
    /// 已注册数据源
    sources: Vec<Box<dyn DataSource>>,
    /// L1 LRU 缓存(`Mutex` 保护 LruCache 的内部可变性)
    pub(crate) cache: Mutex<LruCache<u64, Dataset>>,
    /// 缓存容量
    capacity: NonZeroUsize,
    /// 缓存命中次数
    hits: Arc<AtomicU64>,
    /// 缓存未命中次数
    misses: Arc<AtomicU64>,
    /// L2 mmap 共享缓存
    #[cfg(feature = "mmap-cache")]
    pub(crate) mmap_cache: Option<Mutex<crate::cache::MmapCache>>,
    /// L2 缓存命中次数
    #[cfg(feature = "mmap-cache")]
    mmap_hits: Arc<AtomicU64>,
}

/// 数据服务
#[derive(Clone)]
pub struct DataService {
    /// 共享内部状态(`CacheControl` 通过 `inner.clone()` 持同一引用)
    pub(crate) inner: Arc<DataServiceInner>,
}

/// 缓存统计快照
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    /// L1 命中次数
    pub hits: u64,
    /// L2 命中次数
    pub l2_hits: u64,
    /// 未命中次数
    pub misses: u64,
    /// L1 当前 entry 数
    pub len: usize,
    /// L1 容量上限
    pub capacity: usize,
    /// L2 当前使用量（字节）
    pub l2_size: usize,
    /// L2 容量上限（字节）
    pub l2_capacity: usize,
}

impl DataService {
    /// 构造空数据服务(默认 LRU 缓存容量 64)
    ///
    /// # Examples
    ///
    /// ```
    /// use axon_data::{DataService, DataRequest, Frequency};
    /// use axon_data::sources::MockSource;
    /// use chrono::Utc;
    ///
    /// let svc = DataService::new()
    ///     .register_source(Box::new(MockSource::empty()));
    /// let req = DataRequest::new("BTCUSDT", Utc::now(), Utc::now(), Frequency::Tick);
    /// let ds = futures::executor::block_on(svc.load(&req)).unwrap();
    /// assert_eq!(ds.len(), 0);
    /// ```
    pub fn new() -> Self {
        let cap = NonZeroUsize::new(64).expect("64 is non-zero");
        Self {
            inner: Arc::new(DataServiceInner {
                sources: Vec::new(),
                cache: Mutex::new(LruCache::new(cap)),
                capacity: cap,
                hits: Arc::new(AtomicU64::new(0)),
                misses: Arc::new(AtomicU64::new(0)),
                #[cfg(feature = "mmap-cache")]
                mmap_cache: None,
                #[cfg(feature = "mmap-cache")]
                mmap_hits: Arc::new(AtomicU64::new(0)),
            }),
        }
    }

    /// 注册数据源(builder 风格)
    ///
    /// # Examples
    ///
    /// ```
    /// use axon_data::DataService;
    /// use axon_data::sources::MockSource;
    ///
    /// let svc = DataService::new()
    ///     .register_source(Box::new(MockSource::empty()));
    /// assert_eq!(svc.find_source("mock").map(|s| s.name()), Some("mock"));
    /// ```
    pub fn register_source(mut self, source: Box<dyn DataSource>) -> Self {
        // 唯一所有者(builder 阶段未 clone)→ `get_mut` 一定成功
        let inner = Arc::get_mut(&mut self.inner)
            .expect("DataService::register_source requires unique ownership");
        inner.sources.push(source);
        self
    }

    /// 调整 LRU 容量(builder 风格,需在 `new` 后、`load` 前调用)
    ///
    /// # Examples
    ///
    /// ```
    /// use axon_data::DataService;
    /// use std::num::NonZeroUsize;
    ///
    /// let svc = DataService::new()
    ///     .with_cache_capacity(NonZeroUsize::new(128).unwrap());
    /// assert_eq!(svc.cache_stats().capacity, 128);
    /// ```
    pub fn with_cache_capacity(mut self, cap: NonZeroUsize) -> Self {
        let inner = Arc::get_mut(&mut self.inner)
            .expect("DataService::with_cache_capacity requires unique ownership");
        inner.capacity = cap;
        // 重建缓存以应用新容量(简单做法:新空 LRU 替换;旧 entries 丢弃)
        inner.cache = Mutex::new(LruCache::new(cap));
        self
    }

    /// 启用 L2 mmap 缓存(builder 风格)
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// use axon_data::DataService;
    /// use axon_data::cache::MmapCacheConfig;
    ///
    /// let svc = DataService::new()
    ///     .with_mmap_cache(MmapCacheConfig::new(1024 * 1024 * 100, "/tmp/axon_cache"))
    ///     .unwrap();
    /// ```
    #[cfg(feature = "mmap-cache")]
    pub fn with_mmap_cache(mut self, config: crate::cache::MmapCacheConfig) -> DataResult<Self> {
        let inner = Arc::get_mut(&mut self.inner)
            .expect("DataService::with_mmap_cache requires unique ownership");
        let cache = crate::cache::MmapCache::new(config)?;
        inner.mmap_cache = Some(Mutex::new(cache));
        inner.mmap_hits = Arc::new(AtomicU64::new(0));
        Ok(self)
    }

    /// 读取缓存统计
    pub fn cache_stats(&self) -> CacheStats {
        let cache = self.inner.cache.lock().expect("cache mutex poisoned");

        #[cfg(feature = "mmap-cache")]
        let (l2_size, l2_capacity, l2_hits) = if let Some(ref cache) = self.inner.mmap_cache {
            if let Ok(cache) = cache.lock() {
                (
                    cache.used(),
                    cache.capacity(),
                    self.inner.mmap_hits.load(Ordering::Relaxed),
                )
            } else {
                (0, 0, 0)
            }
        } else {
            (0, 0, 0)
        };

        #[cfg(not(feature = "mmap-cache"))]
        let (l2_size, l2_capacity, l2_hits) = (0, 0, 0);

        CacheStats {
            hits: self.inner.hits.load(Ordering::Relaxed),
            l2_hits,
            misses: self.inner.misses.load(Ordering::Relaxed),
            len: cache.len(),
            capacity: cache.cap().get(),
            l2_size,
            l2_capacity,
        }
    }

    /// 按名称查源
    pub fn find_source(&self, name: &str) -> Option<&dyn DataSource> {
        self.inner
            .sources
            .iter()
            .find(|s| s.name() == name)
            .map(|b| b.as_ref() as &dyn DataSource)
    }

    /// 缓存运维句柄
    ///
    /// 提供 `clear_l1` / `clear_l2` / `resize_l1` 三个管理操作。
    /// 句柄 clone 与 DataService 共享同一缓存状态(Arc),可见同一 L1/L2 视图。
    pub fn cache_control(&self) -> crate::cache::control::CacheControl {
        crate::cache::control::CacheControl::new(self.inner.clone())
    }

    /// 流式订阅:旁路缓存,直透源
    ///
    /// 行为:
    /// 1. 按 `source_name` 查找 `DataSource`(`DataError::SourceNotFound` 错误)
    /// 2. 调用 `source.stream(req).await` 直透
    /// 3. 返回 `Pin<Box<dyn Stream<Item = DataResult<RecordBatch>> + Send>>`
    ///
    /// **不写 L1/L2 缓存**:流式本质是"避免全量加载",写缓存会立即把流式优势抵消。
    /// 若 caller 需要缓存语义,先调 `load()` 拿整 Dataset,再 `dataset.into_iter()` 自行 iter。
    pub async fn stream(
        &self,
        source_name: &str,
        req: &DataRequest,
    ) -> DataResult<Pin<Box<dyn Stream<Item = DataResult<RecordBatch>> + Send>>> {
        // `&Box<dyn DataSource>` 先 deref → `&dyn DataSource`(因 `Box<dyn T>: Deref<Target = dyn T>`)
        // `&**s` 显式两次 deref:`&Box<...>` → `&dyn DataSource` → `&dyn DataSource`
        let source: &dyn DataSource = self
            .inner
            .sources
            .iter()
            .find(|s| s.name() == source_name)
            .ok_or_else(|| DataError::SourceNotFound(source_name.to_string()))?
            .as_ref();

        // 透传:stream 自身错误已编码在每个 Item 的 `DataResult` 中
        let upstream = source.stream(req).await?;
        // 用 futures::StreamExt::map 包装,统一签名
        let mapped = upstream.map(|item| item);
        Ok(Box::pin(mapped))
    }

    /// 按请求查询(优先 L1 → L2 → 数据源)
    pub async fn load(&self, req: &DataRequest) -> DataResult<Dataset> {
        let key = Self::cache_key(req);

        // 1) L1 cache lookup
        {
            let mut cache = self.inner.cache.lock().expect("cache mutex poisoned");
            if let Some(ds) = cache.get(&key) {
                self.inner.hits.fetch_add(1, Ordering::Relaxed);
                return Ok(ds.clone());
            }
        }

        // 2) L2 cache lookup (if enabled)
        #[cfg(feature = "mmap-cache")]
        if let Some(ref cache) = self.inner.mmap_cache
            && let Ok(mut cache) = cache.lock()
        {
            let l2_key = crate::cache::MmapCache::cache_key(
                req.source.as_deref().unwrap_or("unknown"),
                &req.symbol,
                req.frequency.as_str(),
            );
            if let Some(ds) = cache.get(&l2_key) {
                self.inner.mmap_hits.fetch_add(1, Ordering::Relaxed);
                // 写入 L1
                let mut l1_cache = self.inner.cache.lock().expect("cache mutex poisoned");
                l1_cache.put(key, ds.clone());
                return Ok(ds);
            }
        }

        self.inner.misses.fetch_add(1, Ordering::Relaxed);

        // 3) 选择数据源
        let source: &dyn DataSource = match &req.source {
            Some(name) => self
                .find_source(name)
                .ok_or_else(|| DataError::SourceNotFound(name.clone()))?,
            None => self
                .inner
                .sources
                .first()
                .map(|b| b.as_ref() as &dyn DataSource)
                .ok_or_else(|| DataError::SourceNotFound("<no source registered>".into()))?,
        };

        let dataset = source.query(req).await?;

        // 4) 写入 L1 cache(可能触发 LRU 淘汰)
        {
            let mut cache = self.inner.cache.lock().expect("cache mutex poisoned");
            cache.put(key, dataset.clone());
        }

        // 5) 写入 L2 cache (if enabled)
        #[cfg(feature = "mmap-cache")]
        if let Some(ref cache) = self.inner.mmap_cache
            && let Ok(mut cache) = cache.lock()
        {
            let l2_key = crate::cache::MmapCache::cache_key(
                req.source.as_deref().unwrap_or("unknown"),
                &req.symbol,
                req.frequency.as_str(),
            );
            let _ = cache.put(&l2_key, &dataset);
        }

        Ok(dataset)
    }

    fn cache_key(req: &DataRequest) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        req.hash(&mut h);
        h.finish()
    }
}

impl Default for DataService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::MockSource;
    use crate::types::Frequency;
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
    async fn load_with_no_source_returns_error() {
        let svc = DataService::new();
        let req = DataRequest::new("X", Utc::now(), Utc::now(), Frequency::Tick);
        let res = svc.load(&req).await;
        assert!(matches!(res, Err(DataError::SourceNotFound(_))));
    }

    #[tokio::test]
    async fn load_with_mock_returns_dataset() {
        let svc = DataService::new()
            .register_source(Box::new(MockSource::with_rows("mock", vec![tick()])));
        let req = DataRequest::new("X", Utc::now(), Utc::now(), Frequency::Tick);
        let ds = svc.load(&req).await.unwrap();
        assert_eq!(ds.len(), 1);
    }

    #[tokio::test]
    async fn cache_hit_avoids_duplicate_query() {
        let svc = DataService::new()
            .register_source(Box::new(MockSource::with_rows("mock", vec![tick()])));
        let req = DataRequest::new("X", Utc::now(), Utc::now(), Frequency::Tick);
        let ds1 = svc.load(&req).await.unwrap();
        let ds2 = svc.load(&req).await.unwrap();
        assert_eq!(ds1.checksum, ds2.checksum);
    }

    #[tokio::test]
    async fn lru_evicts_oldest_when_capacity_exceeded() {
        // 容量 2,插入 3 个不同 key,触发淘汰
        let svc = DataService::new()
            .with_cache_capacity(NonZeroUsize::new(2).unwrap())
            .register_source(Box::new(MockSource::with_rows("m", vec![tick()])));
        for i in 0..3 {
            let req = DataRequest::new(format!("SYM{i}"), Utc::now(), Utc::now(), Frequency::Tick);
            let _ = svc.load(&req).await.unwrap();
        }
        let stats = svc.cache_stats();
        assert_eq!(stats.len, 2);
        assert_eq!(stats.capacity, 2);
        // 3 次 load 应全是 miss(都不同 key)
        assert_eq!(stats.misses, 3);
        assert_eq!(stats.hits, 0);
    }

    #[tokio::test]
    async fn cache_hit_increments_hits_counter() {
        let svc =
            DataService::new().register_source(Box::new(MockSource::with_rows("m", vec![tick()])));
        let req = DataRequest::new("X", Utc::now(), Utc::now(), Frequency::Tick);
        svc.load(&req).await.unwrap(); // miss
        svc.load(&req).await.unwrap(); // hit
        svc.load(&req).await.unwrap(); // hit
        let stats = svc.cache_stats();
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hits, 2);
    }

    #[tokio::test]
    async fn default_cache_capacity_is_64() {
        let svc = DataService::new();
        let stats = svc.cache_stats();
        assert_eq!(stats.capacity, 64);
        assert_eq!(stats.len, 0);
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
    }

    // ===== stream() 单元测试 =====

    #[tokio::test]
    async fn service_stream_passes_through_to_source() {
        // 用 MockSource 构造 1 个 tick,stream() 应透传
        let svc = DataService::new()
            .register_source(Box::new(MockSource::with_rows("mock", vec![tick()])));
        let req = DataRequest::new("X", Utc::now(), Utc::now(), Frequency::Tick);

        let mut s = svc.stream("mock", &req).await.unwrap();
        let mut total_rows = 0;
        // drain stream
        while let Some(item) = futures::StreamExt::next(&mut s).await {
            let batch = item.unwrap();
            total_rows += batch.num_rows();
        }
        assert_eq!(
            total_rows, 1,
            "stream() must passthrough 1 tick from MockSource"
        );
    }

    #[tokio::test]
    async fn service_stream_returns_source_not_found() {
        let svc = DataService::new()
            .register_source(Box::new(MockSource::with_rows("mock", vec![tick()])));
        let req = DataRequest::new("X", Utc::now(), Utc::now(), Frequency::Tick);

        let res = svc.stream("nonexistent", &req).await;
        assert!(
            matches!(res, Err(DataError::SourceNotFound(_))),
            "expected SourceNotFound error"
        );
    }

    #[tokio::test]
    async fn service_stream_does_not_write_l1() {
        // 构造 100 tick,stream() 完成后 L1 必须仍为空(旁路缓存)
        let svc = DataService::new().register_source(Box::new(MockSource::with_tick_series(
            "mock",
            100,
            1_000_000,
            |i| i as f64,
        )));
        let req = DataRequest::new("X", Utc::now(), Utc::now(), Frequency::Tick);

        // stream drain
        let mut s = svc.stream("mock", &req).await.unwrap();
        while let Some(item) = futures::StreamExt::next(&mut s).await {
            let _ = item.unwrap();
        }

        // 断言 L1 未被写入
        let stats = svc.cache_stats();
        assert_eq!(stats.len, 0, "stream() must not populate L1 cache");
        assert_eq!(
            stats.misses, 0,
            "stream() must not increment misses counter"
        );
    }
}
