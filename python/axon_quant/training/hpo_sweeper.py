"""RL HPO 胶水(基于 axon-hpo OptunaHPO,0.9.0 D1.5a/b)。

设计:
- 单进程:走 axon-hpo OptunaHPO(in-memory,轻量)
- 多进程:sqlite storage + optuna 直接并发(optuna 内置多 worker 模式)
- TensorBoard:make_tb_log_dir(trial_id) 生成独立 TB 目录
"""
from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING, Any, Callable

if TYPE_CHECKING:
    from axon_hpo.search_space import SearchSpaceDef

# 默认 PPO 搜索空间(lr / gamma / clip_param / entropy_coeff)。
# 类型通过 TYPE_CHECKING 引用,实际使用延迟 import(避免无 axon_hpo 时顶层失败)。
DEFAULT_SEARCH_SPACE: dict[str, "SearchSpaceDef"] = {}  # 运行时由 _ensure_axon_hpo 填充


def _ensure_axon_hpo():
    """延迟 import axon_hpo + 填充 DEFAULT_SEARCH_SPACE(单次)。"""
    global DEFAULT_SEARCH_SPACE  # noqa: PLW0603
    from axon_hpo.optuna_runner import OptunaHPO as _OptunaHPO  # noqa: PLC0415
    from axon_hpo.search_space import SearchSpaceDef as _SSD  # noqa: PLC0415

    if not DEFAULT_SEARCH_SPACE:
        DEFAULT_SEARCH_SPACE.update(
            {
                "lr": _SSD(param_type="log_uniform", low=1e-5, high=1e-3),
                "gamma": _SSD(param_type="uniform", low=0.9, high=0.9999),
                "clip_param": _SSD(param_type="uniform", low=0.1, high=0.4),
                "entropy_coeff": _SSD(param_type="log_uniform", low=1e-4, high=1e-1),
            }
        )
    return _OptunaHPO, _SSD


def make_tb_log_dir(trial_id: int, base: str = "./tb_logs") -> str:
    """根据 trial id 生成独立 TensorBoard 日志目录。

    Returns:
        str: 目录路径(已 mkdir -p)
    """
    tb_dir = Path(base) / f"trial_{trial_id}"
    tb_dir.mkdir(parents=True, exist_ok=True)
    return str(tb_dir)


class RLHPOSweeper:
    """RL 训练的 HPO 胶水。

    包装 axon-hpo 的 OptunaHPO,提供 RL 训练场景的默认搜索空间和便捷 API。
    """

    def __init__(
        self,
        study_name: str,
        n_trials: int = 100,
        search_space: dict[str, "SearchSpaceDef"] | None = None,
        storage: str | None = None,
        n_jobs: int = 1,
    ) -> None:
        """
        Args:
            study_name: Optuna study 名(便于跨进程 sync)
            n_trials: 试验数
            search_space: 自定义搜索空间,None 时用 `DEFAULT_SEARCH_SPACE`
            storage: Optuna storage URL,None 时 in-memory
            n_jobs: 并发 job 数(8-CPU 并发传 8)
        """
        self.study_name = study_name
        self.n_trials = n_trials
        # 触发延迟 import(失败时给出清晰错误)
        _OptunaHPO, _SSD = _ensure_axon_hpo()
        self.search_space = search_space or DEFAULT_SEARCH_SPACE
        self.storage = storage
        self.n_jobs = n_jobs

    def sweep(
        self,
        objective_fn: Callable[[dict[str, Any]], list[float]],
    ) -> dict[str, Any]:
        """运行 sweep,返回 best_config(dict)。

        - n_jobs == 1:走 OptunaHPO(in-memory,轻量)
        - n_jobs > 1:走 optuna 原生 + sqlite storage(支持跨进程并发)

        Args:
            objective_fn: 接收 params dict,返回 [reward] 或 [reward, ...] 列表

        Returns:
            best trial 的 params dict
        """
        if self.n_jobs > 1:
            return self._sweep_parallel(objective_fn)
        return self._sweep_serial(objective_fn)

    def _sweep_serial(
        self,
        objective_fn: Callable[[dict[str, Any]], list[float]],
    ) -> dict[str, Any]:
        OptunaHPO, _ = _ensure_axon_hpo()
        sweeper = OptunaHPO(
            search_space=self.search_space,
            objective_fn=objective_fn,
            study_name=self.study_name,
            directions="maximize",
            storage=self.storage,
        )
        _ = sweeper.run(n_trials=self.n_trials, n_jobs=1)

        best = sweeper.get_best_trial()
        if best is None:
            return {}
        return best.params

    def _sweep_parallel(
        self,
        objective_fn: Callable[[dict[str, Any]], list[float]],
    ) -> dict[str, Any]:
        """并发 sweep:sqlite storage + optuna 内置多 worker 模式。

        optuna 的 n_jobs > 1 模式自动 fork workers,sqlite 跨进程同步 trial 状态。
        """
        import optuna  # 延迟导入,避免硬依赖

        storage = self.storage
        if storage is None:
            # 默认 sqlite 文件,放在 cwd 下
            storage = f"sqlite:///{self.study_name}_optuna.db"

        study = optuna.create_study(
            study_name=self.study_name,
            storage=storage,
            direction="maximize",
            sampler=optuna.samplers.TPESampler(n_startup_trials=20),
            load_if_exists=True,
        )

        # optuna.optimize 在 n_jobs > 1 时 fork workers;objective_fn 必须是 picklable
        study.optimize(
            self._optuna_objective_factory(objective_fn),
            n_trials=self.n_trials,
            n_jobs=self.n_jobs,
        )

        if not study.trials:
            return {}
        return dict(study.best_params)

    def _optuna_objective_factory(
        self,
        objective_fn: Callable[[dict[str, Any]], list[float]],
    ):
        """将 SearchSpaceDef 采样 + 用户 objective_fn 包装为 optuna 目标函数。

        Returns:
            Callable[[optuna.Trial], list[float]]
        """
        import optuna  # 延迟导入

        def _objective(trial: optuna.Trial) -> list[float]:
            params: dict[str, Any] = {}
            for name, space_def in self.search_space.items():
                params[name] = space_def.suggest(trial, name)
            values = objective_fn(params)
            return list(values)

        return _objective
