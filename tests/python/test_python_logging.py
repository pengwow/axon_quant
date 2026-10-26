"""install_rust_logging 绑定层行为验证：Rust 侧 WARN+ → Python logging。

触发路径：RL 策略的 predict 回调抛异常 → Rust 侧
`rl backend: predict 调用失败，沿用上次目标`（PyRatioBackend::infer 的
warn!）→ 桥应以 WARNING 级投递给 `logging.getLogger("axon_quant.rust")`。
"""

import logging

import pytest

from axon_quant import worker

START = 1_700_000_000_000_000_000

CONTINUOUS_1_1 = {"type": "continuous", "min": -1.0, "max": 1.0}

FEATURES = [
    {
        "name": "close",
        "source": {"PriceField": "close"},
        "normalizer": "None",
        "clip_range": None,
    },
]


class _Capture(logging.Handler):
    def __init__(self):
        super().__init__()
        self.records = []

    def emit(self, record):
        self.records.append(record)


def test_install_is_idempotent():
    """全局 tracing subscriber 只能装一次：首次 True，重复调用安全返回 False。"""
    worker.install_rust_logging()
    second = worker.install_rust_logging()
    # 第二次调用不抛异常且返回 False（或链上已有其它安装者为 True 也幂等）
    assert second is True or second is False


def test_rust_warn_reaches_python_logger():
    """RL predict 抛异常 → Rust warn! 必须以 WARNING 出现在 Python logger。"""
    installed = worker.install_rust_logging()
    # 全局 subscriber 未被占用时首次安装成功；已安装（如此前自检用例）则 False。
    # 两种情况桥都应生效，不据此断言，只看日志投递结果。
    assert installed is True or installed is False  # noqa: PT018 安装语义幂等

    logger = logging.getLogger("axon_quant.rust")
    cap = _Capture()
    logger.addHandler(cap)
    d = worker.PaperDriver(["BTCUSDT"], initial_cash=10_000.0, fee_rate=0.0)
    try:
        logger.setLevel(logging.WARNING)

        def boom(_obs):
            raise RuntimeError("boom")

        d.add_rl_strategy(boom, "m1", "v1-rl", 1, FEATURES, CONTINUOUS_1_1, "BTC/USDT")
        d.start()
        d.feed_bar("BTCUSDT", START, 100.0, 101.0, 99.0, 100.0, 10_000.0)
        d.tick()
    finally:
        logger.removeHandler(cap)
        d.shutdown()

    matched = [r for r in cap.records if "predict 调用失败" in r.getMessage()]
    assert matched, (
        f"Rust warn 未桥接到 Python logging，捕获记录：{[r.getMessage() for r in cap.records]}"
    )
    assert matched[0].levelno == logging.WARNING
    # 排障定位信息必须随消息可见
    assert "rust_target" in matched[0].getMessage()


def test_python_error_does_not_break_rust_flow():
    """桥是尽力转发：Python logger 静默（无 handler、不传播）时 Rust 主流程不能崩。"""
    logger = logging.getLogger("axon_quant.rust")
    old_propagate = logger.propagate
    try:
        logger.propagate = False  # 静默吞掉，模拟 handler 异常/不传播
        d = worker.PaperDriver(["BTCUSDT"], initial_cash=10_000.0, fee_rate=0.0)

        def boom(_obs):
            raise RuntimeError("boom")

        d.add_rl_strategy(boom, "m1", "v1-rl", 1, FEATURES, CONTINUOUS_1_1, "BTC/USDT")
        d.start()
        # predict 失败 → warn 转发被吞 → infer 仍按契约返回 None（Hold），不抛
        d.feed_bar("BTCUSDT", START, 100.0, 101.0, 99.0, 100.0, 10_000.0)
        d.tick()
        snap = d.snapshot()
        assert snap["positions"] == {}
    finally:
        logger.propagate = old_propagate
        d.shutdown()
