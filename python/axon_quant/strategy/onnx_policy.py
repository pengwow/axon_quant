"""OnnxPolicyStrategy — ONNX policy -> BaseStrategy 适配(0.9.0 D1.4c)。

部署时使用:加载 SB3 导出的 .onnx,接收 obs,返回 action。
基于 onnxruntime 直接调用(轻量、零硬依赖 import 路径)。
"""
from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING, Any

import numpy as np

from axon_quant.strategy.base import BaseStrategy

if TYPE_CHECKING:
    from axon_quant.env import LegSpec


class OnnxPolicyStrategy(BaseStrategy):
    """ONNX policy -> BaseStrategy 适配。"""

    def __init__(
        self,
        onnx_path: Path,
        leg_specs: list["LegSpec"],
        providers: list[str] | None = None,
    ) -> None:
        # 延迟导入 onnxruntime,避免硬依赖
        import onnxruntime as ort  # noqa: PLC0415

        providers = providers or ["CPUExecutionProvider"]
        self.session = ort.InferenceSession(str(onnx_path), providers=providers)
        self.input_name = self.session.get_inputs()[0].name
        self.output_name = self.session.get_outputs()[0].name
        self.leg_specs = leg_specs

    def predict(self, obs: np.ndarray) -> np.ndarray:
        """单条 obs -> action(连续多 leg)。"""
        obs_batch = obs.astype(np.float32).reshape(1, -1)
        outputs = self.session.run([self.output_name], {self.input_name: obs_batch})
        return outputs[0][0]

    def on_bar(self, bar: Any, ctx: Any) -> list[Any]:
        """BaseStrategy 接口实现。

        0.9.0 简化:返回空 list,完整 on_bar 实施见 D1.4d 后续迭代
        (T18 主验收脚本直接调 predict,不走 on_bar 完整路径)。
        """
        return []

    def on_fill(self, fill: Any, ctx: Any) -> list[Any]:
        return []
