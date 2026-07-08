//! Python 端 `InferenceEngine` —— 统一入口,根据 `ModelConfig.backend` 委托到
//! 具体的 Rust 后端实现(Onnx / Candle / Tch)。
//!
//! ## 与 Rust API 的关键差异
//!
//! - **统一入口**:Rust 端 `engine.rs` 只定义 `trait InferenceEngine`,具体实现
//!   在 `backend/{onnx,candle,tch}.rs` 三个独立模块里。Python 端用
//!   `PyInferenceEngine` 一个类统一对外,`__new__` 根据 `config.backend`
//!   分发到具体 backend,Python 用户无需关心 trait object 的存在。
//!
//! - **同步推理**:`infer` / `infer_batch` 是 CPU 同步计算(无 async),
//!   Python 端不需要 `block_on` 包装,直接 `&self` 调 `self.inner.read().infer(...)`。
//!
//! - **`&mut self` 方法用 `PyRefMut<Self>`**:`load` 内部要修改 backend 的
//!   session 字段,需要独占写锁。Python 端用 `PyRefMut` 拿独占借用,然后
//!   `&mut *slf` 解引用,避免借用冲突。
//!
//! - **后端 feature-gating**:Onnx 由 `python` feature 默认带入;Candle/Tch
//!   需额外 `--features candle-backend` / `--features tch-backend`。Python
//!   端选错后端时返回清晰的 `InferenceError`,不在 `__new__` 编译期失败
//!   (避免绑死特定后端)。
//!
//! - **后端选择表**:
//!
//!   | `InferenceBackend` | Rust struct       | Feature            |
//!   |--------------------|-------------------|--------------------|
//!   | `Onnx`             | `OnnxBackend`     | `onnx`(默认)       |
//!   | `Candle`           | `CandleBackend`   | `candle-backend`   |
//!   | `Tch`              | `TchBackend`      | `tch-backend`      |
//!
//!   Python `__new__` 根据 config.backend 选 backend,**只**启用已编译的后端,
//!   避免运行期 symbol 缺失。
//!
//! ## 当前实现覆盖
//!
//! - `__new__(config)` —— 根据 backend 创建对应 backend 实例
//! - `load(path)` / `infer(obs)` / `infer_batch(obs_list)` —— trait 必备 3 件套
//! - `backend` getter —— 返回字符串 `"onnx"` / `"candle"` / `"tch"`
//! - `__repr__` —— 含 backend 名称 + 设备类型
//!
//! 不暴露 `replace_session`(`Box<dyn Any + Send + Sync>` 难直译,且只服务
//! hot-update 内部流程,Python 端用 `ModelHotReloader` 即可)。

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::engine::InferenceEngine as RustEngine;
use crate::error::InferenceBackend as RustBackend;

use super::config::{PyAction, PyModelConfig, PyObservation};
use super::error::to_py_err;

// ═══════════════════════════════════════════════════════════════════════════
// 主类: PyInferenceEngine
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端 `InferenceEngine` —— 统一推理入口,按 `ModelConfig.backend` 委托。
///
/// 内部持有 `Arc<RwLock<dyn InferenceEngine>>`(trait object),与 Rust
/// `ModelHotReloader` / `BatchInferencePipeline` 共享同一锁接口,
/// Python 端可以无缝传入这些高层组件。
///
/// `dyn RustEngine` 字段不实现 `Debug` 派生约束,故手动实现 `Debug`
/// 仅暴露 backend 字符串(避免泄漏 `Arc` 指针信息)。
#[pyclass(name = "InferenceEngine", skip_from_py_object)]
pub struct PyInferenceEngine {
    /// 底层 backend(`Arc<RwLock<dyn InferenceEngine>>`)
    /// pub(crate) 仅供 `python::pipeline` 共享 backend 使用,
    /// Python 端不直接访问。
    pub(crate) inner: Arc<RwLock<dyn RustEngine>>,
    /// 后端名(冗余存,方便 `__repr__` / `backend` getter)
    backend: &'static str,
    /// 模型路径(冗余存,供 `ModelHotReloader` 构造 + 调试输出)
    config_path: String,
    /// num_threads(冗余存,供 `ModelHotReloader` 构造)
    num_threads: usize,
}

impl std::fmt::Debug for PyInferenceEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PyInferenceEngine")
            .field("backend", &self.backend)
            .field("config_path", &self.config_path)
            .finish_non_exhaustive()
    }
}

#[pymethods]
impl PyInferenceEngine {
    /// 构造推理引擎(根据 `config.backend` 选 backend)。
    ///
    /// **注意**:本方法**不**触发模型加载,只创建 backend 实例。
    /// 模型加载需显式调 `load(path)`。`load` 可多次调用(热更新场景)。
    ///
    /// **错误**:
    /// - `Tch` backend 未编译(`tch-backend` feature 未开)→ `InferenceError::Tch(...)`
    /// - `Candle` backend 未编译(`candle-backend` feature 未开)→ `InferenceError::Candle(...)`
    /// - 其他 backend 选择错误 → `InferenceError`
    #[new]
    pub fn new(config: PyModelConfig) -> PyResult<Self> {
        let backend_choice: RustBackend = config.0.backend;
        let cfg = config.0;
        // 提前存 path / num_threads,后续 `__repr__` / `config_path` getter 用
        let config_path = cfg.path.display().to_string();
        let num_threads = cfg.num_threads;
        match backend_choice {
            RustBackend::Onnx => {
                // onnx feature 默认由 `python` feature 带入
                #[cfg(feature = "onnx")]
                {
                    let be = crate::backend::onnx::OnnxBackend::new(cfg);
                    Ok(Self {
                        inner: Arc::new(RwLock::new(be)),
                        backend: "onnx",
                        config_path,
                        num_threads,
                    })
                }
                #[cfg(not(feature = "onnx"))]
                {
                    Err(to_py_err(crate::error::InferenceError::Onnx(
                        "Onnx backend not compiled: enable `onnx` feature \
                         (already in `python` feature by default)"
                            .into(),
                    )))
                }
            }
            RustBackend::Candle => {
                #[cfg(feature = "candle-backend")]
                {
                    let be = crate::backend::candle::CandleBackend::new(cfg);
                    Ok(Self {
                        inner: Arc::new(RwLock::new(be)),
                        backend: "candle",
                        config_path,
                        num_threads,
                    })
                }
                #[cfg(not(feature = "candle-backend"))]
                {
                    Err(to_py_err(crate::error::InferenceError::Candle(
                        "Candle backend not compiled: enable `candle-backend` feature \
                         (build with `--features candle-backend`)"
                            .into(),
                    )))
                }
            }
            RustBackend::Tch => {
                // Stage 6 设计:tch 暂不暴露 Python 绑定(避免 PyTorch C++ 链接)
                Err(to_py_err(crate::error::InferenceError::Tch(
                    "Tch backend is not exposed to Python in Stage 6 \
                     (avoids PyTorch C++ linking); use Onnx or Candle instead"
                        .into(),
                )))
            }
        }
    }

    /// 后端名称(`"onnx"` / `"candle"` / `"tch"`)
    #[getter]
    fn backend(&self) -> &'static str {
        self.backend
    }

    /// 模型路径(冗余存自 `ModelConfig.path`,供 `ModelHotReloader` 构造 + 调试)。
    #[getter]
    fn config_path(&self) -> String {
        self.config_path.clone()
    }

    /// 暴露底层 backend 的 `Arc<RwLock<dyn InferenceEngine>>` —— 用于
    /// 传入 `BatchInferencePipeline` / `ModelHotReloader` 共享 backend。
    ///
    /// Python 端通常**不需要**直接调此方法,留作内部 / 高级用法。
    fn _shared_handle(&self) -> usize {
        // thin pointer first (to Arc), then usize — `Arc::as_ptr` 稳定
        let arc_ptr: *const Arc<RwLock<dyn RustEngine>> = &self.inner;
        arc_ptr as *const () as usize
    }

    /// 从模型文件加载权重。
    ///
    /// 多次调用可热更新模型(走 `&mut self`,内部已加锁)。
    /// **错误**:`InferenceError::ModelNotFound` / `ModelLoadFailed` / `Onnx(...)`。
    fn load(&mut self, path: &str) -> PyResult<()> {
        let p = PathBuf::from(path);
        // acquire write lock — `&mut self` ensures exclusivity
        let mut guard = self.inner.write();
        guard.load(&p).map_err(to_py_err)
    }

    /// 单条推理(同步,CPU 计算)。
    ///
    /// `observation`: 形如 `Observation(symbol="BTC-USDT", timestamp_ns=..., features=[...])`
    /// 返回 `Action`(含 action_type / confidence / target_position / model_id / inference_time_us)。
    fn infer(&self, observation: PyObservation) -> PyResult<PyAction> {
        let guard = self.inner.read();
        let action = guard.infer(&observation.0).map_err(to_py_err)?;
        Ok(PyAction::from(action))
    }

    /// 批量推理(同步,CPU 计算)。
    ///
    /// 内部 `par_iter` 走 rayon 并行(若 num_threads > 1)。
    /// 返回 `list[Action]`,长度 == 输入 observations 长度。
    fn infer_batch(&self, observations: Vec<PyObservation>) -> PyResult<Vec<PyAction>> {
        if observations.is_empty() {
            return Ok(Vec::new());
        }
        let rust_obs: Vec<crate::error::Observation> =
            observations.into_iter().map(|o| o.0).collect();
        let guard = self.inner.read();
        let actions = guard.infer_batch(&rust_obs).map_err(to_py_err)?;
        Ok(actions.into_iter().map(PyAction::from).collect())
    }

    /// 转为 dict 便于序列化(JSON / 跨进程传递)。
    ///
    /// 不暴露路径 / 模型文件内容,只暴露元信息(backend + 模型路径)。
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("backend", self.backend)?;
        // 注:不暴露 Arc 指针,Python 端 `_shared_handle` 单独拿
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "InferenceEngine(backend={}, path={})",
            self.backend, self.config_path
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 内部接口:供 `PyModelHotReloader` 共享 backend
// ═══════════════════════════════════════════════════════════════════════════

impl PyInferenceEngine {
    /// 0.3.0 P0 Stage 6 收口:供 `PyModelHotReloader` 拿
    /// backend 句柄 + config_path + num_threads(走 `pub(crate)`,Python 端不可见)。
    pub(crate) fn _shared_backend(&self) -> (Arc<RwLock<dyn RustEngine>>, String, usize) {
        (
            self.inner.clone(),
            self.config_path.clone(),
            self.num_threads,
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 顶层工厂:从指定路径 + 简单参数快速创建 + 加载
// ═══════════════════════════════════════════════════════════════════════════

/// 一步创建 + 加载的便捷工厂(对应 Rust `Engine::new + load` 两步)。
///
/// 如果只想要未加载的实例(用于 pipeline 共享),用 `InferenceEngine(cfg)`。
///
/// **参数**:
/// - `config`: `ModelConfig` 实例
/// - `path`: 可选模型文件路径,若 `None` 则不调用 `load`(等价于 `InferenceEngine(cfg)`)
///
/// **错误**:模型文件不存在 / 加载失败 → `InferenceError`。
#[pyfunction]
#[pyo3(signature = (config, path=None))]
pub fn create_inference_engine(
    config: PyModelConfig,
    path: Option<&str>,
) -> PyResult<PyInferenceEngine> {
    let mut engine = PyInferenceEngine::new(config)?;
    if let Some(p) = path {
        engine.load(p)?;
    }
    Ok(engine)
}

// ═══════════════════════════════════════════════════════════════════════════
// 模块注册
// ═══════════════════════════════════════════════════════════════════════════

pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyInferenceEngine>()?;
    parent.add_function(wrap_pyfunction!(create_inference_engine, parent)?)?;
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// 单元测试
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{
        Device as RustDevice, InferenceBackend as RustBackend, ModelConfig as RustConfig,
    };

    /// 构造测试用 `PyModelConfig` —— 直接用 Rust 原生类型构造,避免依赖
    /// `pymethods` 中默认私有的 `#[new]`。
    fn make_py_config(backend: RustBackend) -> PyModelConfig {
        PyModelConfig(RustConfig {
            path: "/tmp/m.onnx".into(),
            backend,
            device: RustDevice::Cpu,
            input_shape: [1, 64, 128],
            output_dim: 3,
            fp16: false,
            num_threads: 4,
        })
    }

    /// `__new__` 能识别 `Onnx` backend(默认 onnx feature 已开)。
    #[test]
    fn engine_new_onnx_succeeds() {
        let cfg = make_py_config(RustBackend::Onnx);
        let eng = PyInferenceEngine::new(cfg).expect("Onnx backend should be available");
        assert_eq!(eng.backend(), "onnx");
        // __repr__ 含 backend + path(0.3.0 P0 Stage 6 收口)
        assert_eq!(
            eng.__repr__(),
            "InferenceEngine(backend=onnx, path=/tmp/m.onnx)"
        );
    }

    /// `__new__` 在 `Tch` backend 时返回明确错误(Stage 6 暂不暴露)。
    #[test]
    fn engine_new_tch_returns_error() {
        let cfg = make_py_config(RustBackend::Tch);
        let res = PyInferenceEngine::new(cfg);
        assert!(res.is_err(), "Tch backend should return error in Stage 6");
        let py_err = res.unwrap_err();
        Python::attach(|py| {
            let s = py_err.value(py).to_string();
            assert!(s.contains("Tch"), "error message should mention Tch: {s}");
        });
    }

    /// `__new__` 在 `Candle` backend 时:
    /// - 如果 `candle-backend` feature 开了 → 成功
    /// - 如果未开 → 返回明确错误
    #[test]
    fn engine_new_candle_handles_feature() {
        let cfg = make_py_config(RustBackend::Candle);
        let res = PyInferenceEngine::new(cfg);
        #[cfg(feature = "candle-backend")]
        assert!(
            res.is_ok(),
            "Candle backend should be available with feature"
        );
        #[cfg(not(feature = "candle-backend"))]
        {
            assert!(res.is_err(), "Candle backend should error without feature");
        }
    }

    /// `__new__` 接受 `Device::Cuda` 不 panic(backend 构造失败时降级到 CPU)。
    #[test]
    fn engine_new_with_cuda_device_does_not_panic() {
        let cfg = PyModelConfig(RustConfig {
            path: "/tmp/m.onnx".into(),
            backend: RustBackend::Onnx,
            device: RustDevice::Cuda(0),
            input_shape: [1, 64, 128],
            output_dim: 3,
            fp16: false,
            num_threads: 4,
        });
        // Cuda 设备构造失败时 backend 内部会 fallback 到 CPU,不会 panic
        let _ = PyInferenceEngine::new(cfg);
    }

    /// `load` 路径不存在时返回 `ModelNotFound` 错误。
    #[test]
    fn engine_load_nonexistent_returns_model_not_found() {
        let cfg = make_py_config(RustBackend::Onnx);
        let mut eng = PyInferenceEngine::new(cfg).expect("Onnx backend should compile");
        let res = eng.load("/nonexistent.onnx");
        assert!(res.is_err(), "loading nonexistent path should error");
        let py_err = res.unwrap_err();
        Python::attach(|py| {
            let s = py_err.value(py).to_string();
            assert!(
                s.contains("ModelNotFound"),
                "expected ModelNotFound, got: {s}"
            );
        });
    }

    /// `infer` / `infer_batch` 在模型未加载时返回明确错误(不是 panic)。
    #[test]
    fn engine_infer_without_load_returns_error() {
        let cfg = make_py_config(RustBackend::Onnx);
        let eng = PyInferenceEngine::new(cfg).expect("Onnx backend should compile");
        // 直接用 Rust 原生类型构造 obs(绕开 `PyObservation::new` 默认私有)
        let obs = PyObservation(crate::error::Observation {
            symbol: "BTC-USDT".into(),
            timestamp_ns: 1_000_000_000,
            features: vec![0.0f32; 128],
        });
        // 未调 `load` 直接 infer → backend 内 model=None → InferenceFailed
        let res = eng.infer(obs);
        assert!(res.is_err(), "infer without load should error");
    }

    /// `infer_batch` 空列表不调用 backend,直接返回 `Ok(vec![])`。
    #[test]
    fn engine_infer_batch_empty_returns_empty() {
        let cfg = make_py_config(RustBackend::Onnx);
        let eng = PyInferenceEngine::new(cfg).expect("Onnx backend should compile");
        let res = eng.infer_batch(vec![]);
        assert!(res.is_ok());
        assert!(res.unwrap().is_empty());
    }

    /// `to_dict` 包含 backend 字段。
    #[test]
    fn engine_to_dict_includes_backend() {
        let eng = PyInferenceEngine::new(make_py_config(RustBackend::Onnx)).unwrap();
        Python::attach(|py| {
            let d = eng.to_dict(py).unwrap();
            let backend = d.get_item("backend").unwrap().unwrap();
            assert_eq!(backend.extract::<String>().unwrap(), "onnx");
        });
    }

    /// `register` 函数签名稳定。
    #[test]
    fn register_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register;
    }

    /// `_shared_handle` 多次调用返回相同地址(`Arc::as_ptr` 稳定)。
    #[test]
    fn shared_handle_is_stable() {
        let eng = PyInferenceEngine::new(make_py_config(RustBackend::Onnx)).unwrap();
        let h1 = eng._shared_handle();
        let h2 = eng._shared_handle();
        assert_eq!(h1, h2);
    }

    /// `register` 把 `PyInferenceEngine` 挂到 parent 上。
    #[test]
    fn register_adds_class() {
        Python::attach(|py| {
            let m = pyo3::types::PyModule::new(py, "inference_test").unwrap();
            register(&m).unwrap();
            let cls = m.getattr("InferenceEngine").expect("class should be added");
            // 注:不能直接 instantiate(会触发 __new__ 的模型路径检查),
            // 只验证 class 存在 & name 正确
            assert_eq!(
                cls.getattr("__name__")
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "InferenceEngine"
            );
        });
    }
}
