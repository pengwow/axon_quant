//! 端到端测试:axon-data 数据服务完整流程
//!
//! ## 5 个测试场景
//!
//! 1. `data_service_load_and_cache_pipeline`:注册源 → load → 缓存命中 → cache stats
//! 2. `data_dataset_operations`:Dataset 创建 → iter_rows → checksum → len
//! 3. `data_feature_pipeline_transform`:FeaturePipeline fit_transform → 验证输出
//! 4. `data_request_serialization`:DataRequest JSON 序列化 roundtrip
//! 5. `data_multi_source_selection`:多源注册 → 指定源查询 → 验证选择正确
//!
//! 运行:`cargo test -p axon-data --test e2e_data_pipeline`

use axon_core::market::{Side, Tick};
use axon_core::time::Timestamp;
use axon_core::types::{Price, Quantity};
use axon_data::DataService;
use axon_data::pipeline::{FeaturePipeline, ZScoreNormalizer};
use axon_data::sources::MockSource;
use axon_data::types::{DataRequest, Frequency};
use chrono::Utc;

// ── helpers ────────────────────────────────────────────────────────────

fn make_tick(price: f64, nanos: i64) -> Tick {
    Tick::new(
        Timestamp::from_nanos(nanos),
        Price::from_f64(price),
        Quantity::from_f64(1.0),
        Side::Buy,
    )
}

fn rising_ticks(n: usize) -> Vec<Tick> {
    (0..n)
        .map(|i| make_tick(100.0 + i as f64, i as i64 * 1_000_000_000))
        .collect()
}

// ── 1. DataService: load → 缓存命中 → cache stats ────────────────────

#[tokio::test]
async fn data_service_load_and_cache_pipeline() {
    let ticks = rising_ticks(10);
    let svc = DataService::new().register_source(Box::new(MockSource::with_rows("mock", ticks)));
    let req = DataRequest::new("BTCUSDT", Utc::now(), Utc::now(), Frequency::Tick);

    // 第一次 load → 缓存未命中
    let ds1 = svc.load(&req).await.unwrap();
    assert_eq!(ds1.len(), 10);

    // 第二次 load → 缓存命中
    let ds2 = svc.load(&req).await.unwrap();
    assert_eq!(ds2.len(), 10);

    // 验证缓存统计
    let stats = svc.cache_stats();
    assert!(stats.hits > 0, "应有缓存命中");
    assert!(stats.len > 0);
}

// ── 2. Dataset: iter_rows → checksum → len ─────────────────────────────

#[tokio::test]
async fn data_dataset_operations() {
    let ticks = rising_ticks(5);
    let svc = DataService::new().register_source(Box::new(MockSource::with_rows("mock", ticks)));
    let req = DataRequest::new("BTCUSDT", Utc::now(), Utc::now(), Frequency::Tick);
    let ds = svc.load(&req).await.unwrap();

    // 验证 len
    assert_eq!(ds.len(), 5);

    // 验证 iter_rows
    let rows: Vec<Tick> = ds.iter_rows().collect();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].price.as_f64(), 100.0);
    assert_eq!(rows[4].price.as_f64(), 104.0);

    // 验证 checksum 非空
    let cs = &ds.checksum;
    assert!(!cs.is_empty());
}

// ── 3. FeaturePipeline: fit_transform → 验证输出 ───────────────────────

#[tokio::test]
async fn data_feature_pipeline_transform() {
    let ticks = rising_ticks(20);
    let svc = DataService::new().register_source(Box::new(MockSource::with_rows("mock", ticks)));
    let req = DataRequest::new("BTCUSDT", Utc::now(), Utc::now(), Frequency::Tick);
    let ds = svc.load(&req).await.unwrap();

    // 构造 pipeline
    let mut pipeline = FeaturePipeline::new().with_normalizer(Box::new(ZScoreNormalizer::new()));

    // fit_transform
    let result = pipeline.fit_transform(&ds);

    // 验证输出非空
    assert!(result.n_samples > 0);
    assert!(result.n_features > 0);
}

// ── 4. DataRequest JSON 序列化 roundtrip ──────────────────────────────

#[test]
fn data_request_serialization() {
    let start = Utc::now();
    let end = start + chrono::Duration::hours(1);
    let req = DataRequest::new("BTCUSDT", start, end, Frequency::Min5)
        .with_fields(vec!["price".into(), "volume".into()])
        .with_source("binance");

    let json = serde_json::to_string(&req).unwrap();
    let restored: DataRequest = serde_json::from_str(&json).unwrap();

    assert_eq!(restored.symbol, "BTCUSDT");
    assert_eq!(restored.frequency, Frequency::Min5);
    assert_eq!(restored.fields, vec!["price", "volume"]);
    assert_eq!(restored.source.as_deref(), Some("binance"));
}

// ── 5. 多源注册 → 指定源查询 → 验证选择正确 ──────────────────────────

#[tokio::test]
async fn data_multi_source_selection() {
    let ticks_a = rising_ticks(5);
    let ticks_b: Vec<Tick> = (0..5)
        .map(|i| make_tick(200.0 + i as f64, i as i64 * 1_000_000_000))
        .collect();

    let svc = DataService::new()
        .register_source(Box::new(MockSource::with_rows("src_a", ticks_a)))
        .register_source(Box::new(MockSource::with_rows("src_b", ticks_b)));

    // 查询 src_a
    let req_a =
        DataRequest::new("BTCUSDT", Utc::now(), Utc::now(), Frequency::Tick).with_source("src_a");
    let ds_a = svc.load(&req_a).await.unwrap();
    assert_eq!(ds_a.source, "src_a");
    let rows_a: Vec<Tick> = ds_a.iter_rows().collect();
    assert_eq!(rows_a[0].price.as_f64(), 100.0);

    // 查询 src_b
    let req_b =
        DataRequest::new("BTCUSDT", Utc::now(), Utc::now(), Frequency::Tick).with_source("src_b");
    let ds_b = svc.load(&req_b).await.unwrap();
    assert_eq!(ds_b.source, "src_b");
    let rows_b: Vec<Tick> = ds_b.iter_rows().collect();
    assert_eq!(rows_b[0].price.as_f64(), 200.0);
}
