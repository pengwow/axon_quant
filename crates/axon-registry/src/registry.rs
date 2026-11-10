//! ModelRegistry 核心

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;

use crate::error::{RegistryError, RegistryResult};
use crate::filter::VersionFilter;
use crate::signature::ModelSignature;
use crate::storage::{LocalStorage, StorageBackendTrait};
use crate::types::{ModelMetadata, ModelStage, ModelVersion, SemVer};

/// 模型注册表
///
/// 内存索引：model_name -> `Vec<ModelVersion>`（按版本排序）
/// 版本号分配：每个 model name 独立的 `AtomicU64` 计数器，
///   用 `fetch_add` 原子递增，保证并发 register 下版本号唯一
/// 持久化：每次索引变更后写入 `<base>/<name>/registry.json`
pub struct ModelRegistry {
    storage: Arc<dyn StorageBackendTrait>,
    /// 内存索引
    index: DashMap<String, Vec<ModelVersion>>,
    /// 版本号计数器（按 model name 隔离）
    ///
    /// 不依赖 index 持锁分配，避免与 `storage.upload().await` 跨锁边界。
    /// 初始值 = index 中最大 patch（首次访问时懒初始化）。
    version_counters: DashMap<String, Arc<AtomicU64>>,
    /// 注册表持久化目录（通常是 storage 的 base_dir）
    persist_dir: PathBuf,
}

impl ModelRegistry {
    /// 创建新注册表
    pub fn new(storage: Arc<dyn StorageBackendTrait>) -> Self {
        let persist_dir = Self::infer_persist_dir(&storage);
        std::fs::create_dir_all(&persist_dir).ok();
        Self {
            storage,
            index: DashMap::new(),
            version_counters: DashMap::new(),
            persist_dir,
        }
    }

    /// 创建并指定持久化目录
    pub fn with_persist_dir(storage: Arc<dyn StorageBackendTrait>, persist_dir: PathBuf) -> Self {
        std::fs::create_dir_all(&persist_dir).ok();
        Self {
            storage,
            index: DashMap::new(),
            version_counters: DashMap::new(),
            persist_dir,
        }
    }

    fn infer_persist_dir(storage: &Arc<dyn StorageBackendTrait>) -> PathBuf {
        if let Some(local) = storage.as_any().downcast_ref::<LocalStorage>() {
            return local.base_dir().to_path_buf();
        }
        PathBuf::from("./registry")
    }

    /// 注册新模型版本
    pub async fn register(
        &self,
        name: &str,
        artifact_path: &Path,
        metadata: ModelMetadata,
        signature: Option<ModelSignature>,
    ) -> RegistryResult<ModelVersion> {
        // 版本号在持原子计数器时分配，不依赖 index 持锁，
        // 后续 `storage.upload().await` 期间不会被并发 register 干扰。
        let version = self.next_version(name);
        let dest_key = format!("{name}/{version}/model.bin");
        let upload = self.storage.upload(artifact_path, &dest_key).await?;

        let model_version = ModelVersion {
            name: name.to_string(),
            version: version.clone(),
            stage: ModelStage::Staging,
            metadata,
            signature,
            storage_uri: upload.key,
            artifact_size_bytes: upload.size_bytes,
            artifact_hash: upload.content_hash,
        };

        self.index
            .entry(name.to_string())
            .or_default()
            .push(model_version.clone());

        self.persist_index(name).await?;
        Ok(model_version)
    }

    /// 获取指定版本（None 返回最新）
    pub async fn get(&self, name: &str, version: Option<&SemVer>) -> RegistryResult<ModelVersion> {
        let versions = self
            .index
            .get(name)
            .ok_or_else(|| RegistryError::ModelNotFound(name.to_string()))?;

        match version {
            Some(v) => versions
                .iter()
                .find(|mv| &mv.version == v)
                .cloned()
                .ok_or_else(|| RegistryError::VersionNotFound(name.to_string(), v.to_string())),
            None => versions
                .iter()
                .max_by_key(|mv| mv.version.clone())
                .cloned()
                .ok_or_else(|| RegistryError::VersionNotFound(name.to_string(), "latest".into())),
        }
    }

    /// 获取当前 Production 阶段版本
    pub async fn get_production(&self, name: &str) -> RegistryResult<ModelVersion> {
        let versions = self
            .index
            .get(name)
            .ok_or_else(|| RegistryError::ModelNotFound(name.to_string()))?;
        versions
            .iter()
            .find(|mv| mv.stage == ModelStage::Production)
            .cloned()
            .ok_or_else(|| RegistryError::NoProductionVersion(name.to_string()))
    }

    /// 查询版本列表
    pub async fn list_versions(
        &self,
        name: &str,
        filter: &VersionFilter,
    ) -> RegistryResult<Vec<ModelVersion>> {
        let versions = self
            .index
            .get(name)
            .ok_or_else(|| RegistryError::ModelNotFound(name.to_string()))?;

        let mut results: Vec<ModelVersion> = versions
            .iter()
            .filter(|mv| {
                if let Some(stage) = filter.stage
                    && mv.stage != stage
                {
                    return false;
                }
                if let Some(ref min) = filter.min_version
                    && &mv.version < min
                {
                    return false;
                }
                if let Some(ref max) = filter.max_version
                    && &mv.version > max
                {
                    return false;
                }
                for (k, v) in &filter.tags {
                    if mv.metadata.tags.get(k).map(String::as_str) != Some(v.as_str()) {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();

        results.sort_by(|a, b| b.version.cmp(&a.version));

        if let Some(limit) = filter.limit {
            results.truncate(limit);
        }

        Ok(results)
    }

    /// 阶段转换
    pub async fn transition_stage(
        &self,
        name: &str,
        version: &SemVer,
        new_stage: ModelStage,
    ) -> RegistryResult<ModelVersion> {
        // 先读取 + 验证（不持有可变借用）
        let current_stage = {
            let versions = self
                .index
                .get(name)
                .ok_or_else(|| RegistryError::ModelNotFound(name.to_string()))?;
            versions
                .iter()
                .find(|mv| &mv.version == version)
                .ok_or_else(|| {
                    RegistryError::VersionNotFound(name.to_string(), version.to_string())
                })?
                .stage
        };
        Self::validate_transition(current_stage, new_stage)?;

        // 提升到 Production 时降级旧版本
        if new_stage == ModelStage::Production {
            let mut versions = self
                .index
                .get_mut(name)
                .ok_or_else(|| RegistryError::ModelNotFound(name.to_string()))?;
            for existing in versions.iter_mut() {
                if existing.stage == ModelStage::Production && &existing.version != version {
                    existing.stage = ModelStage::Archived;
                }
            }
        }

        // 更新目标版本
        let result = {
            let mut versions = self
                .index
                .get_mut(name)
                .ok_or_else(|| RegistryError::ModelNotFound(name.to_string()))?;
            let mv = versions
                .iter_mut()
                .find(|mv| &mv.version == version)
                .ok_or_else(|| {
                    RegistryError::VersionNotFound(name.to_string(), version.to_string())
                })?;
            mv.stage = new_stage;
            mv.clone()
        };

        self.persist_index(name).await?;
        Ok(result)
    }

    /// 回滚到上一个 Archived 版本
    pub async fn rollback(&self, name: &str) -> RegistryResult<ModelVersion> {
        let current_prod = {
            let versions = self
                .index
                .get(name)
                .ok_or_else(|| RegistryError::ModelNotFound(name.to_string()))?;
            versions
                .iter()
                .find(|mv| mv.stage == ModelStage::Production)
                .cloned()
        };

        let target_version = {
            let versions = self
                .index
                .get(name)
                .ok_or_else(|| RegistryError::ModelNotFound(name.to_string()))?;
            versions
                .iter()
                .filter(|mv| mv.stage == ModelStage::Archived)
                .max_by_key(|mv| mv.version.clone())
                .map(|mv| mv.version.clone())
                .ok_or_else(|| {
                    RegistryError::RollbackFailed("no previous production version".to_string())
                })?
        };

        if let Some(current) = current_prod {
            self.transition_stage(name, &current.version, ModelStage::RolledBack)
                .await?;
        }

        self.transition_stage(name, &target_version, ModelStage::Production)
            .await
    }

    /// 取消晋升:把当前 Production 版本还原回 Staging。
    ///
    /// 为什么两段流转:状态机无 production→staging 直连(晋升刻意单向),
    /// production→archived→staging 是合法的等效路径;首次晋升后取消与
    /// rollback 不同——没有上一个 production 版本可回,只能回到待验证态。
    pub async fn unpromote(&self, name: &str) -> RegistryResult<ModelVersion> {
        let current = self.get_production(name).await?;
        self.transition_stage(name, &current.version, ModelStage::Archived)
            .await?;
        self.transition_stage(name, &current.version, ModelStage::Staging)
            .await
    }

    /// 下载模型产物
    pub async fn download_artifact(
        &self,
        name: &str,
        version: &SemVer,
        dest: &Path,
    ) -> RegistryResult<()> {
        let mv = self.get(name, Some(version)).await?;
        self.storage.download(&mv.storage_uri, dest).await
    }

    /// 列出所有已注册模型
    pub fn list_models(&self) -> Vec<String> {
        self.index.iter().map(|e| e.key().clone()).collect()
    }

    // --- 内部辅助方法 ---

    /// 分配下一个版本号（patch 单调递增）
    ///
    /// 用 `AtomicU64::fetch_add` 原子递增 patch，保证并发 register 下版本号唯一。
    /// 不持 `index` 锁、不跨 `.await` 边界，因此与 `storage.upload()` 并发安全。
    ///
    /// 首个版本 = `1.0.0`，之后 `bump_patch` 单调递增（与 `test_register_first_version`
    /// 单元测试期望一致）。
    fn next_version(&self, name: &str) -> SemVer {
        let counter = self
            .version_counters
            .entry(name.to_string())
            .or_insert_with(|| {
                // 首次访问：懒初始化为 index 中最大 patch（index 为空时 0）
                let max_patch = self
                    .index
                    .get(name)
                    .and_then(|v| v.iter().map(|mv| mv.version.patch).max())
                    .unwrap_or(0);
                Arc::new(AtomicU64::new(u64::from(max_patch)))
            })
            .clone();
        let patch = counter.fetch_add(1, Ordering::SeqCst);
        SemVer::new(1, 0, patch as u32)
    }

    fn validate_transition(from: ModelStage, to: ModelStage) -> RegistryResult<()> {
        let valid = matches!(
            (from, to),
            (ModelStage::Staging, ModelStage::Production)
                | (ModelStage::Staging, ModelStage::Archived)
                | (ModelStage::Production, ModelStage::Archived)
                | (ModelStage::Production, ModelStage::RolledBack)
                | (ModelStage::Archived, ModelStage::Staging)
                | (ModelStage::Archived, ModelStage::Production) // 回滚场景
        );
        if valid {
            Ok(())
        } else {
            Err(RegistryError::InvalidTransition { from, to })
        }
    }

    async fn persist_index(&self, name: &str) -> RegistryResult<()> {
        if let Some(versions) = self.index.get(name) {
            let data = serde_json::to_string_pretty(&*versions)
                .map_err(|e| RegistryError::Serialization(e.to_string()))?;
            let path = self.persist_dir.join(name).join("registry.json");
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| RegistryError::Io(e.to_string()))?;
            }
            std::fs::write(&path, data).map_err(|e| RegistryError::Io(e.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn make_artifact(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, data).unwrap();
        path
    }

    fn make_registry(dir: &Path) -> (TempDir, ModelRegistry) {
        let tmp = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(tmp.path().to_path_buf()).unwrap());
        let registry = ModelRegistry::with_persist_dir(storage, dir.to_path_buf());
        (tmp, registry)
    }

    #[tokio::test]
    async fn test_register_first_version() {
        let dir = TempDir::new().unwrap();
        let (_tmp, registry) = make_registry(dir.path());
        let src = make_artifact(dir.path(), "m1.bin", b"weights").await;

        let mv = registry
            .register("ppo", &src, ModelMetadata::default(), None)
            .await
            .unwrap();
        assert_eq!(mv.version, SemVer::new(1, 0, 0));
        assert_eq!(mv.stage, ModelStage::Staging);
        assert!(!mv.artifact_hash.is_empty());
    }

    #[tokio::test]
    async fn test_register_increments_patch() {
        let dir = TempDir::new().unwrap();
        let (_tmp, registry) = make_registry(dir.path());

        let src = make_artifact(dir.path(), "m1.bin", b"a").await;
        let v1 = registry
            .register("ppo", &src, ModelMetadata::default(), None)
            .await
            .unwrap();

        let src2 = make_artifact(dir.path(), "m2.bin", b"b").await;
        let v2 = registry
            .register("ppo", &src2, ModelMetadata::default(), None)
            .await
            .unwrap();
        assert_eq!(v1.version, SemVer::new(1, 0, 0));
        assert_eq!(v2.version, SemVer::new(1, 0, 1));
    }

    #[tokio::test]
    async fn test_promote_to_production_archives_previous() {
        let dir = TempDir::new().unwrap();
        let (_tmp, registry) = make_registry(dir.path());

        let src1 = make_artifact(dir.path(), "m1.bin", b"a").await;
        let v1 = registry
            .register("ppo", &src1, ModelMetadata::default(), None)
            .await
            .unwrap();
        registry
            .transition_stage("ppo", &v1.version, ModelStage::Production)
            .await
            .unwrap();

        let src2 = make_artifact(dir.path(), "m2.bin", b"b").await;
        let v2 = registry
            .register("ppo", &src2, ModelMetadata::default(), None)
            .await
            .unwrap();
        registry
            .transition_stage("ppo", &v2.version, ModelStage::Production)
            .await
            .unwrap();

        // v1 应被自动归档
        let v1_now = registry.get("ppo", Some(&v1.version)).await.unwrap();
        assert_eq!(v1_now.stage, ModelStage::Archived);
        let prod = registry.get_production("ppo").await.unwrap();
        assert_eq!(prod.version, v2.version);
    }

    #[tokio::test]
    async fn test_rollback_to_archived() {
        let dir = TempDir::new().unwrap();
        let (_tmp, registry) = make_registry(dir.path());

        let src1 = make_artifact(dir.path(), "m1.bin", b"a").await;
        let v1 = registry
            .register("ppo", &src1, ModelMetadata::default(), None)
            .await
            .unwrap();
        registry
            .transition_stage("ppo", &v1.version, ModelStage::Production)
            .await
            .unwrap();

        let src2 = make_artifact(dir.path(), "m2.bin", b"b").await;
        let v2 = registry
            .register("ppo", &src2, ModelMetadata::default(), None)
            .await
            .unwrap();
        registry
            .transition_stage("ppo", &v2.version, ModelStage::Production)
            .await
            .unwrap();

        // v2 是 Production，v1 是 Archived
        let prod = registry.rollback("ppo").await.unwrap();
        assert_eq!(prod.version, v1.version);
        // v2 应被标记为 RolledBack
        let v2_now = registry.get("ppo", Some(&v2.version)).await.unwrap();
        assert_eq!(v2_now.stage, ModelStage::RolledBack);
    }

    #[tokio::test]
    async fn test_unpromote_returns_to_staging() {
        let dir = TempDir::new().unwrap();
        let (_tmp, registry) = make_registry(dir.path());

        let src = make_artifact(dir.path(), "m1.bin", b"a").await;
        let v1 = registry
            .register("ppo", &src, ModelMetadata::default(), None)
            .await
            .unwrap();
        registry
            .transition_stage("ppo", &v1.version, ModelStage::Production)
            .await
            .unwrap();

        let unp = registry.unpromote("ppo").await.unwrap();
        assert_eq!(unp.version, v1.version);
        assert_eq!(unp.stage, ModelStage::Staging);

        // 查证持久化后的状态也回到 Staging，且不再有生产版本
        let now = registry.get("ppo", Some(&v1.version)).await.unwrap();
        assert_eq!(now.stage, ModelStage::Staging);
        let r = registry.get_production("ppo").await.unwrap_err();
        assert!(matches!(r, RegistryError::NoProductionVersion(_)));
    }

    #[tokio::test]
    async fn test_invalid_stage_transition() {
        let dir = TempDir::new().unwrap();
        let (_tmp, registry) = make_registry(dir.path());

        let src = make_artifact(dir.path(), "m.bin", b"a").await;
        let v = registry
            .register("ppo", &src, ModelMetadata::default(), None)
            .await
            .unwrap();
        // Staging -> Staging 是非法的
        let r = registry
            .transition_stage("ppo", &v.version, ModelStage::Staging)
            .await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn test_list_versions_filtered() {
        let dir = TempDir::new().unwrap();
        let (_tmp, registry) = make_registry(dir.path());

        for i in 0..3 {
            let src = make_artifact(dir.path(), &format!("m{i}.bin"), b"a").await;
            let v = registry
                .register("ppo", &src, ModelMetadata::default(), None)
                .await
                .unwrap();
            if i == 1 {
                registry
                    .transition_stage("ppo", &v.version, ModelStage::Production)
                    .await
                    .unwrap();
            }
        }

        // 列出所有版本
        let all = registry
            .list_versions("ppo", &VersionFilter::new())
            .await
            .unwrap();
        assert_eq!(all.len(), 3);

        // 过滤 Production
        let prod = registry
            .list_versions(
                "ppo",
                &VersionFilter::new().with_stage(ModelStage::Production),
            )
            .await
            .unwrap();
        assert_eq!(prod.len(), 1);

        // 限制数量
        let limited = registry
            .list_versions("ppo", &VersionFilter::new().with_limit(2))
            .await
            .unwrap();
        assert_eq!(limited.len(), 2);
    }

    #[tokio::test]
    async fn test_get_latest_version() {
        let dir = TempDir::new().unwrap();
        let (_tmp, registry) = make_registry(dir.path());

        let src1 = make_artifact(dir.path(), "m1.bin", b"a").await;
        registry
            .register("ppo", &src1, ModelMetadata::default(), None)
            .await
            .unwrap();
        let src2 = make_artifact(dir.path(), "m2.bin", b"b").await;
        registry
            .register("ppo", &src2, ModelMetadata::default(), None)
            .await
            .unwrap();

        let latest = registry.get("ppo", None).await.unwrap();
        assert_eq!(latest.version, SemVer::new(1, 0, 1));
    }

    #[tokio::test]
    async fn test_download_artifact() {
        let dir = TempDir::new().unwrap();
        let (_tmp, registry) = make_registry(dir.path());

        let src = make_artifact(dir.path(), "model.bin", b"test weights").await;
        let v = registry
            .register("ppo", &src, ModelMetadata::default(), None)
            .await
            .unwrap();

        let dest = dir.path().join("downloaded.bin");
        registry
            .download_artifact("ppo", &v.version, &dest)
            .await
            .unwrap();
        let content = std::fs::read(&dest).unwrap();
        assert_eq!(content, b"test weights");
    }
}
