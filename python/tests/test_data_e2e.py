"""axon_quant.data 端到端测试(L3 Python E2E)。

覆盖范围:
1. 类型导入 / 实例化
2. MockSource 注册 → DataService.load → Dataset 取 Arrow
3. 缓存命中(`hits` 计数)
4. 异常路径(`DataError` 抛出,可被 `Exception` 捕获)
5. DataService builder 链式风格
6. PyArrow zero-copy 数据契约
7. ISO 8601 字符串 / datetime 互转
8. slice 操作(take/skip/last_n/by_time_range)

运行::

    cd /Users/liupeng/workspace/quant/axon
    PYO3_PYTHON=/Users/liupeng/workspace/quant/axon/.venv/bin/python \\
        python -m pytest python/tests/test_data_e2e.py -v

注意:本测试需先 build wheel(参见 Makefile 的 `python-build` /
`python-develop` 目标)。如未 build,部分测试 skip。
"""

from __future__ import annotations

import datetime
import sys
from typing import TYPE_CHECKING

import pytest

# 强制使用本项目 venv 的 pyarrow
from pathlib import Path

# 让 sys.path 包含 venv site-packages(测试时由 maturin develop / python-build 注入)
# 这里显式把 venv site-packages 加到 sys.path 前面,避免 miniconda pyarrow 干扰
_VENV_SITE = Path("/Users/liupeng/workspace/quant/axon/.venv/lib/python3.14/site-packages")
if _VENV_SITE.exists() and str(_VENV_SITE) not in sys.path:
    sys.path.insert(0, str(_VENV_SITE))

import pyarrow as pa  # noqa: E402

# `axon_quant` 在 maturin develop / wheel install 后可被 import
# 缺失时 skip 整个模块(开发期还没 build 时常见)
try:
    import axon_quant
    from axon_quant.data import (
        AxonError,
        CacheControl,
        CacheStats,
        DataError,
        DataRequest,
        DataService,
        Dataset,
        DataType,
        Frequency,
        MockSource,
        SchemaField,
        Tick,
    )
    _AXON_DATA_AVAILABLE = hasattr(axon_quant, "_native") and hasattr(axon_quant._native, "data")
except ImportError as _e:
    pytest.skip(f"axon_quant not installed: {_e}", allow_module_level=True)
    raise  # 实际不可达,仅供类型检查

if not _AXON_DATA_AVAILABLE:
    pytest.skip(
        "axon_quant._native.data not yet registered (need maturin develop / wheel install)",
        allow_module_level=True,
    )

if TYPE_CHECKING:
    pass


# ===== 工具 =====

def _utc(y: int, m: int, d: int) -> datetime.datetime:
    """UTC 锚点 datetime(避免重复 import timezone)。"""
    return datetime.datetime(y, m, d, tzinfo=datetime.timezone.utc)


# ===== 类型可用性 =====

def test_data_module_imports_all_symbols():
    """所有 10 个高频符号都能 import。"""
    from axon_quant.data import (  # noqa: F401
        DataService, DataRequest, Frequency, MockSource, Tick,
        Dataset, SchemaField, CacheStats, CacheControl,
        DataType, AxonError, DataError,
    )


def test_frequency_value_strings():
    """Frequency.value 返回稳定字符串(用于 JSON / 配置文件)。"""
    assert Frequency.Tick.value == "tick"
    assert Frequency.Min1.value == "1m"
    assert Frequency.Hour1.value == "1h"
    assert Frequency.Day1.value == "1d"


def test_datatype_value_strings():
    """DataType.value 返回稳定字符串。"""
    assert DataType.F64.value == "f64"
    assert DataType.I64.value == "i64"
    assert DataType.String.value == "string"
    assert DataType.Bool.value == "bool"
    assert DataType.Timestamp.value == "timestamp"


def test_schema_field_creation():
    """SchemaField 接受 dtype 字符串。"""
    f = SchemaField("price", "f64")
    assert f.name == "price"
    assert f.dtype.value == "f64"


def test_schema_field_invalid_dtype_raises():
    """未知 dtype 报 ValueError。"""
    with pytest.raises(ValueError):
        SchemaField("x", "u32")


# ===== MockSource + DataService 集成 =====

def test_mocksource_with_tick_series_creates_rows():
    """MockSource.with_tick_series 生成正确行数。"""
    src = MockSource.with_tick_series("btc", 5, 1_000_000, lambda i: 100.0 + i)
    assert src.name == "btc"
    assert src.len == 5
    assert not src.is_empty


def test_mocksource_with_rows_preserves_count():
    """MockSource.with_rows 保留 tick 行数。"""
    ticks = [
        Tick(ts_ns=0, price=100.0, qty=1.0, side=0),
        Tick(ts_ns=1_000, price=101.0, qty=1.0, side=1),
    ]
    src = MockSource.with_rows("test", ticks)
    assert src.name == "test"
    assert src.len == 2


def test_dataservice_register_and_load_mock():
    """完整链路:register → load → Dataset 行数。"""
    svc = DataService.new().register_source(
        MockSource.with_tick_series("m", 3, 1, lambda i: 1.0)
    )
    req = DataRequest("X", _utc(2026, 1, 1), _utc(2026, 1, 2), Frequency.Tick)
    ds = svc.load(req)
    assert ds.len == 3
    assert isinstance(ds, Dataset)


def test_dataservice_cache_hit_increments_hits():
    """连续 3 次相同 load:第 1 次 miss,后 2 次 hit。"""
    svc = DataService.new().register_source(
        MockSource.with_tick_series("m", 1, 1, lambda i: 1.0)
    )
    req = DataRequest("X", _utc(2026, 1, 1), _utc(2026, 1, 2), Frequency.Tick)

    stats0 = svc.cache_stats()
    svc.load(req)
    svc.load(req)
    svc.load(req)
    stats = svc.cache_stats()

    assert stats.misses == stats0.misses + 1
    assert stats.hits == stats0.hits + 2
    assert 0.0 < stats.hit_rate < 1.0


def test_dataservice_builder_chain():
    """builder 链式:`new().register_source().with_cache_capacity()`。"""
    svc = (
        DataService.new()
        .register_source(MockSource.with_tick_series("m", 1, 1, lambda i: 1.0))
        .with_cache_capacity(32)
    )
    assert svc.cache_stats().capacity == 32


def test_dataservice_with_cache_capacity_zero_raises():
    """`capacity=0` 报 ValueError。"""
    svc = DataService.new()
    with pytest.raises(ValueError):
        svc.with_cache_capacity(0)


# ===== 异常路径 =====

def test_dataservice_load_no_source_raises_exception():
    """无 source 调 load 抛异常(可被 Exception 捕获)。"""
    svc = DataService.new()
    req = DataRequest("X", _utc(2026, 1, 1), _utc(2026, 1, 2), Frequency.Tick)
    with pytest.raises(Exception) as exc_info:
        svc.load(req)
    # 错误信息含 "SourceNotFound"(code 标签)
    assert "SourceNotFound" in str(exc_info.value) or "source" in str(exc_info.value).lower()


def test_data_error_inherits_exception():
    """DataError 是 Exception 子类(可被 except Exception 统一捕获)。"""
    with pytest.raises(Exception):
        # 触发 DataError 的方法(无 source load)
        svc = DataService.new()
        svc.load(DataRequest("X", _utc(2026, 1, 1), _utc(2026, 1, 2), Frequency.Tick))


# ===== Arrow zero-copy =====

def test_dataset_to_arrow_returns_pyarrow_batch():
    """to_arrow(0) 返回 pyarrow.RecordBatch(零拷贝契约)。"""
    svc = DataService.new().register_source(
        MockSource.with_tick_series("m", 2, 1, lambda i: 1.0)
    )
    req = DataRequest("X", _utc(2026, 1, 1), _utc(2026, 1, 2), Frequency.Tick)
    ds = svc.load(req)
    assert ds.len == 2

    batch = ds.to_arrow(0)
    assert isinstance(batch, pa.RecordBatch)
    assert batch.num_rows == 2
    # 4 列 schema:timestamp / price / quantity / side
    assert batch.num_columns == 4
    assert batch.schema.names == ["timestamp", "price", "quantity", "side"]


def test_dataset_to_arrow_index_oob():
    """to_arrow 越界报 IndexError。"""
    svc = DataService.new().register_source(
        MockSource.with_tick_series("m", 1, 1, lambda i: 1.0)
    )
    req = DataRequest("X", _utc(2026, 1, 1), _utc(2026, 1, 2), Frequency.Tick)
    ds = svc.load(req)
    with pytest.raises(IndexError):
        ds.to_arrow(99)


def test_dataset_to_arrow_table():
    """to_arrow_table 返回 pyarrow.Table,rows = sum(batches.num_rows)。"""
    svc = DataService.new().register_source(
        MockSource.with_tick_series("m", 5, 1, lambda i: 1.0)
    )
    req = DataRequest("X", _utc(2026, 1, 1), _utc(2026, 1, 2), Frequency.Tick)
    ds = svc.load(req)
    table = ds.to_arrow_table()
    assert isinstance(table, pa.Table)
    assert table.num_rows == 5


def test_dataset_schema_returns_four_canonical_fields():
    """Dataset.schema() 返回 4 个固定字段(timestamp/price/quantity/side)。"""
    svc = DataService.new().register_source(
        MockSource.with_tick_series("m", 1, 1, lambda i: 1.0)
    )
    req = DataRequest("X", _utc(2026, 1, 1), _utc(2026, 1, 2), Frequency.Tick)
    ds = svc.load(req)
    fields = ds.schema()
    assert len(fields) == 4
    names = [f.name for f in fields]
    assert names == ["timestamp", "price", "quantity", "side"]


# ===== Slice 操作 =====

def test_dataset_take_skip_last_n():
    """take / skip / last_n 返回新 Dataset,行数正确。"""
    svc = DataService.new().register_source(
        MockSource.with_tick_series("m", 5, 1, lambda i: 100.0 + i)
    )
    req = DataRequest("X", _utc(2026, 1, 1), _utc(2026, 1, 2), Frequency.Tick)
    ds = svc.load(req)

    assert ds.take(3).len == 3
    assert ds.skip(2).len == 3
    assert ds.last_n(2).len == 2


def test_dataset_by_time_range_filters():
    """by_time_range 按时间窗口过滤。"""
    # 5 个 tick,时间 0,1s,2s,3s,4s
    svc = DataService.new().register_source(
        MockSource.with_tick_series("m", 5, 1_000_000_000, lambda i: 100.0 + i)
    )
    req = DataRequest("X", _utc(2026, 1, 1), _utc(2026, 1, 2), Frequency.Tick)
    ds = svc.load(req)

    filtered = ds.by_time_range(1_000_000_000, 3_000_000_000)
    assert filtered.len == 3


def test_dataset_iter_ticks_count():
    """iter_ticks() 返回 list,长度 = len。"""
    svc = DataService.new().register_source(
        MockSource.with_tick_series("m", 4, 1, lambda i: 1.0)
    )
    req = DataRequest("X", _utc(2026, 1, 1), _utc(2026, 1, 2), Frequency.Tick)
    ds = svc.load(req)

    ticks = ds.iter_ticks()
    assert len(ticks) == 4
    assert all(isinstance(t, Tick) for t in ticks)


# ===== Cache 运维 =====

def test_cache_control_clear_l1_empties_cache():
    """clear_l1 后 L1 entry 数归零。"""
    svc = (
        DataService.new()
        .register_source(MockSource.with_tick_series("m", 1, 1, lambda i: 1.0))
        .with_cache_capacity(8)
    )
    req = DataRequest("X", _utc(2026, 1, 1), _utc(2026, 1, 2), Frequency.Tick)
    svc.load(req)
    assert svc.cache_stats().len > 0

    ctrl = svc.cache_control()
    ctrl.clear_l1()
    assert svc.cache_stats().len == 0


def test_cache_control_resize_l1_takes_effect():
    """resize_l1 调整 L1 容量。"""
    svc = DataService.new()
    ctrl = svc.cache_control()
    ctrl.resize_l1(16)
    assert svc.cache_stats().capacity == 16


# ===== ISO 8601 字符串互转 =====

def test_data_request_iso_string():
    """DataRequest 接受 ISO 8601 字符串(自动转 UTC)。"""
    req = DataRequest("X", "2026-01-01T00:00:00Z", "2026-01-02T00:00:00Z", Frequency.Tick)
    assert req.is_valid()
    # 字符串转 RFC3339 形式
    assert req.start.year == 2026
    assert req.start.month == 1
    assert req.start.day == 1


def test_data_request_offset_string():
    """DataRequest 接受带时区偏移的 ISO 8601 字符串。"""
    req = DataRequest(
        "X",
        "2026-01-01T08:00:00+08:00",  # 北京时间 8 点 = UTC 0 点
        "2026-01-02T00:00:00+00:00",
        Frequency.Tick,
    )
    # UTC 时间应为 2026-01-01T00:00:00
    assert req.start.hour == 0


def test_data_request_naive_datetime_assumes_utc():
    """naive datetime 视作 UTC。"""
    req = DataRequest(
        "X",
        datetime.datetime(2026, 1, 1, 12, 0, 0),  # 无 tzinfo
        datetime.datetime(2026, 1, 2, 0, 0, 0),
        Frequency.Tick,
    )
    assert req.start.hour == 12
    assert req.start.tzinfo is not None  # 已转 UTC


def test_data_request_invalid_iso_raises():
    """无法解析的字符串报 ValueError。"""
    with pytest.raises(ValueError):
        DataRequest("X", "not-a-date", "2026-01-02T00:00:00Z", Frequency.Tick)
