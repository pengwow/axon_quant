//! Python 端 `BatchInferencePipeline` + `ModelHotReloader`
//!
//! ## 与 Rust API 的关键差异
//!
//! - **`BatchInferencePipeline` 简化语义**:Rust 端
//!   `BatchInferencePipeline::new(backend, config)` 返回
//!   `(pipeline, sender, receiver)` 三元组(内部 spawn 一个
//!   `batch_loop` task 用 tokio + rayon 做攒批 + 并行推理),Python 端
//!   的"批推理"语义改用 `engine.infer_batch([obs1, obs2, ...])` 一行
//!   即可达成,且 Rust 端 `infer_batch` 内部已走 `par_iter` 并行。
//!
//!   **Stage 6 决策**:Python 端 `BatchInferencePipeline` 保留类型名,
//!   但内部实现改为"自带 obs 缓冲 + 调 `infer_batch`",避免与 Rust
//!   `batch_loop` 的 channel 桥接复杂度(跨 GIL 持有 mpsc::Receiver
//!   会让 task 提前退出,语义不清晰)。
//!
//! - **`ModelHotReloader` 同步包装**:Rust 端 `reload()` 是 `async`,
//!   Python 端用 `tokio::runtime::Runtime::block_on` 同步包装,
//!   符合 Python 端无 asyncio 的调用习惯(同
//!   `axon-exchange::python::binance::PyBinanceAdapter`)。
//!
//! - **`ModelHotReloader::spawn_watcher` 不暴露**:notify watcher 返回
//!   `JoinHandle` 难处理,Stage 6 简化为"只手动 reload";自动 watcher
//!   留给内部 / Rust 用户用。
//!
//! - **Python reload 回调**:每次 reload 后调 `callback(path: str,
//!   version: int) -> None`,错误写 stderr 不抛(reload 已成功)。
//!
//! - **后端共享**:`PyBatchInferencePipeline` 与 `PyModelHotReloader`
//!   都接受 `PyInferenceEngine` 实例,从其内部 clone
//!   `Arc<RwLock<dyn InferenceEngine>>`,避免重复创建 backend。
//!
//! ## 当前实现覆盖
//!
//! - `BatchInferencePipeline(engine, batch_config)` — 启动"虚拟"管线
//! - `pipeline.submit(obs)` — 推入 observation
//! - `pipeline.collect()` — 触发一次 `infer_batch`,返回 `list[Action]`
//! - `pipeline.stats()` — 拿 `InferenceStats` 快照
//! - `ModelHotReloader(engine)` — 构造热更新器
//! - `reloader.reload()` — 手动触发 reload,返回新版本号
//! - `reloader.version()` — 当前版本号
//! - `reloader.subscribe(callback)` — 注册 Python 端 reload 回调
//! - `reloader.unsubscribe()` — 清空回调

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use pyo3::prelude::*;

use tokio::runtime::{Builder, Runtime};

use crate::engine::InferenceEngine as RustEngine;
use crate::error::Observation as RustObs;
use crate::hot_reload::ModelHotReloader as RustReloader;

use super::config::{PyAction, PyBatchConfig, PyInferenceStats, PyObservation};
use super::engine::PyInferenceEngine;
use super::error::to_py_err;

// ═══════════════════════════════════════════════════════════════════════════
// BatchInferencePipeline
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端 `BatchInferencePipeline` —— 批推理管线(Stage 6 简化版)。
///
/// 内部持有:
/// - `engine_arc`:复用 `PyInferenceEngine` 的 backend(同一 `Arc`)
/// - `buffer`:Python 端 `submit` 推入的 observations 缓存
/// - `stats_accum`:本地累加的 `InferenceStats`(供 `stats()` 返回)
/// - `batch_size` / `collect_timeout_us`:`BatchConfig` 字段(冗余存,`__repr__` 用)
///
/// **不**保留 Rust 端 `BatchInferencePipeline`(channel + spawn task 模式),
/// 因其内部 `action_tx.send().await` 在 receiver 被 drop 后会立即退出
/// task,跨 GIL 持有 receiver 易触发死锁。Python 端用"攒 obs → 调
/// `engine.infer_batch`"更直观,功能等价。
#[pyclass(name = "BatchInferencePipeline", skip_from_py_object)]
pub struct PyBatchInferencePipeline {
    /// 后端(从 `PyInferenceEngine` clone 出来,共享同一 Arc)
    engine_arc: Arc<parking_lot::RwLock<dyn RustEngine>>,
    /// observation 缓冲(`submit` 推入,`collect` 一次性消费)
    buffer: Arc<Mutex<Vec<RustObs>>>,
    /// 累加 stats(每次 `collect` 完成后更新)
    stats_accum: Arc<Mutex<crate::error::InferenceStats>>,
    /// `BatchConfig.max_batch_size`(冗余存)
    batch_size: usize,
    /// `BatchConfig.collect_timeout_us`(冗余存)
    collect_timeout_us: u64,
}

impl std::fmt::Debug for PyBatchInferencePipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PyBatchInferencePipeline")
            .field("batch_size", &self.batch_size)
            .field("collect_timeout_us", &self.collect_timeout_us)
            .field("pending", &self.buffer.lock().len())
            .finish_non_exhaustive()
    }
}

#[pymethods]
impl PyBatchInferencePipeline {
    /// 构造批推理管线。
    ///
    /// **参数**:
    /// - `batch_config`:`BatchConfig` 实例(批大小 / 收集超时等)
    /// - `engine`:`InferenceEngine` 实例(必须已经 `load()` 过模型)
    ///
    /// **错误**:无(`engine` 已构造,`batch_config` 仅读取字段)
    #[new]
    fn new(batch_config: &PyBatchConfig, engine: &PyInferenceEngine) -> PyResult<Self> {
        let engine_arc = engine.inner.clone();
        let cfg = &batch_config.0;
        Ok(Self {
            engine_arc,
            buffer: Arc::new(Mutex::new(Vec::with_capacity(cfg.max_batch_size))),
            stats_accum: Arc::new(Mutex::new(crate::error::InferenceStats::default())),
            batch_size: cfg.max_batch_size,
            collect_timeout_us: cfg.collect_timeout_us,
        })
    }

    /// 推入单条 observation(非阻塞,内部加锁)。
    ///
    /// observation 缓存在 `buffer` 里,到 `collect()` 时一次性消费并
    /// 调 `engine.infer_batch` 做并行推理。
    fn submit(&self, observation: PyObservation) -> PyResult<()> {
        let obs = observation.0;
        let mut buf = self.buffer.lock();
        buf.push(obs);
        // 缓冲超过 max_batch_size 自动截断前 N 条(`collect` 会消费掉)
        if buf.len() > self.batch_size * 4 {
            // 防御:避免无限堆积,丢弃最旧的(实际场景 `collect` 应及时调用)
            let excess = buf.len() - self.batch_size * 4;
            buf.drain(0..excess);
        }
        Ok(())
    }

    /// 触发一次批推理,返回 `list[Action]`。
    ///
    /// **行为**:
    /// 1. 取出 `buffer` 全部 observation(可能少于 `max_batch_size`)
    /// 2. 调 `engine.infer_batch(obs_list)` 拿 actions
    /// 3. 更新本地 stats(total_batch_inferences += 1, total_inferences += N)
    /// 4. 返回 actions
    ///
    /// **错误**:`InferenceError`(模型未加载 / 维度不匹配 / 后端错误)
    fn collect(&self) -> PyResult<Vec<PyAction>> {
        // 取出缓冲(最小临界区)
        let obs_list: Vec<RustObs> = {
            let mut buf = self.buffer.lock();
            std::mem::take(&mut *buf)
        };
        if obs_list.is_empty() {
            return Ok(Vec::new());
        }
        let n = obs_list.len();
        // 调 backend(读锁,允许与 collect 并发)
        let actions = {
            let guard = self.engine_arc.read();
            guard.infer_batch(&obs_list).map_err(to_py_err)?
        };
        // 更新 stats
        {
            let mut s = self.stats_accum.lock();
            s.total_batch_inferences += 1;
            s.total_inferences += n as u64;
        }
        Ok(actions.into_iter().map(PyAction::from).collect())
    }

    /// 当前 buffer 中待 collect 的 observation 数
    fn pending(&self) -> usize {
        self.buffer.lock().len()
    }

    /// 拿当前 `InferenceStats` 快照
    fn stats(&self) -> PyResult<PyInferenceStats> {
        let s = self.stats_accum.lock().clone();
        Ok(PyInferenceStats::from(s))
    }

    /// 当前批大小(`BatchConfig.max_batch_size`)
    #[getter]
    fn batch_size(&self) -> usize {
        self.batch_size
    }

    /// 收集超时(微秒,`BatchConfig.collect_timeout_us`)
    ///
    /// Stage 6 简化版**不**实际用此字段(没有 tokio timer),仅记录
    /// 原始值供 `__repr__` 显示。
    #[getter]
    fn collect_timeout_us(&self) -> u64 {
        self.collect_timeout_us
    }

    fn __repr__(&self) -> String {
        format!(
            "BatchInferencePipeline(batch_size={}, pending={})",
            self.batch_size,
            self.pending()
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// ModelHotReloader
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端 `ModelHotReloader` —— 模型热更新器(同步包装)。
///
/// 内部持有 Rust `ModelHotReloader` + tokio runtime;`reload` 等
/// `async` 方法走 `block_on` 同步包装。
///
/// `PyObject` / `tokio` runtime / `RustReloader` 等内部字段无 Debug 派生,
/// 故手动实现 `Debug` 仅暴露 path / version / callback。
#[pyclass(name = "ModelHotReloader", skip_from_py_object)]
pub struct PyModelHotReloader {
    /// Rust 端 reloader 句柄
    inner: RustReloader,
    /// 模型路径(冗余存,`model_path` getter 用)
    config_path: String,
    /// tokio 运行时(`block_on` 包装)
    rt: Arc<Runtime>,
    /// Python reload 回调(`Option<PyObject>`,None = 无回调)
    callback: Arc<Mutex<Option<Py<PyAny>>>>,
}

impl std::fmt::Debug for PyModelHotReloader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PyModelHotReloader")
            .field("config_path", &self.config_path)
            .field("version", &self.inner.version())
            .field("has_callback", &self.callback.lock().is_some())
            .finish()
    }
}

#[pymethods]
impl PyModelHotReloader {
    /// 构造模型热更新器。
    ///
    /// **参数**:
    /// - `engine`:`InferenceEngine` 实例(必须已经 `load()` 过模型)
    ///
    /// **错误**:`PyRuntimeError`(若 tokio runtime 创建失败)
    #[new]
    fn new(engine: &PyInferenceEngine) -> PyResult<Self> {
        // 0.3.0 P0 Stage 6 收口:从 `PyInferenceEngine` 拿共享 backend + config_path,
        // 构造 Rust `ModelHotReloader` 真实例,不再返回 `RuntimeError`。
        let (backend, config_path, num_threads) = engine._shared_backend();
        let inner = RustReloader::new_from_path(backend, PathBuf::from(&config_path), num_threads);
        let rt = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "failed to create tokio runtime: {e}"
                ))
            })?;
        Ok(Self {
            inner,
            config_path,
            rt: Arc::new(rt),
            callback: Arc::new(Mutex::new(None)),
        })
    }

    /// 手动触发 reload(走 `build_session` + `replace_session` 两步原子路径)。
    ///
    /// **返回**:新版本号(`u64`,`>= 1`)。
    fn reload<'py>(&self, py: Python<'py>) -> PyResult<u64> {
        let version = self.rt.block_on(self.inner.reload()).map_err(to_py_err)?;
        if let Some(cb) = self.callback.lock().as_ref() {
            // 回调错误仅 warn 不让 reload 失败
            if let Err(e) = cb.call1(py, (self.config_path.clone(), version)) {
                let _ = py.import("sys")?.getattr("stderr")?.call_method1(
                    "write",
                    (format!("ModelHotReloader callback error: {e}\n"),),
                )?;
            }
        }
        Ok(version)
    }

    /// 当前版本号(`0` 表示从未 reload 过)
    fn version(&self) -> u64 {
        self.inner.version()
    }

    /// 注册 Python 端 reload 回调。
    ///
    /// **参数**:`callback: Callable[[str, int], None]`
    /// - 第 1 参数:模型路径(字符串)
    /// - 第 2 参数:新版本号(int)
    fn subscribe(&self, callback: Py<PyAny>) {
        *self.callback.lock() = Some(callback);
    }

    /// 清空回调(等同 `subscribe(None)`)
    fn unsubscribe(&self) {
        *self.callback.lock() = None;
    }

    /// 当前模型路径
    #[getter]
    fn model_path(&self) -> String {
        self.config_path.clone()
    }

    /// 是否已注册回调
    fn has_callback(&self) -> bool {
        self.callback.lock().is_some()
    }

    fn __repr__(&self) -> String {
        format!(
            "ModelHotReloader(path={:?}, version={}, callback={})",
            self.config_path,
            self.version(),
            if self.has_callback() { "set" } else { "none" },
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 模块注册
// ═══════════════════════════════════════════════════════════════════════════

pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyBatchInferencePipeline>()?;
    parent.add_class::<PyModelHotReloader>()?;
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// 单元测试
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::super::config::PyModelConfig;
    use super::*;
    use crate::error::{
        BatchConfig as RustBatch, Device as RustDevice, InferenceBackend as RustBackend,
        ModelConfig as RustConfig,
    };

    /// 拿一个已构造好的 `PyInferenceEngine`(未加载模型,只用于引用计数测试)。
    ///
    /// 直接用 Rust 原生类型构造,避免依赖 `pymethods` 中默认私有的 `#[new]`。
    fn make_engine() -> PyInferenceEngine {
        let cfg = PyModelConfig(RustConfig {
            path: "/tmp/m.onnx".into(),
            backend: RustBackend::Onnx,
            device: RustDevice::Cpu,
            input_shape: [1, 64, 128],
            output_dim: 3,
            fp16: false,
            num_threads: 4,
        });
        PyInferenceEngine::new(cfg).unwrap()
    }

    /// 拿一个测试用 `PyBatchConfig`
    fn make_batch_config() -> PyBatchConfig {
        PyBatchConfig(RustBatch {
            max_batch_size: 8,
            collect_timeout_us: 500,
            num_workers: 2,
            prealloc_buffer_size: 16,
            collect_cpu_cores: Vec::new(),
            collect_gpu_device_id: None,
        })
    }

    /// `BatchInferencePipeline.__new__(batch_config, engine)` 能成功。
    #[test]
    fn pipeline_new_succeeds() {
        let engine = make_engine();
        let cfg = make_batch_config();
        let pipe = PyBatchInferencePipeline::new(&cfg, &engine);
        assert!(pipe.is_ok(), "new should succeed");
        let pipe = pipe.unwrap();
        assert_eq!(pipe.batch_size(), 8);
        assert_eq!(pipe.collect_timeout_us(), 500);
        assert_eq!(pipe.pending(), 0);
    }

    /// `pipeline.submit(obs)` 能把 obs 推入缓冲,`pending` 计数上涨。
    #[test]
    fn pipeline_submit_increases_pending() {
        let engine = make_engine();
        let cfg = make_batch_config();
        let pipe = PyBatchInferencePipeline::new(&cfg, &engine).unwrap();
        let obs = PyObservation(RustObs {
            symbol: "BTC-USDT".into(),
            timestamp_ns: 1_000,
            features: vec![0.0f32; 128],
        });
        pipe.submit(obs).unwrap();
        assert_eq!(pipe.pending(), 1);
        let obs2 = PyObservation(RustObs {
            symbol: "ETH-USDT".into(),
            timestamp_ns: 2_000,
            features: vec![0.0f32; 128],
        });
        pipe.submit(obs2).unwrap();
        assert_eq!(pipe.pending(), 2);
    }

    /// `pipeline.collect()` 在 model 未加载时返回错误(不是 panic)。
    #[test]
    fn pipeline_collect_without_load_returns_error() {
        let engine = make_engine();
        let cfg = make_batch_config();
        let pipe = PyBatchInferencePipeline::new(&cfg, &engine).unwrap();
        let obs = PyObservation(RustObs {
            symbol: "BTC-USDT".into(),
            timestamp_ns: 1_000,
            features: vec![0.0f32; 128],
        });
        pipe.submit(obs).unwrap();
        let res = pipe.collect();
        assert!(res.is_err(), "collect without load should error");
    }

    /// `pipeline.collect()` 在 buffer 为空时返回空 vec(不调 backend)。
    #[test]
    fn pipeline_collect_empty_returns_empty() {
        let engine = make_engine();
        let cfg = make_batch_config();
        let pipe = PyBatchInferencePipeline::new(&cfg, &engine).unwrap();
        let res = pipe.collect();
        assert!(res.is_ok());
        assert!(res.unwrap().is_empty());
    }

    /// `pipeline.stats()` 返回默认 `InferenceStats`(全 0)。
    #[test]
    fn pipeline_stats_default_all_zero() {
        let engine = make_engine();
        let cfg = make_batch_config();
        let pipe = PyBatchInferencePipeline::new(&cfg, &engine).unwrap();
        let stats = pipe.stats().unwrap();
        assert_eq!(stats.0.total_inferences, 0);
        assert_eq!(stats.0.total_batch_inferences, 0);
    }

    /// `ModelHotReloader.__new__(engine)` 在 0.3.0 P0 Stage 6 收口后
    /// 成功构造(不再返回 `RuntimeError`),`version() == 0`。
    #[test]
    fn reloader_new_succeeds_with_engine() {
        let engine = make_engine();
        let reloader = PyModelHotReloader::new(&engine)
            .expect("reloader.new should succeed with engine (Stage 6 收口后)");
        assert_eq!(reloader.version(), 0, "fresh reloader version is 0");
        assert!(!reloader.has_callback(), "no callback by default");
        // model_path 与 engine config 路径一致(冗余存,确认)
        assert!(!reloader.model_path().is_empty());
    }

    /// `ModelHotReloader.subscribe(cb)` 记录回调;`unsubscribe()` 清空。
    #[test]
    fn reloader_subscribe_and_unsubscribe() {
        let engine = make_engine();
        let reloader = PyModelHotReloader::new(&engine).unwrap();
        Python::attach(|py| {
            // pyo3 0.28:eval 需要 `&CStr`,返回 `Bound<PyAny>`,`.unbind()` 转 `Py<PyAny>`
            let cb = py
                .eval(c"lambda path, version: None", None, None)
                .unwrap()
                .unbind();
            reloader.subscribe(cb);
            assert!(reloader.has_callback());
            reloader.unsubscribe();
            assert!(!reloader.has_callback());
        });
    }

    /// `register` 函数签名稳定。
    #[test]
    fn register_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register;
    }

    /// `register` 把两个类都挂到 parent 上。
    #[test]
    fn register_adds_classes() {
        Python::attach(|py| {
            let m = pyo3::types::PyModule::new(py, "pipeline_test").unwrap();
            register(&m).unwrap();
            m.getattr("BatchInferencePipeline")
                .expect("BatchInferencePipeline class should be added");
            m.getattr("ModelHotReloader")
                .expect("ModelHotReloader class should be added");
        });
    }
}
