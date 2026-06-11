"""AXON Walk-Forward 类型定义（与 Rust 端对应）。"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class WindowType(Enum):
    """窗口类型。"""

    ROLLING = "rolling"
    EXPANDING = "expanding"


@dataclass
class WalkForwardConfig:
    """Walk-Forward 验证配置。"""

    train_size: int
    test_size: int
    step_size: int
    window_type: WindowType = WindowType.EXPANDING
    validation_size: int = 0
    purge_gap: int = 0
    embargo_pct: float = 0.0

    def validate(self) -> None:
        """校验配置合法性，失败抛 ValueError。"""
        if self.train_size <= 0:
            raise ValueError(f"train_size ({self.train_size}) must be > 0")
        if self.test_size <= 0:
            raise ValueError(f"test_size ({self.test_size}) must be > 0")
        if self.step_size <= 0:
            raise ValueError(f"step_size ({self.step_size}) must be > 0")
        if not 0.0 <= self.embargo_pct <= 1.0:
            raise ValueError(
                f"embargo_pct ({self.embargo_pct}) must be in [0.0, 1.0]"
            )

    @classmethod
    def expanding(cls, train_size: int, test_size: int, step_size: int) -> "WalkForwardConfig":
        return cls(
            train_size=train_size,
            test_size=test_size,
            step_size=step_size,
            window_type=WindowType.EXPANDING,
        )

    @classmethod
    def rolling(cls, train_size: int, test_size: int, step_size: int) -> "WalkForwardConfig":
        return cls(
            train_size=train_size,
            test_size=test_size,
            step_size=step_size,
            window_type=WindowType.ROLLING,
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "train_size": self.train_size,
            "test_size": self.test_size,
            "step_size": self.step_size,
            "window_type": self.window_type.value,
            "validation_size": self.validation_size,
            "purge_gap": self.purge_gap,
            "embargo_pct": self.embargo_pct,
        }


@dataclass
class FoldSplit:
    """单个 fold 的索引分割。"""

    fold_id: int
    train_start: int
    train_end: int
    validation_start: int
    validation_end: int
    test_start: int
    test_end: int

    @property
    def train_size(self) -> int:
        return self.train_end - self.train_start

    @property
    def val_size(self) -> int:
        return self.validation_end - self.validation_start

    @property
    def test_size(self) -> int:
        return self.test_end - self.test_start

    def train_range(self) -> range:
        return range(self.train_start, self.train_end)

    def val_range(self) -> range:
        return range(self.validation_start, self.validation_end)

    def test_range(self) -> range:
        return range(self.test_start, self.test_end)


@dataclass
class ISMetrics:
    """In-Sample 指标。"""

    total_return: float = 0.0
    sharpe_ratio: float = 0.0
    max_drawdown: float = 0.0
    win_rate: float = 0.0
    profit_factor: float = 0.0


@dataclass
class OOSMetrics:
    """Out-of-Sample 指标。"""

    total_return: float = 0.0
    sharpe_ratio: float = 0.0
    max_drawdown: float = 0.0
    win_rate: float = 0.0
    profit_factor: float = 0.0
    calmar_ratio: float = 0.0


@dataclass
class FoldResult:
    """单个 fold 的结果。"""

    fold_id: int
    train_return: float
    validation_return: float
    test_return: float
    test_sharpe: float
    test_max_drawdown: float
    overfit_ratio: float
    train_predictions: Any = None
    test_predictions: Any = None


@dataclass
class AggregatedMetrics:
    """汇总指标。"""

    mean_oos_return: float = 0.0
    std_oos_return: float = 0.0
    mean_oos_sharpe: float = 0.0
    std_oos_sharpe: float = 0.0
    median_oos_return: float = 0.0
    worst_fold_return: float = 0.0
    best_fold_return: float = 0.0
    pct_profitable_folds: float = 0.0


@dataclass
class StabilityMetrics:
    """稳定性指标。"""

    sharpe_of_sharpe: float = 0.0
    return_autocorrelation: float = 0.0
    deflated_sharpe: float = 0.0
    probability_of_loss: float = 0.0


@dataclass
class WalkForwardResult:
    """Walk-Forward 完整结果。"""

    config: WalkForwardConfig
    folds: list[FoldResult] = field(default_factory=list)
    mean_oos_return: float = 0.0
    std_oos_return: float = 0.0
    mean_oos_sharpe: float = 0.0
    stability_score: float = 0.0
