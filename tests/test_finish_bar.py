"""Test finish_bar tool functionality."""

import pytest


class TestFinishBarTool:
    """FinishBarTool 测试类"""

    def test_finish_bar_tool_exists(self):
        """测试 finish_bar_tool 模块可导入"""
        from axon_quant.trading import FinishBarTool
        assert FinishBarTool is not None

    def test_finish_bar_tool_constructs(self):
        """测试 FinishBarTool 构造"""
        from axon_quant.trading import MockTradingBackend, FinishBarTool
        
        backend = MockTradingBackend()
        tool = FinishBarTool(backend)
        
        assert tool is not None
        assert tool.name == "finish_bar"
        assert "结束" in tool.description

    def test_finish_bar_tool_execute(self):
        """测试 FinishBarTool execute 方法"""
        from axon_quant.trading import MockTradingBackend, FinishBarTool
        
        backend = MockTradingBackend()
        tool = FinishBarTool(backend)
        
        result = tool.execute({})
        
        assert result["finished"] is True
        assert result["summary"] == "bar finished"
        assert "timestamp_ms" in result

    def test_finish_bar_tool_execute_with_note(self):
        """测试带备注的 execute"""
        from axon_quant.trading import MockTradingBackend, FinishBarTool
        
        backend = MockTradingBackend()
        tool = FinishBarTool(backend)
        
        result = tool.execute({"note": "end of day"})
        
        assert result["finished"] is True
        assert result["note"] == "end of day"

    def test_finish_bar_tool_empty_args(self):
        """测试空参数"""
        from axon_quant.trading import MockTradingBackend, FinishBarTool
        
        backend = MockTradingBackend()
        tool = FinishBarTool(backend)
        
        result = tool.execute({})
        
        assert result["finished"] is True
        assert "note" not in result or result["note"] is None
