"""AXON 滚动前向验证 Python 辅助库。

设计原则：
- **零硬依赖**：仅依赖 `numpy`（用于索引数组），运行时检测缺失则报错
- **可独立运行**：Rust 扩展未编译时，Python 版本可独立完成所有计算
- **与 Rust 端类型对应**：通过 dataclass / Enum 镜像 Rust 端的 `WalkForwardConfig` 等
"""

from __future__ import annotations

# 版本号从已安装 wheel 的元数据自动读出(PEP 621 规范),
# 跟 pyproject.toml [project].version 自动同步,无需手动改这里
try:
    from importlib.metadata import version as _pkg_version, PackageNotFoundError

    try:
        __version__ = _pkg_version("axon-quant")
    except PackageNotFoundError:  # 开发态 / 未安装
        __version__ = "0.0.0+unknown"
except ImportError:  # Python < 3.8 兜底
    __version__ = "0.0.0+unknown"

__all__ = ["__version__"]
