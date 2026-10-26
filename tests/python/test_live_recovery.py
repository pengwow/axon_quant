"""LiveRecovery 崩溃恢复绑定层行为验证。

链路：Kernel.run(journal_path=...) 首跑录制 → read_manifest 探查 →
LiveRecovery.resume 重演恢复快照 → reconcile 三路对账 → resume_guard 恢复门禁。
构造语义对齐 Rust 侧 crash_recovery 演练；重演保真的前提是宿主决策确定。

语义边界（与 `replay` 文档一致）：纯行情 journal（无 venue 回报条目）重演后
OMS 只有 ack 过的订单——ReplayGateway 不产生成交、回报队列恒空，因此快照的
`positions` 恒空、`cash` = 入金、`fill_ids` 恒空；带 venue 回报的重演保真由
Rust 侧 tests/crash_recovery.rs 用注入回报的 journal 守护。
"""

import pytest

from axon_quant import worker


def _bars(start: float, step: float, n: int):
    return [
        {
            "timestamp": i * 60_000_000_000,
            "open": start + i * step - 0.5,
            "high": start + i * step + 0.3,
            "low": start + i * step - 0.8,
            "close": start + i * step,
            "volume": 1000.0,
        }
        for i in range(n)
    ]


class AlwaysBuy:
    """确定性策略：每根 bar 给同样的买入目标（同一输入同一输出）。"""

    def __init__(self, symbol: str = "BTC/USDT"):
        self.symbol = symbol

    def on_bar(self, bar):
        return {"action_type": "buy", "confidence": 0.8, "target_position": 0.1}


def _record_journal(tmp_path, n: int = 6) -> str:
    """首跑录制：用与重演侧完全一致的参数把 n 根 bar 落成 journal。"""
    k = worker.Kernel(initial_cash=100_000.0, interval="1m")
    k.add_strategy(AlwaysBuy(), symbol="BTC/USDT")
    journal = str(tmp_path / "live.journal")
    result = k.run(
        bars_by_symbol={"BTC/USDT": _bars(100.0, 1.0, n)},
        journal_path=journal,
    )
    assert result.n_bars == n
    return journal


def test_read_manifest_fields(tmp_path):
    journal = _record_journal(tmp_path)
    m = worker.read_manifest(journal)
    assert m["journal_format"] == 1
    assert m["entries"] == 6, "每根 bar 一条事件 + 无 venue 回报，条目数 = bar 数"
    assert isinstance(m["code_version"], str) and m["code_version"]
    assert m["seed"] == 0
    assert m["model_artifacts"] == []


def test_read_manifest_missing_file_rejected(tmp_path):
    with pytest.raises(ValueError, match="读取 journal 失败"):
        worker.read_manifest(str(tmp_path / "nope.journal"))


def test_resume_replay_snapshot_shape(tmp_path):
    journal = _record_journal(tmp_path)
    rec = worker.LiveRecovery(initial_cash=100_000.0, interval="1m")
    rec.add_strategy(AlwaysBuy(), symbol="BTC/USDT")

    snap = rec.resume(journal)
    # 纯行情 journal：无 venue 回报 → 无成交 → 空仓、现金 = 入金
    assert snap["positions"] == {}
    assert snap["cash"] == pytest.approx(100_000.0)
    assert snap["fill_ids"] == []
    assert snap["manifest"]["journal_format"] == 1
    assert snap["manifest"]["entries"] == 6


def test_resume_and_guard_pass_clean_reconcile(tmp_path):
    journal = _record_journal(tmp_path)
    rec = worker.LiveRecovery(initial_cash=100_000.0, interval="1m")
    rec.add_strategy(AlwaysBuy(), symbol="BTC/USDT")
    snap = rec.resume(journal)

    venue = {"positions": {}, "cash": 100_000.0, "fill_ids": []}
    report = worker.reconcile(snap, venue)
    assert report["position_breaks"] == []
    assert report["missing_fills"] == []
    assert report["extra_fills"] == []
    assert report["cash_break"] is None
    assert worker.resume_guard(report) is None


def test_resume_guard_rejects_position_break(tmp_path):
    journal = _record_journal(tmp_path)
    rec = worker.LiveRecovery(initial_cash=100_000.0, interval="1m")
    rec.add_strategy(AlwaysBuy(), symbol="BTC/USDT")
    snap = rec.resume(journal)

    # 交易所在崩溃窗口内开了仓：持仓 break → 门禁拒绝自动续跑
    venue = {"positions": {"BTC/USDT": 1.5}, "cash": 100_000.0, "fill_ids": []}
    report = worker.reconcile(snap, venue)
    assert len(report["position_breaks"]) == 1
    assert report["position_breaks"][0]["instrument"] == "BTC/USDT"
    assert report["position_breaks"][0]["local"] == pytest.approx(0.0)
    assert report["position_breaks"][0]["venue"] == pytest.approx(1.5)
    assert report["frozen_instruments"] == ["BTC/USDT"]
    with pytest.raises(ValueError, match="恢复门禁"):
        worker.resume_guard(report)


def test_resume_guard_allows_fill_only_divergence():
    """仅成交差异（漏记）→ 门禁放行，调用方负责告警人工核对。"""
    local = {"positions": {}, "cash": 100_000.0, "fill_ids": []}
    venue = {"positions": {}, "cash": 100_000.0, "fill_ids": ["v-1"]}
    report = worker.reconcile(local, venue)
    assert report["missing_fills"] == ["v-1"]
    assert worker.resume_guard(report) is None


def test_reconcile_cash_tolerance():
    """现金差异落在容差内 → 干净；超阈值 → cash_break。"""
    local = {"positions": {}, "cash": 100_000.0, "fill_ids": []}
    # 1e-9 < 默认现金容差 1e-6 → 干净
    within = worker.reconcile(local, {"positions": {}, "cash": 100_000.0 + 1e-9, "fill_ids": []})
    assert within["cash_break"] is None
    # 0.01 > 1e-6 → 报差异，diff = venue - local
    beyond = worker.reconcile(local, {"positions": {}, "cash": 100_000.01, "fill_ids": []})
    assert beyond["cash_break"]["local"] == pytest.approx(100_000.0)
    assert beyond["cash_break"]["venue"] == pytest.approx(100_000.01)
    assert beyond["cash_break"]["diff"] == pytest.approx(0.01)
    with pytest.raises(ValueError, match="恢复门禁"):
        worker.resume_guard(beyond)


def test_reconcile_negative_tolerance_rejected():
    local = {"positions": {}, "cash": 100_000.0, "fill_ids": []}
    with pytest.raises(ValueError, match="position_tolerance"):
        worker.reconcile(local, local, position_tolerance=-1.0)
    with pytest.raises(ValueError, match="cash_tolerance"):
        worker.reconcile(local, local, cash_tolerance=float("nan"))


def test_constructor_futures_cash_rejected():
    with pytest.raises(ValueError, match="wallet"):
        worker.LiveRecovery(futures=True, accounting="cash")


def test_constructor_negative_cash_rejected():
    with pytest.raises(ValueError, match="initial_cash"):
        worker.LiveRecovery(initial_cash=-1.0)


def test_resume_without_strategy_rejected(tmp_path):
    rec = worker.LiveRecovery()
    with pytest.raises(ValueError, match="至少一个宿主策略"):
        rec.resume(str(tmp_path / "unused.journal"))


def test_resume_missing_file_rejected(tmp_path):
    rec = worker.LiveRecovery()
    rec.add_strategy(AlwaysBuy(), symbol="BTC/USDT")
    with pytest.raises(ValueError, match="读取 journal 失败"):
        rec.resume(str(tmp_path / "nope.journal"))


def test_resume_corrupt_journal_rejected(tmp_path):
    """中间坏行：预读（Journal::open 全量严格口径）直接报错，不带病进重演。"""
    journal = _record_journal(tmp_path)
    with open(journal, "r+", encoding="utf-8") as f:
        lines = f.readlines()
    lines[2] = "{not json\n"  # 选第 3 行（首行 manifest + 两条 bar 之后）保持行数不变
    with open(journal, "w", encoding="utf-8") as f:
        f.writelines(lines)

    rec = worker.LiveRecovery(initial_cash=100_000.0, interval="1m")
    rec.add_strategy(AlwaysBuy(), symbol="BTC/USDT")
    with pytest.raises(ValueError, match="读取 journal 失败"):
        rec.resume(journal)
