"""AXON 滚动前向验证 Python 辅助库。

设计原则：
- **零硬依赖**：仅依赖 `numpy`（用于索引数组），运行时检测缺失则报错
- **可独立运行**：Rust 扩展未编译时，Python 版本可独立完成所有计算
- **与 Rust 端类型对应**：通过 dataclass / Enum 镜像 Rust 端的 `WalkForwardConfig` 等
"""

from __future__ import annotations

__version__ = "0.0.1"

__all__ = ["__version__"]
