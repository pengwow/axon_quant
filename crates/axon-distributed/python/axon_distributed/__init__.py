"""AXON 分布式训练 Python 包。

设计原则：
- **零硬依赖**：ray / torch 仅在需要时延迟导入
- **本地 mock 模式**：不连接真实 Ray 集群，CI/示例友好
- **与 Rust 端类型对应**：dataclass / Enum 镜像 Rust 配置
"""

from __future__ import annotations

__version__ = "0.0.1"

__all__ = ["__version__"]
