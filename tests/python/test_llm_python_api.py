"""axon_quant.llm 顶层 API 测试

覆盖:
- LLMConfig dataclass 构造 + to_dict
- LlmMessage 构造 + repr
- make_backend 工厂(校验成功/失败路径)
- load_config_from_toml 加载 + 校验
- module 公开 API 表面(LLMConfig / LlmBackend / LlmMessage / make_backend / load_config_from_toml)
"""

from __future__ import annotations

import os
import tempfile
import textwrap

import pytest

# ──────────────────────────────────────────────────────────────
# 模块可见性 / 公开 API
# ──────────────────────────────────────────────────────────────


class TestModuleSurface:
    """验证模块的公开 API 表面稳定"""

    def test_axon_quant_llm_submodule_exists(self):
        import axon_quant

        assert hasattr(axon_quant, "llm"), "axon_quant.llm submodule missing"
        assert hasattr(axon_quant, "LLMConfig"), "axon_quant.LLMConfig missing"

    def test_top_level_exports_match_module(self):
        from axon_quant import (
            LlmBackend,
            LLMConfig,
            LlmMessage,
            llm,
            load_config_from_toml,
            make_backend,
        )

        # 顶层 re-export 与 submodule 引用一致
        assert llm.LLMConfig is LLMConfig
        assert llm.LlmBackend is LlmBackend
        assert llm.LlmMessage is LlmMessage
        assert llm.make_backend is make_backend
        assert llm.load_config_from_toml is load_config_from_toml


# ──────────────────────────────────────────────────────────────
# LLMConfig dataclass
# ──────────────────────────────────────────────────────────────


class TestLLMConfig:
    """LLMConfig dataclass 行为"""

    def test_minimal_construction(self):
        from axon_quant import LLMConfig

        cfg = LLMConfig(backends=[{"base_url": "https://x/v1", "api_key": "k", "model": "m"}])
        d = cfg.to_dict()
        assert d == {"backends": [{"base_url": "https://x/v1", "api_key": "k", "model": "m"}]}

    def test_retry_and_explain_pass_through(self):
        from axon_quant import LLMConfig

        cfg = LLMConfig(
            backends=[{"base_url": "u", "api_key": "k", "model": "m"}],
            retry={"max_retries": 5, "initial_backoff_ms": 100, "max_backoff_ms": 2000},
            explain={"record_decisions": True, "store_path": "/tmp/x.jsonl"},
        )
        d = cfg.to_dict()
        assert d["retry"]["max_retries"] == 5
        assert d["explain"]["record_decisions"] is True
        assert d["explain"]["store_path"] == "/tmp/x.jsonl"

    def test_to_dict_copies_lists(self):
        """to_dict 必须复制 backends 列表,避免外部修改污染 dataclass 内部状态"""
        from axon_quant import LLMConfig

        backends = [{"base_url": "u", "api_key": "k", "model": "m"}]
        cfg = LLMConfig(backends=backends)
        d = cfg.to_dict()
        d["backends"].append({"base_url": "evil"})
        # 原 cfg 内部状态不应被外部 dict 修改影响
        assert len(cfg.to_dict()["backends"]) == 1


# ──────────────────────────────────────────────────────────────
# LlmMessage
# ──────────────────────────────────────────────────────────────


class TestLlmMessage:
    """LlmMessage 构造 + repr"""

    def test_basic_message(self):
        from axon_quant import LlmMessage

        m = LlmMessage("user", "hello")
        assert repr(m)  # repr 不抛错
        r = repr(m)
        assert "user" in r
        assert "hello" in r

    def test_message_with_tool_call_id(self):
        from axon_quant import LlmMessage

        m = LlmMessage("tool", "result", tool_call_id="abc-123")
        r = repr(m)
        assert "tool_call_id=abc-123" in r


# ──────────────────────────────────────────────────────────────
# make_backend
# ──────────────────────────────────────────────────────────────


class TestMakeBackend:
    """make_backend 工厂函数"""

    def test_dataclass_input(self):
        from axon_quant import LlmBackend, LLMConfig, make_backend

        cfg = LLMConfig(backends=[{"base_url": "https://x/v1", "api_key": "k", "model": "m"}])
        backend = make_backend(cfg)
        assert isinstance(backend, LlmBackend)
        # repr 不抛错,包含可读信息
        assert "LlmBackend" in repr(backend)

    def test_dict_input(self):
        from axon_quant import LlmBackend, make_backend

        backend = make_backend(
            {
                "backends": [
                    {
                        "base_url": "https://x/v1",
                        "api_key": "k",
                        "model": "m",
                        "max_tokens": 256,
                        "temperature": 0.3,
                    }
                ]
            }
        )
        assert isinstance(backend, LlmBackend)

    def test_string_input_raises_type_error(self):
        from axon_quant import make_backend

        with pytest.raises(TypeError, match="must be LLMConfig or dict"):
            make_backend("not a config")  # type: ignore[arg-type]

    def test_int_input_raises_type_error(self):
        from axon_quant import make_backend

        with pytest.raises(TypeError):
            make_backend(123)  # type: ignore[arg-type]

    def test_empty_backends_raises_value_error(self):
        from axon_quant import make_backend

        with pytest.raises(ValueError, match=r"backends"):
            make_backend({"backends": []})

    def test_empty_api_key_raises_value_error(self):
        from axon_quant import make_backend

        with pytest.raises(ValueError, match=r"api_key"):
            make_backend({"backends": [{"base_url": "https://x/v1", "api_key": "", "model": "m"}]})


# ──────────────────────────────────────────────────────────────
# load_config_from_toml
# ──────────────────────────────────────────────────────────────


def _write_toml(content: str) -> str:
    """写入临时 TOML 文件,返回路径(调用方负责 unlink)"""
    f = tempfile.NamedTemporaryFile(mode="w", suffix=".toml", delete=False)
    f.write(textwrap.dedent(content))
    f.close()
    return f.name


class TestLoadConfigFromToml:
    """load_config_from_toml 加载器"""

    def test_minimal_toml(self):
        from axon_quant import load_config_from_toml

        path = _write_toml(
            """
            [[backends]]
            base_url = "https://x/v1"
            api_key = "k"
            model = "m"
            """
        )
        try:
            cfg = load_config_from_toml(path)
            assert len(cfg.backends) == 1
            assert cfg.backends[0]["model"] == "m"
            assert cfg.retry is None
        finally:
            os.unlink(path)

    def test_full_toml(self):
        from axon_quant import load_config_from_toml

        path = _write_toml(
            """
            [[backends]]
            name = "primary"
            base_url = "https://x/v1"
            api_key = "k"
            model = "m"
            max_tokens = 1024
            temperature = 0.5
            timeout_secs = 30

            [retry]
            max_retries = 5
            initial_backoff_ms = 100
            max_backoff_ms = 2000

            [explain]
            record_decisions = true
            store_path = "/tmp/x.jsonl"
            """
        )
        try:
            cfg = load_config_from_toml(path)
            assert cfg.backends[0]["name"] == "primary"
            assert cfg.retry == {
                "max_retries": 5,
                "initial_backoff_ms": 100,
                "max_backoff_ms": 2000,
            }
            assert cfg.explain == {
                "record_decisions": True,
                "store_path": "/tmp/x.jsonl",
            }
        finally:
            os.unlink(path)

    def test_missing_file_raises(self):
        from axon_quant import load_config_from_toml

        with pytest.raises(FileNotFoundError):
            load_config_from_toml("/nonexistent/path/to/config.toml")

    def test_missing_backends_section_raises(self):
        from axon_quant import load_config_from_toml

        path = _write_toml(
            """
            [retry]
            max_retries = 3
            """
        )
        try:
            with pytest.raises(ValueError, match=r"backends"):
                load_config_from_toml(path)
        finally:
            os.unlink(path)

    def test_empty_backends_raises(self):
        from axon_quant import load_config_from_toml

        path = _write_toml(
            """
            backends = []
            """
        )
        try:
            with pytest.raises(ValueError, match=r"backends"):
                load_config_from_toml(path)
        finally:
            os.unlink(path)


# ──────────────────────────────────────────────────────────────
# 集成:make_backend 与 load_config_from_toml 串联
# ──────────────────────────────────────────────────────────────


class TestIntegration:
    """端到端串联:load_config_from_toml → make_backend"""

    def test_toml_to_backend_succeeds(self):
        from axon_quant import LlmBackend, load_config_from_toml, make_backend

        path = _write_toml(
            """
            [[backends]]
            base_url = "https://x/v1"
            api_key = "k"
            model = "m"
            """
        )
        try:
            cfg = load_config_from_toml(path)
            backend = make_backend(cfg)
            assert isinstance(backend, LlmBackend)
        finally:
            os.unlink(path)
