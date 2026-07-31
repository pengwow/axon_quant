"""常见 RL 搜索空间预设。

针对 PPO / SAC 等主流算法的超参搜索空间。
"""

from __future__ import annotations

from .types import SearchSpaceDef


def default_ppo_search_space() -> dict[str, SearchSpaceDef]:
    """PPO 默认超参数搜索空间。

    包含 PPO 全部主要超参：学习率、gamma、gae_lambda、clip_range、
    entropy/value loss 系数、batch_size、network 结构等。
    """
    return {
        "learning_rate": SearchSpaceDef(param_type="log_uniform", low=1e-5, high=1e-2),
        "gamma": SearchSpaceDef(param_type="uniform", low=0.95, high=0.999),
        "gae_lambda": SearchSpaceDef(param_type="uniform", low=0.9, high=1.0),
        "clip_range": SearchSpaceDef(param_type="uniform", low=0.1, high=0.4),
        "entropy_coef": SearchSpaceDef(param_type="log_uniform", low=1e-4, high=0.05),
        "value_loss_coef": SearchSpaceDef(param_type="uniform", low=0.1, high=1.0),
        "batch_size": SearchSpaceDef(
            param_type="choice", choices=[32, 64, 128, 256, 512]
        ),
        "n_epochs": SearchSpaceDef(param_type="int_uniform", low=3, high=20, step=1),
        "num_layers": SearchSpaceDef(param_type="int_uniform", low=1, high=4, step=1),
        "hidden_size": SearchSpaceDef(
            param_type="choice", choices=[64, 128, 256, 512]
        ),
        "nminibatches": SearchSpaceDef(
            param_type="choice", choices=[4, 8, 16, 32]
        ),
    }


def default_sac_search_space() -> dict[str, SearchSpaceDef]:
    """SAC 默认超参数搜索空间。"""
    return {
        "learning_rate": SearchSpaceDef(param_type="log_uniform", low=1e-5, high=1e-2),
        "gamma": SearchSpaceDef(param_type="uniform", low=0.95, high=0.999),
        "tau": SearchSpaceDef(param_type="uniform", low=0.001, high=0.02),
        "batch_size": SearchSpaceDef(
            param_type="choice", choices=[64, 128, 256, 512]
        ),
        "buffer_size": SearchSpaceDef(
            param_type="choice", choices=[10_000, 100_000, 1_000_000]
        ),
        "learning_starts": SearchSpaceDef(
            param_type="choice", choices=[100, 1000, 10_000]
        ),
        "train_freq": SearchSpaceDef(
            param_type="choice", choices=[1, 4, 8]
        ),
        "gradient_steps": SearchSpaceDef(
            param_type="choice", choices=[1, 2, 4]
        ),
    }


def small_search_space() -> dict[str, SearchSpaceDef]:
    """小型搜索空间（用于快速验证 HPO 流程）。

    只含 2 个参数：learning_rate 与 gamma。约 100 trial 即可看出趋势。
    """
    return {
        "learning_rate": SearchSpaceDef(param_type="log_uniform", low=1e-4, high=1e-2),
        "gamma": SearchSpaceDef(param_type="uniform", low=0.95, high=0.999),
    }
