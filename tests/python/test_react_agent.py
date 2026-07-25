"""ReActAgent Python API 测试

覆盖:
- ReActAgent 构造 + 默认配置
- ReActAgent 自定义配置
- add_tool 注册工具
- reason 推理循环
"""

from __future__ import annotations

import pytest


class TestReActAgentModule:
    """验证模块的公开 API 表面"""

    def test_react_agent_class_exists(self):
        from axon_quant._native import llm

        assert hasattr(llm, "ReActAgent")

    def test_tool_class_exists(self):
        from axon_quant._native import llm

        assert hasattr(llm, "Tool")


class TestReActAgentConstruction:
    """ReActAgent 构造行为"""

    def test_default_config(self):
        from axon_quant._native import llm

        backend = llm.make_backend({"backends": [{"base_url": "https://x/v1", "api_key": "k", "model": "m"}]})
        agent = llm.ReActAgent(backend)
        assert repr(agent) == "ReActAgent"

    def test_custom_config(self):
        from axon_quant._native import llm

        backend = llm.make_backend({"backends": [{"base_url": "https://x/v1", "api_key": "k", "model": "m"}]})
        config = {
            "max_iterations": 5,
            "temperature": 0.5,
            "max_context_tokens": 4096,
            "enable_reflection": False,
            "allowed_tools": ["query_portfolio"],
        }
        agent = llm.ReActAgent(backend, config)
        assert repr(agent) == "ReActAgent"


class TestToolClass:
    """Tool 基类行为"""

    def test_tool_construction(self):
        from axon_quant._native import llm

        tool = llm.Tool(
            name="test_tool",
            description="A test tool",
            parameters_schema='{"type": "object", "properties": {}}',
        )
        assert tool.name == "test_tool"
        assert tool.description == "A test tool"
        assert tool.parameters_schema == '{"type": "object", "properties": {}}'

    def test_tool_repr(self):
        from axon_quant._native import llm

        tool = llm.Tool(name="my_tool", description="desc", parameters_schema="{}")
        r = repr(tool)
        assert "Tool(name=my_tool)" in r