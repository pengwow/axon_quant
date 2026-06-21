//! axon-tracker 端到端测试

use axon_tracker::{
    ExperimentId, ImageFormat, MetricValue, ParamValue, RunId, RunStatus, TrackerError,
};

// ═══════════════════════════════════════════════════════════════════════════
// ExperimentId 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_experiment_id_creation() {
    let id = ExperimentId("exp-001".into());
    assert_eq!(id.0, "exp-001");
}

#[test]
fn test_experiment_id_equality() {
    let id1 = ExperimentId("exp-001".into());
    let id2 = ExperimentId("exp-001".into());
    let id3 = ExperimentId("exp-002".into());
    assert_eq!(id1, id2);
    assert_ne!(id1, id3);
}

#[test]
fn test_experiment_id_serialization() {
    let id = ExperimentId("exp-001".into());
    let json = serde_json::to_string(&id).unwrap();
    let restored: ExperimentId = serde_json::from_str(&json).unwrap();
    assert_eq!(id, restored);
}

// ═══════════════════════════════════════════════════════════════════════════
// RunId 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_run_id_creation() {
    let id = RunId("run-001".into());
    assert_eq!(id.0, "run-001");
}

#[test]
fn test_run_id_equality() {
    let id1 = RunId("run-001".into());
    let id2 = RunId("run-001".into());
    let id3 = RunId("run-002".into());
    assert_eq!(id1, id2);
    assert_ne!(id1, id3);
}

#[test]
fn test_run_id_serialization() {
    let id = RunId("run-001".into());
    let json = serde_json::to_string(&id).unwrap();
    let restored: RunId = serde_json::from_str(&json).unwrap();
    assert_eq!(id, restored);
}

// ═══════════════════════════════════════════════════════════════════════════
// ImageFormat 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_image_format_variants() {
    assert_ne!(ImageFormat::Png, ImageFormat::Jpeg);
    assert_ne!(ImageFormat::Jpeg, ImageFormat::Svg);
    assert_ne!(ImageFormat::Png, ImageFormat::Svg);
}

#[test]
fn test_image_format_serialization() {
    let formats = vec![ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::Svg];
    for format in formats {
        let json = serde_json::to_string(&format).unwrap();
        let restored: ImageFormat = serde_json::from_str(&json).unwrap();
        assert_eq!(format, restored);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// MetricValue 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_metric_value_scalar() {
    let value = MetricValue::Scalar(42.0);
    assert!(matches!(value, MetricValue::Scalar(v) if v == 42.0));
}

#[test]
fn test_metric_value_histogram() {
    let value = MetricValue::Histogram {
        values: vec![1.0, 2.0, 3.0],
        bins: vec![0.0, 1.0, 2.0, 3.0],
    };
    assert!(matches!(value, MetricValue::Histogram { .. }));
}

#[test]
fn test_metric_value_image() {
    let value = MetricValue::Image {
        data: vec![0, 1, 2, 3],
        format: ImageFormat::Png,
        width: 100,
        height: 100,
    };
    assert!(matches!(value, MetricValue::Image { .. }));
}

#[test]
fn test_metric_value_table() {
    let value = MetricValue::Table {
        columns: vec!["name".into(), "value".into()],
        rows: vec![vec!["a".into(), "1".into()], vec!["b".into(), "2".into()]],
    };
    assert!(matches!(value, MetricValue::Table { .. }));
}

#[test]
fn test_metric_value_serialization() {
    let values = vec![
        MetricValue::Scalar(42.0),
        MetricValue::Histogram {
            values: vec![1.0, 2.0],
            bins: vec![0.0, 1.0, 2.0],
        },
        MetricValue::Table {
            columns: vec!["a".into()],
            rows: vec![vec!["1".into()]],
        },
    ];
    for value in values {
        let json = serde_json::to_string(&value).unwrap();
        let restored: MetricValue = serde_json::from_str(&json).unwrap();
        // 比较序列化后的 JSON 字符串（因为 f64 精度问题）
        let json2 = serde_json::to_string(&restored).unwrap();
        assert_eq!(json, json2);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// ParamValue 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_param_value_int() {
    let value = ParamValue::Int(42);
    assert_eq!(value.to_string(), "42");
}

#[test]
fn test_param_value_float() {
    let value = ParamValue::Float(std::f64::consts::PI);
    assert_eq!(value.to_string(), std::f64::consts::PI.to_string());
}

#[test]
fn test_param_value_string() {
    let value = ParamValue::String("hello".into());
    assert_eq!(value.to_string(), "hello");
}

#[test]
fn test_param_value_bool() {
    let value = ParamValue::Bool(true);
    assert_eq!(value.to_string(), "true");
}

#[test]
fn test_param_value_list() {
    let value = ParamValue::List(vec![
        ParamValue::Int(1),
        ParamValue::Int(2),
        ParamValue::Int(3),
    ]);
    assert_eq!(value.to_string(), "[1, 2, 3]");
}

#[test]
fn test_param_value_serialization() {
    let values = vec![
        ParamValue::Int(42),
        ParamValue::Float(std::f64::consts::PI),
        ParamValue::String("test".into()),
        ParamValue::Bool(false),
        ParamValue::List(vec![ParamValue::Int(1), ParamValue::Int(2)]),
    ];
    for value in values {
        let json = serde_json::to_string(&value).unwrap();
        let restored: ParamValue = serde_json::from_str(&json).unwrap();
        assert_eq!(value.to_string(), restored.to_string());
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// RunStatus 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_run_status_variants() {
    assert_ne!(RunStatus::Running, RunStatus::Completed);
    assert_ne!(RunStatus::Completed, RunStatus::Failed);
    assert_ne!(RunStatus::Failed, RunStatus::Killed);
}

#[test]
fn test_run_status_serialization() {
    let statuses = vec![
        RunStatus::Running,
        RunStatus::Completed,
        RunStatus::Failed,
        RunStatus::Killed,
    ];
    for status in statuses {
        let json = serde_json::to_string(&status).unwrap();
        let restored: RunStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(status, restored);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// TrackerError 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_tracker_error_display() {
    let errors: Vec<TrackerError> = vec![
        TrackerError::Network("connection failed".into()),
        TrackerError::Io("permission denied".into()),
        TrackerError::Parse("invalid json".into()),
        TrackerError::Auth("bad credentials".into()),
        TrackerError::RateLimited,
        TrackerError::ExperimentNotFound("exp-001".into()),
        TrackerError::RunNotFound("run-001".into()),
        TrackerError::Config("missing field".into()),
        TrackerError::Serialization("invalid format".into()),
    ];

    for err in errors {
        assert!(!err.to_string().is_empty());
    }
}

#[test]
fn test_tracker_error_network() {
    let err = TrackerError::Network("connection timeout".into());
    assert!(err.to_string().contains("connection timeout"));
}

#[test]
fn test_tracker_error_io() {
    let err = TrackerError::Io("file not found".into());
    assert!(err.to_string().contains("file not found"));
}

#[test]
fn test_tracker_error_parse() {
    let err = TrackerError::Parse("invalid format".into());
    assert!(err.to_string().contains("invalid format"));
}

#[test]
fn test_tracker_error_auth() {
    let err = TrackerError::Auth("bad api key".into());
    assert!(err.to_string().contains("bad api key"));
}

#[test]
fn test_tracker_error_rate_limited() {
    let err = TrackerError::RateLimited;
    assert!(err.to_string().contains("rate limited"));
}

#[test]
fn test_tracker_error_experiment_not_found() {
    let err = TrackerError::ExperimentNotFound("exp-001".into());
    assert!(err.to_string().contains("exp-001"));
}

#[test]
fn test_tracker_error_run_not_found() {
    let err = TrackerError::RunNotFound("run-001".into());
    assert!(err.to_string().contains("run-001"));
}

#[test]
fn test_tracker_error_artifact_too_large() {
    let err = TrackerError::ArtifactTooLarge {
        size: 1000000,
        limit: 500000,
    };
    assert!(err.to_string().contains("1000000"));
    assert!(err.to_string().contains("500000"));
}

#[test]
fn test_tracker_error_config() {
    let err = TrackerError::Config("missing endpoint".into());
    assert!(err.to_string().contains("missing endpoint"));
}

#[test]
fn test_tracker_error_serialization() {
    let err = TrackerError::Serialization("invalid json".into());
    assert!(err.to_string().contains("invalid json"));
}

#[test]
fn test_tracker_error_is_retryable() {
    assert!(TrackerError::Network("timeout".into()).is_retryable());
    assert!(TrackerError::RateLimited.is_retryable());
    assert!(!TrackerError::Auth("bad key".into()).is_retryable());
    assert!(!TrackerError::Config("missing".into()).is_retryable());
}
