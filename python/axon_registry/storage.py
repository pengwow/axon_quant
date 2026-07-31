"""本地文件存储（纯 Python，无外部依赖）。"""

from __future__ import annotations

import hashlib
import shutil
from dataclasses import dataclass
from pathlib import Path


@dataclass
class UploadResult:
    """上传结果。"""

    key: str
    size_bytes: int
    content_hash: str


class LocalStorageBackend:
    """本地文件存储。"""

    def __init__(self, base_dir: str) -> None:
        self.base_dir = Path(base_dir)
        self.base_dir.mkdir(parents=True, exist_ok=True)

    def upload(self, source_path: str, dest_key: str) -> UploadResult:
        """复制 source_path 到 base_dir/dest_key，返回 SHA-256 hash。"""
        src = Path(source_path)
        dest = self.base_dir / dest_key
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, dest)
        size = dest.stat().st_size
        content_hash = hashlib.sha256(dest.read_bytes()).hexdigest()
        return UploadResult(key=dest_key, size_bytes=size, content_hash=content_hash)

    def download(self, source_key: str, dest_path: str) -> None:
        """复制 base_dir/source_key 到 dest_path。"""
        src = self.base_dir / source_key
        if not src.exists():
            raise FileNotFoundError(f"artifact not found: {source_key}")
        dest = Path(dest_path)
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, dest)

    def exists(self, key: str) -> bool:
        return (self.base_dir / key).exists()

    def delete(self, key: str) -> None:
        path = self.base_dir / key
        if path.exists():
            path.unlink()
