//! Python 端推理配置:`ModelConfig` / `InferenceBackend` / `Device` /
//! `Observation` / `Action` / `ActionType` / `BatchConfig` / `InferenceStats`
//!
//! 设计原因:同 Stage 1-5 模式,Python 端 flat pyclass,不做嵌套模块。
//! 内部 `From` / `Into` 转换与 Rust 端 API 1:1 对应。

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::error::{
    Action as RustAction, ActionType as RustActionType, BatchConfig as RustBatchConfig,
    Device as RustDevice, InferenceBackend as RustBackend, InferenceStats as RustStats,
    ModelConfig as RustModelConfig, Observation as RustObs,
};

// ─── InferenceBackend ────────────────────────────────────

/// Python 端推理后端枚举
#[pyclass(name = "InferenceBackend", eq, eq_int, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PyInferenceBackend {
    Onnx,
    Tch,
    Candle,
}

impl From<RustBackend> for PyInferenceBackend {
    fn from(b: RustBackend) -> Self {
        match b {
            RustBackend::Onnx => Self::Onnx,
            RustBackend::Tch => Self::Tch,
            RustBackend::Candle => Self::Candle,
        }
    }
}

impl From<PyInferenceBackend> for RustBackend {
    fn from(b: PyInferenceBackend) -> Self {
        match b {
            PyInferenceBackend::Onnx => Self::Onnx,
            PyInferenceBackend::Tch => Self::Tch,
            PyInferenceBackend::Candle => Self::Candle,
        }
    }
}

#[pymethods]
impl PyInferenceBackend {
    fn __str__(&self) -> &'static str {
        match self {
            Self::Onnx => "onnx",
            Self::Tch => "tch",
            Self::Candle => "candle",
        }
    }
    fn __repr__(&self) -> String {
        format!("InferenceBackend.{}", self.__str__())
    }
}

// ─── Device ────────────────────────────────────

/// Python 端推理设备
///
/// `Cuda(device_id)` 接受 `int`(CUDA 设备 ID);`Metal` 无参数。
#[pyclass(name = "Device", from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PyDevice(pub RustDevice);

#[pymethods]
impl PyDevice {
    /// 构造 CPU 设备(`Device.cpu()`)
    #[staticmethod]
    fn cpu() -> Self {
        Self(RustDevice::Cpu)
    }
    /// 构造 CUDA 设备,接受 device_id(`Device.cuda(0)`)
    #[staticmethod]
    fn cuda(device_id: u32) -> Self {
        Self(RustDevice::Cuda(device_id))
    }
    /// 构造 Metal 设备(`Device.metal()`)
    #[staticmethod]
    fn metal() -> Self {
        Self(RustDevice::Metal)
    }
    /// 设备类型:`"cpu"` / `"cuda"` / `"metal"`
    #[getter]
    fn kind(&self) -> &'static str {
        match self.0 {
            RustDevice::Cpu => "cpu",
            RustDevice::Cuda(_) => "cuda",
            RustDevice::Metal => "metal",
        }
    }
    /// CUDA 设备 ID(非 CUDA 设备返回 `None`)
    #[getter]
    fn cuda_device_id(&self) -> Option<u32> {
        match self.0 {
            RustDevice::Cuda(id) => Some(id),
            _ => None,
        }
    }
    fn __repr__(&self) -> String {
        match self.0 {
            RustDevice::Cpu => "Device.cpu()".to_string(),
            RustDevice::Cuda(id) => format!("Device.cuda({id})"),
            RustDevice::Metal => "Device.metal()".to_string(),
        }
    }
    fn __str__(&self) -> String {
        self.__repr__()
    }
}

impl From<RustDevice> for PyDevice {
    fn from(d: RustDevice) -> Self {
        Self(d)
    }
}
impl From<PyDevice> for RustDevice {
    fn from(d: PyDevice) -> Self {
        d.0
    }
}

// ─── ModelConfig ────────────────────────────────────

/// Python 端模型配置
///
/// 必填字段:`path` / `backend` / `device` / `input_shape`(3 维 tuple)/
/// `output_dim`;`fp16` / `num_threads` 有默认值。
#[pyclass(name = "ModelConfig", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyModelConfig(pub RustModelConfig);

#[pymethods]
impl PyModelConfig {
    #[new]
    #[pyo3(signature = (
        path,
        backend,
        device,
        input_shape,
        output_dim,
        fp16=false,
        num_threads=4,
    ))]
    fn new(
        path: String,
        backend: PyInferenceBackend,
        device: PyDevice,
        input_shape: (usize, usize, usize),
        output_dim: usize,
        fp16: bool,
        num_threads: usize,
    ) -> Self {
        Self(RustModelConfig {
            path: path.into(),
            backend: backend.into(),
            device: device.into(),
            input_shape: [input_shape.0, input_shape.1, input_shape.2],
            output_dim,
            fp16,
            num_threads,
        })
    }
    #[getter]
    fn path(&self) -> String {
        self.0.path.display().to_string()
    }
    #[getter]
    fn backend(&self) -> PyInferenceBackend {
        self.0.backend.into()
    }
    #[getter]
    fn device(&self) -> PyDevice {
        self.0.device.into()
    }
    #[getter]
    fn input_shape(&self) -> (usize, usize, usize) {
        (
            self.0.input_shape[0],
            self.0.input_shape[1],
            self.0.input_shape[2],
        )
    }
    #[getter]
    fn output_dim(&self) -> usize {
        self.0.output_dim
    }
    #[getter]
    fn fp16(&self) -> bool {
        self.0.fp16
    }
    #[getter]
    fn num_threads(&self) -> usize {
        self.0.num_threads
    }
    fn __repr__(&self) -> String {
        format!(
            "ModelConfig(path={:?}, backend={:?}, device={}, input_shape={:?}, output_dim={}, fp16={}, num_threads={})",
            self.0.path.display(),
            self.0.backend,
            self.device().__str__(),
            self.0.input_shape,
            self.0.output_dim,
            self.0.fp16,
            self.0.num_threads,
        )
    }
}

impl From<RustModelConfig> for PyModelConfig {
    fn from(c: RustModelConfig) -> Self {
        Self(c)
    }
}
impl From<PyModelConfig> for RustModelConfig {
    fn from(c: PyModelConfig) -> Self {
        c.0
    }
}

// ─── Observation ────────────────────────────────────

/// Python 端观测数据
#[pyclass(name = "Observation", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyObservation(pub RustObs);

#[pymethods]
impl PyObservation {
    #[new]
    #[pyo3(signature = (symbol, timestamp_ns, features))]
    fn new(symbol: String, timestamp_ns: u64, features: Vec<f32>) -> Self {
        Self(RustObs {
            symbol,
            timestamp_ns,
            features,
        })
    }
    #[getter]
    fn symbol(&self) -> String {
        self.0.symbol.clone()
    }
    #[getter]
    fn timestamp_ns(&self) -> u64 {
        self.0.timestamp_ns
    }
    #[getter]
    fn features(&self) -> Vec<f32> {
        self.0.features.clone()
    }
    /// features 长度
    #[getter]
    fn feature_dim(&self) -> usize {
        self.0.features.len()
    }
    fn __repr__(&self) -> String {
        format!(
            "Observation(symbol={:?}, timestamp_ns={}, feature_dim={})",
            self.0.symbol,
            self.0.timestamp_ns,
            self.0.features.len(),
        )
    }
}

impl From<RustObs> for PyObservation {
    fn from(o: RustObs) -> Self {
        Self(o)
    }
}
impl From<PyObservation> for RustObs {
    fn from(o: PyObservation) -> Self {
        o.0
    }
}

// ─── ActionType ────────────────────────────────────

/// Python 端 action 类型
#[pyclass(name = "ActionType", eq, eq_int, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PyActionType {
    Hold,
    Buy,
    Sell,
    ReduceLong,
    ReduceShort,
}

impl From<RustActionType> for PyActionType {
    fn from(a: RustActionType) -> Self {
        match a {
            RustActionType::Hold => Self::Hold,
            RustActionType::Buy => Self::Buy,
            RustActionType::Sell => Self::Sell,
            RustActionType::ReduceLong => Self::ReduceLong,
            RustActionType::ReduceShort => Self::ReduceShort,
        }
    }
}
impl From<PyActionType> for RustActionType {
    fn from(a: PyActionType) -> Self {
        match a {
            PyActionType::Hold => Self::Hold,
            PyActionType::Buy => Self::Buy,
            PyActionType::Sell => Self::Sell,
            PyActionType::ReduceLong => Self::ReduceLong,
            PyActionType::ReduceShort => Self::ReduceShort,
        }
    }
}

#[pymethods]
impl PyActionType {
    fn __str__(&self) -> &'static str {
        match self {
            Self::Hold => "hold",
            Self::Buy => "buy",
            Self::Sell => "sell",
            Self::ReduceLong => "reduce_long",
            Self::ReduceShort => "reduce_short",
        }
    }
    fn __repr__(&self) -> String {
        format!("ActionType.{}", self.__str__())
    }
}

// ─── Action ────────────────────────────────────

/// Python 端 action 输出
#[pyclass(name = "Action", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyAction(pub RustAction);

#[pymethods]
impl PyAction {
    /// 内部构造(从 Rust `Action` 转 Python `Action`)。
    /// Python 端通常不直接调,应使用 `from rust_action` 的自动转换或
    /// `engine.infer(obs)` 返回值。
    #[new]
    #[pyo3(signature = (action_type, confidence, target_position, model_id, inference_time_us))]
    fn new(
        action_type: &str,
        confidence: f32,
        target_position: f32,
        model_id: String,
        inference_time_us: u64,
    ) -> Self {
        // 字符串 → ActionType 解析("buy" / "sell" / "hold" / "reduce_long" / "reduce_short")
        let at = match action_type {
            "buy" => RustActionType::Buy,
            "sell" => RustActionType::Sell,
            "hold" => RustActionType::Hold,
            "reduce_long" => RustActionType::ReduceLong,
            "reduce_short" => RustActionType::ReduceShort,
            _ => RustActionType::Hold, // 默认 Hold
        };
        Self(RustAction {
            action_type: at,
            confidence,
            target_position,
            model_id,
            inference_time_us,
        })
    }
    #[getter]
    fn action_type(&self) -> PyActionType {
        self.0.action_type.into()
    }
    #[getter]
    fn confidence(&self) -> f32 {
        self.0.confidence
    }
    #[getter]
    fn target_position(&self) -> f32 {
        self.0.target_position
    }
    #[getter]
    fn model_id(&self) -> String {
        self.0.model_id.clone()
    }
    #[getter]
    fn inference_time_us(&self) -> u64 {
        self.0.inference_time_us
    }
    fn __repr__(&self) -> String {
        format!(
            "Action(type={}, confidence={:.3}, target_position={:.3}, model_id={:?}, inference_time_us={})",
            self.action_type().__str__(),
            self.0.confidence,
            self.0.target_position,
            self.0.model_id,
            self.0.inference_time_us,
        )
    }
    /// 序列化为 dict(便于 JSON 持久化 / 跨进程传递)
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("action_type", self.action_type().__str__())?;
        d.set_item("confidence", self.0.confidence)?;
        d.set_item("target_position", self.0.target_position)?;
        d.set_item("model_id", self.0.model_id.clone())?;
        d.set_item("inference_time_us", self.0.inference_time_us)?;
        Ok(d)
    }
}

impl From<RustAction> for PyAction {
    fn from(a: RustAction) -> Self {
        Self(a)
    }
}
impl From<PyAction> for RustAction {
    fn from(a: PyAction) -> Self {
        a.0
    }
}

// ─── BatchConfig ────────────────────────────────────

/// Python 端批推理配置
#[pyclass(name = "BatchConfig", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyBatchConfig(pub RustBatchConfig);

#[pymethods]
impl PyBatchConfig {
    #[new]
    #[pyo3(signature = (
        max_batch_size=32,
        collect_timeout_us=500,
        num_workers=2,
        prealloc_buffer_size=64,
        collect_cpu_cores=Vec::new(),
        collect_gpu_device_id=None,
    ))]
    fn new(
        max_batch_size: usize,
        collect_timeout_us: u64,
        num_workers: usize,
        prealloc_buffer_size: usize,
        collect_cpu_cores: Vec<u32>,
        collect_gpu_device_id: Option<u32>,
    ) -> Self {
        Self(RustBatchConfig {
            max_batch_size,
            collect_timeout_us,
            num_workers,
            prealloc_buffer_size,
            collect_cpu_cores,
            collect_gpu_device_id,
        })
    }
    #[getter]
    fn max_batch_size(&self) -> usize {
        self.0.max_batch_size
    }
    #[getter]
    fn collect_timeout_us(&self) -> u64 {
        self.0.collect_timeout_us
    }
    #[getter]
    fn num_workers(&self) -> usize {
        self.0.num_workers
    }
    #[getter]
    fn prealloc_buffer_size(&self) -> usize {
        self.0.prealloc_buffer_size
    }
    #[getter]
    fn collect_cpu_cores(&self) -> Vec<u32> {
        self.0.collect_cpu_cores.clone()
    }
    #[getter]
    fn collect_gpu_device_id(&self) -> Option<u32> {
        self.0.collect_gpu_device_id
    }
    fn __repr__(&self) -> String {
        format!(
            "BatchConfig(max_batch_size={}, collect_timeout_us={}, num_workers={}, prealloc_buffer_size={})",
            self.0.max_batch_size,
            self.0.collect_timeout_us,
            self.0.num_workers,
            self.0.prealloc_buffer_size,
        )
    }
}

// ─── InferenceStats ────────────────────────────────────

/// Python 端推理统计
#[pyclass(name = "InferenceStats", from_py_object)]
#[derive(Debug, Clone, Default)]
pub struct PyInferenceStats(pub RustStats);

#[pymethods]
impl PyInferenceStats {
    #[new]
    fn new() -> Self {
        Self(RustStats::default())
    }
    #[getter]
    fn total_inferences(&self) -> u64 {
        self.0.total_inferences
    }
    #[getter]
    fn total_batch_inferences(&self) -> u64 {
        self.0.total_batch_inferences
    }
    #[getter]
    fn avg_latency_us(&self) -> f64 {
        self.0.avg_latency_us
    }
    #[getter]
    fn p99_latency_us(&self) -> f64 {
        self.0.p99_latency_us
    }
    #[getter]
    fn hot_reloads(&self) -> u64 {
        self.0.hot_reloads
    }
    #[getter]
    fn errors(&self) -> u64 {
        self.0.errors
    }
    /// 综合状态 dict
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("total_inferences", self.0.total_inferences)?;
        d.set_item("total_batch_inferences", self.0.total_batch_inferences)?;
        d.set_item("avg_latency_us", self.0.avg_latency_us)?;
        d.set_item("p99_latency_us", self.0.p99_latency_us)?;
        d.set_item("hot_reloads", self.0.hot_reloads)?;
        d.set_item("errors", self.0.errors)?;
        Ok(d)
    }
    fn __repr__(&self) -> String {
        format!(
            "InferenceStats(total={}, batch={}, avg={:.1}us, p99={:.1}us, hot_reloads={}, errors={})",
            self.0.total_inferences,
            self.0.total_batch_inferences,
            self.0.avg_latency_us,
            self.0.p99_latency_us,
            self.0.hot_reloads,
            self.0.errors,
        )
    }
}

impl From<RustStats> for PyInferenceStats {
    fn from(s: RustStats) -> Self {
        Self(s)
    }
}
impl From<PyInferenceStats> for RustStats {
    fn from(s: PyInferenceStats) -> Self {
        s.0
    }
}

pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyInferenceBackend>()?;
    parent.add_class::<PyDevice>()?;
    parent.add_class::<PyModelConfig>()?;
    parent.add_class::<PyObservation>()?;
    parent.add_class::<PyActionType>()?;
    parent.add_class::<PyAction>()?;
    parent.add_class::<PyBatchConfig>()?;
    parent.add_class::<PyInferenceStats>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_config_construct() {
        let cfg = PyModelConfig::new(
            "/tmp/m.onnx".into(),
            PyInferenceBackend::Onnx,
            PyDevice::cpu(),
            (1, 64, 128),
            3,
            false,
            4,
        );
        assert_eq!(cfg.output_dim(), 3);
        assert!(!cfg.fp16());
    }

    #[test]
    fn device_factories() {
        assert_eq!(PyDevice::cpu().kind(), "cpu");
        assert_eq!(PyDevice::cuda(0).kind(), "cuda");
        assert_eq!(PyDevice::cuda(0).cuda_device_id(), Some(0));
        assert_eq!(PyDevice::metal().kind(), "metal");
    }

    #[test]
    fn observation_construct() {
        let obs = PyObservation::new("BTC-USDT".into(), 1_000_000_000, vec![0.0f32; 128]);
        assert_eq!(obs.symbol(), "BTC-USDT");
        assert_eq!(obs.feature_dim(), 128);
    }

    #[test]
    fn action_to_dict_roundtrip() {
        let act = PyAction::new("buy", 0.95, 0.5, "model-1".into(), 250);
        Python::attach(|py| {
            let d = act.to_dict(py).unwrap();
            assert_eq!(
                d.get_item("action_type")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "buy"
            );
            assert_eq!(
                d.get_item("model_id")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "model-1"
            );
        });
    }

    #[test]
    fn batch_config_defaults() {
        let c = PyBatchConfig::new(32, 500, 2, 64, vec![], None);
        assert_eq!(c.max_batch_size(), 32);
        assert!(c.collect_cpu_cores().is_empty());
        assert!(c.collect_gpu_device_id().is_none());
    }

    #[test]
    fn register_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register;
    }
}
