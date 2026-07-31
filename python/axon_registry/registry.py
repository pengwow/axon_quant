"""模型注册表（纯 Python）。"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Optional

from .storage import LocalStorageBackend
from .types import ModelMetadata, ModelStage, ModelVersion, SemVer


class ModelRegistry:
    """模型注册表（纯 Python，CI 友好）。"""

    def __init__(
        self,
        storage: LocalStorageBackend,
        persist_dir: Optional[str] = None,
    ) -> None:
        self.storage = storage
        self.persist_dir = Path(persist_dir) if persist_dir else storage.base_dir
        self.persist_dir.mkdir(parents=True, exist_ok=True)
        # 内存索引：name -> {version_str -> ModelVersion}
        self._index: dict[str, dict[str, ModelVersion]] = {}

    def register(
        self,
        name: str,
        artifact_path: str,
        metadata: Optional[ModelMetadata] = None,
    ) -> ModelVersion:
        """注册新版本。"""
        if metadata is None:
            metadata = ModelMetadata()
        version = self._next_version(name)
        dest_key = f"{name}/{version}/model.bin"
        upload = self.storage.upload(artifact_path, dest_key)

        mv = ModelVersion(
            name=name,
            version=version,
            stage=ModelStage.STAGING,
            metadata=metadata,
            storage_uri=dest_key,
            artifact_size_bytes=upload.size_bytes,
            artifact_hash=upload.content_hash,
        )

        self._index.setdefault(name, {})[str(version)] = mv
        self._persist(name)
        return mv

    def get(self, name: str, version: Optional[SemVer] = None) -> Optional[ModelVersion]:
        """获取指定版本。"""
        versions = self._index.get(name)
        if not versions:
            return None
        if version is None:
            return max(versions.values(), key=lambda mv: mv.version)
        return versions.get(str(version))

    def get_production(self, name: str) -> Optional[ModelVersion]:
        """获取当前 Production 版本。"""
        versions = self._index.get(name, {})
        for mv in versions.values():
            if mv.stage == ModelStage.PRODUCTION:
                return mv
        return None

    def list_versions(
        self,
        name: str,
        stage: Optional[ModelStage] = None,
        limit: Optional[int] = None,
    ) -> list[ModelVersion]:
        """查询版本列表。"""
        versions = list(self._index.get(name, {}).values())
        if stage is not None:
            versions = [v for v in versions if v.stage == stage]
        versions.sort(key=lambda v: v.version, reverse=True)
        if limit is not None:
            versions = versions[:limit]
        return versions

    def transition_stage(
        self,
        name: str,
        version: SemVer,
        new_stage: ModelStage,
    ) -> ModelVersion:
        """阶段转换。"""
        versions = self._index.get(name)
        if not versions:
            raise KeyError(f"model not found: {name}")
        mv = versions.get(str(version))
        if mv is None:
            raise KeyError(f"version not found: {name}@{version}")

        self._validate_transition(mv.stage, new_stage)

        # 提升到 Production 时降级旧的
        if new_stage == ModelStage.PRODUCTION:
            for other in versions.values():
                if other.stage == ModelStage.PRODUCTION and other.version != version:
                    other.stage = ModelStage.ARCHIVED

        mv.stage = new_stage
        self._persist(name)
        return mv

    def rollback(self, name: str) -> ModelVersion:
        """回滚到上一个 Archived 版本。"""
        versions = self._index.get(name)
        if not versions:
            raise KeyError(f"model not found: {name}")
        current_prod = next(
            (mv for mv in versions.values() if mv.stage == ModelStage.PRODUCTION),
            None,
        )
        target = max(
            (mv for mv in versions.values() if mv.stage == ModelStage.ARCHIVED),
            key=lambda mv: mv.version,
            default=None,
        )
        if target is None:
            raise RuntimeError("no previous production version to rollback to")
        if current_prod is not None:
            self.transition_stage(name, current_prod.version, ModelStage.ROLLED_BACK)
        return self.transition_stage(name, target.version, ModelStage.PRODUCTION)

    def list_models(self) -> list[str]:
        """列出所有已注册模型。"""
        return list(self._index.keys())

    def download_artifact(self, name: str, version: SemVer, dest_path: str) -> None:
        """下载模型产物。"""
        mv = self.get(name, version)
        if mv is None:
            raise KeyError(f"{name}@{version} not found")
        self.storage.download(mv.storage_uri, dest_path)

    # --- 内部 ---

    def _next_version(self, name: str) -> SemVer:
        versions = self._index.get(name, {})
        if not versions:
            return SemVer(1, 0, 0)
        max_v = max(versions.values(), key=lambda mv: mv.version).version
        next_v = SemVer(max_v.major, max_v.minor, max_v.patch)
        next_v.bump_patch()
        return next_v

    @staticmethod
    def _validate_transition(from_stage: ModelStage, to_stage: ModelStage) -> None:
        valid = {
            (ModelStage.STAGING, ModelStage.PRODUCTION),
            (ModelStage.STAGING, ModelStage.ARCHIVED),
            (ModelStage.PRODUCTION, ModelStage.ARCHIVED),
            (ModelStage.PRODUCTION, ModelStage.ROLLED_BACK),
            (ModelStage.ARCHIVED, ModelStage.STAGING),
            (ModelStage.ARCHIVED, ModelStage.PRODUCTION),  # 回滚场景
        }
        if (from_stage, to_stage) not in valid:
            raise ValueError(f"invalid transition: {from_stage.value} -> {to_stage.value}")

    def _persist(self, name: str) -> None:
        versions = self._index.get(name, {})
        path = self.persist_dir / name / "registry.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        data = {
            str(mv.version): {
                "name": mv.name,
                "version": str(mv.version),
                "stage": mv.stage.value,
                "description": mv.metadata.description,
                "storage_uri": mv.storage_uri,
                "artifact_size_bytes": mv.artifact_size_bytes,
                "artifact_hash": mv.artifact_hash,
            }
            for mv in versions.values()
        }
        with open(path, "w", encoding="utf-8") as f:
            json.dump(data, f, indent=2, ensure_ascii=False)
