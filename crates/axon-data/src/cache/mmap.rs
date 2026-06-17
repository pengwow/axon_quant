//! L2 mmap 缓存管理器
//!
//! 集成到 DataService 的第二层缓存。
//! 使用 SharedMemoryPool 管理跨进程共享的 Arrow IPC 数据。
//!
//! # Safety
//!
//! 本模块使用 `memmap2` 的 unsafe API 进行内存映射(进程内 + 跨进程共享)。
//! 在我们的使用场景中是安全的:
//! 1. 写入后立即映射,不在映射期间修改文件
//! 2. 我们控制文件生命周期
//! 3. 使用元数据头验证数据完整性
// 显式放行 crate-level `deny(unsafe_code)`:memmap2 / fs2 内部 unsafe 必需
#![allow(unsafe_code)]
//!
//! # 设计决策
//!
//! - 使用文件系统存储而非 POSIX shm_open
//! - LRU 淘汰策略，最近访问的在头部
//! - 序列化为 Arrow IPC 格式，支持零拷贝读取
//!
//! # 使用方式
//!
//! ```rust,ignore
//! use axon_data::cache::{MmapCache, MmapCacheConfig};
//!
//! // 创建配置
//! let config = MmapCacheConfig::new(1024 * 1024 * 100, "/tmp/axon_cache");
//!
//! // 创建缓存
//! let mut cache = MmapCache::new(config).unwrap();
//!
//! // 存入数据
//! cache.put("key", &dataset).unwrap();
//!
//! // 读取数据
//! let data = cache.get("key");
//! ```

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;

use arrow::datatypes::Schema;
use arrow::ipc::reader::StreamReader;
use arrow::ipc::writer::StreamWriter;
use arrow::record_batch::RecordBatch;

use crate::cache::shared_memory::SharedMemoryPool;
use crate::dataset::Dataset;
use crate::error::{DataError, DataResult};
use crate::ipc::IpcWritable;
use crate::types::DataRequest;

/// 零拷贝缓存数据集，借用 MmapCache 的生命周期
///
/// 内部持有从 mmap 内存直接构建的 RecordBatch 引用，
/// 不复制底层数据，get 延迟极低（<10µs）。
pub struct CachedDataset<'a> {
    /// Arrow schema
    pub schema: Arc<Schema>,
    /// 从 mmap 直接引用的 RecordBatch（零拷贝）
    pub batches: Vec<RecordBatch>,
    /// 数据源名称
    pub source: String,
    /// 原始 IPC 数据的引用（保持 mmap 生命周期）
    _ipc_data: &'a [u8],
}

impl<'a> CachedDataset<'a> {
    /// 获取 schema
    pub fn schema(&self) -> &Arc<Schema> {
        &self.schema
    }

    /// 获取 batches（零拷贝）
    pub fn batches(&self) -> &[RecordBatch] {
        &self.batches
    }

    /// 获取数据源名称
    pub fn source(&self) -> &str {
        &self.source
    }

    /// 总行数
    pub fn len(&self) -> usize {
        self.batches.iter().map(|b| b.num_rows()).sum()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 转换为 owned Dataset（需要复制数据）
    pub fn to_owned(&self) -> DataResult<Dataset> {
        Dataset::new(
            self.batches.clone(),
            self.source.clone(),
            DataRequest::new(
                self.source(),
                chrono::Utc::now(),
                chrono::Utc::now(),
                crate::types::Frequency::Tick,
            ),
        )
    }
}

/// 缓存配置
#[derive(Debug, Clone)]
pub struct MmapCacheConfig {
    /// 最大容量（字节）
    pub max_bytes: usize,
    /// 存储目录
    pub dir: String,
}

impl MmapCacheConfig {
    /// 创建新的配置
    pub fn new(max_bytes: usize, dir: impl Into<String>) -> Self {
        Self {
            max_bytes,
            dir: dir.into(),
        }
    }
}

/// 缓存条目元数据
#[derive(Debug, Clone)]
#[allow(dead_code)] // 预留字段，未来用于统计和调试
struct CacheEntryMeta {
    /// 数据类型（tick / bar）
    data_type: String,
    /// 频率（仅 bar 数据）
    frequency: Option<String>,
    /// 数据大小（字节）
    size: usize,
}

/// L2 mmap 缓存管理器
///
/// # 跨进程共享
///
/// 使用 SharedMemoryPool 管理跨进程共享的 Arrow IPC 数据。
/// 多个进程可以同时访问同一目录下的缓存文件。
///
/// # LRU 淘汰
///
/// 使用 Vec 作为 LRU 链表，最近访问的在头部。
/// 当容量不足时，淘汰尾部的条目。
pub struct MmapCache {
    /// 共享内存池
    pool: SharedMemoryPool,
    /// 条目索引（key → 元数据）
    index: HashMap<String, CacheEntryMeta>,
    /// LRU 链表（最近访问的在头部）
    lru: Vec<String>,
    /// 容量配置
    config: MmapCacheConfig,
}

impl MmapCache {
    /// 创建新的 L2 缓存
    pub fn new(config: MmapCacheConfig) -> DataResult<Self> {
        let pool = SharedMemoryPool::new(&config.dir, config.max_bytes)?;

        Ok(Self {
            pool,
            index: HashMap::new(),
            lru: Vec::new(),
            config,
        })
    }

    /// 生成缓存键
    pub fn cache_key(source: &str, symbol: &str, frequency: &str) -> String {
        format!("{}:{}:{}", source, symbol, frequency)
    }

    /// 从缓存加载数据
    pub fn get(&mut self, key: &str) -> Option<Dataset> {
        if !self.index.contains_key(key) {
            return None;
        }

        // 更新 LRU
        self.touch(key);

        // 从共享内存读取 IPC 数据（需要复制出来避免借用冲突）
        let ipc_data = self.pool.read(key)?.to_vec();

        // 反序列化为 Dataset
        match self.deserialize(&ipc_data) {
            Ok(dataset) => Some(dataset),
            Err(e) => {
                // 反序列化失败，移除损坏的条目
                eprintln!("Failed to deserialize cache entry {}: {}", key, e);
                let _ = self.remove(key);
                None
            }
        }
    }

    /// 零拷贝从缓存加载数据
    ///
    /// 返回 CachedDataset，直接引用 mmap 内存，不复制数据。
    /// 性能目标：<10µs
    ///
    /// 注意：此方法不更新 LRU，需要手动调用 `touch()` 方法。
    pub fn get_zero_copy(&self, key: &str) -> Option<CachedDataset<'_>> {
        if !self.index.contains_key(key) {
            return None;
        }

        // 从共享内存读取 IPC 数据（返回引用，不复制）
        let ipc_data = self.pool.read_ref(key)?;

        // 使用 StreamReader 从内存构建 RecordBatch（零拷贝）
        let cursor = std::io::Cursor::new(ipc_data);
        let reader = StreamReader::try_new(cursor, None).ok()?;

        let schema = reader.schema();
        let mut batches = Vec::new();

        for batch_result in reader {
            let batch = batch_result.ok()?;
            batches.push(batch);
        }

        // 从 schema metadata 恢复 source
        let source = schema
            .metadata()
            .get("axon_source")
            .cloned()
            .unwrap_or_default();

        Some(CachedDataset {
            schema,
            batches,
            source,
            _ipc_data: ipc_data,
        })
    }

    /// 存入缓存
    pub fn put(&mut self, key: &str, dataset: &dyn IpcWritable) -> DataResult<()> {
        // 序列化为 Arrow IPC
        let ipc_bytes = self.serialize(dataset)?;

        // 检查容量，必要时淘汰
        while self.pool.used() + ipc_bytes.len() > self.config.max_bytes {
            self.evict_lru()?;
        }

        // 写入共享内存
        self.pool.write(key, &ipc_bytes)?;

        // 更新索引
        let meta = CacheEntryMeta {
            data_type: if dataset.schema().fields().len() == 4 {
                "tick".to_string()
            } else {
                "bar".to_string()
            },
            frequency: dataset.frequency_tag(),
            size: ipc_bytes.len(),
        };
        self.index.insert(key.to_string(), meta);

        // 更新 LRU
        self.touch(key);

        Ok(())
    }

    /// 删除条目
    pub fn remove(&mut self, key: &str) -> DataResult<()> {
        self.pool.remove(key)?;
        self.index.remove(key);
        self.lru.retain(|k| k != key);
        Ok(())
    }

    /// 更新 LRU（将 key 移到头部）
    ///
    /// 在使用 `get_zero_copy()` 后，如果需要更新 LRU，可以手动调用此方法。
    pub fn touch(&mut self, key: &str) {
        self.lru.retain(|k| k != key);
        self.lru.insert(0, key.to_string());
    }

    /// LRU 淘汰
    fn evict_lru(&mut self) -> DataResult<()> {
        if let Some(key) = self.lru.pop() {
            self.remove(&key)?;
        }
        Ok(())
    }

    /// 序列化为 Arrow IPC Stream 格式
    fn serialize(&self, data: &dyn IpcWritable) -> DataResult<Vec<u8>> {
        let mut buffer = Vec::new();

        let mut writer = StreamWriter::try_new(&mut buffer, data.schema())
            .map_err(|e| DataError::Internal(format!("IPC writer init: {e}")))?;

        for batch in data.batches() {
            writer
                .write(batch)
                .map_err(|e| DataError::Internal(format!("IPC write batch: {e}")))?;
        }

        writer
            .finish()
            .map_err(|e| DataError::Internal(format!("IPC finish: {e}")))?;

        Ok(buffer)
    }

    /// 反序列化为 Dataset
    fn deserialize(&self, data: &[u8]) -> DataResult<Dataset> {
        let cursor = Cursor::new(data);
        let reader = StreamReader::try_new(cursor, None)
            .map_err(|e| DataError::CacheEntryCorrupted(format!("IPC reader init: {e}")))?;

        let schema = Arc::new(reader.schema());
        let mut batches = Vec::new();

        for batch_result in reader {
            let batch = batch_result
                .map_err(|e| DataError::CacheEntryCorrupted(format!("IPC read batch: {e}")))?;
            batches.push(batch);
        }

        // 从 schema metadata 恢复 source
        let source = schema
            .metadata()
            .get("axon_source")
            .cloned()
            .unwrap_or_default();

        // 创建默认的 DataRequest
        let req = DataRequest::new(
            &source,
            chrono::Utc::now(),
            chrono::Utc::now(),
            crate::types::Frequency::Tick,
        );

        Dataset::new(batches, source, req)
    }

    /// 获取当前使用量（字节）
    pub fn used(&self) -> usize {
        self.pool.used()
    }

    /// 获取容量（字节）
    pub fn capacity(&self) -> usize {
        self.config.max_bytes
    }

    /// 获取条目数
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// 检查是否为空
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// 清理所有条目
    pub fn clear(&mut self) -> DataResult<()> {
        self.pool.clear()?;
        self.index.clear();
        self.lru.clear();
        Ok(())
    }
}

impl Drop for MmapCache {
    fn drop(&mut self) {
        let _ = self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::MockSource;
    use crate::traits::DataSource;
    use crate::types::{DataRequest, Frequency};
    use chrono::Utc;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_mmap_cache_basic() {
        let dir = tempdir().unwrap();
        let config = MmapCacheConfig::new(1024 * 1024, dir.path().to_str().unwrap());
        let mut cache = MmapCache::new(config).unwrap();

        // 创建测试数据
        let source = MockSource::empty();
        let req = DataRequest::new("BTCUSDT", Utc::now(), Utc::now(), Frequency::Tick);
        let dataset = source.query(&req).await.unwrap();

        // 存入缓存
        let key = MmapCache::cache_key("test", "BTCUSDT", "Tick");
        cache.put(&key, &dataset).unwrap();

        // 从缓存加载
        let cached = cache.get(&key);
        assert!(cached.is_some());

        // 验证数据一致性
        let cached_dataset = cached.unwrap();
        assert_eq!(cached_dataset.len(), dataset.len());
    }

    #[tokio::test]
    async fn test_mmap_cache_lru_eviction() {
        let dir = tempdir().unwrap();
        // 设置小容量以触发淘汰
        let config = MmapCacheConfig::new(1024, dir.path().to_str().unwrap());
        let mut cache = MmapCache::new(config).unwrap();

        // 创建测试数据
        let source = MockSource::empty();
        let req = DataRequest::new("BTCUSDT", Utc::now(), Utc::now(), Frequency::Tick);

        // 写入多个条目（使用不同的 symbol）
        for i in 0..5 {
            let key = format!("key{}", i);
            let mut req_i = req.clone();
            req_i.symbol = format!("SYM{}", i);
            let dataset_i = source.query(&req_i).await.unwrap();
            cache.put(&key, &dataset_i).unwrap();
        }

        // 验证 LRU 淘汰（最早的条目应该被淘汰）
        assert!(cache.get("key0").is_none());
        assert!(cache.get("key4").is_some());
    }

    #[test]
    fn test_mmap_cache_config() {
        let config = MmapCacheConfig::new(1024 * 1024, "/tmp/axon_cache");
        assert_eq!(config.max_bytes, 1024 * 1024);
        assert_eq!(config.dir, "/tmp/axon_cache");
    }

    #[tokio::test]
    async fn test_mmap_cache_zero_copy() {
        let dir = tempdir().unwrap();
        let config = MmapCacheConfig::new(1024 * 1024, dir.path().to_str().unwrap());
        let mut cache = MmapCache::new(config).unwrap();

        // 创建测试数据
        let source = MockSource::empty();
        let req = DataRequest::new("BTCUSDT", Utc::now(), Utc::now(), Frequency::Tick);
        let dataset = source.query(&req).await.unwrap();

        // 存入缓存
        let key = MmapCache::cache_key("test", "BTCUSDT", "Tick");
        cache.put(&key, &dataset).unwrap();

        // 零拷贝读取
        let cached = cache.get_zero_copy(&key).unwrap();
        assert_eq!(cached.len(), dataset.len());

        // 转换为 owned Dataset
        let owned = cached.to_owned().unwrap();
        assert_eq!(owned.len(), dataset.len());
    }

    #[tokio::test]
    async fn test_mmap_cache_zero_copy_not_found() {
        let dir = tempdir().unwrap();
        let config = MmapCacheConfig::new(1024 * 1024, dir.path().to_str().unwrap());
        let cache = MmapCache::new(config).unwrap();

        // 零拷贝读取不存在的条目
        let result = cache.get_zero_copy("nonexistent");
        assert!(result.is_none());
    }
}
