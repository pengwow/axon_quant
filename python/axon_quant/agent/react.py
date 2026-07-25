"""ReAct Agent Implementation

基于 ReAct 模式的交易 Agent，支持:
1. 思考-行动循环
2. 工具调用
3. 轨迹记录
"""

from __future__ import annotations

import json
import re
from typing import Any, Callable, Dict, List, Optional


class ReActAgent:
    """ReAct 交易 Agent"""

    def __init__(
        self,
        llm_provider: Callable[[str], str],
        tools: List[Dict[str, Any]],
        trajectory_recorder: Optional["TrajectoryRecorder"] = None,
    ):
        self.llm_provider = llm_provider
        self.tools = tools
        self.trajectory_recorder = trajectory_recorder
        self.tool_map = {tool["name"]: tool for tool in tools}

    def build_prompt(self, history: List[Dict[str, Any]], current_observation: str) -> str:
        """构建 ReAct 提示词"""
        tool_descriptions = "\n".join([
            f"- {tool['name']}: {tool['description']}"
            for tool in self.tools
        ])

        history_str = "\n".join([
            f"Thought: {item['thought']}\nAction: {json.dumps(item['action'])}\nObservation: {item['observation']}"
            for item in history
        ])

        prompt = f"""You are a trading agent. Use the following tools:
{tool_descriptions}

Follow the format:
Thought: [your reasoning]
Action: [tool call as JSON]
Observation: [result]

History:
{history_str}

Current observation: {current_observation}

What do you do next?"""

        return prompt

    def parse_response(self, response: str) -> Dict[str, Any]:
        """解析 LLM 响应"""
        thought_match = re.search(r"Thought:\s*(.+?)(?=\nAction:|$)", response, re.DOTALL)
        action_match = re.search(r"Action:\s*({.*?})(?=\nObservation:|$)", response, re.DOTALL)

        thought = thought_match.group(1).strip() if thought_match else ""
        action = json.loads(action_match.group(1)) if action_match else None

        return {"thought": thought, "action": action}

    def run_step(self, history: List[Dict[str, Any]], observation: str) -> Dict[str, Any]:
        """运行单个 ReAct 步骤"""
        prompt = self.build_prompt(history, observation)
        llm_response = self.llm_provider(prompt)
        parsed = self.parse_response(llm_response)

        thought = parsed["thought"]
        action = parsed["action"]
        tool_result = None

        if action and "tool" in action:
            tool_name = action["tool"]
            args = action.get("args", {})

            if tool_name in self.tool_map:
                tool_func = self.tool_map[tool_name]["func"]
                tool_result = tool_func(**args)

        step_record = {
            "thought": thought,
            "action": action,
            "observation": tool_result,
        }

        if self.trajectory_recorder:
            self.trajectory_recorder.record_step(step_record)

        return step_record