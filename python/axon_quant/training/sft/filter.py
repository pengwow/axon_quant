"""Trajectory 质量过滤（0.11.0 E11）

从 trajectory JSON 中筛选高质量 bar episode 用于 SFT 训练。
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any


@dataclass
class FilterConfig:
    """过滤配置。"""

    min_confidence: float = 0.5
    exclude_vetoed: bool = True
    exclude_hold: bool = False
    top_k: int | None = None  # 只保留前 K 个（按 confidence 排序）


@dataclass
class BarEpisode:
    """单 bar 训练样本。"""

    bar_data: dict
    votes: list[dict]
    aggregated: dict
    final_action: str
    final_confidence: float
    risk_approved: bool


def filter_trajectory(path: str | Path, config: FilterConfig | None = None) -> list[BarEpisode]:
    """加载 trajectory 并按质量过滤。

    Args:
        path: trajectory JSON 文件路径
        config: 过滤配置（None 使用默认）

    Returns:
        过滤后的 BarEpisode 列表
    """
    cfg = config or FilterConfig()
    p = Path(path)

    with open(p) as f:
        data = json.load(f)

    bars = data if isinstance(data, list) else data.get("bars", [])
    episodes: list[BarEpisode] = []

    for bar in bars:
        final_action = bar.get("final_action", "Hold")
        final_conf = bar.get("final_confidence", 0.0)
        risk = bar.get("risk_verdict", {})
        approved = risk.get("approved", True)

        # 过滤：被否决的
        if cfg.exclude_vetoed and not approved:
            continue

        # 过滤：Hold 动作
        if cfg.exclude_hold and final_action == "Hold":
            continue

        # 过滤：低置信度
        if final_conf < cfg.min_confidence:
            continue

        episodes.append(
            BarEpisode(
                bar_data=bar.get("bar_data", bar),
                votes=bar.get("votes", []),
                aggregated=bar.get("aggregated", {}),
                final_action=final_action,
                final_confidence=final_conf,
                risk_approved=approved,
            )
        )

    # Top-K
    if cfg.top_k is not None and len(episodes) > cfg.top_k:
        episodes.sort(key=lambda e: e.final_confidence, reverse=True)
        episodes = episodes[: cfg.top_k]

    return episodes
