//! 端到端测试:`ReplayStreamSource::from_csv` CSV 真实回放
//!
//! ## 测试目标
//!
//! 验证 0.4.0 新增的 `ReplayStreamSource::from_csv` / `from_csv_with_mapping`:
//! 1. 默认 4 列 `timestamp,price,quantity,side` 格式正确解析
//! 2. header 行被跳过(数据行从 line 2 开始)
//! 3. 自定义列映射 + 微秒单位正确换算
//! 4. 文件不存在 → `StreamError::FileNotFound`
//! 5. 数字/方向列解析失败 → `StreamError::ParseError(line N: ...)`
//!
//! ## 测试基础设施
//!
//! 用 `tempfile` crate(dev-dep)的 `tempfile::NamedTempFile` 创建临时 CSV。
//! 0.4.0 0 增量(workspace 已有)。
//!
//! 运行:`cargo test -p axon-backtest --test csv_replay_integration`

use std::io::Write;

use axon_backtest::streaming::{
    CsvMapping, MarketDataEvent, ReplayStreamSource, StreamDataSource, StreamError, TimestampUnit,
};
use axon_core::market::Side;
use axon_core::types::Symbol;

fn btc() -> Symbol {
    Symbol::from("BTC-USDT")
}

/// 把字符串写入 NamedTempFile,返回 path
fn write_csv(content: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::Builder::new()
        .suffix(".csv")
        .tempfile()
        .expect("create tempfile");
    f.write_all(content.as_bytes()).expect("write");
    f.flush().expect("flush");
    f
}

// ── 1. 默认 4 列格式 ────────────────────────────────────────────────

#[test]
fn csv_default_format_4_columns() {
    // 含 header,默认列映射:ts nanos / price / qty / side
    let csv = "timestamp,price,quantity,side\n1000,100.0,0.5,buy\n2000,101.0,0.6,sell\n3000,102.0,0.7,buy\n";
    let f = write_csv(csv);
    let path = f.path().to_path_buf();

    let src = ReplayStreamSource::from_csv(&path, btc()).expect("from_csv ok");
    assert_eq!(src.remaining(), 3);
    assert_eq!(src.consumed(), 0);

    // path 在 path 字段被保留
    assert_eq!(src.name(), format!("replay:{}", path.display()));
}

#[test]
fn csv_with_header_row() {
    // 含 header
    let csv = "timestamp,price,quantity,side\n1000,100.0,0.5,buy\n2000,101.0,0.6,sell\n";
    let f = write_csv(csv);
    let src = ReplayStreamSource::from_csv(f.path(), btc()).expect("from_csv ok");
    // header 不算 1 行数据
    assert_eq!(src.remaining(), 2);
}

// ── 2. 自定义列映射 + 微秒单位 ──────────────────────────────────────

#[test]
fn csv_custom_mapping_and_timestamp_unit_micros() {
    // 列序:side, quantity, price, timestamp(微秒)
    let csv = "buy,0.5,100.0,1000\nsell,0.6,101.0,2000\n";
    let mapping = CsvMapping {
        has_header: false,
        side_col: 0,
        quantity_col: 1,
        price_col: 2,
        timestamp_col: 3,
        timestamp_unit: TimestampUnit::Micros,
    };
    let f = write_csv(csv);
    let src =
        ReplayStreamSource::from_csv_with_mapping(f.path(), btc(), mapping).expect("from_csv ok");
    assert_eq!(src.remaining(), 2);

    // 验证时间戳已转换为纳秒
    let block_on = futures::executor::block_on;
    let mut src = src;
    let e1 = block_on(src.next_event()).expect("e1");
    if let MarketDataEvent::Tick { tick, .. } = e1 {
        assert_eq!(tick.timestamp.nanos, 1_000_000); // 1000 micros = 1ms = 1e6 ns
        assert_eq!(tick.price.as_f64(), 100.0);
        assert_eq!(tick.quantity.as_f64(), 0.5);
        assert_eq!(tick.side, Side::Buy);
    } else {
        panic!("e1 应为 Tick");
    }
}

// ── 3. 文件不存在 → FileNotFound ──────────────────────────────────

#[test]
fn csv_missing_file_returns_file_not_found() {
    let result = ReplayStreamSource::from_csv("/tmp/this_path_should_not_exist_99999.csv", btc());
    assert!(matches!(result, Err(StreamError::FileNotFound(_))));
}

// ── 4. 数字列含字母 → ParseError ───────────────────────────────────

#[test]
fn csv_malformed_line_returns_parse_error() {
    // line 2 数据行 price 列含字母
    let csv = "timestamp,price,quantity,side\n1000,NOT_A_NUMBER,0.5,buy\n";
    let f = write_csv(csv);
    let result = ReplayStreamSource::from_csv(f.path(), btc());
    match result {
        Err(StreamError::ParseError(msg)) => {
            // 错误信息应包含行号 2 + price 字段
            assert!(msg.contains("line 2"), "msg 应包含 'line 2',实为 {msg}");
            assert!(msg.contains("price"), "msg 应指明 price 字段,实为 {msg}");
        }
        other => panic!("期望 ParseError,实为 {other:?}"),
    }
}
