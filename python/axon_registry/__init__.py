"""AXON 模型注册表 Python 包。"""

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
