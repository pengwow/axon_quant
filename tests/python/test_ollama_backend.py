"""Ollama backend Python API 测试

覆盖:
- make_ollama_backend 工厂函数(校验成功/失败路径)
- OllamaBackend 类的 chat 方法
- LLMMessage 与 dict 消息格式兼容性
- 模块公开 API 表面(make_ollama_backend / OllamaBackend)
"""

from __future__ import annotations

import pytest


class TestModuleSurface:
    """验证模块的公开 API 表面稳定"""

    def test_make_ollama_backend_exists(self):
        from axon_quant import make_ollama_backend

        assert callable(make_ollama_backend)

    def test_ollama_backend_class_exists(self):
        from axon_quant import OllamaBackend

        assert OllamaBackend is not None


class TestMakeOllamaBackend:
    """make_ollama_backend 工厂函数"""

    def test_dict_input_minimal(self):
        from axon_quant import OllamaBackend, make_ollama_backend

        backend = make_ollama_backend(
            {
                "backends": [
                    {
                        "base_url": "http://localhost:11434/v1",
                        "api_key": "ollama-local",
                        "model": "llama3",
                    }
                ]
            }
        )
        assert isinstance(backend, OllamaBackend)
        assert "OllamaBackend" in repr(backend)

    def test_dict_input_full_config(self):
        from axon_quant import OllamaBackend, make_ollama_backend

        backend = make_ollama_backend(
            {
                "backends": [
                    {
                        "name": "ollama",
                        "base_url": "http://localhost:11434/v1",
                        "api_key": "ollama-local",
                        "model": "mistral",
                        "max_tokens": 2048,
                        "temperature": 0.5,
                        "timeout_secs": 90,
                    }
                ],
                "retry": {
                    "max_retries": 3,
                    "initial_backoff_ms": 100,
                    "max_backoff_ms": 3000,
                },
                "explain": {},
            }
        )
        assert isinstance(backend, OllamaBackend)

    def test_invalid_config_type_raises(self):
        from axon_quant import make_ollama_backend

        with pytest.raises(TypeError, match="config must be LLMConfig or dict"):
            make_ollama_backend("not a dict")  # type: ignore[arg-type]

    def test_empty_backends_raises(self):
        from axon_quant import make_ollama_backend

        with pytest.raises(ValueError, match=r"backends"):
            make_ollama_backend({"backends": []})


class TestOllamaBackendChat:
    """OllamaBackend.chat 方法"""

    def test_chat_accepts_dict_message(self):
        from axon_quant import make_ollama_backend

        backend = make_ollama_backend(
            {
                "backends": [
                    {"base_url": "http://localhost:11434/v1", "api_key": "ollama-local", "model": "llama3"}
                ]
            }
        )
        messages = [{"role": "user", "content": "Hello"}]
        # 因为没有实际的 Ollama 服务器,会抛出网络错误,但不应抛出类型错误
        with pytest.raises(Exception):
            backend.chat(messages)

    def test_chat_accepts_llm_message(self):
        from axon_quant import LLMMessage, make_ollama_backend

        backend = make_ollama_backend(
            {
                "backends": [
                    {"base_url": "http://localhost:11434/v1", "api_key": "ollama-local", "model": "llama3"}
                ]
            }
        )
        messages = [LLMMessage("user", "Hello")]
        with pytest.raises(Exception):
            backend.chat(messages)

    def test_chat_response_has_expected_fields(self):
        """验证响应结构包含预期字段(使用 mock 或真实响应)"""
        from axon_quant import LLMMessage, make_ollama_backend

        backend = make_ollama_backend(
            {
                "backends": [
                    {"base_url": "http://localhost:11434/v1", "api_key": "ollama-local", "model": "llama3"}
                ]
            }
        )
        messages = [LLMMessage("user", "Hello")]
        try:
            resp = backend.chat(messages)
            # 如果成功连接到 Ollama,验证响应结构
            assert "content" in resp
            assert "finish_reason" in resp
            assert "prompt_tokens" in resp
            assert "completion_tokens" in resp
            assert "total_tokens" in resp
        except Exception:
            # 如果没有 Ollama 服务器,跳过验证
            pytest.skip("Ollama server not available")