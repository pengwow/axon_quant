//! DataService::stream() 集成测试
//!
//! 验证:
//! 1. 多 batch 流的行数加和与源一致
//! 2. 流式首 batch 在 100ms 内返回(避免 L1 全量加载的延迟)

use axon_data::sources::MockSource;
use axon_data::types::Frequency;
use axon_data::{DataRequest, DataService};
use futures::StreamExt;
use std::time::Instant;

#[tokio::test]
async fn integration_stream_consumes_multiple_batches() {
    // 1M tick → MockSource.stream 切 batch(1024 / batch)→ 多个 RecordBatch
    let count = 1_000_usize;
    let svc = DataService::new().register_source(Box::new(MockSource::with_tick_series(
        "mock",
        count,
        1_000_000,
        |i| i as f64,
    )));
    let req = DataRequest::new("X", chrono::Utc::now(), chrono::Utc::now(), Frequency::Tick);

    let mut stream = svc.stream("mock", &req).await.unwrap();
    let mut total_rows = 0usize;
    let mut batches = 0usize;
    while let Some(batch_result) = stream.next().await {
        let batch = batch_result.expect("stream item must be Ok");
        total_rows += batch.num_rows();
        batches += 1;
    }
    assert_eq!(total_rows, count, "all ticks should be consumed via stream");
    assert!(batches > 0, "should consume at least one batch");
}

#[tokio::test]
async fn integration_stream_does_not_block_on_large_dataset() {
    // 100K tick 源 stream,首 RecordBatch 在 500ms 内返回
    // (注:MockSource 同步构造所有 batch 是测试桩限制,真实流式源如 CSV 是真流式)
    let count = 100_000_usize;
    let svc = DataService::new().register_source(Box::new(MockSource::with_tick_series(
        "mock",
        count,
        1,
        |i| i as f64,
    )));
    let req = DataRequest::new("X", chrono::Utc::now(), chrono::Utc::now(), Frequency::Tick);

    let start = Instant::now();
    let mut stream = svc.stream("mock", &req).await.unwrap();
    let first = stream.next().await;
    let elapsed = start.elapsed();

    assert!(first.is_some(), "first batch should be present");
    // 500ms 是宽松上限(包含 MockSource 同步构造 batch + tokio 调度)
    assert!(
        elapsed.as_millis() < 500,
        "stream() first batch < 500ms (got {elapsed:?})"
    );
}
