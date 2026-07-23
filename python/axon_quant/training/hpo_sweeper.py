"""RL HPO 胶水(基于 axon-hpo OptunaHPO,0.9.0 D1.5a)。

设计:8-CPU 并发 sweep,100 trial x 50K timesteps ~= 1-1.5h wall time。
"""
from __future__ import annotations

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

        Args:
            objective_fn: 接收 params dict,返回 [reward] 或 [reward, ...] 列表

        Returns:
            best trial 的 params dict
        """
        OptunaHPO, _ = _ensure_axon_hpo()
        sweeper = OptunaHPO(
            search_space=self.search_space,
            objective_fn=objective_fn,
            study_name=self.study_name,
            directions="maximize",
            storage=self.storage,
        )
        _ = sweeper.run(n_trials=self.n_trials, n_jobs=self.n_jobs)

        best = sweeper.get_best_trial()
        if best is None:
            return {}
        return best.params
