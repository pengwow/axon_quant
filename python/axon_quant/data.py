"""axon_quant.data 顶层 Python API —— thin wrapper 模式。

约定:
- 核心实现走 `axon_quant._native.data`(PyO3 绑定)
- 本模块负责:
  * 重新导出高频符号(用户 `from axon_quant.data import ...`)
  * `DataRequest` 工厂:支持 ISO 8601 字符串 / `datetime` 自动转换
  * 类型别名(IDE 友好):`FrequencyStr` / `TickDict`
  * 异常类型 `AxonError` 从 `_native` 重新导出,统一 `except` 入口

用法::

    from axon_quant.data import (
        DataService, DataRequest, Frequency,
        MockSource, Tick, Dataset, SchemaField,
        AxonError, DataError,
    )
    import pyarrow as pa
    import datetime

    svc = DataService.new().register_source(
        MockSource.with_tick_series("btc", 1000, 1_000_000, lambda i: 100.0 + i)
    )
    # `DataRequest` 接受 str 或 datetime(自动转 UTC)
    req = DataRequest(
        "BTCUSDT",
        "2026-01-01T00:00:00Z",  # ISO 8601 字符串
        "2026-01-02T00:00:00Z",
        Frequency.Min1,
    )
    ds = svc.load(req)
    assert ds.len == 1000
    batch = ds.to_arrow(0)  # 零拷贝 pyarrow.RecordBatch
"""

from __future__ import annotations

from datetime import datetime, timezone
from typing import Callable, Optional, Union

# 重新导出原生符号(Stage 1 全量)
# 注意:`_native` 是 cdylib 单文件扩展(不是 Python package 目录),
# 所以 `from axon_quant._native.data import ...` 这种 dot 路径不可用;
# 改用 `from axon_quant._native import data` 先把子模块对象取出来,
# 再用属性访问取出类 / 函数(与 `llm.py` 保持一致)。
from axon_quant._native import data as _native_data_module  # noqa: E402

# 显式从子模块对象取值(避免在 top-level 用 `from X import *` 的副作用)
CacheControl = _native_data_module.CacheControl
CacheStats = _native_data_module.CacheStats
Dataset = _native_data_module.Dataset
DataService = _native_data_module.DataService
DataType = _native_data_module.DataType
Frequency = _native_data_module.Frequency
MockSource = _native_data_module.MockSource
SchemaField = _native_data_module.SchemaField
Tick = _native_data_module.Tick
_NativeDataRequest = _native_data_module.DataRequest

# `AxonError` 从 `_native` 顶层导入(基类,由 `axon-python` 的公共 error 模块注册)。
# `DataError` 从 `_native.data` 子模块导入(`axon-data::python::error::register` 注册)。
# 注:DataError 当前继承 PyException(为避免 axon-data 依赖 axon-python 循环依赖,
# 见 .axon-internal/specs/2026-06-19-python-bindings-expansion-design.md §3.1.6)
from axon_quant._native import AxonError  # noqa: E402
DataError = _native_data_module.DataError

# 类型别名(IDE 友好)
FrequencyStr = str  # `"tick" / "1m" / "1h" / ...`
DatetimeLike = Union[str, datetime]


__all__ = [
    # 数据服务
    "DataService",
    "DataRequest",
    "CacheControl",
    "CacheStats",
    # 数据源 / 数据模型
    "MockSource",
    "Tick",
    "Dataset",
    "SchemaField",
    "DataType",
    "Frequency",
    # 异常
    "AxonError",
    "DataError",
    # 类型别名
    "FrequencyStr",
    "DatetimeLike",
    # 工厂函数
    "make_data_request",
]


def _to_utc_datetime(value: DatetimeLike) -> datetime:
    """把 `str` / `datetime` 统一转 UTC `datetime`。

    - `datetime`:若无 tzinfo 视作 UTC;有 tzinfo 则转 UTC。
    - `str`:支持 ISO 8601(`2026-01-01T00:00:00Z` / `2026-01-01T00:00:00+08:00`)。

    错误:ValueError(若 str 解析失败)。
    """
    if isinstance(value, datetime):
        if value.tzinfo is None:
            return value.replace(tzinfo=timezone.utc)
        return value.astimezone(timezone.utc)
    if isinstance(value, str):
        # 用 datetime.fromisoformat(Python 3.11+ 支持 "Z" 后缀)
        try:
            # 替换 trailing "Z" 为 "+00:00"(兼容 Python 3.10 之前的 fromisoformat)
            iso = value.replace("Z", "+00:00") if value.endswith("Z") else value
            dt = datetime.fromisoformat(iso)
        except ValueError as e:
            raise ValueError(
                f"invalid ISO 8601 datetime: {value!r} (expect e.g. '2026-01-01T00:00:00Z')"
            ) from e
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        return dt.astimezone(timezone.utc)
    raise TypeError(f"unsupported datetime type: {type(value).__name__}")


# `DataRequest` 工厂:接受 ISO 8601 字符串,内部转 datetime 后调原生类
# 注:这里覆盖 `_native.data.DataRequest` 的导出名,这是设计意图(thin wrapper)。
#   Python 端用户从 `axon_quant.data` 导入时拿到这个工厂函数,
#   直接调原生类需 `from axon_quant._native.data import DataRequest`。
def DataRequest(  # noqa: F811
    symbol: str,
    start: DatetimeLike,
    end: DatetimeLike,
    frequency: Frequency,
    fields: Optional[list] = None,
    source: Optional[str] = None,
) -> _NativeDataRequest:
    """构造 `DataRequest`,`start` / `end` 接受 ISO 8601 字符串或 `datetime`。

    Args:
        symbol: 标的符号,如 `"BTCUSDT"` / `"AAPL"`。
        start: 起始时间(包含)。支持 ISO 8601 字符串或 `datetime`(无 tzinfo 视作 UTC)。
        end: 结束时间(包含),格式同 `start`。
        frequency: 数据频率(`Frequency.Tick` / `Frequency.Min1` / ...)。
        fields: 字段子集,`None` = 全部。
        source: 数据源名称,`None` = 自动选择(首个已注册 source)。

    Returns:
        `_NativeDataRequest`:原生 `DataRequest` 实例。

    Raises:
        ValueError: ISO 8601 字符串解析失败。
    """
    return _NativeDataRequest(
        symbol=symbol,
        start=_to_utc_datetime(start),
        end=_to_utc_datetime(end),
        frequency=frequency,
        fields=fields,
        source=source,
    )


# 工厂别名(更显式的命名风格)
make_data_request = DataRequest
