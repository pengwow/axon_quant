//! axon-inference 端到端测试

use axon_inference::{
    Action, ActionType, BatchConfig, Device, InferenceBackend, InferenceError, InferenceStats,
    ModelConfig, Observation,
};
use std::path::PathBuf;

// ═══════════════════════════════════════════════════════════════════════════
// ModelConfig 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_model_config_creation() {
    let config = ModelConfig {
        path: "model.onnx".into(),
        backend: InferenceBackend::Onnx,
        device: Device::Cpu,
        input_shape: [1, 64, 128],
        output_dim: 3,
        fp16: false,
        num_threads: 4,
    };
    assert_eq!(config.backend, InferenceBackend::Onnx);
    assert_eq!(config.device, Device::Cpu);
    assert_eq!(config.input_shape, [1, 64, 128]);
    assert_eq!(config.output_dim, 3);
    assert!(!config.fp16);
    assert_eq!(config.num_threads, 4);
}

#[test]
fn test_model_config_serialization() {
    let config = ModelConfig {
        path: "model.onnx".into(),
        backend: InferenceBackend::Candle,
        device: Device::Cpu,
        input_shape: [1, 32, 64],
        output_dim: 5,
        fp16: true,
        num_threads: 8,
    };
    let json = serde_json::to_string(&config).unwrap();
    let restored: ModelConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.backend, InferenceBackend::Candle);
    assert!(restored.fp16);
}

// ═══════════════════════════════════════════════════════════════════════════
// Device 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_device_variants() {
    let cpu = Device::Cpu;
    let cuda = Device::Cuda(0);
    let metal = Device::Metal;

    assert_eq!(cpu, Device::Cpu);
    assert_eq!(cuda, Device::Cuda(0));
    assert_eq!(metal, Device::Metal);
}

#[test]
fn test_device_serialization() {
    let devices = vec![Device::Cpu, Device::Cuda(0), Device::Cuda(1), Device::Metal];
    for device in devices {
        let json = serde_json::to_string(&device).unwrap();
        let restored: Device = serde_json::from_str(&json).unwrap();
        assert_eq!(device, restored);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// InferenceBackend 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_backend_variants() {
    assert_eq!(InferenceBackend::Onnx, InferenceBackend::Onnx);
    assert_eq!(InferenceBackend::Tch, InferenceBackend::Tch);
    assert_eq!(InferenceBackend::Candle, InferenceBackend::Candle);
    assert_ne!(InferenceBackend::Onnx, InferenceBackend::Tch);
}

#[test]
fn test_backend_serialization() {
    let backends = vec![
        InferenceBackend::Onnx,
        InferenceBackend::Tch,
        InferenceBackend::Candle,
    ];
    for backend in backends {
        let json = serde_json::to_string(&backend).unwrap();
        let restored: InferenceBackend = serde_json::from_str(&json).unwrap();
        assert_eq!(backend, restored);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// BatchConfig 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_batch_config_default() {
    let config = BatchConfig::default();
    assert_eq!(config.max_batch_size, 32);
    assert_eq!(config.collect_timeout_us, 500);
    assert_eq!(config.num_workers, 2);
    assert_eq!(config.prealloc_buffer_size, 64);
    assert!(config.collect_cpu_cores.is_empty());
    assert!(config.collect_gpu_device_id.is_none());
}

#[test]
fn test_batch_config_serialization() {
    let config = BatchConfig {
        max_batch_size: 64,
        collect_timeout_us: 1000,
        num_workers: 4,
        prealloc_buffer_size: 128,
        collect_cpu_cores: vec![0, 1, 2, 3],
        collect_gpu_device_id: Some(0),
    };
    let json = serde_json::to_string(&config).unwrap();
    let restored: BatchConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.max_batch_size, 64);
    assert_eq!(restored.collect_cpu_cores, vec![0, 1, 2, 3]);
    assert_eq!(restored.collect_gpu_device_id, Some(0));
}

// ═══════════════════════════════════════════════════════════════════════════
// Observation 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_observation_creation() {
    let obs = Observation {
        symbol: "BTC-USDT".into(),
        timestamp_ns: 1_000_000_000,
        features: vec![0.1, 0.2, 0.3],
    };
    assert_eq!(obs.symbol, "BTC-USDT");
    assert_eq!(obs.timestamp_ns, 1_000_000_000);
    assert_eq!(obs.features.len(), 3);
}

#[test]
fn test_observation_serialization() {
    let obs = Observation {
        symbol: "ETH-USDT".into(),
        timestamp_ns: 2_000_000_000,
        features: vec![0.5; 128],
    };
    let json = serde_json::to_string(&obs).unwrap();
    let restored: Observation = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.symbol, "ETH-USDT");
    assert_eq!(restored.features.len(), 128);
}

// ═══════════════════════════════════════════════════════════════════════════
// Action 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_action_creation() {
    let action = Action {
        action_type: ActionType::Buy,
        confidence: 0.85,
        target_position: 0.5,
        model_id: "model_v1".into(),
        inference_time_us: 150,
    };
    assert_eq!(action.action_type, ActionType::Buy);
    assert!((action.confidence - 0.85).abs() < f32::EPSILON);
    assert!((action.target_position - 0.5).abs() < f32::EPSILON);
    assert_eq!(action.inference_time_us, 150);
}

#[test]
fn test_action_type_variants() {
    assert_ne!(ActionType::Hold, ActionType::Buy);
    assert_ne!(ActionType::Buy, ActionType::Sell);
    assert_ne!(ActionType::Sell, ActionType::ReduceLong);
    assert_ne!(ActionType::ReduceLong, ActionType::ReduceShort);
}

#[test]
fn test_action_serialization() {
    let action = Action {
        action_type: ActionType::Sell,
        confidence: 0.95,
        target_position: -0.3,
        model_id: "model_v2".into(),
        inference_time_us: 200,
    };
    let json = serde_json::to_string(&action).unwrap();
    let restored: Action = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.action_type, ActionType::Sell);
    assert!((restored.confidence - 0.95).abs() < f32::EPSILON);
}

// ═══════════════════════════════════════════════════════════════════════════
// InferenceStats 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_inference_stats_default() {
    let stats = InferenceStats::default();
    assert_eq!(stats.total_inferences, 0);
    assert_eq!(stats.total_batch_inferences, 0);
    assert_eq!(stats.avg_latency_us, 0.0);
    assert_eq!(stats.p99_latency_us, 0.0);
    assert_eq!(stats.hot_reloads, 0);
    assert_eq!(stats.errors, 0);
}

#[test]
fn test_inference_stats_serialization() {
    let stats = InferenceStats {
        total_inferences: 1000,
        total_batch_inferences: 100,
        avg_latency_us: 150.5,
        p99_latency_us: 500.0,
        hot_reloads: 5,
        errors: 2,
    };
    let json = serde_json::to_string(&stats).unwrap();
    let restored: InferenceStats = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.total_inferences, 1000);
    assert_eq!(restored.hot_reloads, 5);
}

// ═══════════════════════════════════════════════════════════════════════════
// InferenceError 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_error_model_not_found() {
    let err = InferenceError::ModelNotFound {
        path: PathBuf::from("missing.onnx"),
    };
    assert!(err.to_string().contains("missing.onnx"));
}

#[test]
fn test_error_model_load_failed() {
    let err = InferenceError::ModelLoadFailed {
        reason: "invalid format".into(),
    };
    assert!(err.to_string().contains("invalid format"));
}

#[test]
fn test_error_model_not_loaded() {
    let err = InferenceError::ModelNotLoaded;
    assert!(err.to_string().contains("not loaded"));
}

#[test]
fn test_error_inference_failed() {
    let err = InferenceError::InferenceFailed {
        reason: "timeout".into(),
    };
    assert!(err.to_string().contains("timeout"));
}

#[test]
fn test_error_dimension_mismatch() {
    let err = InferenceError::DimensionMismatch {
        expected: 128,
        actual: 64,
    };
    assert!(err.to_string().contains("128"));
    assert!(err.to_string().contains("64"));
}

#[test]
fn test_error_device_unavailable() {
    let err = InferenceError::DeviceUnavailable {
        device: Device::Cuda(0),
    };
    assert!(err.to_string().contains("Cuda"));
}

#[test]
fn test_error_hot_reload_failed() {
    let err = InferenceError::HotReloadFailed {
        reason: "file locked".into(),
    };
    assert!(err.to_string().contains("file locked"));
}

#[test]
fn test_error_onnx() {
    let err = InferenceError::Onnx("session creation failed".into());
    assert!(err.to_string().contains("session creation failed"));
}

#[test]
fn test_error_tch() {
    let err = InferenceError::Tch("tensor load failed".into());
    assert!(err.to_string().contains("tensor load failed"));
}

#[test]
fn test_error_candle() {
    let err = InferenceError::Candle("model parse failed".into());
    assert!(err.to_string().contains("model parse failed"));
}

// ═══════════════════════════════════════════════════════════════════════════
// Observation 扩展测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_observation_empty_features() {
    let obs = Observation {
        symbol: "BTC-USDT".into(),
        timestamp_ns: 1_000_000_000,
        features: vec![],
    };
    assert!(obs.features.is_empty());
}

#[test]
fn test_observation_large_features() {
    let obs = Observation {
        symbol: "BTC-USDT".into(),
        timestamp_ns: 1_000_000_000,
        features: vec![0.0; 1024],
    };
    assert_eq!(obs.features.len(), 1024);
}

#[test]
fn test_observation_different_symbols() {
    let symbols = vec!["BTC-USDT", "ETH-USDT", "SOL-USDT", "AAPL"];
    for symbol in symbols {
        let obs = Observation {
            symbol: symbol.into(),
            timestamp_ns: 1_000_000_000,
            features: vec![1.0, 2.0, 3.0],
        };
        assert_eq!(obs.symbol, symbol);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Action 扩展测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_action_type_all_variants() {
    let variants = [
        ActionType::Hold,
        ActionType::Buy,
        ActionType::Sell,
        ActionType::ReduceLong,
        ActionType::ReduceShort,
    ];
    assert_eq!(variants.len(), 5);
}

#[test]
fn test_action_confidence_range() {
    let action = Action {
        action_type: ActionType::Buy,
        confidence: 0.0,
        target_position: 0.0,
        model_id: "test".into(),
        inference_time_us: 0,
    };
    assert!((action.confidence - 0.0).abs() < f32::EPSILON);

    let action = Action {
        action_type: ActionType::Buy,
        confidence: 1.0,
        target_position: 1.0,
        model_id: "test".into(),
        inference_time_us: 0,
    };
    assert!((action.confidence - 1.0).abs() < f32::EPSILON);
}

// ═══════════════════════════════════════════════════════════════════════════
// InferenceBackend 扩展测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_backend_clone() {
    let backend = InferenceBackend::Onnx;
    let cloned = backend;
    assert_eq!(backend, cloned);
}

#[test]
fn test_backend_debug() {
    let backend = InferenceBackend::Candle;
    let debug_str = format!("{:?}", backend);
    assert!(debug_str.contains("Candle"));
}

// ═══════════════════════════════════════════════════════════════════════════
// Device 扩展测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_device_cuda_ids() {
    let devices = [Device::Cuda(0), Device::Cuda(1), Device::Cuda(2)];
    for (i, device) in devices.iter().enumerate() {
        assert_eq!(*device, Device::Cuda(i as u32));
    }
}

#[test]
fn test_device_debug() {
    let device = Device::Metal;
    let debug_str = format!("{:?}", device);
    assert!(debug_str.contains("Metal"));
}

// ═══════════════════════════════════════════════════════════════════════════
// BatchConfig 扩展测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_batch_config_custom_values() {
    let config = BatchConfig {
        max_batch_size: 64,
        collect_timeout_us: 1000,
        num_workers: 4,
        prealloc_buffer_size: 128,
        collect_cpu_cores: vec![0, 1, 2, 3],
        collect_gpu_device_id: Some(0),
    };
    assert_eq!(config.max_batch_size, 64);
    assert_eq!(config.num_workers, 4);
    assert_eq!(config.collect_cpu_cores.len(), 4);
    assert_eq!(config.collect_gpu_device_id, Some(0));
}

// ═══════════════════════════════════════════════════════════════════════════
// InferenceStats 扩展测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_inference_stats_clone() {
    let stats = InferenceStats {
        total_inferences: 100,
        total_batch_inferences: 10,
        avg_latency_us: 50.0,
        p99_latency_us: 100.0,
        hot_reloads: 1,
        errors: 0,
    };
    let cloned = stats.clone();
    assert_eq!(cloned.total_inferences, 100);
    assert_eq!(cloned.hot_reloads, 1);
}

#[test]
fn test_inference_stats_debug() {
    let stats = InferenceStats::default();
    let debug_str = format!("{:?}", stats);
    assert!(debug_str.contains("InferenceStats"));
}

// ═══════════════════════════════════════════════════════════════════════════
// ModelConfig 扩展测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_model_config_different_backends() {
    let backends = vec![
        InferenceBackend::Onnx,
        InferenceBackend::Tch,
        InferenceBackend::Candle,
    ];
    for backend in backends {
        let config = ModelConfig {
            path: "model.bin".into(),
            backend,
            device: Device::Cpu,
            input_shape: [1, 32, 64],
            output_dim: 3,
            fp16: false,
            num_threads: 4,
        };
        assert_eq!(config.backend, backend);
    }
}

#[test]
fn test_model_config_fp16() {
    let config = ModelConfig {
        path: "model.bin".into(),
        backend: InferenceBackend::Onnx,
        device: Device::Cpu,
        input_shape: [1, 32, 64],
        output_dim: 3,
        fp16: true,
        num_threads: 4,
    };
    assert!(config.fp16);
}

#[test]
fn test_model_config_debug() {
    let config = ModelConfig {
        path: "model.bin".into(),
        backend: InferenceBackend::Onnx,
        device: Device::Cpu,
        input_shape: [1, 32, 64],
        output_dim: 3,
        fp16: false,
        num_threads: 4,
    };
    let debug_str = format!("{:?}", config);
    assert!(debug_str.contains("ModelConfig"));
}
