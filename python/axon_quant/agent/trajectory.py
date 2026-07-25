"""Trajectory Recorder

交易轨迹记录器，支持:
1. 实时记录交易步骤
2. 生成符合 schema 的 JSON
3. 落盘保存
"""

from __future__ import annotations

import json
import os
from typing import Any, Dict, List, Optional


class TrajectoryRecorder:
    """交易轨迹记录器"""

    def __init__(
        self,
        run_id: str,
        instrument: str,
        provider: str,
        model: str,
        seed: int,
        output_dir: str = "output",
    ):
        self.run_id = run_id
        self.instrument = instrument
        self.provider = provider
        self.model = model
        self.seed = seed
        self.output_dir = output_dir
        self.steps: List[Dict[str, Any]] = []
        self.summary: Optional[Dict[str, Any]] = None

    def record_step(self, step: Dict[str, Any]) -> None:
        """记录单个步骤"""
        self.steps.append(step)

    def set_summary(self, summary: Dict[str, Any]) -> None:
        """设置交易总结"""
        self.summary = summary

    def to_dict(self) -> Dict[str, Any]:
        """转换为字典格式"""
        return {
            "version": "0.10.0",
            "run_id": self.run_id,
            "instrument": self.instrument,
            "provider": self.provider,
            "model": self.model,
            "seed": self.seed,
            "bars": self.steps,
            "summary": self.summary,
        }

    def save(self, filename: Optional[str] = None) -> str:
        """保存到文件"""
        os.makedirs(self.output_dir, exist_ok=True)

        if filename is None:
            filename = f"trajectory_{self.seed}.json"

        filepath = os.path.join(self.output_dir, filename)

        with open(filepath, "w") as f:
            json.dump(self.to_dict(), f, indent=2)

        return filepath