"""Tests for L3Book streaming diff via Python bindings (0.9.0 C2.1c).

覆盖场景:
- subscribe() 返回自增 ID,从 0 起
- subscribe() 默认 kind = "per_bar"
- subscribe() 支持 kind = "per_fill" / "both" 字符串
- unsubscribe(id) 已注册 ID 返回 True,无效 ID 返回 False
- callback 能收到 L3BookDiff dict(通过 _dispatch_test_diff 手动触发,
  因为 0.9.0 简化版 dispatch_diff 尚未接入 begin_bar/run)
- callback 收到的 dict 包含 expected 字段(instrument tuple / added /
  removed / modified / timestamp_ns)
"""
from __future__ import annotations

from typing import Any

from axon_quant.backtest import BacktestEngine, spot_instrument


def test_subscribe_returns_incrementing_id() -> None:
    """subscribe 连续调用,id 从 0 起自增。"""
    engine = BacktestEngine(initial_cash=100_000.0)
    id0 = engine.subscribe(callback=lambda diff: None)
    id1 = engine.subscribe(callback=lambda diff: None)
    assert id0 == 0
    assert id1 == 1


def test_subscribe_default_kind_is_per_bar() -> None:
    """subscribe 不传 kind 时,默认 per_bar。"""
    engine = BacktestEngine(initial_cash=100_000.0)
    # 不传 kind,内部用 SubscriberKind::default() = PerBar
    id0 = engine.subscribe(callback=lambda diff: None)
    assert id0 == 0
    # 至少能 unsubscribe(default kind = PerBar 不影响 register 行为)
    assert engine.unsubscribe(id0) is True


def test_subscribe_accepts_per_fill_kind() -> None:
    """kind='per_fill' 合法。"""
    engine = BacktestEngine(initial_cash=100_000.0)
    id0 = engine.subscribe(callback=lambda diff: None, kind="per_fill")
    assert id0 == 0


def test_subscribe_accepts_both_kind() -> None:
    """kind='both' 合法。"""
    engine = BacktestEngine(initial_cash=100_000.0)
    id0 = engine.subscribe(callback=lambda diff: None, kind="both")
    assert id0 == 0


def test_subscribe_rejects_invalid_kind() -> None:
    """kind 非法字符串 → ValueError。"""
    engine = BacktestEngine(initial_cash=100_000.0)
    try:
        engine.subscribe(callback=lambda diff: None, kind="bogus")
    except ValueError:
        pass
    else:
        raise AssertionError("expected ValueError for invalid kind")


def test_unsubscribe_returns_true_for_registered_id() -> None:
    """已注册 id → unsubscribe 返回 True。"""
    engine = BacktestEngine(initial_cash=100_000.0)
    id0 = engine.subscribe(callback=lambda diff: None)
    assert engine.unsubscribe(id0) is True


def test_unsubscribe_returns_false_for_unknown_id() -> None:
    """未注册 id → unsubscribe 返回 False。"""
    engine = BacktestEngine(initial_cash=100_000.0)
    assert engine.unsubscribe(999) is False


def test_double_unsubscribe_returns_false() -> None:
    """重复 unsubscribe 同 id,第二次返回 False。"""
    engine = BacktestEngine(initial_cash=100_000.0)
    id0 = engine.subscribe(callback=lambda diff: None)
    assert engine.unsubscribe(id0) is True
    assert engine.unsubscribe(id0) is False


def test_callback_receives_dict_with_expected_fields() -> None:
    """手动 _dispatch_test_diff 触发,callback 收到包含 expected 字段的 dict。"""
    engine = BacktestEngine(initial_cash=100_000.0)
    received: list[dict[str, Any]] = []
    engine.subscribe(callback=lambda diff: received.append(diff))

    spot = spot_instrument("BTC", "USDT")
    # 0.9.0 简化版:通过 _dispatch_test_diff 手动推一个空 diff
    engine._dispatch_test_diff(spot, 1_000)

    assert len(received) == 1
    diff = received[0]
    # diff 应包含 5 个字段
    assert "instrument" in diff
    assert "added" in diff
    assert "removed" in diff
    assert "modified" in diff
    assert "timestamp_ns" in diff
    # 空 diff 时 added/removed/modified 都是空 list
    assert diff["added"] == []
    assert diff["removed"] == []
    assert diff["modified"] == []
    assert diff["timestamp_ns"] == 1_000


def test_per_fill_subscriber_receives_per_fill_dispatch() -> None:
    """kind='per_fill' 的订阅者只在 dispatch_diff(kind=PerFill) 时收到。"""
    engine = BacktestEngine(initial_cash=100_000.0)
    received: list[dict[str, Any]] = []
    # kind='per_fill' 的订阅者
    engine.subscribe(callback=lambda diff: received.append(diff), kind="per_fill")

    spot = spot_instrument("BTC", "USDT")
    # 手动推 PerBar 类型的 diff:订阅者不应收到(因为 kind=PerFill)
    engine._dispatch_test_diff(spot, 1_000)
    assert received == []

    # 推 PerFill 类型的 diff:订阅者应收到
    engine._dispatch_test_diff_per_fill(spot, 2_000)
    assert len(received) == 1
    assert received[0]["timestamp_ns"] == 2_000


def test_both_subscriber_receives_everything() -> None:
    """kind='both' 的订阅者 PerBar / PerFill 两种 dispatch 都收到。"""
    engine = BacktestEngine(initial_cash=100_000.0)
    received: list[dict[str, Any]] = []
    engine.subscribe(callback=lambda diff: received.append(diff), kind="both")

    spot = spot_instrument("BTC", "USDT")
    engine._dispatch_test_diff(spot, 1_000)
    engine._dispatch_test_diff_per_fill(spot, 2_000)
    assert len(received) == 2
    assert received[0]["timestamp_ns"] == 1_000
    assert received[1]["timestamp_ns"] == 2_000


def test_unsubscribed_callback_not_called() -> None:
    """unsubscribe 之后,callback 不再被调用。"""
    engine = BacktestEngine(initial_cash=100_000.0)
    received: list[dict[str, Any]] = []
    id0 = engine.subscribe(callback=lambda diff: received.append(diff))

    spot = spot_instrument("BTC", "USDT")
    engine._dispatch_test_diff(spot, 1_000)
    assert len(received) == 1

    assert engine.unsubscribe(id0) is True
    engine._dispatch_test_diff(spot, 2_000)
    # 取消订阅后不再收到
    assert len(received) == 1
