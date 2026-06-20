"""axon_quant.inference 端到端测试(L3 Python E2E,Stage 6)。

覆盖范围:
1. 类型导入 / 实例化(15 个核心符号 + 2 个工厂函数)
2. InferenceBackend 枚举(Onnx / Tch / Candle)
3. Device 枚举(Cpu / Cuda / Metal)
4. ModelConfig 字段透传 + 必填校验
5. Observation / Action / ActionType 数据类
6. BatchConfig 字段透传
7. InferenceStats 默认全 0
8. InferenceEngine 构造(Onnx 成功 / Tch 错误 / Candle 按 feature 分支)
9. InferenceEngine.load(路径不存在 → ModelNotFound)
10. InferenceEngine.infer(未 load → 错误)
11. InferenceEngine.infer_batch(空列表 → 立即返回 [])
12. BatchInferencePipeline 构造 + submit / pending / collect / stats
13. ModelHotReloader.__new__(Stage 6 暂不可用 → 明确错误)
14. InferenceError 错误码(继承 PyException,不继承 AxonError)
15. create_onnx_engine 工厂(一步创建 + 加载)
16. create_candle_engine 工厂(Candle feature 关闭时返回 InferenceError)

运行::

    cd /Users/liupeng/workspace/quant/axon
    PYO3_PYTHON=/Users/liupeng/workspace/quant/axon/.venv/bin/python \\
        python -m pytest python/tests/test_inference_e2e.py -v

**注意**:
- 默认**所有**测试都跑(无需真实模型文件),仅依赖 mock config
- 真实 ONNX 模型加载 / 推理不在本文件范围(避免 CI 拉模型)
- 需先 build wheel(参见 Makefile 的 ``python-build`` / ``python-develop`` 目标)
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

# 强制使用本项目 venv(避免 miniconda pyarrow / numpy 干扰)
_VENV_SITE = Path("/Users/liupeng/workspace/quant/axon/.venv/lib/python3.14/site-packages")
if _VENV_SITE.exists() and str(_VENV_SITE) not in sys.path:
    sys.path.insert(0, str(_VENV_SITE))

# ``axon_quant`` 在 maturin develop / wheel install 后可被 import
# 缺失时 skip 整个模块(开发期还没 build 时常见)
try:
    import axon_quant  # noqa: F401
    from axon_quant import (  # noqa: F401
        Action,
        ActionType,
        AxonError,
        BatchConfig,
        BatchInferencePipeline,
        Device,
        InferenceBackend,
        InferenceEngine,
        InferenceError,
        InferenceStats,
        ModelConfig,
        ModelHotReloader,
        Observation,
        create_candle_engine,
        create_onnx_engine,
    )
    from axon_quant.inference import create_inference_engine  # noqa: F401

    _IMPORT_OK = True
    _IMPORT_ERROR = ""  # 占位,避免 pytestmark 处 NameError
except Exception as _exc:  # pragma: no cover - 缺失时 skip
    _IMPORT_OK = False
    _IMPORT_ERROR = repr(_exc)


pytestmark = pytest.mark.skipif(
    not _IMPORT_OK,
    reason=f"axon_quant.inference 不可用 (build wheel 后再跑): {_IMPORT_ERROR}",
)


# ═══════════════════════════════════════════════════════════════════════════
# 1. InferenceBackend 枚举
# ═══════════════════════════════════════════════════════════════════════════


def test_inference_backend_enum_members():
    """`InferenceBackend` 至少包含 Onnx / Tch / Candle 三个变体。"""
    assert InferenceBackend.Onnx is not None
    assert InferenceBackend.Tch is not None
    assert InferenceBackend.Candle is not None
    # 互不相等
    assert InferenceBackend.Onnx != InferenceBackend.Tch
    assert InferenceBackend.Onnx != InferenceBackend.Candle
    assert InferenceBackend.Tch != InferenceBackend.Candle


def test_inference_backend_str_repr():
    """`InferenceBackend.__str__` 返回小写字符串。"""
    assert str(InferenceBackend.Onnx) == "onnx"
    assert str(InferenceBackend.Candle) == "candle"
    assert str(InferenceBackend.Tch) == "tch"


# ═══════════════════════════════════════════════════════════════════════════
# 2. Device 枚举
# ═══════════════════════════════════════════════════════════════════════════


def test_device_cpu():
    """`Device.cpu()` 静态工厂。"""
    d = Device.cpu()
    assert d.kind == "cpu"
    assert d.cuda_device_id is None


def test_device_cuda():
    """`Device.cuda(0)` 接受 device_id。"""
    d = Device.cuda(0)
    assert d.kind == "cuda"
    assert d.cuda_device_id == 0


def test_device_metal():
    """`Device.metal()` 静态工厂。"""
    d = Device.metal()
    assert d.kind == "metal"
    assert d.cuda_device_id is None


# ═══════════════════════════════════════════════════════════════════════════
# 3. ModelConfig
# ═══════════════════════════════════════════════════════════════════════════


def test_model_config_construct_with_defaults():
    """`ModelConfig` 用默认值构造,fp16=false / num_threads=4 默认。"""
    cfg = ModelConfig(
        path="/tmp/m.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    assert cfg.path == "/tmp/m.onnx"
    assert cfg.backend == InferenceBackend.Onnx
    assert cfg.fp16 is False
    assert cfg.num_threads == 4
    assert cfg.output_dim == 3


def test_model_config_explicit_fields():
    """`ModelConfig` 显式 fp16 / num_threads 字段透传。"""
    cfg = ModelConfig(
        path="/tmp/m.safetensors",
        backend=InferenceBackend.Candle,
        device=Device.cuda(0),
        input_shape=(8, 32, 64),
        output_dim=10,
        fp16=True,
        num_threads=8,
    )
    assert cfg.fp16 is True
    assert cfg.num_threads == 8
    assert cfg.output_dim == 10
    assert cfg.backend == InferenceBackend.Candle
    assert cfg.input_shape == (8, 32, 64)


# ═══════════════════════════════════════════════════════════════════════════
# 4. Observation / Action / ActionType
# ═══════════════════════════════════════════════════════════════════════════


def test_observation_construct():
    """`Observation` 三字段构造(features 走 f32 精度比较,避免 f64 误判)。"""
    obs = Observation(symbol="BTC-USDT", timestamp_ns=1_000_000_000, features=[0.1, 0.2, 0.3])
    assert obs.symbol == "BTC-USDT"
    assert obs.timestamp_ns == 1_000_000_000
    # Rust 端 features 内部是 Vec<f32>,Python 端通过 getter 出来仍是 f32;
    # 0.1 f32 → 0.10000000149...(f32 精度),不要直接用 f64 list 比较
    assert len(obs.features) == 3
    assert abs(obs.features[0] - 0.1) < 1e-6
    assert abs(obs.features[1] - 0.2) < 1e-6
    assert abs(obs.features[2] - 0.3) < 1e-6


def test_action_construct_default():
    """`Action` 字段透传(`action_type` 走字符串小写解析)。"""
    act = Action(
        action_type="buy",
        confidence=0.85,
        target_position=1.0,
        model_id="model_v1",
        inference_time_us=42,
    )
    # action_type getter 返回 PyActionType 枚举(其 str() == "buy")
    assert str(act.action_type) == "buy"
    assert abs(act.confidence - 0.85) < 1e-6
    assert abs(act.target_position - 1.0) < 1e-6
    assert act.model_id == "model_v1"
    assert act.inference_time_us == 42


def test_action_type_enum_members():
    """`ActionType` 至少包含 Buy / Sell / Hold 三个变体。"""
    assert ActionType.Buy is not None
    assert ActionType.Sell is not None
    assert ActionType.Hold is not None


# ═══════════════════════════════════════════════════════════════════════════
# 5. BatchConfig / InferenceStats
# ═══════════════════════════════════════════════════════════════════════════


def test_batch_config_defaults():
    """`BatchConfig` 默认值(max_batch_size=32 / collect_timeout_us=500 / num_workers=2)。"""
    cfg = BatchConfig()
    assert cfg.max_batch_size == 32
    assert cfg.collect_timeout_us == 500
    assert cfg.num_workers == 2
    assert cfg.prealloc_buffer_size == 64
    assert cfg.collect_cpu_cores == []
    assert cfg.collect_gpu_device_id is None


def test_batch_config_custom():
    """`BatchConfig` 自定义字段透传。"""
    cfg = BatchConfig(
        max_batch_size=64,
        collect_timeout_us=200,
        num_workers=4,
        prealloc_buffer_size=128,
        collect_cpu_cores=[0, 1, 2],
        collect_gpu_device_id=0,
    )
    assert cfg.max_batch_size == 64
    assert cfg.num_workers == 4
    assert cfg.collect_cpu_cores == [0, 1, 2]
    assert cfg.collect_gpu_device_id == 0


def test_inference_stats_default_zero():
    """`InferenceStats` 默认全 0。"""
    s = InferenceStats()
    assert s.total_inferences == 0
    assert s.total_batch_inferences == 0
    assert s.avg_latency_us == 0.0
    assert s.p99_latency_us == 0.0
    assert s.hot_reloads == 0
    assert s.errors == 0


# ═══════════════════════════════════════════════════════════════════════════
# 6. InferenceEngine
# ═══════════════════════════════════════════════════════════════════════════


def test_engine_new_onnx_succeeds():
    """`InferenceEngine(Onnx backend)` 能成功构造。"""
    cfg = ModelConfig(
        path="/tmp/m.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    eng = InferenceEngine(cfg)
    assert eng.backend == "onnx"


def test_engine_new_tch_raises_inference_error():
    """`InferenceEngine(Tch backend)` 在 Stage 6 返回 `InferenceError`(避免 PyTorch C++ 链接)。"""
    cfg = ModelConfig(
        path="/tmp/m.pt",
        backend=InferenceBackend.Tch,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    with pytest.raises(InferenceError) as exc_info:
        InferenceEngine(cfg)
    # 错误码应提到 Tch(Stage 6 暂不暴露)
    msg = str(exc_info.value)
    assert "Tch" in msg, f"expected 'Tch' in error, got: {msg}"


def test_engine_new_candle_handles_feature():
    """`InferenceEngine(Candle backend)` 根据 `candle-backend` feature 分支:
    - 编译时启用 → 成功
    - 未启用 → 返回 `InferenceError` 含 'Candle' 信息
    """
    cfg = ModelConfig(
        path="/tmp/m.safetensors",
        backend=InferenceBackend.Candle,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    try:
        eng = InferenceEngine(cfg)
        # candle-backend 启用了
        assert eng.backend == "candle"
    except InferenceError as e:
        # candle-backend 未启用
        assert "Candle" in str(e), f"expected 'Candle' in error, got: {e}"


def test_engine_load_nonexistent_returns_model_not_found():
    """`engine.load("/nonexistent.onnx")` → `ModelNotFound` 错误。"""
    cfg = ModelConfig(
        path="/nonexistent.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    eng = InferenceEngine(cfg)
    with pytest.raises(InferenceError) as exc_info:
        eng.load("/nonexistent.onnx")
    msg = str(exc_info.value)
    assert "ModelNotFound" in msg, f"expected 'ModelNotFound', got: {msg}"


def test_engine_infer_without_load_returns_error():
    """未调 `load` 直接 `infer` → `InferenceError`(不 panic)。"""
    cfg = ModelConfig(
        path="/tmp/m.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    eng = InferenceEngine(cfg)
    obs = Observation(symbol="BTC-USDT", timestamp_ns=1_000, features=[0.0] * 128)
    with pytest.raises(InferenceError):
        eng.infer(obs)


def test_engine_infer_batch_empty_returns_empty_list():
    """`infer_batch([])` → 空 list(不调 backend)。"""
    cfg = ModelConfig(
        path="/tmp/m.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    eng = InferenceEngine(cfg)
    actions = eng.infer_batch([])
    assert actions == []


def test_engine_to_dict_includes_backend():
    """`engine.to_dict()` 包含 backend 字段。"""
    cfg = ModelConfig(
        path="/tmp/m.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    eng = InferenceEngine(cfg)
    d = eng.to_dict()
    assert d["backend"] == "onnx"


# ═══════════════════════════════════════════════════════════════════════════
# 7. BatchInferencePipeline
# ═══════════════════════════════════════════════════════════════════════════


def test_pipeline_new_succeeds():
    """`BatchInferencePipeline(batch_config, engine)` 能成功构造。"""
    cfg = ModelConfig(
        path="/tmp/m.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    eng = InferenceEngine(cfg)
    bcfg = BatchConfig(max_batch_size=8, collect_timeout_us=500, num_workers=2)
    pipe = BatchInferencePipeline(bcfg, eng)
    assert pipe.batch_size == 8
    assert pipe.collect_timeout_us == 500
    assert pipe.pending() == 0


def test_pipeline_submit_increases_pending():
    """`pipeline.submit(obs)` 推入缓冲,`pending()` 计数上涨。"""
    cfg = ModelConfig(
        path="/tmp/m.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    eng = InferenceEngine(cfg)
    bcfg = BatchConfig(max_batch_size=8)
    pipe = BatchInferencePipeline(bcfg, eng)
    pipe.submit(Observation(symbol="BTC-USDT", timestamp_ns=1_000, features=[0.0] * 128))
    assert pipe.pending() == 1
    pipe.submit(Observation(symbol="ETH-USDT", timestamp_ns=2_000, features=[0.0] * 128))
    assert pipe.pending() == 2


def test_pipeline_collect_empty_returns_empty_list():
    """`pipeline.collect()` 在 buffer 为空时返回空 list(不调 backend)。"""
    cfg = ModelConfig(
        path="/tmp/m.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    eng = InferenceEngine(cfg)
    bcfg = BatchConfig(max_batch_size=8)
    pipe = BatchInferencePipeline(bcfg, eng)
    actions = pipe.collect()
    assert actions == []


def test_pipeline_collect_without_load_returns_error():
    """`pipeline.collect()` 在 model 未加载时返回 `InferenceError`(不 panic)。"""
    cfg = ModelConfig(
        path="/tmp/m.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    eng = InferenceEngine(cfg)
    bcfg = BatchConfig(max_batch_size=8)
    pipe = BatchInferencePipeline(bcfg, eng)
    pipe.submit(Observation(symbol="BTC-USDT", timestamp_ns=1_000, features=[0.0] * 128))
    with pytest.raises(InferenceError):
        pipe.collect()


def test_pipeline_stats_initial_zero():
    """`pipeline.stats()` 初始全 0(未 collect 过)。"""
    cfg = ModelConfig(
        path="/tmp/m.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    eng = InferenceEngine(cfg)
    bcfg = BatchConfig(max_batch_size=8)
    pipe = BatchInferencePipeline(bcfg, eng)
    s = pipe.stats()
    assert s.total_inferences == 0
    assert s.total_batch_inferences == 0


# ═══════════════════════════════════════════════════════════════════════════
# 8. ModelHotReloader
# ═══════════════════════════════════════════════════════════════════════════


def test_reloader_new_returns_runtime_error_in_stage6():
    """`ModelHotReloader(engine)` 在 Stage 6 返回 `PyRuntimeError`
    (`PyInferenceEngine` 不暴露 `ModelConfig`)。"""
    cfg = ModelConfig(
        path="/tmp/m.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    eng = InferenceEngine(cfg)
    with pytest.raises(RuntimeError) as exc_info:
        ModelHotReloader(eng)
    assert "Stage 6" in str(exc_info.value), (
        f"expected 'Stage 6' in error, got: {exc_info.value}"
    )


# ═══════════════════════════════════════════════════════════════════════════
# 9. InferenceError 异常路径
# ═══════════════════════════════════════════════════════════════════════════


def test_inference_error_inherits_exception_not_axon_error():
    """`InferenceError` 继承 `PyException`,**不**继承 `AxonError`(cargo 循环规避)。"""
    cfg = ModelConfig(
        path="/nonexistent.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    eng = InferenceEngine(cfg)
    with pytest.raises(Exception) as exc_info:  # noqa: B017, PT011
        eng.load("/nonexistent.onnx")
    # 必须是 InferenceError 实例
    assert isinstance(exc_info.value, InferenceError)
    # 必须是 Exception(builtin)实例 → 满足 except Exception 通用捕获
    assert isinstance(exc_info.value, Exception)
    # 但**不**应是 AxonError
    if AxonError is not None:
        assert not isinstance(exc_info.value, AxonError), (
            "InferenceError should NOT inherit AxonError (cargo cycle)"
        )


def test_inference_error_args_contain_code():
    """`InferenceError` 的 `args[0]` 是稳定错误码(如 `ModelNotFound`)。"""
    cfg = ModelConfig(
        path="/nonexistent.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    eng = InferenceEngine(cfg)
    with pytest.raises(InferenceError) as exc_info:
        eng.load("/nonexistent.onnx")
    # Python 端 args[0] 是错误码
    assert exc_info.value.args[0] == "ModelNotFound", (
        f"expected 'ModelNotFound', got: {exc_info.value.args[0]}"
    )
    # args[1] 是展示信息
    msg = exc_info.value.args[1]
    assert "ModelNotFound" in msg


# ═══════════════════════════════════════════════════════════════════════════
# 10. 便捷工厂函数
# ═══════════════════════════════════════════════════════════════════════════


def test_create_onnx_engine_construct_only():
    """`create_onnx_engine` 总是先 load(由 Rust 端实现),空文件触发
    `ModelLoadFailed`/`Onnx(...)` 错误,而不应是 `ModelNotFound`。
    这里改用真正存在的空文件验证 load 阶段被触达。
    """
    import tempfile
    with tempfile.NamedTemporaryFile(suffix=".onnx", delete=False) as tmp:
        tmp_path = tmp.name
        # 写 4 字节让文件非空(仍不是合法 ONNX)
        tmp.write(b"\x00\x00\x00\x00")
    try:
        with pytest.raises(InferenceError) as exc_info:
            create_onnx_engine(
                model_path=tmp_path,
                input_shape=(1, 64, 128),
                output_dim=3,
            )
        # 错误码可能是 Onnx(...) / ModelLoadFailed / ModelNotFound
        # 关键是文件存在后,ModelNotFound 不应再出现
        assert "ModelNotFound" not in str(exc_info.value), (
            f"file exists but got ModelNotFound: {exc_info.value}"
        )
    finally:
        import os as _os
        _os.unlink(tmp_path)


def test_create_onnx_engine_load_nonexistent_returns_error():
    """`create_onnx_engine(path="/nonexistent.onnx")` → `ModelNotFound`。"""
    with pytest.raises(InferenceError) as exc_info:
        create_onnx_engine(
            model_path="/nonexistent.onnx",
            input_shape=(1, 64, 128),
            output_dim=3,
        )
    assert "ModelNotFound" in str(exc_info.value)


def test_create_candle_engine_load_nonexistent_returns_error():
    """`create_candle_engine(path="/nonexistent.safetensors")` → `ModelNotFound`。"""
    with pytest.raises(InferenceError) as exc_info:
        create_candle_engine(
            model_path="/nonexistent.safetensors",
            input_shape=(1, 64, 128),
            output_dim=3,
        )
    # Candle feature 关闭 / 文件不存在 → 任何 InferenceError 都算正常路径
    assert "ModelNotFound" in str(exc_info.value) or "Candle" in str(exc_info.value)


# ═══════════════════════════════════════════════════════════════════════════
# 11. create_inference_engine(底层工厂)
# ═══════════════════════════════════════════════════════════════════════════


def test_create_inference_engine_without_path():
    """`create_inference_engine(cfg, path=None)` → 只构造,不 load。"""
    cfg = ModelConfig(
        path="/tmp/m.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    eng = create_inference_engine(cfg, None)
    assert eng.backend == "onnx"


def test_create_inference_engine_with_nonexistent_path():
    """`create_inference_engine(cfg, "/nonexistent.onnx")` → `ModelNotFound`。"""
    cfg = ModelConfig(
        path="/nonexistent.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
    )
    with pytest.raises(InferenceError) as exc_info:
        create_inference_engine(cfg, "/nonexistent.onnx")
    assert "ModelNotFound" in str(exc_info.value)


# ═══════════════════════════════════════════════════════════════════════════
# 12. 顶层 API 总览
# ═══════════════════════════════════════════════════════════════════════════


def test_top_level_inference_reexports():
    """`axon_quant.*` 顶层 API 包含全部 15 个 inference 符号。"""
    # 核心类
    assert axon_quant.InferenceEngine is InferenceEngine
    assert axon_quant.ModelConfig is ModelConfig
    assert axon_quant.InferenceBackend is InferenceBackend
    assert axon_quant.Device is Device
    assert axon_quant.Observation is Observation
    assert axon_quant.Action is Action
    assert axon_quant.ActionType is ActionType
    assert axon_quant.BatchConfig is BatchConfig
    assert axon_quant.BatchInferencePipeline is BatchInferencePipeline
    assert axon_quant.ModelHotReloader is ModelHotReloader
    assert axon_quant.InferenceStats is InferenceStats
    assert axon_quant.InferenceError is InferenceError
    # 工厂函数
    assert axon_quant.create_onnx_engine is create_onnx_engine
    assert axon_quant.create_candle_engine is create_candle_engine


def test_inference_submodule_attributes():
    """`axon_quant.inference` 子模块的所有符号可访问。"""
    inf = axon_quant.inference
    assert inf.InferenceEngine is InferenceEngine
    assert inf.ModelConfig is ModelConfig
    assert inf.create_onnx_engine is create_onnx_engine
    assert inf.InferenceError is InferenceError
