"""AXON HPO 类型定义。

所有类型可与 Rust 端（`axon_hpo` crate）通过 JSON 互转。
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any, ClassVar


class StudyDirection(Enum):
    """Study 优化方向。"""

    MINIMIZE = "minimize"
    MAXIMIZE = "maximize"

    @property
    def is_maximize(self) -> bool:
        """是否最大化。"""
        return self == StudyDirection.MAXIMIZE


class SamplerType(Enum):
    """Sampler 类型。"""

    TPE = "tpe"
    RANDOM = "random"
    CMA_ES = "cma_es"
    GRID = "grid"


class PrunerType(Enum):
    """Pruner 类型。"""

    MEDIAN = "median"
    HYPERBAND = "hyperband"
    SUCCESSIVE_HALVING = "successive_halving"
    NOP = "none"


@dataclass
class SearchSpaceDef:
    """搜索空间参数定义。

    6 种参数类型：
    - `"uniform"`：浮点均匀分布
    - `"log_uniform"`：浮点对数均匀分布（low > 0）
    - `"int_uniform"`：整数均匀分布
    - `"discrete"`：离散浮点列表（optuna 的 `suggest_float` + step）
    - `"choice"`：离散字符串列表
    - `"categorical"`：任意 JSON 值列表
    """

    param_type: str
    low: float | None = None
    high: float | None = None
    step: float | None = None
    choices: list[Any] | None = None
    log: bool = False

    PARAM_TYPES: ClassVar[tuple[str, ...]] = (
        "uniform",
        "log_uniform",
        "int_uniform",
        "discrete",
        "choice",
        "categorical",
    )

    def __post_init__(self) -> None:
        if self.param_type not in self.PARAM_TYPES:
            raise ValueError(
                f"param_type 必须是 {self.PARAM_TYPES} 之一，得到：{self.param_type}"
            )

    def validate(self) -> None:
        """校验参数空间合法性，失败抛 ValueError。"""
        if self.param_type in ("uniform", "log_uniform", "int_uniform", "discrete"):
            if self.low is None or self.high is None:
                raise ValueError(f"{self.param_type}: 必须指定 low / high")
            if self.low >= self.high:
                raise ValueError(f"{self.param_type}: low ({self.low}) 必须 < high ({self.high})")
            if self.param_type == "log_uniform" and self.low <= 0:
                raise ValueError(f"log_uniform: low ({self.low}) 必须 > 0")
        elif self.param_type in ("choice", "categorical"):
            if not self.choices:
                raise ValueError(f"{self.param_type}: choices 不能为空")

    def suggest(self, trial: Any, name: str) -> Any:
        """在 Optuna trial 中采样参数。

        Args:
            trial: optuna.Trial 实例
            name: 参数名

        Returns:
            采样得到的参数值
        """
        if self.param_type == "uniform":
            return trial.suggest_float(name, self.low, self.high, step=self.step)
        if self.param_type == "log_uniform":
            return trial.suggest_float(name, self.low, self.high, log=True)
        if self.param_type == "int_uniform":
            step = int(self.step) if self.step else 1
            return trial.suggest_int(name, int(self.low), int(self.high), step=step)
        if self.param_type == "discrete":
            return trial.suggest_float(name, self.low, self.high, step=self.step)
        if self.param_type == "choice":
            return trial.suggest_categorical(name, self.choices)
        if self.param_type == "categorical":
            return trial.suggest_categorical(name, self.choices)
        raise ValueError(f"Unknown param_type: {self.param_type}")

    def to_dict(self) -> dict[str, Any]:
        """转为 Rust 端 `SearchSpaceDef` 兼容的 dict。"""
        out: dict[str, Any] = {"type": self.param_type}
        if self.low is not None:
            out["low"] = self.low
        if self.high is not None:
            out["high"] = self.high
        if self.step is not None:
            out["step"] = self.step
        if self.choices is not None:
            out["choices"] = self.choices
        return out


@dataclass
class SamplerConfig:
    """Sampler 配置。"""

    sampler_type: SamplerType = SamplerType.TPE
    seed: int | None = None
    n_startup_trials: int = 10
    n_warmup_steps: int = 0

    def to_dict(self) -> dict[str, Any]:
        """转为 TOML 兼容的 dict。"""
        out: dict[str, Any] = {
            "sampler_type": self.sampler_type.value,
        }
        if self.seed is not None:
            out["seed"] = self.seed
        if self.sampler_type == SamplerType.TPE:
            out["n_startup_trials"] = self.n_startup_trials
            out["n_warmup_steps"] = self.n_warmup_steps
        return out


@dataclass
class PrunerConfig:
    """Pruner 配置。"""

    pruner_type: PrunerType = PrunerType.MEDIAN
    n_startup_trials: int = 5
    n_warmup_steps: int = 0
    reduction_factor: float = 3.0
    min_resource: int = 1
    max_resource: int = 100

    def build(self) -> Any:
        """构造 Optuna pruner 对象。"""
        import optuna  # noqa: PLC0415

        if self.pruner_type == PrunerType.MEDIAN:
            return optuna.pruners.MedianPruner(
                n_startup_trials=self.n_startup_trials,
                n_warmup_steps=self.n_warmup_steps,
            )
        if self.pruner_type == PrunerType.HYPERBAND:
            return optuna.pruners.HyperbandPruner(
                min_resource=self.min_resource,
                max_resource=self.max_resource,
                reduction_factor=self.reduction_factor,
            )
        if self.pruner_type == PrunerType.SUCCESSIVE_HALVING:
            return optuna.pruners.SuccessiveHalvingPruner(
                min_resource=self.min_resource,
                reduction_factor=self.reduction_factor,
            )
        return optuna.pruners.NopPruner()

    def to_dict(self) -> dict[str, Any]:
        """转为 TOML 兼容的 dict。"""
        out: dict[str, Any] = {"pruner_type": self.pruner_type.value}
        if self.pruner_type == PrunerType.MEDIAN:
            out["n_startup_trials"] = self.n_startup_trials
            out["n_warmup_steps"] = self.n_warmup_steps
        elif self.pruner_type == PrunerType.HYPERBAND:
            out["reduction_factor"] = self.reduction_factor
        elif self.pruner_type == PrunerType.SUCCESSIVE_HALVING:
            out["min_resource"] = self.min_resource
            out["reduction_factor"] = self.reduction_factor
        return out


@dataclass
class TrialResult:
    """单次 Trial 的结果。"""

    trial_id: int
    params: dict[str, Any]
    values: list[float]
    state: str  # "complete" | "pruned" | "fail" | "running"
    duration_ms: int
    intermediate_values: list[tuple[int, float]] = field(default_factory=list)
