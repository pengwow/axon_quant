"""axon_quant.inference 顶层 Python API —— thin wrapper 模式(Stage 6)。

约定:
- 核心实现走 ``axon_quant._native.inference``(PyO3 绑定)
- 本模块负责:
  * 重新导出 9 个核心类 / 异常:
    - 配置:`ModelConfig` / `InferenceBackend` / `Device` / `BatchConfig` / `InferenceStats`
    - 数据:`Observation` / `Action` / `ActionType`
    - 引擎:`InferenceEngine`(支持 Onnx / Candle 后端)
    - 管线:`BatchInferencePipeline` / `ModelHotReloader`(Stage 6 简化版)
    - 异常:`InferenceError`(继承 builtin ``PyException`` 而非 ``AxonError``)
  * 便捷工厂 ``create_onnx_engine(path, ...)`` / ``create_candle_engine(path, ...)``,
    一步创建 + 加载(避免 Rust 端分两步)

**后端选择**:
- 默认 ``InferenceBackend.Onnx``(Stage 6 默认 feature,生产环境主推)。
- ``InferenceBackend.Candle`` 需额外启用 ``candle-backend`` feature
  (纯 Rust,无外部依赖,适合无 ONNX runtime 场景)。
- ``InferenceBackend.Tch`` 暂不暴露 Python 绑定(避免 PyTorch C++ 链接)。

**异步桥**:推理是 CPU 同步计算,``infer`` / ``infer_batch`` 都是
``&self`` 方法,Python 端无 asyncio 依赖,直接同步调用即可。

用法::

    from axon_quant.inference import (
        InferenceEngine, ModelConfig, Device, Observation,
        InferenceBackend, BatchInferencePipeline, create_onnx_engine,
    )

    # 1) 一步创建 + 加载
    engine = create_onnx_engine(
        model_path="model.onnx",
        input_shape=(1, 64, 128),
        output_dim=3,
    )

    # 2) 单条推理
    obs = Observation(symbol="BTC-USDT", timestamp_ns=1_000, features=[0.0] * 128)
    action = engine.infer(obs)
    print(action.action_type, action.confidence)

    # 3) 批量推理
    obs_list = [obs] * 32
    actions = engine.infer_batch(obs_list)
    print(len(actions))  # 32
"""

from __future__ import annotations

# 重新导出原生符号(Stage 6 全量)
# 注意:`_native` 是 cdylib 单文件扩展(不是 Python package 目录),
# 所以 `from axon_quant._native.inference import ...` 这种 dot 路径不可用;
# 改用 `from axon_quant._native import inference` 先把子模块对象取出来,
# 再用属性访问取出类(与 `oms.py` / `backtest.py` / `data.py` / `exchange.py` 保持一致)。
from axon_quant._native import inference as _native_inference_module  # noqa: E402

# 配置类
InferenceBackend = _native_inference_module.InferenceBackend
Device = _native_inference_module.Device
ModelConfig = _native_inference_module.ModelConfig
BatchConfig = _native_inference_module.BatchConfig
InferenceStats = _native_inference_module.InferenceStats

# 数据类
Observation = _native_inference_module.Observation
Action = _native_inference_module.Action
ActionType = _native_inference_module.ActionType

# 引擎 + 管线
InferenceEngine = _native_inference_module.InferenceEngine
BatchInferencePipeline = _native_inference_module.BatchInferencePipeline
ModelHotReloader = _native_inference_module.ModelHotReloader

# 顶层工厂(从 PyO3 `create_inference_engine` 包装一层,允许 Python 端只传 path)
create_inference_engine = _native_inference_module.create_inference_engine

# 异常:InferenceError 继承 builtin `PyException`(避免 cargo 循环,见
# `.axon-internal/specs/2026-06-19-python-bindings-expansion-design.md` §3.1.6)
# 这里**不**继承 `AxonError`(Stage 1 实战发现 cargo 循环不可行)。
# Python 端可走 `except Exception` 统一捕获。
InferenceError = _native_inference_module.InferenceError

# AxonError 基类(Stage 1 引入,放 data 子模块顶层),
# 这里重新导出方便 Stage 6 用户一处 import。
# 注:InferenceError **不**继承 AxonError,所以 `except AxonError` 不会
# 捕获 InferenceError;若想统一处理需 `except (AxonError, InferenceError)`
# 或直接 `except Exception`。
try:
    from axon_quant import AxonError  # noqa: F401
except ImportError:  # pragma: no cover
    AxonError = None  # type: ignore[assignment]


__all__ = [
    # 配置
    "InferenceBackend",
    "Device",
    "ModelConfig",
    "BatchConfig",
    "InferenceStats",
    # 数据
    "Observation",
    "Action",
    "ActionType",
    # 引擎 + 管线
    "InferenceEngine",
    "BatchInferencePipeline",
    "ModelHotReloader",
    "create_inference_engine",
    # 异常
    "InferenceError",
    "AxonError",
    # 工厂函数(后端特定便捷构造)
    "create_onnx_engine",
    "create_candle_engine",
]


# ═══════════════════════════════════════════════════════════════════════════
# 便捷工厂函数(后端特定)
# ═══════════════════════════════════════════════════════════════════════════


def create_onnx_engine(
    model_path: str,
    input_shape: tuple[int, int, int] = (1, 64, 128),
    output_dim: int = 3,
    device: Device | None = None,
    fp16: bool = False,
    num_threads: int = 4,
) -> InferenceEngine:
    """一步创建 + 加载 ONNX 推理引擎(Stage 6 默认后端)。

    Args:
        model_path: ONNX 模型文件路径(.onnx)
        input_shape: 模型输入 shape(3 维 tuple,batch × seq × feature)
        output_dim: 模型输出维度
        device: 推理设备,默认 ``Device.cpu()``
        fp16: 是否启用 fp16 推理(仅 GPU 后端支持)
        num_threads: CPU 线程数(默认 4)

    Returns:
        已加载模型的 ``InferenceEngine`` 实例

    Raises:
        ``InferenceError``: 模型文件不存在 / ONNX runtime 未安装 / shape 不匹配
    """
    cfg = ModelConfig(
        path=model_path,
        backend=InferenceBackend.Onnx,
        device=device if device is not None else Device.cpu(),
        input_shape=input_shape,
        output_dim=output_dim,
        fp16=fp16,
        num_threads=num_threads,
    )
    return create_inference_engine(cfg, model_path)


def create_candle_engine(
    model_path: str,
    input_shape: tuple[int, int, int] = (1, 64, 128),
    output_dim: int = 3,
    device: Device | None = None,
    fp16: bool = False,
    num_threads: int = 4,
) -> InferenceEngine:
    """一步创建 + 加载 Candle 推理引擎(纯 Rust,无外部依赖)。

    **前置条件**:axon-inference 必须以 ``candle-backend`` feature 编译
    (``cargo build -p axon-inference --features python --features candle-backend``)。
    若未编译,会在 ``__new__`` 时返回 ``InferenceError``(明确错误码 ``Candle(...)``)。

    Args:
        model_path: 模型文件路径(.safetensors)
        input_shape: 模型输入 shape(3 维 tuple)
        output_dim: 模型输出维度
        device: 推理设备,默认 ``Device.cpu()``
        fp16: 是否启用 fp16 推理
        num_threads: CPU 线程数

    Returns:
        已加载模型的 ``InferenceEngine`` 实例

    Raises:
        ``InferenceError``: 模型文件不存在 / Candle feature 未编译 / shape 不匹配
    """
    cfg = ModelConfig(
        path=model_path,
        backend=InferenceBackend.Candle,
        device=device if device is not None else Device.cpu(),
        input_shape=input_shape,
        output_dim=output_dim,
        fp16=fp16,
        num_threads=num_threads,
    )
    return create_inference_engine(cfg, model_path)
