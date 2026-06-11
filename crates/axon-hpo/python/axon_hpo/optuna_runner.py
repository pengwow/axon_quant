"""Optuna HPO 主循环封装。

将 Optuna study 的创建、运行、结果收集封装为统一的 `OptunaHPO` 类。
支持：
- 单目标 / 多目标
- TPE / Random / CMA-ES sampler
- Median / Hyperband / SuccessiveHalving pruner
- 中间值报告（用于早停）
- 中途异常转 `TrialPruned`
"""

from __future__ import annotations

import logging
import time
import traceback
from typing import Any, Callable

from .multi_objective import ParetoPoint, compute_hypervolume, compute_pareto_front
from .types import (
    PrunerConfig,
    SamplerConfig,
    SearchSpaceDef,
    StudyDirection,
    TrialResult,
)

logger = logging.getLogger(__name__)


def _build_sampler(sampler_cfg: SamplerConfig) -> Any:
    """构造 Optuna sampler 对象。"""
    import optuna  # noqa: PLC0415

    seed = sampler_cfg.seed
    if sampler_cfg.sampler_type.value == "tpe":
        # optuna 4.x 移除了 `n_warmup_steps` 参数，仅保留 `n_startup_trials` + `seed`
        return optuna.samplers.TPESampler(
            n_startup_trials=sampler_cfg.n_startup_trials,
            seed=seed,
        )
    if sampler_cfg.sampler_type.value == "random":
        return optuna.samplers.RandomSampler(seed=seed)
    if sampler_cfg.sampler_type.value == "cma_es":
        return optuna.samplers.CmaEsSampler(seed=seed)
    if sampler_cfg.sampler_type.value == "grid":
        return optuna.samplers.GridSampler({})  # 空 grid 由调用方填充
    raise ValueError(f"Unknown sampler_type: {sampler_cfg.sampler_type}")


class OptunaHPO:
    """Optuna HPO 执行器。"""

    def __init__(
        self,
        search_space: dict[str, SearchSpaceDef],
        objective_fn: Callable[[dict[str, Any]], list[float]],
        study_name: str,
        directions: list[str] | str = "maximize",
        pruner: PrunerConfig | None = None,
        sampler: SamplerConfig | None = None,
        storage: str | None = None,
    ):
        self.search_space = search_space
        self.objective_fn = objective_fn

        # 校验搜索空间
        for name, sp in search_space.items():
            try:
                sp.validate()
            except ValueError as e:
                raise ValueError(f"search_space[{name}]: {e}") from e

        sampler_cfg = sampler or SamplerConfig()
        self.study = self._create_study(
            study_name=study_name,
            directions=directions,
            pruner=pruner or PrunerConfig(),
            sampler_cfg=sampler_cfg,
            storage=storage,
        )

        # 缓存中间值（按 trial_id → list[(step, value)]）
        self._intermediate: dict[int, list[tuple[int, float]]] = {}

    def _create_study(
        self,
        study_name: str,
        directions: list[str] | str,
        pruner: PrunerConfig,
        sampler_cfg: SamplerConfig,
        storage: str | None,
    ) -> Any:
        """创建 Optuna study。"""
        import optuna  # noqa: PLC0415

        # 归一化 directions
        if isinstance(directions, str):
            opt_directions = [directions]
        else:
            opt_directions = list(directions)

        sampler = _build_sampler(sampler_cfg)
        pruner_obj = pruner.build()

        return optuna.create_study(
            study_name=study_name,
            directions=opt_directions,
            sampler=sampler,
            pruner=pruner_obj,
            storage=storage,
            load_if_exists=True,
        )

    def report(self, trial_id: int, step: int, value: float) -> None:
        """由 `objective_fn` 内部调用，向 Optuna 报告中间值。

        此方法通过 `_intermediate` 缓存中间值，trial 完成后由 `_objective`
        统一设置到 `intermediate_values` 字段。
        """
        self._intermediate.setdefault(trial_id, []).append((step, value))

    def _objective(self, trial: Any) -> list[float]:
        """Optuna 目标函数：从 trial 采样参数，调用用户目标函数。"""
        import optuna  # noqa: PLC0415

        params: dict[str, Any] = {}
        for name, space_def in self.search_space.items():
            params[name] = space_def.suggest(trial, name)

        start = time.monotonic()
        try:
            values = self.objective_fn(params)
        except optuna.TrialPruned:
            raise
        except Exception as e:
            logger.error("Trial %s failed: %s\n%s", trial.number, e, traceback.format_exc())
            raise optuna.TrialPruned(f"Objective raised: {e}") from e
        elapsed_ms = int((time.monotonic() - start) * 1000)
        # 把耗时挂到 trial 上（用户自定义 attribute，Optuna 不直接支持 duration）
        trial.set_user_attr("duration_ms", elapsed_ms)
        return list(values)

    def run(
        self,
        n_trials: int,
        n_jobs: int = 1,
        timeout_seconds: int | None = None,
    ) -> list[TrialResult]:
        """执行 HPO 搜索。"""
        self.study.optimize(
            self._objective,
            n_trials=n_trials,
            n_jobs=n_jobs,
            timeout=timeout_seconds,
        )

        results: list[TrialResult] = []
        for t in self.study.trials:
            duration = t.user_attrs.get("duration_ms", 0)
            intermediate = self._intermediate.get(t.number, [])
            results.append(
                TrialResult(
                    trial_id=t.number,
                    params=dict(t.params),
                    values=list(t.values) if t.values else [],
                    state=t.state.name.lower(),
                    duration_ms=int(duration),
                    intermediate_values=intermediate,
                )
            )
        return results

    def collect_results(self) -> list[dict[str, Any]]:
        """收集当前 study 的所有 trial 结果（dict 格式，供 Rust 端解析）。

        与 `run` 不同，本方法**不**触发新的 trial 搜索，仅返回已有结果。
        由 Rust 端在 `HPORunner.run` 完成后调用。
        """
        results: list[dict[str, Any]] = []
        for t in self.study.trials:
            duration = int(t.user_attrs.get("duration_ms", 0))
            intermediate = self._intermediate.get(t.number, [])
            results.append(
                {
                    "trial_id": t.number,
                    "params": dict(t.params),
                    "values": list(t.values) if t.values else [],
                    "state": t.state.name.lower(),
                    "duration_ms": duration,
                    "intermediate_values": intermediate,
                }
            )
        return results

    def get_best_trial(self) -> TrialResult | None:
        """获取单目标场景下的最佳 trial。"""
        if not self.study.trials:
            return None
        try:
            best = self.study.best_trial
        except ValueError:
            # 多目标场景没有 best_trial
            return None
        return TrialResult(
            trial_id=best.number,
            params=dict(best.params),
            values=list(best.values) if best.values else [],
            state=best.state.name.lower(),
            duration_ms=int(best.user_attrs.get("duration_ms", 0)),
        )

    def get_pareto_front(
        self, directions: list[StudyDirection] | None = None
    ) -> list[ParetoPoint]:
        """获取多目标场景下的 Pareto 前沿。"""
        if directions is None:
            directions = [StudyDirection(d) for d in self.study.directions]
        trials = self.run(n_trials=0) if not self.study.trials else [
            TrialResult(
                trial_id=t.number,
                params=dict(t.params),
                values=list(t.values) if t.values else [],
                state=t.state.name.lower(),
                duration_ms=int(t.user_attrs.get("duration_ms", 0)),
            )
            for t in self.study.trials
        ]
        return compute_pareto_front(trials, directions)

    def compute_hypervolume(
        self,
        reference_point: list[float],
        directions: list[StudyDirection] | None = None,
    ) -> float:
        """计算多目标 Pareto 前沿的超体积。"""
        front = self.get_pareto_front(directions)
        if directions is None:
            directions = [StudyDirection(d) for d in self.study.directions]
        return compute_hypervolume(front, directions, reference_point)
