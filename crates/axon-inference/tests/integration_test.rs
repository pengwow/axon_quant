use axon_inference::{ActionType, BatchConfig, Device, InferenceBackend, ModelConfig, Observation};

fn make_observation(features: Vec<f32>) -> Observation {
    Observation {
        symbol: "BTC-USDT".to_string(),
        timestamp_ns: 1_000_000_000,
        features,
    }
}

#[test]
fn test_observation_creation() {
    let obs = make_observation(vec![1.0, 2.0, 3.0]);
    assert_eq!(obs.symbol, "BTC-USDT");
    assert_eq!(obs.features.len(), 3);
}

#[test]
fn test_action_types() {
    assert_ne!(ActionType::Buy, ActionType::Sell);
    assert_ne!(ActionType::Hold, ActionType::Buy);
}

#[test]
fn test_batch_config_default() {
    let config = BatchConfig::default();
    assert_eq!(config.max_batch_size, 32);
    assert_eq!(config.collect_timeout_us, 500);
}

#[test]
fn test_model_config() {
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
    assert_eq!(config.input_shape, [1, 64, 128]);
}
