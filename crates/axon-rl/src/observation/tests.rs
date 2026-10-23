//! 观测空间模块测试
//!
//! 覆盖范围：
//! - 类型构造（MarketState / FeatureConfig / Observation）
//! - 归一化器（Z-Score / Min-Max / Robust / Noop）
//! - RunningStats（Welford 在线更新）
//! - TickBuffer（push / window / 聚合）
//! - DefaultObservationSpace（构造 / build / shape / 归一化）
//! - 边界场景（空配置 / 零窗口 / 数据不足 / 重复名）

use super::*;
use crate::observation::error::ObservationError;
use crate::observation::types::{NormalizerType, ObservationSpace, extract_feature_value};

// ── 辅助：构造标准 MarketState ──────────────────────────────

fn sample_state(timestamp_ms: u64, close: f64) -> MarketState {
    MarketState {
        timestamp: timestamp_ms,
        symbol: "BTCUSDT".into(),
        open: close,
        high: close + 1.0,
        low: close - 1.0,
        close,
        last_price: close,
        volume: 100.0,
        bid: Some(close - 0.5),
        ask: Some(close + 0.5),
        spread: Some(1.0),
        position: 0.0,
        cash: 10_000.0,
        portfolio_value: 10_000.0,
        unrealized_pnl: 0.0,
        realized_pnl: 0.0,
    }
}

fn close_feature() -> FeatureConfig {
    FeatureConfig {
        name: "close".into(),
        source: FeatureSource::PriceField("close".into()),
        normalizer: NormalizerType::ZScore,
        clip_range: Some((-5.0, 5.0)),
    }
}

fn volume_feature() -> FeatureConfig {
    FeatureConfig {
        name: "volume".into(),
        source: FeatureSource::VolumeField("volume".into()),
        normalizer: NormalizerType::MinMax,
        clip_range: Some((0.0, 1.0)),
    }
}

// ── 类型测试 ──────────────────────────────────────────────

#[test]
fn test_observation_empty_shape() {
    let obs = Observation::empty();
    assert_eq!(obs.shape(), vec![0]);
    assert!(obs.features.is_empty());
}

#[test]
fn test_observation_f32_conversion() {
    let obs = Observation {
        features: vec![1.0, 2.0, 3.0],
        feature_names: vec!["a".into(), "b".into(), "c".into()],
        timestamp: Some(0),
    };
    let arr = obs.as_f32_slice();
    assert_eq!(arr, vec![1.0_f32, 2.0, 3.0]);
}

#[test]
fn test_market_state_default() {
    let s = MarketState::default();
    assert_eq!(s.timestamp, 0);
    assert_eq!(s.symbol, "");
    assert_eq!(s.close, 0.0);
    assert!(s.bid.is_none());
}

// ── RunningStats 测试 ────────────────────────────────────

#[test]
fn test_running_stats_initial_state() {
    let s = RunningStats::new();
    assert_eq!(s.count, 0);
    assert_eq!(s.mean, 0.0);
    assert_eq!(s.variance(), 0.0);
    assert!(s.min == f64::INFINITY);
    assert!(s.max == f64::NEG_INFINITY);
}

#[test]
fn test_running_stats_welford_correctness() {
    let mut s = RunningStats::new();
    let values = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
    for v in values {
        s.update(v);
    }
    // 期望均值 = 5.0，样本方差 = 32/7 ≈ 4.5714
    assert!((s.mean - 5.0).abs() < 1e-10);
    assert!((s.variance() - 32.0 / 7.0).abs() < 1e-10);
}

#[test]
fn test_running_stats_min_max() {
    let mut s = RunningStats::new();
    s.update(10.0);
    s.update(-3.0);
    s.update(7.0);
    assert_eq!(s.min, -3.0);
    assert_eq!(s.max, 10.0);
}

#[test]
fn test_running_stats_reset() {
    let mut s = RunningStats::new();
    s.update(1.0);
    s.update(2.0);
    s.reset();
    assert_eq!(s.count, 0);
    assert!(s.min == f64::INFINITY);
}

// ── Normalizer 测试 ──────────────────────────────────────

#[test]
fn test_zscore_normalize_insufficient_data() {
    let n = ZScoreNormalizer;
    let mut s = RunningStats::new();
    s.update(10.0);
    // count = 1 < 2 ⇒ 返回 0
    assert_eq!(n.normalize(10.0, &s), 0.0);
}

#[test]
fn test_zscore_normalize_zero_std() {
    let n = ZScoreNormalizer;
    let mut s = RunningStats::new();
    s.update(5.0);
    s.update(5.0);
    s.update(5.0);
    // std = 0 ⇒ 返回 0
    assert_eq!(n.normalize(5.0, &s), 0.0);
}

#[test]
fn test_zscore_normalize_basic() {
    let n = ZScoreNormalizer;
    let mut s = RunningStats::new();
    for v in [10.0, 20.0, 30.0] {
        s.update(v);
    }
    // mean=20, std≈8.165
    // normalize(20) = 0
    let z = n.normalize(20.0, &s);
    assert!(z.abs() < 1e-9);
}

#[test]
fn test_minmax_normalize_basic() {
    let n = MinMaxNormalizer;
    let mut s = RunningStats::new();
    s.update(0.0);
    s.update(10.0);
    // min=0, max=10
    assert_eq!(n.normalize(0.0, &s), 0.0);
    assert_eq!(n.normalize(5.0, &s), 0.5);
    assert_eq!(n.normalize(10.0, &s), 1.0);
    // clip
    assert_eq!(n.normalize(-5.0, &s), 0.0);
    assert_eq!(n.normalize(15.0, &s), 1.0);
}

#[test]
fn test_minmax_normalize_zero_range() {
    let n = MinMaxNormalizer;
    let mut s = RunningStats::new();
    s.update(5.0);
    s.update(5.0);
    // range = 0 ⇒ 返回 0.5
    assert_eq!(n.normalize(5.0, &s), 0.5);
}

#[test]
fn test_robust_normalize_empty_buffer() {
    let n = RobustNormalizer;
    let s = RunningStats::new();
    assert_eq!(n.normalize(0.0, &s), 0.0);
}

#[test]
fn test_robust_normalize_basic() {
    let n = RobustNormalizer;
    let mut s = RunningStats::new();
    for v in [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0] {
        s.update(v);
    }
    // 中位数 = 4.5, q1 = 2.5, q3 = 6.5, IQR = 4
    let z = n.normalize(4.5, &s);
    assert!(z.abs() < 1e-9);
}

#[test]
fn test_noop_normalizer() {
    let n = NoopNormalizer;
    let s = RunningStats::new();
    assert_eq!(n.normalize(42.5, &s), 42.5);
}

#[test]
fn test_make_normalizer_all_variants() {
    let _ = make_normalizer(&NormalizerType::ZScore);
    let _ = make_normalizer(&NormalizerType::MinMax);
    let _ = make_normalizer(&NormalizerType::Robust);
    let _ = make_normalizer(&NormalizerType::None);
}

// ── TickBuffer 测试 ──────────────────────────────────────

#[test]
fn test_tick_buffer_push_pop() {
    let mut buf = TickBuffer::new(3);
    assert!(buf.is_empty());
    buf.push(sample_state(0, 100.0));
    buf.push(sample_state(1, 101.0));
    buf.push(sample_state(2, 102.0));
    assert_eq!(buf.len(), 3);
    // 第四个：弹出最旧
    buf.push(sample_state(3, 103.0));
    assert_eq!(buf.len(), 3);
    assert_eq!(buf.to_vec()[0].close, 101.0);
    assert_eq!(buf.to_vec()[2].close, 103.0);
}

#[test]
fn test_tick_buffer_window() {
    let mut buf = TickBuffer::new(5);
    for i in 0..5 {
        buf.push(sample_state(i, 100.0 + i as f64));
    }
    let w = buf.window(3);
    assert_eq!(w.len(), 3);
    assert_eq!(w[0].close, 102.0);
    assert_eq!(w[2].close, 104.0);
}

#[test]
fn test_tick_buffer_mean_std() {
    let mut buf = TickBuffer::new(5);
    for i in 0..5 {
        buf.push(sample_state(i, 100.0 + i as f64));
    }
    // close: 100..104
    let mean = buf.mean_of(5, |s| s.close).unwrap();
    assert!((mean - 102.0).abs() < 1e-9);
    let std = buf.std_of(5, |s| s.close).unwrap();
    assert!(std > 0.0);
}

#[test]
fn test_tick_buffer_clear() {
    let mut buf = TickBuffer::new(3);
    buf.push(sample_state(0, 1.0));
    buf.push(sample_state(1, 2.0));
    buf.clear();
    assert!(buf.is_empty());
}

// ── DefaultObservationSpace 测试 ─────────────────────────

#[test]
fn test_observation_space_new_invalid_window() {
    let result = DefaultObservationSpace::new(0, vec![close_feature()]);
    assert!(matches!(
        result,
        Err(ObservationError::InvalidWindowSize(0))
    ));
}

#[test]
fn test_observation_space_new_empty_features() {
    let result = DefaultObservationSpace::new(10, vec![]);
    assert!(matches!(
        result,
        Err(ObservationError::FeatureCountMismatch { .. })
    ));
}

#[test]
fn test_observation_space_new_duplicate_name() {
    let result = DefaultObservationSpace::new(10, vec![close_feature(), close_feature()]);
    assert!(matches!(
        result,
        Err(ObservationError::NormalizationFailed { .. })
    ));
}

#[test]
fn test_observation_space_shape() {
    let space = DefaultObservationSpace::new(5, vec![close_feature(), volume_feature()]).unwrap();
    assert_eq!(space.shape(), vec![10]);
    assert_eq!(space.num_features(), 10);
}

#[test]
fn test_observation_space_low_high() {
    let space = DefaultObservationSpace::new(5, vec![close_feature()]).unwrap();
    assert_eq!(space.low(), vec![-5.0; 5]);
    assert_eq!(space.high(), vec![5.0; 5]);
}

#[test]
fn test_observation_space_gymnasium_box() {
    let space = DefaultObservationSpace::new(5, vec![close_feature()]).unwrap();
    let box_space = space.gymnasium_box();
    assert_eq!(box_space.shape, vec![5]);
    assert!(matches!(box_space.dtype, DType::Float32));
}

#[test]
fn test_observation_space_build_basic() {
    let space = DefaultObservationSpace::new(3, vec![close_feature()]).unwrap();
    let history: Vec<MarketState> = (0..3)
        .map(|i| sample_state(i * 60_000, 100.0 + i as f64))
        .collect();
    let state = history.last().unwrap();
    let obs = space.build(state, &history).unwrap();
    assert_eq!(obs.features.len(), 3);
    assert_eq!(obs.feature_names.len(), 3);
    assert!(obs.feature_names[0].contains("close"));
}

#[test]
fn test_observation_space_build_with_insufficient_history() {
    // history 短于 window 时应仍能 build（仅取可用）
    let space = DefaultObservationSpace::new(5, vec![close_feature()]).unwrap();
    let history: Vec<MarketState> = (0..2)
        .map(|i| sample_state(i * 60_000, 100.0 + i as f64))
        .collect();
    let state = history.last().unwrap();
    let obs = space.build(state, &history).unwrap();
    assert_eq!(obs.features.len(), 2);
}

#[test]
fn test_observation_space_feature_names() {
    let space = DefaultObservationSpace::new(2, vec![close_feature(), volume_feature()]).unwrap();
    let names = space.feature_names();
    assert_eq!(names.len(), 4);
    assert_eq!(names[0], "close_t0");
    assert_eq!(names[1], "close_t1");
    assert_eq!(names[2], "volume_t0");
    assert_eq!(names[3], "volume_t1");
}

#[test]
fn test_observation_space_reset_stats() {
    let mut space = DefaultObservationSpace::new(2, vec![close_feature()]).unwrap();
    space.running_stats_mut(0).update(10.0);
    space.running_stats_mut(0).update(20.0);
    assert_eq!(space.running_stats(0).count, 2);
    space.reset_stats();
    assert_eq!(space.running_stats(0).count, 0);
}

// ── 特征提取测试 ─────────────────────────────────────────

#[test]
fn test_extract_price_field_close() {
    let s = sample_state(0, 42.0);
    let v = extract_feature_value(&FeatureSource::PriceField("close".into()), &s, &[]).unwrap();
    assert_eq!(v, 42.0);
}

#[test]
fn test_extract_price_field_bid_ask() {
    let s = sample_state(0, 100.0);
    let bid = extract_feature_value(&FeatureSource::PriceField("bid".into()), &s, &[]).unwrap();
    let ask = extract_feature_value(&FeatureSource::PriceField("ask".into()), &s, &[]).unwrap();
    assert_eq!(bid, 99.5);
    assert_eq!(ask, 100.5);
}

#[test]
fn test_extract_price_field_missing() {
    let s = sample_state(0, 100.0);
    let result = extract_feature_value(&FeatureSource::PriceField("nonexistent".into()), &s, &[]);
    assert!(matches!(
        result,
        Err(ObservationError::FeatureNotFound { .. })
    ));
}

#[test]
fn test_extract_volume_field() {
    let s = sample_state(0, 100.0);
    let v = extract_feature_value(&FeatureSource::VolumeField("volume".into()), &s, &[]).unwrap();
    assert_eq!(v, 100.0);
}

#[test]
fn test_extract_position_fields() {
    let mut s = sample_state(0, 100.0);
    s.position = 1.5;
    s.cash = 5000.0;
    s.portfolio_value = 7500.0;
    let pos =
        extract_feature_value(&FeatureSource::PositionField("position".into()), &s, &[]).unwrap();
    let cash =
        extract_feature_value(&FeatureSource::PositionField("cash".into()), &s, &[]).unwrap();
    let pv = extract_feature_value(
        &FeatureSource::PositionField("portfolio_value".into()),
        &s,
        &[],
    )
    .unwrap();
    assert_eq!(pos, 1.5);
    assert_eq!(cash, 5000.0);
    assert_eq!(pv, 7500.0);
}

#[test]
fn test_extract_time_field_minute_of_day() {
    let s = sample_state(60_000 * 100, 100.0); // 第 100 分钟
    let v = extract_feature_value(&FeatureSource::TimeField(TimeFeature::MinuteOfDay), &s, &[])
        .unwrap();
    assert_eq!(v, 100.0);
}

#[test]
fn test_extract_time_field_sin_cos() {
    let s = sample_state(60_000 * 30, 100.0);
    let sin = extract_feature_value(
        &FeatureSource::TimeField(TimeFeature::SinCycle { period: 60 }),
        &s,
        &[],
    )
    .unwrap();
    let cos = extract_feature_value(
        &FeatureSource::TimeField(TimeFeature::CosCycle { period: 60 }),
        &s,
        &[],
    )
    .unwrap();
    // period=60, minutes=30 ⇒ sin(π)≈0, cos(π)=-1
    assert!(sin.abs() < 1e-9);
    assert!((cos - (-1.0)).abs() < 1e-9);
}

#[test]
fn test_extract_window_agg_mean() {
    let s = sample_state(0, 100.0);
    let history: Vec<MarketState> = (0..5).map(|i| sample_state(i, 100.0 + i as f64)).collect();
    let v = extract_feature_value(
        &FeatureSource::WindowAgg {
            source: Box::new(FeatureSource::PriceField("close".into())),
            agg: AggregationType::Mean,
            window: 5,
        },
        &s,
        &history,
    )
    .unwrap();
    // close: 100,101,102,103,104 ⇒ mean = 102
    assert!((v - 102.0).abs() < 1e-9);
}

#[test]
fn test_extract_window_agg_insufficient_data() {
    let s = sample_state(0, 100.0);
    let history: Vec<MarketState> = (0..2).map(|i| sample_state(i, 100.0)).collect();
    let result = extract_feature_value(
        &FeatureSource::WindowAgg {
            source: Box::new(FeatureSource::PriceField("close".into())),
            agg: AggregationType::Mean,
            window: 5,
        },
        &s,
        &history,
    );
    assert!(matches!(
        result,
        Err(ObservationError::InsufficientData { .. })
    ));
}

#[test]
fn test_extract_window_agg_min_max() {
    let s = sample_state(0, 100.0);
    let history: Vec<MarketState> = (0..5).map(|i| sample_state(i, 100.0 + i as f64)).collect();
    let mn = extract_feature_value(
        &FeatureSource::WindowAgg {
            source: Box::new(FeatureSource::PriceField("close".into())),
            agg: AggregationType::Min,
            window: 5,
        },
        &s,
        &history,
    )
    .unwrap();
    let mx = extract_feature_value(
        &FeatureSource::WindowAgg {
            source: Box::new(FeatureSource::PriceField("close".into())),
            agg: AggregationType::Max,
            window: 5,
        },
        &s,
        &history,
    )
    .unwrap();
    assert_eq!(mn, 100.0);
    assert_eq!(mx, 104.0);
}

#[test]
fn test_extract_window_agg_std_last() {
    let s = sample_state(0, 100.0);
    let history: Vec<MarketState> = (0..4).map(|i| sample_state(i, 100.0 + i as f64)).collect();
    let std = extract_feature_value(
        &FeatureSource::WindowAgg {
            source: Box::new(FeatureSource::PriceField("close".into())),
            agg: AggregationType::Std,
            window: 4,
        },
        &s,
        &history,
    )
    .unwrap();
    assert!(std > 0.0);
    let last = extract_feature_value(
        &FeatureSource::WindowAgg {
            source: Box::new(FeatureSource::PriceField("close".into())),
            agg: AggregationType::Last,
            window: 4,
        },
        &s,
        &history,
    )
    .unwrap();
    assert_eq!(last, 103.0);
}

// ── validate_observation_space 测试 ──────────────────────

#[test]
fn test_validate_observation_space_ok() {
    let result = validate_observation_space(&[close_feature(), volume_feature()], 5);
    assert!(result.is_ok());
}

#[test]
fn test_validate_observation_space_window_zero() {
    let result = validate_observation_space(&[close_feature()], 0);
    assert!(matches!(
        result,
        Err(ObservationError::InvalidWindowSize(0))
    ));
}

#[test]
fn test_validate_observation_space_empty_features() {
    let result = validate_observation_space(&[], 5);
    assert!(matches!(
        result,
        Err(ObservationError::FeatureCountMismatch { .. })
    ));
}
