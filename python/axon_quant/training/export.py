"""SB3 policy -> ONNX 导出工具(0.9.0 D1.4a)。

用法:
    from axon_quant.training.export import export_onnx
    export_onnx(model, Path("policy.onnx"), obs_sample)

依赖:`stable_baselines3` + `torch`(在 `axon-quant[rl]` extra)。
"""
from __future__ import annotations

from pathlib import Path

import numpy as np


def export_onnx(
    model,  # stable_baselines3.BaseAlgorithm
    path: Path,
    obs_sample: np.ndarray,
) -> Path:
    """将 SB3 policy 导出为 ONNX。

    Args:
        model: SB3 训练好的 model(BaseAlgorithm)
        path: 输出 .onnx 文件路径
        obs_sample: 单条样本 obs(用于 trace shape)

    Returns:
        path(便于链式调用)
    """
    import torch  # 延迟导入,避免硬依赖

    obs_tensor = torch.from_numpy(obs_sample.astype(np.float32)).unsqueeze(0)
    torch.onnx.export(
        model.policy,
        (obs_tensor,),
        str(path),
        input_names=["obs"],
        output_names=["action"],
        dynamic_axes={"obs": {0: "batch"}, "action": {0: "batch"}},
        opset_version=17,
    )
    return path
