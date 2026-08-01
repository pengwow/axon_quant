"""SFT 数据格式化（0.11.0 E11）

将 BarEpisode 转换为 chat 格式 JSONL，供 SFT 训练使用。
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import IO, Any

from .filter import BarEpisode

SYSTEM_PROMPT = """You are a quantitative trading agent. Given market bar data (OHLCV), \
decide the trading action: Buy, Sell, or Hold. \
Respond with a JSON object: {"action": "...", "confidence": 0.0-1.0, "reasoning": "..."}"""


def _episode_to_chat(ep: BarEpisode, agent_id: str | None = None) -> dict:
    """将单个 episode 转为 chat 格式样本。

    如果指定 agent_id，只取该 agent 的投票作为 assistant 回复；
    否则取最终决策。
    """
    # User message: bar data
    bar = ep.bar_data
    user_content = json.dumps(
        {
            "open": bar.get("open", 0),
            "high": bar.get("high", 0),
            "low": bar.get("low", 0),
            "close": bar.get("close", 0),
            "volume": bar.get("volume", 0),
            "symbol": bar.get("symbol", "BTCUSDT"),
        },
        ensure_ascii=False,
    )

    # Assistant message
    if agent_id:
        # 取指定 agent 的投票
        vote = next((v for v in ep.votes if v.get("agent_id") == agent_id), None)
        if vote:
            assistant_content = json.dumps(
                {
                    "action": vote.get("action", "Hold"),
                    "confidence": vote.get("confidence", 0.0),
                    "reasoning": vote.get("reasoning", ""),
                },
                ensure_ascii=False,
            )
        else:
            return {}
    else:
        assistant_content = json.dumps(
            {
                "action": ep.final_action,
                "confidence": ep.final_confidence,
                "reasoning": ep.aggregated.get("strategy", "ensemble"),
            },
            ensure_ascii=False,
        )

    return {
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": user_content},
            {"role": "assistant", "content": assistant_content},
        ]
    }


def format_episodes(
    episodes: list[BarEpisode],
    output: str | Path | IO | None = None,
    per_agent: bool = False,
) -> str:
    """将 episodes 格式化为 JSONL 字符串。

    Args:
        episodes: BarEpisode 列表
        output: 输出文件路径或 file-like（None 只返回字符串）
        per_agent: True 时每个 trader 独立成样本

    Returns:
        JSONL 格式字符串
    """
    lines: list[str] = []

    for ep in episodes:
        if per_agent and ep.votes:
            # 每个 agent 独立成样本
            for v in ep.votes:
                aid = v.get("agent_id", "unknown")
                sample = _episode_to_chat(ep, agent_id=aid)
                if sample:
                    lines.append(json.dumps(sample, ensure_ascii=False))
        else:
            sample = _episode_to_chat(ep)
            if sample:
                lines.append(json.dumps(sample, ensure_ascii=False))

    content = "\n".join(lines) + "\n" if lines else ""

    if output is not None:
        if isinstance(output, (str, Path)):
            Path(output).write_text(content)
        else:
            output.write(content)

    return content
