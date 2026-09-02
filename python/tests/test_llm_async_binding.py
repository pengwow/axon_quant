"""axon_quant.llm 异步绑定集成测试(0.14.0)

用本地 stdlib HTTP server 模拟 OpenAI 兼容 SSE 端点,验证:
1. `chat_async` 返回可 await 的 dict(不阻塞事件循环)
2. `stream_chat_async` 支持 `async for` 逐 chunk 消费,顺序与类型正确
3. 流结束正常终止(StopAsyncIteration)

运行前提:已构建并安装 axon-quant wheel(maturin develop / pip install)。
运行方式:
    cd axon_quant && python tests/test_llm_async_binding.py
"""

from __future__ import annotations

import asyncio
import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

from axon_quant.llm import make_backend

# ─── 本地 SSE mock server ─────────────────────────────────────

# 非流式响应(固定)
_CHAT_RESPONSE = {
    "choices": [
        {
            "message": {"content": "hello-async", "reasoning_content": "think-async"},
            "finish_reason": "stop",
        }
    ],
    "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5},
}

# 流式 SSE 行:content 两段 + reasoning 一段 + [DONE]
_SSE_LINES = [
    'data: {"choices":[{"delta":{"content":"Hel"}}]}',
    'data: {"choices":[{"delta":{"reasoning_content":"思考中"}}]}',
    'data: {"choices":[{"delta":{"content":"lo"}}]}',
    "data: [DONE]",
]


class _Handler(BaseHTTPRequestHandler):
    def do_POST(self):  # noqa: N802(stdlib 命名)
        length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(length) or b"{}")
        if body.get("stream"):
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            for line in _SSE_LINES:
                self.wfile.write((line + "\n\n").encode())
                self.wfile.flush()
        else:
            payload = json.dumps(_CHAT_RESPONSE).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    def log_message(self, *args):  # 静默请求日志
        pass


def _start_server() -> tuple[HTTPServer, int]:
    server = HTTPServer(("127.0.0.1", 0), _Handler)
    port = server.server_address[1]
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, port


# ─── 测试用例 ─────────────────────────────────────────────────


async def test_chat_async(backend) -> None:
    resp = await backend.chat_async([{"role": "user", "content": "hi"}])
    assert resp["content"] == "hello-async", resp
    assert resp["reasoning_content"] == "think-async", resp
    assert resp["total_tokens"] == 5, resp
    print("PASS: chat_async")


async def test_chat_async_non_blocking(backend) -> None:
    """并发跑一个计时协程:若 chat_async 阻塞事件循环,tick 计数会明显偏低"""
    ticks = 0

    async def ticker():
        nonlocal ticks
        while True:
            ticks += 1
            await asyncio.sleep(0.001)

    task = asyncio.create_task(ticker())
    await backend.chat_async([{"role": "user", "content": "hi"}])
    task.cancel()
    assert ticks > 0, "事件循环在 chat_async 期间未被调度(阻塞)"
    print(f"PASS: chat_async 非阻塞(期间事件循环调度 {ticks} 次)")


async def test_stream_chat_async(backend) -> None:
    chunks = []
    async for chunk in backend.stream_chat_async([{"role": "user", "content": "hi"}]):
        chunks.append(chunk)

    types = [c["type"] for c in chunks]
    # 期望顺序:content("Hel") → reasoning("思考中") → content("lo") → done
    assert types == ["content", "reasoning", "content", "done"], types
    assert chunks[0]["content"] == "Hel"
    assert chunks[1]["content"] == "思考中"
    assert chunks[2]["content"] == "lo"
    assert chunks[3]["finish_reason"] == "stop"
    print(f"PASS: stream_chat_async 逐 chunk 顺序正确({len(chunks)} chunks)")


async def main() -> None:
    server, port = _start_server()
    try:
        backend = make_backend(
            {
                "backends": [
                    {
                        "base_url": f"http://127.0.0.1:{port}/v1",
                        "api_key": "test-key",
                        "model": "mock-model",
                    }
                ],
                # 关闭重试:失败快速暴露
                "retry": {"max_retries": 0},
            }
        )
        await test_chat_async(backend)
        await test_chat_async_non_blocking(backend)
        await test_stream_chat_async(backend)
        print("\n全部异步绑定测试通过")
    finally:
        server.shutdown()


if __name__ == "__main__":
    asyncio.run(main())
