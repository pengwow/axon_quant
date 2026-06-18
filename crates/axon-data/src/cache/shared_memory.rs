//! 共享内存池管理器
//!
//! 使用文件映射创建跨进程共享的内存区域。
//! 每个缓存条目是一个独立的文件，包含：
//! - 元数据头（Magic、版本、长度、校验和）
// 显式放行 crate-level `deny(unsafe_code)`:memmap2 unsafe 必需
#![allow(unsafe_code)]
//! - Arrow IPC 数据体
//!
//! # 设计决策
//!
//! - 使用文件系统而非 POSIX shm_open，因为：
//!   1. 跨平台兼容性更好（Linux/macOS/Windows）
//!   2. 文件系统自动管理存储
//!   3. 支持大文件（超出内存）
//! - 使用 memmap2 实现内存映射，支持零拷贝读取
//! - 元数据头包含访问统计，支持 LRU 淘汰

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;

use memmap2::Mmap;

use crate::error::{DataError, DataResult};

/// 魔数 "AXON" (0x41584F4E)
const MAGIC: u32 = 0x41584F4E;

/// 当前版本号
const VERSION: u16 = 1;

/// 元数据头大小（字节）
const METADATA_SIZE: usize = 64;

/// 条目元数据（存储在共享内存头部）
///
/// # 内存布局
///
/// ```text
/// ┌─────────────────────────────────────────┐
/// │ EntryMetadata (64 bytes)                │
/// ├─────────────────────────────────────────┤
/// │ Arrow IPC Data (variable length)        │
/// │   - Schema                              │
/// │   - RecordBatch 1                       │
/// │   - RecordBatch 2                       │
/// │   - ...                                 │
/// │   - Footer                              │
/// └─────────────────────────────────────────┘
/// ```
#[derive(Debug, Clone)]
pub struct EntryMetadata {
    /// 魔数 "AXON" (0x41584F4E)
    pub magic: u32,
    /// 版本号
    pub version: u16,
    /// 数据长度（字节）
    pub data_len: u32,
    /// 数据校验和（简化：使用数据长度）
    pub checksum: u64,
    /// 创建时间（Unix 时间戳，秒）
    pub created_at: u64,
    /// 最后访问时间（Unix 时间戳，秒）
    pub last_accessed: u64,
    /// 访问次数
    pub access_count: u64,
}

impl EntryMetadata {
    /// 创建新的元数据
    pub fn new(data_len: u32, checksum: u64) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Self {
            magic: MAGIC,
            version: VERSION,
            data_len,
            checksum,
            created_at: now,
            last_accessed: now,
            access_count: 0,
        }
    }

    /// 验证元数据完整性
    pub fn is_valid(&self) -> bool {
        self.magic == MAGIC && self.version == VERSION
    }

    /// 更新访问统计
    pub fn touch(&mut self) {
        self.last_accessed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.access_count += 1;
    }

    /// 序列化为字节数组
    pub fn to_bytes(&self) -> [u8; METADATA_SIZE] {
        let mut bytes = [0u8; METADATA_SIZE];
        bytes[0..4].copy_from_slice(&self.magic.to_le_bytes());
        bytes[4..6].copy_from_slice(&self.version.to_le_bytes());
        bytes[6..10].copy_from_slice(&self.data_len.to_le_bytes());
        bytes[10..18].copy_from_slice(&self.checksum.to_le_bytes());
        bytes[18..26].copy_from_slice(&self.created_at.to_le_bytes());
        bytes[26..34].copy_from_slice(&self.last_accessed.to_le_bytes());
        bytes[34..42].copy_from_slice(&self.access_count.to_le_bytes());
        bytes
    }

    /// 从字节数组反序列化
    pub fn from_bytes(bytes: &[u8; METADATA_SIZE]) -> Self {
        Self {
            magic: u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
            version: u16::from_le_bytes(bytes[4..6].try_into().unwrap()),
            data_len: u32::from_le_bytes(bytes[6..10].try_into().unwrap()),
            checksum: u64::from_le_bytes(bytes[10..18].try_into().unwrap()),
            created_at: u64::from_le_bytes(bytes[18..26].try_into().unwrap()),
            last_accessed: u64::from_le_bytes(bytes[26..34].try_into().unwrap()),
            access_count: u64::from_le_bytes(bytes[34..42].try_into().unwrap()),
        }
    }
}

/// 共享内存条目
pub struct SharedMemoryEntry {
    /// 条目名称
    pub name: String,
    /// 文件路径
    pub path: PathBuf,
    /// 内存映射
    pub mmap: Mmap,
    /// 元数据
    pub metadata: EntryMetadata,
}

/// 共享内存池管理器
///
/// # 跨进程共享
///
/// 使用文件系统存储共享内存对象，多个进程可以同时访问同一目录下的缓存文件。
/// 每个进程维护自己的内存映射，但数据是共享的。
///
/// # 容量管理
///
/// 支持固定容量限制，当容量不足时返回 `CacheCapacityExceeded` 错误。
/// 调用者需要先淘汰旧条目再写入新条目。
pub struct SharedMemoryPool {
    /// 存储目录
    dir: PathBuf,
    /// 条目索引
    entries: HashMap<String, SharedMemoryEntry>,
    /// 总容量（字节）
    capacity: usize,
    /// 当前使用量（字节）
    used: usize,
}

impl SharedMemoryPool {
    /// 创建新的共享内存池
    ///
    /// # 参数
    ///
    /// - `dir`: 存储目录，会自动创建
    /// - `capacity`: 总容量（字节）
    pub fn new(dir: impl Into<PathBuf>, capacity: usize) -> DataResult<Self> {
        let dir = dir.into();
        // 确保目录存在
        std::fs::create_dir_all(&dir).map_err(|e| {
            DataError::SharedMemoryCreation(format!("create dir {}: {}", dir.display(), e))
        })?;

        Ok(Self {
            dir,
            entries: HashMap::new(),
            capacity,
            used: 0,
        })
    }

    /// 获取条目路径
    fn entry_path(&self, name: &str) -> PathBuf {
        // 将名称转换为安全的文件名
        let safe_name = name.replace(['/', '\\'], "_");
        self.dir.join(format!("{}.shm", safe_name))
    }

    /// 写入数据
    ///
    /// # 参数
    ///
    /// - `name`: 条目名称
    /// - `data`: 要写入的数据
    ///
    /// # 错误
    ///
    /// - `CacheCapacityExceeded`: 容量不足
    /// - `SharedMemoryCreation`: 文件创建失败
    pub fn write(&mut self, name: &str, data: &[u8]) -> DataResult<()> {
        let path = self.entry_path(name);
        let data_len = data.len();
        let total_size = METADATA_SIZE + data_len;

        // 检查容量
        if self.used + total_size > self.capacity {
            return Err(DataError::CacheCapacityExceeded {
                needed: total_size,
                available: self.capacity - self.used,
            });
        }

        // 计算校验和（简化：使用数据长度作为校验和）
        let checksum = data_len as u64;

        // 创建元数据
        let metadata = EntryMetadata::new(data_len as u32, checksum);

        // 写入文件
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .map_err(|e| {
                DataError::SharedMemoryCreation(format!("open {}: {}", path.display(), e))
            })?;

        // 设置文件大小
        file.set_len(total_size as u64).map_err(|e| {
            DataError::SharedMemoryCreation(format!("set_len {}: {}", path.display(), e))
        })?;

        // 写入元数据（使用安全的序列化方法）
        let metadata_bytes = metadata.to_bytes();
        file.write_all(&metadata_bytes).map_err(|e| {
            DataError::SharedMemoryCreation(format!("write metadata {}: {}", path.display(), e))
        })?;

        // 写入数据
        file.write_all(data).map_err(|e| {
            DataError::SharedMemoryCreation(format!("write data {}: {}", path.display(), e))
        })?;

        // 创建内存映射
        let mmap = unsafe { Mmap::map(&file) }.map_err(|e| {
            DataError::SharedMemoryMapping(format!("mmap {}: {}", path.display(), e))
        })?;

        // 创建条目
        let entry = SharedMemoryEntry {
            name: name.to_string(),
            path,
            mmap,
            metadata,
        };

        self.entries.insert(name.to_string(), entry);
        self.used += total_size;

        Ok(())
    }

    /// 读取数据
    ///
    /// # 参数
    ///
    /// - `name`: 条目名称
    ///
    /// # 返回
    ///
    /// - `Some(&[u8])`: 数据切片
    /// - `None`: 条目不存在
    pub fn read(&mut self, name: &str) -> Option<&[u8]> {
        if let Some(entry) = self.entries.get_mut(name) {
            // 更新访问统计
            entry.metadata.touch();
            // 返回数据切片
            Some(&entry.mmap[METADATA_SIZE..])
        } else {
            None
        }
    }

    /// 零拷贝读取数据（返回引用）
    ///
    /// 返回 mmap 内存的引用，不复制数据。
    /// 需要确保 SharedMemoryPool 存活。
    pub fn read_ref(&self, name: &str) -> Option<&[u8]> {
        let entry = self.entries.get(name)?;
        Some(&entry.mmap[METADATA_SIZE..])
    }

    /// 删除条目
    ///
    /// # 参数
    ///
    /// - `name`: 条目名称
    pub fn remove(&mut self, name: &str) -> DataResult<()> {
        if let Some(entry) = self.entries.remove(name) {
            // 删除文件
            std::fs::remove_file(&entry.path).map_err(|e| {
                DataError::SharedMemoryCreation(format!("remove {}: {}", entry.path.display(), e))
            })?;
            self.used -= METADATA_SIZE + entry.metadata.data_len as usize;
        }
        Ok(())
    }

    /// 获取当前使用量（字节）
    pub fn used(&self) -> usize {
        self.used
    }

    /// 获取容量（字节）
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// 获取条目数
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 检查是否为空
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 清理所有条目
    pub fn clear(&mut self) -> DataResult<()> {
        let names: Vec<String> = self.entries.keys().cloned().collect();
        for name in names {
            self.remove(&name)?;
        }
        Ok(())
    }

    /// 清理残留的共享内存文件
    ///
    /// 扫描存储目录，删除无效的 .shm 文件。
    /// 用于清理进程崩溃后遗留的文件。
    pub fn cleanup_stale(&self) -> DataResult<()> {
        for entry in std::fs::read_dir(&self.dir).map_err(|e| {
            DataError::SharedMemoryCreation(format!("read_dir {}: {}", self.dir.display(), e))
        })? {
            let entry = entry
                .map_err(|e| DataError::SharedMemoryCreation(format!("read_dir entry: {}", e)))?;

            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "shm") {
                // 尝试加载并验证元数据
                let should_delete = if let Ok(mut file) = File::open(&path) {
                    let mut metadata_bytes = [0u8; METADATA_SIZE];
                    if file.read_exact(&mut metadata_bytes).is_ok() {
                        // 使用安全的反序列化方法
                        let metadata = EntryMetadata::from_bytes(&metadata_bytes);
                        // 元数据无效则删除
                        !metadata.is_valid()
                    } else {
                        // 无法读取完整元数据，删除文件
                        true
                    }
                } else {
                    // 无法打开文件，跳过
                    false
                };

                if should_delete {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        Ok(())
    }
}

impl Drop for SharedMemoryPool {
    fn drop(&mut self) {
        // 清理所有条目
        let _ = self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_shared_memory_pool_basic() {
        let dir = tempdir().unwrap();
        let mut pool = SharedMemoryPool::new(dir.path(), 1024 * 1024).unwrap();

        // 写入数据
        let data = b"Hello, World!";
        pool.write("test", data).unwrap();

        // 读取数据
        let read_data = pool.read("test").unwrap();
        assert_eq!(read_data, data);

        // 删除数据
        pool.remove("test").unwrap();
        assert!(pool.read("test").is_none());
    }

    #[test]
    fn test_shared_memory_pool_capacity() {
        let dir = tempdir().unwrap();
        // 容量需要大于元数据大小(64字节) + 数据大小
        let mut pool = SharedMemoryPool::new(dir.path(), 200).unwrap();

        // 写入数据（需要留出元数据空间）
        let data = vec![0u8; 50];
        pool.write("test1", &data).unwrap();

        // 验证使用量
        assert!(pool.used() > 0);

        // 尝试写入超过容量的数据（50 + 64 = 114，剩余空间不足）
        let large_data = vec![0u8; 100];
        assert!(pool.write("test2", &large_data).is_err());
    }

    #[test]
    fn test_shared_memory_pool_multiple_entries() {
        let dir = tempdir().unwrap();
        let mut pool = SharedMemoryPool::new(dir.path(), 1024 * 1024).unwrap();

        // 写入多个条目
        for i in 0..10 {
            let data = format!("data{}", i);
            pool.write(&format!("key{}", i), data.as_bytes()).unwrap();
        }

        // 验证条目数
        assert_eq!(pool.len(), 10);

        // 读取所有条目
        for i in 0..10 {
            let expected = format!("data{}", i);
            let actual = pool.read(&format!("key{}", i)).unwrap();
            assert_eq!(actual, expected.as_bytes());
        }

        // 清理
        pool.clear().unwrap();
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn test_shared_memory_pool_metadata() {
        let dir = tempdir().unwrap();
        let mut pool = SharedMemoryPool::new(dir.path(), 1024 * 1024).unwrap();

        // 写入数据
        let data = b"test data";
        pool.write("test", data).unwrap();

        // 验证元数据
        let entry = pool.entries.get("test").unwrap();
        assert!(entry.metadata.is_valid());
        assert_eq!(entry.metadata.data_len, data.len() as u32);
        assert_eq!(entry.metadata.access_count, 0);

        // 读取数据（会更新访问统计）
        pool.read("test").unwrap();

        // 验证访问统计更新
        let entry = pool.entries.get("test").unwrap();
        assert_eq!(entry.metadata.access_count, 1);
    }

    #[test]
    fn test_shared_memory_pool_cleanup_stale() {
        let dir = tempdir().unwrap();
        let pool = SharedMemoryPool::new(dir.path(), 1024 * 1024).unwrap();

        // 创建一个无效的 .shm 文件
        let invalid_path = dir.path().join("invalid.shm");
        std::fs::write(&invalid_path, b"invalid data").unwrap();

        // 验证文件存在
        assert!(invalid_path.exists());

        // 清理残留文件
        pool.cleanup_stale().unwrap();

        // 验证文件被删除
        assert!(!invalid_path.exists());
    }
}
