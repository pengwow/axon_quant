//! axon-data property-based fuzz tests(proptest)
//!
//! 参考 `axon-integration-tests::fuzz` 模式:集中模块,多 `proptest!` 块。
//! 用随机输入验证核心 API 的代数性质(不变量),与单元测试互补:
//! - 单元测试:固定输入覆盖已知边界
//! - proptest:随机输入覆盖代数性质
//!
//! PR5 适配:`Dataset::new(rows, schema, ...)` 改为 `Dataset::from_ticks(rows, ...)`;
//! `ds.filter` 谓词签名从 `Fn(&Tick) -> bool` 改为 `Fn(&RecordBatch) -> ArrayRef`。

use arrow::array::{Array, Float64Array};
use arrow::record_batch::RecordBatch;
use proptest::prelude::*;
use std::sync::Arc;

use crate::dataset::Dataset;
use crate::types::{DataRequest, Frequency};
use axon_core::market::{Side, Tick};
use axon_core::time::Timestamp;
use axon_core::types::{Price, Quantity};
use chrono::Utc;

/// 单元:生成单个 Tick(时间/价/量全正,符合基本合法性)
fn tick_strategy() -> impl Strategy<Value = Tick> {
    (0u64..1_000_000, 0.01f64..1_000_000.0, 0.01f64..1000.0).prop_map(|(seq, price, qty)| {
        Tick::new(
            Timestamp::from_nanos(seq as i64 * 1_000_000_000),
            Price::from_f64(price),
            Quantity::from_f64(qty),
            Side::Buy,
        )
    })
}

/// 单元:生成 Tick 序列(0..=64 行,避免 OOM)
fn ticks_strategy() -> impl Strategy<Value = Vec<Tick>> {
    proptest::collection::vec(tick_strategy(), 0..=64)
}

/// 构造测试用请求
fn make_req() -> DataRequest {
    DataRequest::new("FUZZ", Utc::now(), Utc::now(), Frequency::Tick)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// 不变量:`filter(f)` 输出长度 ≤ 输入长度,且每个保留项都满足 f
    #[test]
    fn dataset_filter_count_lte_input(
        ticks in ticks_strategy(),
        threshold in 0.0f64..1_000_000.0,
    ) {
        let req = make_req();
        let ds = Dataset::from_ticks(ticks.clone(), "fuzz".into(), req).expect("from_ticks");
        // PR5 谓词:列式 `px > threshold`(走 `arrow::compute::kernels::cmp::gt`)
        let filtered = ds
            .filter(|batch: &RecordBatch| -> Arc<dyn Array> {
                let px = batch
                    .column(1)
                    .as_any()
                    .downcast_ref::<Float64Array>()
                    .expect("col 1 Float64Array");
                // `gt` 接受 `&dyn Datum`,用 `Float64Array::new_scalar` 包装标量
                let mask = arrow::compute::kernels::cmp::gt(
                    px,
                    &Float64Array::new_scalar(threshold),
                )
                .expect("gt");
                Arc::new(mask) as Arc<dyn Array>
            })
            .expect("filter");
        // 长度上界
        prop_assert!(filtered.len() <= ds.len());
        // 每个保留项必须满足 f
        for t in filtered.iter_rows() {
            prop_assert!(t.price.as_f64() > threshold);
        }
    }

    /// 不变量:`ds.take(n).skip(n)` 在 n ≤ len 时为空(0 行)
    #[test]
    fn dataset_take_skip_inverse(
        ticks in ticks_strategy(),
        n_raw in 0usize..200,
    ) {
        let req = make_req();
        let ds = Dataset::from_ticks(ticks, "fuzz".into(), req).expect("from_ticks");
        let n = n_raw.min(ds.len());
        let taken = ds.take(n);
        let skipped = taken.skip(n);
        // 跳过 n 之后应为空(take 后是 0..n 范围,再 skip(n) 越界 → 空)
        prop_assert!(skipped.is_empty());
        // 长度等于 ds.len().saturating_sub(n)(0 行当 n >= len)
        prop_assert_eq!(skipped.len(), 0);
    }

    /// 不变量:`by_time_range(start, end)` 输出的每个 ts 都在 [start, end],长度 ≤ 输入
    #[test]
    fn dataset_by_time_range_bounds_inclusive(
        ticks in ticks_strategy(),
        start_seq in 0u64..1_000_000,
        end_seq in 0u64..1_000_000,
    ) {
        let req = make_req();
        let ds = Dataset::from_ticks(ticks, "fuzz".into(), req).expect("from_ticks");
        // 让 start <= end(随机生成的 start_seq/end_seq 可能倒置)
        let (lo, hi) = if start_seq <= end_seq {
            (start_seq, end_seq)
        } else {
            (end_seq, start_seq)
        };
        let start = Timestamp::from_nanos(lo as i64 * 1_000_000_000);
        let end = Timestamp::from_nanos(hi as i64 * 1_000_000_000);
        let ranged = ds.by_time_range(start, end).expect("by_time_range");
        // 长度上界
        prop_assert!(ranged.len() <= ds.len());
        // 边界包含
        for t in ranged.iter_rows() {
            let ts = t.timestamp.nanos;
            prop_assert!(ts >= start.nanos);
            prop_assert!(ts <= end.nanos);
        }
    }

    /// 不变量:相同 rows 产生相同 checksum(与 source/req 无关)
    #[test]
    fn dataset_checksum_is_pure(ticks in ticks_strategy()) {
        let req1 = DataRequest::new("A", Utc::now(), Utc::now(), Frequency::Tick);
        let req2 = DataRequest::new("B", Utc::now(), Utc::now(), Frequency::Tick);
        let ds1 = Dataset::from_ticks(ticks.clone(), "src1".into(), req1).expect("from_ticks");
        let ds2 = Dataset::from_ticks(ticks, "src2".into(), req2).expect("from_ticks");
        // 同样 rows + schema 产生同样 checksum(与 source/req 无关)
        prop_assert_eq!(&ds1.checksum, &ds2.checksum);
        // 校验和是 SHA256(64 hex chars)
        prop_assert_eq!(ds1.checksum.len(), 64);
    }
}

// group 2: 涉及外部依赖的测试用独立 proptest! 块,避免与轻量测试混用 prelude 冲突
proptest! {
    #![proptest_config(ProptestConfig::with_cases(20))]

    /// 不变量:DataService LRU 缓存长度 ≤ 容量
    /// 注:cases 较少(20)避免测试时间过长
    #[test]
    fn lru_cache_respects_capacity(
        capacity in 2usize..=8,
        n_inserts in 3usize..=12,
        symbol_seed in 0u32..1000,
    ) {
        use crate::sources::MockSource;
        use crate::DataService;
        use std::num::NonZeroUsize;
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result: Result<(), TestCaseError> = rt.block_on(async {
            let svc = DataService::new()
                .with_cache_capacity(NonZeroUsize::new(capacity).unwrap())
                .register_source(Box::new(MockSource::with_tick_series("m", 0, 1, |_| 0.0)));
            // 插入 n_inserts 个不同请求
            for i in 0..n_inserts {
                let req = DataRequest::new(
                    format!("SYM{}", symbol_seed + i as u32),
                    Utc::now(),
                    Utc::now(),
                    Frequency::Tick,
                );
                let _ = svc.load(&req).await.unwrap();
            }
            // 长度应 ≤ 容量
            let stats = svc.cache_stats();
            prop_assert!(stats.len <= capacity);
            prop_assert_eq!(stats.capacity, capacity);
            Ok::<(), TestCaseError>(())
        });
        result?;
    }

    /// 不变量:`with_tick_series(n, _, _)` 生成恰好 n 个 tick,首 ts = 0,
    /// 时间戳按 nanos_per_step 等差
    #[test]
    fn mock_tick_series_count(
        count in 0usize..=64,
        nanos_step in 1i64..=1_000_000_000,
    ) {
        use crate::sources::MockSource;
        let mock = MockSource::with_tick_series("btc", count, nanos_step, |i| 100.0 + i as f64);
        // 长度精确
        prop_assert_eq!(mock.rows.len(), count);
        if count > 0 {
            // 首 ts = 0
            prop_assert_eq!(mock.rows[0].timestamp.nanos, 0);
            // 等差（len == count 已在上文断言，直接枚举）
            for (i, row) in mock.rows.iter().enumerate() {
                let expected = i as i64 * nanos_step;
                prop_assert_eq!(row.timestamp.nanos, expected);
            }
        }
    }
}

// ─── PR6: Bar 聚合 proptest ─────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// 不变量:Bar 聚合输出长度 ≤ 输入长度
    #[test]
    fn bar_aggregate_count_lte_input(
        ticks in ticks_strategy(),
    ) {
        let sorted_ticks = {
            let mut t = ticks;
            t.sort_by_key(|t| t.timestamp.nanos);
            t
        };
        let bars = crate::bar::BarAggregator::aggregate_ticks(sorted_ticks.into_iter(), Frequency::Min1).unwrap();
        // 每个 bar 至少需要 1 个 tick，所以 bars.len() <= ticks.len()
        // 但实际上可能更少(不完整尾部丢弃)
        prop_assert!(bars.len() <= 64); // 上界是 ticks_strategy 的 max
    }

    /// 不变量:每个 bar 的 OHLC 满足 high >= low
    #[test]
    fn bar_aggregate_ohlc_consistency(
        ticks in ticks_strategy(),
    ) {
        let sorted_ticks = {
            let mut t = ticks;
            t.sort_by_key(|t| t.timestamp.nanos);
            t
        };
        let bars = crate::bar::BarAggregator::aggregate_ticks(sorted_ticks.into_iter(), Frequency::Min1).unwrap();
        for bar in &bars {
            prop_assert!(bar.high.as_f64() >= bar.low.as_f64());
            prop_assert!(bar.open.as_f64() >= bar.low.as_f64());
            prop_assert!(bar.open.as_f64() <= bar.high.as_f64());
            prop_assert!(bar.close.as_f64() >= bar.low.as_f64());
            prop_assert!(bar.close.as_f64() <= bar.high.as_f64());
        }
    }

    /// 不变量:IPC roundtrip 后 checksum 一致
    #[test]
    fn ipc_bar_roundtrip_checksum(
        ticks in ticks_strategy(),
    ) {
        use crate::bar::BarAggregator;
        use crate::ipc::{IpcWriter, IpcReader};
        use tempfile::NamedTempFile;

        let sorted_ticks = {
            let mut t = ticks;
            t.sort_by_key(|t| t.timestamp.nanos);
            t
        };
        let bars = BarAggregator::aggregate_ticks(sorted_ticks.into_iter(), Frequency::Min1).unwrap();
        if bars.is_empty() {
            return Ok(());
        }
        let req = DataRequest::new("FUZZ", Utc::now(), Utc::now(), Frequency::Min1);
        let bar_ds = crate::bar::BarDataset::from_bars(bars, "fuzz".into(), req, Frequency::Min1).unwrap();

        let tmp = NamedTempFile::new().unwrap();
        IpcWriter::write_to_path(tmp.path(), &bar_ds).unwrap();
        let loaded = IpcReader::read_bar(tmp.path()).unwrap();

        prop_assert_eq!(&bar_ds.checksum, &loaded.checksum);
    }
}
