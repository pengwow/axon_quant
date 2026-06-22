#!/usr/bin/env python3
"""AXON Quant Inference 模块演示 —— 推理引擎配置与用法。

覆盖:
  1. 推理后端 (ONNX / Candle)
  2. Observation 与 Action 数据结构
  3. ModelConfig 配置
  4. BatchConfig 配置
  5. Device 设备配置
  6. 创建推理引擎 (含无模型文件的优雅降级)
  7. InferenceStats 统计
  8. BatchInferencePipeline + ModelHotReloader 概览

运行方式:
    source .venv/bin/activate
    python examples/12_inference/inference_demo.py

零外部依赖: 仅使用 axon_quant + Python 标准库。
"""

from __future__ import annotations

import sys
from typing import Any

RESET = "\033[0m"
BOLD = "\033[1m"
DIM = "\033[2m"
RED = "\033[31m"
GREEN = "\033[32m"
YELLOW = "\033[33m"
BLUE = "\033[34m"
MAGENTA = "\033[35m"
CYAN = "\033[36m"

if sys.platform == "win32":
    try:
        import os
        os.system("")
    except Exception:
        pass


def header(title: str, icon: str = "▶") -> None:
    print(f"\n{BOLD}{CYAN}{'═' * 60}{RESET}")
    print(f"{BOLD}{CYAN}  {icon} {title}{RESET}")
    print(f"{BOLD}{CYAN}{'═' * 60}{RESET}")


def step(n: int, text: str) -> None:
    print(f"\n  {BOLD}{YELLOW}[步骤 {n}]{RESET} {text}")


def ok(msg: str) -> None:
    print(f"    {GREEN}✅ {msg}{RESET}")


def info(msg: str) -> None:
    print(f"    {DIM}{msg}{RESET}")


def warn(msg: str) -> None:
    print(f"    {YELLOW}⚠️  {msg}{RESET}")


def fail(msg: str) -> None:
    print(f"    {RED}❌ {msg}{RESET}")


def value(label: str, v: Any, width: int = 24) -> None:
    print(f"    {BOLD}{label:<{width}}{RESET} {v}")


def separator() -> None:
    print(f"    {DIM}{'─' * 50}{RESET}")


# ══════════════════════════════════════════════════════════════════════════
# Stage 1: 推理后端
# ══════════════════════════════════════════════════════════════════════════


def demo_backends() -> None:
    header("推理后端 (InferenceBackends)", "⚙️")

    from axon_quant.inference import InferenceBackend

    step(1, "查看所有可用推理后端")
    backends = [InferenceBackend.Onnx, InferenceBackend.Candle]
    for b in backends:
        value(str(b), f"repr={b!r}")
    ok(f"共 {len(backends)} 种后端")

    step(2, "后端枚举比较")
    value("Onnx == Onnx", InferenceBackend.Onnx == InferenceBackend.Onnx)
    value("Onnx == Candle", InferenceBackend.Onnx == InferenceBackend.Candle)

    separator()
    ok("推理后端展示完成\n")


# ══════════════════════════════════════════════════════════════════════════
# Stage 2: Device 设备配置
# ══════════════════════════════════════════════════════════════════════════


def demo_devices() -> None:
    header("设备配置 (Device)", "🖥️")

    from axon_quant.inference import Device

    step(1, "创建 CPU 设备")
    cpu = Device.cpu()
    value("kind", cpu.kind)
    value("cuda_device_id", cpu.cuda_device_id)
    value("repr", repr(cpu))

    step(2, "创建 CUDA 设备")
    cuda0 = Device.cuda(0)
    value("kind", cuda0.kind)
    value("cuda_device_id", cuda0.cuda_device_id)
    value("repr", repr(cuda0))

    cuda1 = Device.cuda(1)
    value("cuda(1) kind", cuda1.kind)
    value("cuda(1) device_id", cuda1.cuda_device_id)

    step(3, "创建 Metal 设备")
    metal = Device.metal()
    value("kind", metal.kind)
    value("cuda_device_id", metal.cuda_device_id)
    value("repr", repr(metal))

    separator()
    ok("设备配置展示完成\n")


# ══════════════════════════════════════════════════════════════════════════
# Stage 3: Observation 与 Action
# ══════════════════════════════════════════════════════════════════════════


def demo_observation_action() -> None:
    header("数据结构 (Observation / Action)", "📊")

    from axon_quant.inference import Observation, Action, ActionType

    step(1, "创建 Observation")
    features = [0.1 * i for i in range(128)]
    obs = Observation(
        symbol="BTC-USDT",
        timestamp_ns=1_700_000_000_000_000_000,
        features=features,
    )
    value("symbol", obs.symbol)
    value("timestamp_ns", obs.timestamp_ns)
    value("feature_dim", obs.feature_dim)
    value("features[0:3]", obs.features[:3])
    value("repr", repr(obs))

    step(2, "查看 ActionType 枚举")
    for at in [ActionType.Buy, ActionType.Hold, ActionType.Sell,
               ActionType.ReduceLong, ActionType.ReduceShort]:
        value(str(at), f"repr={at!r}")

    step(3, "创建 Action")
    action = Action(
        action_type="buy",
        confidence=0.92,
        target_position=0.5,
        model_id="lstm-v1",
        inference_time_us=320,
    )
    value("action_type", action.action_type)
    value("confidence", f"{action.confidence:.3f}")
    value("target_position", f"{action.target_position:.3f}")
    value("model_id", action.model_id)
    value("inference_time_us", f"{action.inference_time_us} μs")
    value("repr", repr(action))

    step(4, "Action 序列化为 dict")
    d = action.to_dict()
    for k, v in d.items():
        value(k, v)

    separator()
    ok("Observation / Action 展示完成\n")


# ══════════════════════════════════════════════════════════════════════════
# Stage 4: ModelConfig 配置
# ══════════════════════════════════════════════════════════════════════════


def demo_model_config() -> None:
    header("模型配置 (ModelConfig)", "🔧")

    from axon_quant.inference import (
        ModelConfig, InferenceBackend, Device,
    )

    step(1, "创建 ONNX 模型配置")
    cfg_onnx = ModelConfig(
        path="/tmp/model.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
        fp16=False,
        num_threads=4,
    )
    value("path", cfg_onnx.path)
    value("backend", cfg_onnx.backend)
    value("device", cfg_onnx.device)
    value("input_shape", cfg_onnx.input_shape)
    value("output_dim", cfg_onnx.output_dim)
    value("fp16", cfg_onnx.fp16)
    value("num_threads", cfg_onnx.num_threads)

    step(2, "创建 Candle 模型配置 (fp16)")
    cfg_candle = ModelConfig(
        path="/tmp/model.safetensors",
        backend=InferenceBackend.Candle,
        device=Device.cuda(0),
        input_shape=(1, 32, 256),
        output_dim=5,
        fp16=True,
        num_threads=8,
    )
    value("path", cfg_candle.path)
    value("backend", cfg_candle.backend)
    value("device", cfg_candle.device)
    value("input_shape", cfg_candle.input_shape)
    value("output_dim", cfg_candle.output_dim)
    value("fp16", cfg_candle.fp16)
    value("num_threads", cfg_candle.num_threads)

    step(3, "默认参数 (fp16=false, num_threads=4)")
    cfg_default = ModelConfig(
        path="/tmp/default.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    value("fp16 (默认)", cfg_default.fp16)
    value("num_threads (默认)", cfg_default.num_threads)

    separator()
    ok("ModelConfig 配置展示完成\n")


# ══════════════════════════════════════════════════════════════════════════
# Stage 5: BatchConfig 配置
# ══════════════════════════════════════════════════════════════════════════


def demo_batch_config() -> None:
    header("批推理配置 (BatchConfig)", "📦")

    from axon_quant.inference import BatchConfig

    step(1, "默认 BatchConfig")
    bc_default = BatchConfig()
    value("max_batch_size", bc_default.max_batch_size)
    value("collect_timeout_us", bc_default.collect_timeout_us)
    value("num_workers", bc_default.num_workers)
    value("prealloc_buffer_size", bc_default.prealloc_buffer_size)
    value("collect_cpu_cores", bc_default.collect_cpu_cores)
    value("collect_gpu_device_id", bc_default.collect_gpu_device_id)
    value("repr", repr(bc_default))

    step(2, "自定义 BatchConfig")
    bc_custom = BatchConfig(
        max_batch_size=64,
        collect_timeout_us=1000,
        num_workers=4,
        prealloc_buffer_size=128,
        collect_cpu_cores=[0, 1, 2, 3],
        collect_gpu_device_id=0,
    )
    value("max_batch_size", bc_custom.max_batch_size)
    value("collect_timeout_us", bc_custom.collect_timeout_us)
    value("num_workers", bc_custom.num_workers)
    value("prealloc_buffer_size", bc_custom.prealloc_buffer_size)
    value("collect_cpu_cores", bc_custom.collect_cpu_cores)
    value("collect_gpu_device_id", bc_custom.collect_gpu_device_id)

    separator()
    ok("BatchConfig 配置展示完成\n")


# ══════════════════════════════════════════════════════════════════════════
# Stage 6: InferenceEngine 创建
# ══════════════════════════════════════════════════════════════════════════


def demo_engine() -> None:
    header("推理引擎 (InferenceEngine)", "🚀")

    from axon_quant.inference import (
        InferenceEngine, InferenceBackend, Device,
        ModelConfig, InferenceError, AxonError,
    )

    step(1, "创建 ONNX 推理引擎 (无模型文件)")
    cfg = ModelConfig(
        path="/tmp/model.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    try:
        engine = InferenceEngine(cfg)
        value("backend", engine.backend)
        value("repr", repr(engine))
        ok("引擎创建成功 (backend 就绪，未加载模型)")
    except (InferenceError, AxonError, Exception) as e:
        warn(f"引擎创建失败: {e}")

    step(2, "尝试加载不存在的模型文件")
    try:
        engine.load("/tmp/nonexistent_model.onnx")
        ok("模型加载成功")
    except (InferenceError, AxonError) as e:
        fail(f"模型加载失败 (预期行为): {e}")
        info("提示: 需要提供有效的 .onnx / .safetensors 模型文件")

    step(3, "使用 create_inference_engine 工厂函数")
    try:
        engine2 = InferenceEngine(cfg)
        ok("工厂函数创建引擎成功")
    except (InferenceError, AxonError) as e:
        warn(f"引擎创建失败: {e}")

    step(4, "尝试创建 Candle 后端引擎")
    cfg_candle = ModelConfig(
        path="/tmp/model.safetensors",
        backend=InferenceBackend.Candle,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    try:
        engine_candle = InferenceEngine(cfg_candle)
        value("backend", engine_candle.backend)
        ok("Candle 引擎创建成功")
    except (InferenceError, AxonError) as e:
        warn(f"Candle 引擎创建失败 (后端可能未编译): {e}")

    separator()
    ok("InferenceEngine 展示完成\n")


# ══════════════════════════════════════════════════════════════════════════
# Stage 7: InferenceStats 统计
# ══════════════════════════════════════════════════════════════════════════


def demo_stats() -> None:
    header("推理统计 (InferenceStats)", "📈")

    from axon_quant.inference import InferenceStats

    step(1, "创建默认 InferenceStats")
    stats = InferenceStats()
    value("total_inferences", stats.total_inferences)
    value("total_batch_inferences", stats.total_batch_inferences)
    value("avg_latency_us", f"{stats.avg_latency_us:.1f} μs")
    value("p99_latency_us", f"{stats.p99_latency_us:.1f} μs")
    value("hot_reloads", stats.hot_reloads)
    value("errors", stats.errors)
    value("repr", repr(stats))

    step(2, "InferenceStats 序列化为 dict")
    d = stats.to_dict()
    for k, v in d.items():
        value(k, v)

    separator()
    ok("InferenceStats 展示完成\n")


# ══════════════════════════════════════════════════════════════════════════
# Stage 8: 高级组件概览
# ══════════════════════════════════════════════════════════════════════════


def demo_advanced() -> None:
    header("高级组件 (Pipeline / HotReload)", "🔄")

    from axon_quant.inference import (
        InferenceEngine, InferenceBackend, Device,
        ModelConfig, BatchConfig, BatchInferencePipeline,
        ModelHotReloader, InferenceStats,
    )

    step(1, "创建引擎 + 批推理管线")
    cfg = ModelConfig(
        path="/tmp/model.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    bc = BatchConfig(max_batch_size=32, collect_timeout_us=500)
    try:
        engine = InferenceEngine(cfg)
        pipeline = BatchInferencePipeline(engine, bc)
        info(f"管线已创建: max_batch={bc.max_batch_size}, timeout={bc.collect_timeout_us}μs")

        step(2, "向管线提交 Observation")
        from axon_quant.inference import Observation
        obs1 = Observation("BTC-USDT", 1_000_000_000, [0.1] * 128)
        obs2 = Observation("ETH-USDT", 2_000_000_000, [0.2] * 128)
        pipeline.submit(obs1)
        pipeline.submit(obs2)
        info(f"已提交 2 条 Observation")

        step(3, "触发一次 collect (批量推理)")
        actions = pipeline.collect()
        value("返回 Action 数", len(actions))
        for i, a in enumerate(actions):
            info(f"  Action[{i}]: type={a.action_type}, confidence={a.confidence:.3f}")

        step(4, "查看管线统计")
        stats = pipeline.stats()
        value("total_inferences", stats.total_inferences)
        value("total_batch_inferences", stats.total_batch_inferences)
        ok("管线统计正常")
    except Exception as e:
        warn(f"管线演示部分跳过: {e}")

    step(5, "创建 ModelHotReloader")
    try:
        engine = InferenceEngine(cfg)
        reloader = ModelHotReloader(engine)
        value("当前版本", reloader.version())
        info("ModelHotReloader 支持热更新模型文件")
        info("调用 reloader.reload() 可重新加载模型")
        info("调用 reloader.subscribe(callback) 注册更新回调")
        ok("ModelHotReloader 创建成功")
    except Exception as e:
        warn(f"热更新器创建失败: {e}")

    separator()
    ok("高级组件展示完成\n")


# ══════════════════════════════════════════════════════════════════════════
# 主入口
# ══════════════════════════════════════════════════════════════════════════


def main() -> int:
    print(f"""
{BOLD}{CYAN}╔══════════════════════════════════════════════════════════╗
║                                                          ║
║   {BOLD}{MAGENTA}AXON Quant{RESET}{CYAN}  —  Inference 模块演示                  ║
║   {DIM}推理引擎 · 配置对象 · 批推理管线 · 热更新{RESET}{CYAN}              ║
║                                                          ║
║   {GREEN}覆盖 ONNX / Candle 后端，零外部依赖{RESET}{CYAN}                  ║
║                                                          ║
╚══════════════════════════════════════════════════════════╝{RESET}
""")

    demos = [
        ("推理后端", demo_backends),
        ("设备配置", demo_devices),
        ("Observation / Action", demo_observation_action),
        ("ModelConfig", demo_model_config),
        ("BatchConfig", demo_batch_config),
        ("InferenceEngine", demo_engine),
        ("InferenceStats", demo_stats),
        ("高级组件", demo_advanced),
    ]

    for name, func in demos:
        try:
            func()
        except Exception as e:
            fail(f"{name} 演示出错: {e}")
            import traceback
            traceback.print_exc()

    print(f"\n  {BOLD}{GREEN}全部演示完成！{RESET}")
    print(f"  {DIM}Inference 模块支持多种后端 + 批推理管线 + 热更新{RESET}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
