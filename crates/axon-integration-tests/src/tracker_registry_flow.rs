//! 场景 5：实验追踪全流程
//!
//! 验证：MemoryTracker 创建 → 参数/指标记录 → 查询

use axon_tracker::backends::MemoryTracker;
use axon_tracker::tracker::ExperimentTracker;
use axon_tracker::types::{MetricValue, ParamValue};

/// 场景 5.1: 创建 MemoryTracker
pub fn run_tracker_creation() {
    let tracker = MemoryTracker::new();
    let run_id = tracker.run_id();
    assert!(!run_id.0.is_empty(), "run_id 不应为空");
}

/// 场景 5.2: log_param + log_metric
pub fn run_param_metric_logging() {
    let tracker = MemoryTracker::new();
    tracker
        .log_param("learning_rate", &ParamValue::Float(0.01))
        .unwrap();
    tracker.log_metric("loss", 0.5, 0).unwrap();
    tracker.log_metric("loss", 0.3, 1).unwrap();
    let lr = tracker.get_param("learning_rate");
    assert!(lr.is_some(), "应能查询到参数");
    if let Some(ParamValue::Float(v)) = lr {
        assert!((v - 0.01).abs() < f64::EPSILON);
    }
}

/// 场景 5.3: get_metrics 返回正确数据
pub fn run_metrics_query() {
    let tracker = MemoryTracker::new();
    tracker.log_metric("accuracy", 0.95, 0).unwrap();
    tracker.log_metric("accuracy", 0.97, 1).unwrap();
    let metrics = tracker.get_metrics_by_key("accuracy");
    assert_eq!(metrics.len(), 2, "应有 2 条 accuracy 指标");
    // 验证值
    let values: Vec<f64> = metrics
        .iter()
        .map(|m| match m.value {
            MetricValue::Scalar(v) => v,
            _ => 0.0,
        })
        .collect();
    assert!((values[0] - 0.95).abs() < f64::EPSILON);
    assert!((values[1] - 0.97).abs() < f64::EPSILON);
}

/// 多参数记录
pub fn run_multi_param_logging() {
    let tracker = MemoryTracker::new();
    tracker.log_param("n_layers", &ParamValue::Int(3)).unwrap();
    tracker
        .log_param("activation", &ParamValue::String("relu".to_string()))
        .unwrap();
    tracker
        .log_param("use_batch_norm", &ParamValue::Bool(true))
        .unwrap();
    let all = tracker.get_all_params();
    assert_eq!(all.len(), 3);
    assert!(matches!(all.get("n_layers"), Some(ParamValue::Int(3))));
}

/// 状态管理
pub fn run_status_management() {
    use axon_tracker::types::RunStatus;
    let tracker = MemoryTracker::new();
    assert_eq!(tracker.get_status(), RunStatus::Running);
    tracker.finish(RunStatus::Completed).unwrap();
    assert_eq!(tracker.get_status(), RunStatus::Completed);
}
