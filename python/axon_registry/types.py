"""AXON 模型注册表 Python 类型。"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class ModelStage(Enum):
    """模型阶段。"""

    STAGING = "staging"
    PRODUCTION = "production"
    ARCHIVED = "archived"
    ROLLED_BACK = "rolled_back"


@dataclass
class SemVer:
    """语义化版本。"""

    major: int = 1
    minor: int = 0
    patch: int = 0

    def bump_patch(self) -> None:
        self.patch += 1

    def bump_minor(self) -> None:
        self.minor += 1
        self.patch = 0

    def bump_major(self) -> None:
        self.major += 1
        self.minor = 0
        self.patch = 0

    def __str__(self) -> str:
        return f"{self.major}.{self.minor}.{self.patch}"

    def __lt__(self, other: "SemVer") -> bool:
        return (self.major, self.minor, self.patch) < (other.major, other.minor, other.patch)

    def __le__(self, other: "SemVer") -> bool:
        return (self.major, self.minor, self.patch) <= (other.major, other.minor, other.patch)

    def __gt__(self, other: "SemVer") -> bool:
        return (self.major, self.minor, self.patch) > (other.major, other.minor, other.patch)

    def __ge__(self, other: "SemVer") -> bool:
        return (self.major, self.minor, self.patch) >= (other.major, other.minor, other.patch)

    def __eq__(self, other: object) -> bool:
        if not isinstance(other, SemVer):
            return NotImplemented
        return (self.major, self.minor, self.patch) == (other.major, other.minor, other.patch)

    def __hash__(self) -> int:
        return hash((self.major, self.minor, self.patch))

    @classmethod
    def parse(cls, s: str) -> "SemVer":
        parts = s.split(".")
        if len(parts) != 3:
            raise ValueError(f"invalid version: {s}")
        return cls(int(parts[0]), int(parts[1]), int(parts[2]))


@dataclass
class ModelMetadata:
    """模型元数据。"""

    description: str = ""
    hyperparameters: dict[str, Any] = field(default_factory=dict)
    metrics: dict[str, float] = field(default_factory=dict)
    dataset_hash: str | None = None
    git_commit: str | None = None
    training_duration_secs: float | None = None
    created_at: float = field(default_factory=time.time)
    author: str | None = None
    tags: dict[str, str] = field(default_factory=dict)


@dataclass
class ModelVersion:
    """模型版本记录。"""

    name: str
    version: SemVer
    stage: ModelStage
    metadata: ModelMetadata
    storage_uri: str
    artifact_size_bytes: int
    artifact_hash: str

    def __str__(self) -> str:
        return f"{self.name} v{self.version} ({self.stage.value})"
