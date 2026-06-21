//! axon-data 端到端测试

use axon_data::{DataRequest, Frequency};
use chrono::{DateTime, Utc};

// ═══════════════════════════════════════════════════════════════════════════
// Frequency 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_frequency_as_str() {
    assert_eq!(Frequency::Tick.as_str(), "tick");
    assert_eq!(Frequency::Min1.as_str(), "1m");
    assert_eq!(Frequency::Min5.as_str(), "5m");
    assert_eq!(Frequency::Min15.as_str(), "15m");
    assert_eq!(Frequency::Min30.as_str(), "30m");
    assert_eq!(Frequency::Hour1.as_str(), "1h");
    assert_eq!(Frequency::Hour4.as_str(), "4h");
    assert_eq!(Frequency::Day1.as_str(), "1d");
    assert_eq!(Frequency::Week1.as_str(), "1w");
    assert_eq!(Frequency::Month1.as_str(), "1M");
}

#[test]
fn test_frequency_is_bar() {
    assert!(!Frequency::Tick.is_bar());
    assert!(Frequency::Min1.is_bar());
    assert!(Frequency::Min5.is_bar());
    assert!(Frequency::Hour1.is_bar());
    assert!(Frequency::Day1.is_bar());
}

#[test]
fn test_frequency_serialization() {
    let frequencies = vec![
        Frequency::Tick,
        Frequency::Min1,
        Frequency::Min5,
        Frequency::Hour1,
        Frequency::Day1,
    ];
    for freq in frequencies {
        let json = serde_json::to_string(&freq).unwrap();
        let restored: Frequency = serde_json::from_str(&json).unwrap();
        assert_eq!(freq, restored);
    }
}

#[test]
fn test_frequency_variants() {
    assert_ne!(Frequency::Tick, Frequency::Min1);
    assert_ne!(Frequency::Min1, Frequency::Min5);
    assert_ne!(Frequency::Hour1, Frequency::Day1);
}

// ═══════════════════════════════════════════════════════════════════════════
// DataRequest 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_data_request_creation() {
    let start: DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let end: DateTime<Utc> = "2024-01-02T00:00:00Z".parse().unwrap();
    let req = DataRequest::new("BTCUSDT", start, end, Frequency::Min1);
    assert_eq!(req.symbol, "BTCUSDT");
    assert_eq!(req.frequency, Frequency::Min1);
    assert!(req.fields.is_empty());
    assert!(req.source.is_none());
}

#[test]
fn test_data_request_with_fields() {
    let start: DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let end: DateTime<Utc> = "2024-01-02T00:00:00Z".parse().unwrap();
    let req = DataRequest::new("BTCUSDT", start, end, Frequency::Min1).with_fields(vec![
        "open".into(),
        "close".into(),
        "volume".into(),
    ]);
    assert_eq!(req.fields.len(), 3);
    assert!(req.fields.contains(&"open".to_string()));
    assert!(req.fields.contains(&"close".to_string()));
}

#[test]
fn test_data_request_with_source() {
    let start: DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let end: DateTime<Utc> = "2024-01-02T00:00:00Z".parse().unwrap();
    let req = DataRequest::new("BTCUSDT", start, end, Frequency::Min1).with_source("binance");
    assert_eq!(req.source, Some("binance".to_string()));
}

#[test]
fn test_data_request_serialization() {
    let start: DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let end: DateTime<Utc> = "2024-01-02T00:00:00Z".parse().unwrap();
    let req = DataRequest::new("BTCUSDT", start, end, Frequency::Hour1);
    let json = serde_json::to_string(&req).unwrap();
    let restored: DataRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.symbol, "BTCUSDT");
    assert_eq!(restored.frequency, Frequency::Hour1);
}

#[test]
fn test_data_request_different_symbols() {
    let start: DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let end: DateTime<Utc> = "2024-01-02T00:00:00Z".parse().unwrap();
    let symbols = vec!["BTCUSDT", "ETHUSDT", "AAPL", "TSLA"];
    for symbol in symbols {
        let req = DataRequest::new(symbol, start, end, Frequency::Day1);
        assert_eq!(req.symbol, symbol);
    }
}

#[test]
fn test_data_request_different_frequencies() {
    let start: DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let end: DateTime<Utc> = "2024-01-02T00:00:00Z".parse().unwrap();
    let frequencies = vec![
        Frequency::Tick,
        Frequency::Min1,
        Frequency::Min5,
        Frequency::Hour1,
        Frequency::Day1,
    ];
    for freq in frequencies {
        let req = DataRequest::new("BTCUSDT", start, end, freq);
        assert_eq!(req.frequency, freq);
    }
}

#[test]
fn test_data_request_with_multiple_fields() {
    let start: DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let end: DateTime<Utc> = "2024-01-02T00:00:00Z".parse().unwrap();
    let req = DataRequest::new("BTCUSDT", start, end, Frequency::Min1).with_fields(vec![
        "open".into(),
        "high".into(),
        "low".into(),
        "close".into(),
        "volume".into(),
    ]);
    assert_eq!(req.fields.len(), 5);
}

#[test]
fn test_data_request_with_empty_fields() {
    let start: DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let end: DateTime<Utc> = "2024-01-02T00:00:00Z".parse().unwrap();
    let req = DataRequest::new("BTCUSDT", start, end, Frequency::Min1).with_fields(vec![]);
    assert!(req.fields.is_empty());
}

#[test]
fn test_data_request_with_custom_source() {
    let start: DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let end: DateTime<Utc> = "2024-01-02T00:00:00Z".parse().unwrap();
    let sources = vec!["binance", "okx", "coinbase", "kraken"];
    for source in sources {
        let req = DataRequest::new("BTCUSDT", start, end, Frequency::Min1).with_source(source);
        assert_eq!(req.source, Some(source.to_string()));
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Frequency 扩展测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_frequency_all_variants() {
    let variants = vec![
        Frequency::Tick,
        Frequency::Min1,
        Frequency::Min5,
        Frequency::Min15,
        Frequency::Min30,
        Frequency::Hour1,
        Frequency::Hour4,
        Frequency::Day1,
        Frequency::Week1,
        Frequency::Month1,
    ];
    assert_eq!(variants.len(), 10);
}

#[test]
fn test_frequency_clone() {
    let freq = Frequency::Min1;
    let cloned = freq;
    assert_eq!(freq, cloned);
}

#[test]
fn test_frequency_debug() {
    let freq = Frequency::Hour1;
    let debug_str = format!("{:?}", freq);
    assert!(debug_str.contains("Hour1"));
}

#[test]
fn test_frequency_hash() {
    use std::collections::HashMap;
    let mut map = HashMap::new();
    map.insert(Frequency::Min1, "1m");
    map.insert(Frequency::Hour1, "1h");
    assert_eq!(map.get(&Frequency::Min1), Some(&"1m"));
    assert_eq!(map.get(&Frequency::Hour1), Some(&"1h"));
}

// ═══════════════════════════════════════════════════════════════════════════
// DataRequest 扩展测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_data_request_clone() {
    let start: DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let end: DateTime<Utc> = "2024-01-02T00:00:00Z".parse().unwrap();
    let req = DataRequest::new("BTCUSDT", start, end, Frequency::Min1);
    let cloned = req.clone();
    assert_eq!(req, cloned);
}

#[test]
fn test_data_request_debug() {
    let start: DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let end: DateTime<Utc> = "2024-01-02T00:00:00Z".parse().unwrap();
    let req = DataRequest::new("BTCUSDT", start, end, Frequency::Min1);
    let debug_str = format!("{:?}", req);
    assert!(debug_str.contains("BTCUSDT"));
}

#[test]
fn test_data_request_eq() {
    let start: DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let end: DateTime<Utc> = "2024-01-02T00:00:00Z".parse().unwrap();
    let req1 = DataRequest::new("BTCUSDT", start, end, Frequency::Min1);
    let req2 = DataRequest::new("BTCUSDT", start, end, Frequency::Min1);
    let req3 = DataRequest::new("ETHUSDT", start, end, Frequency::Min1);
    assert_eq!(req1, req2);
    assert_ne!(req1, req3);
}

#[test]
fn test_data_request_hash() {
    use std::collections::HashMap;
    let start: DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let end: DateTime<Utc> = "2024-01-02T00:00:00Z".parse().unwrap();
    let req = DataRequest::new("BTCUSDT", start, end, Frequency::Min1);
    let mut map = HashMap::new();
    map.insert(req.clone(), "test");
    assert_eq!(map.get(&req), Some(&"test"));
}

#[test]
fn test_data_request_with_long_time_range() {
    let start: DateTime<Utc> = "2020-01-01T00:00:00Z".parse().unwrap();
    let end: DateTime<Utc> = "2024-12-31T23:59:59Z".parse().unwrap();
    let req = DataRequest::new("BTCUSDT", start, end, Frequency::Day1);
    assert_eq!(req.frequency, Frequency::Day1);
}

#[test]
fn test_data_request_with_short_time_range() {
    let start: DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let end: DateTime<Utc> = "2024-01-01T00:01:00Z".parse().unwrap();
    let req = DataRequest::new("BTCUSDT", start, end, Frequency::Min1);
    assert_eq!(req.frequency, Frequency::Min1);
}

#[test]
fn test_data_request_crypto_symbols() {
    let start: DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let end: DateTime<Utc> = "2024-01-02T00:00:00Z".parse().unwrap();
    let symbols = vec!["BTCUSDT", "ETHUSDT", "SOLUSDT", "ADAUSDT", "DOTUSDT"];
    for symbol in symbols {
        let req = DataRequest::new(symbol, start, end, Frequency::Min1);
        assert_eq!(req.symbol, symbol);
    }
}

#[test]
fn test_data_request_stock_symbols() {
    let start: DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let end: DateTime<Utc> = "2024-01-02T00:00:00Z".parse().unwrap();
    let symbols = vec!["AAPL", "GOOGL", "MSFT", "TSLA", "AMZN"];
    for symbol in symbols {
        let req = DataRequest::new(symbol, start, end, Frequency::Day1);
        assert_eq!(req.symbol, symbol);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// DataError 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_data_error_display() {
    let errors: Vec<axon_data::DataError> = vec![
        axon_data::DataError::SourceNotFound("binance".into()),
        axon_data::DataError::SchemaMismatch {
            expected: "f64".into(),
            actual: "i64".into(),
        },
        axon_data::DataError::Network("timeout".into()),
        axon_data::DataError::InvalidRequest("missing field".into()),
        axon_data::DataError::Internal("internal error".into()),
        axon_data::DataError::UnsupportedFrequency("tick".into()),
    ];

    for err in errors {
        assert!(!err.to_string().is_empty());
    }
}

#[test]
fn test_data_error_source_not_found() {
    let err = axon_data::DataError::SourceNotFound("unknown".into());
    assert!(err.to_string().contains("unknown"));
}

#[test]
fn test_data_error_schema_mismatch() {
    let err = axon_data::DataError::SchemaMismatch {
        expected: "f64".into(),
        actual: "i64".into(),
    };
    assert!(err.to_string().contains("f64"));
    assert!(err.to_string().contains("i64"));
}

#[test]
fn test_data_error_network() {
    let err = axon_data::DataError::Network("connection refused".into());
    assert!(err.to_string().contains("connection refused"));
}

#[test]
fn test_data_error_rate_limited() {
    let err = axon_data::DataError::RateLimited {
        retry_after_ms: 5000,
    };
    assert!(err.to_string().contains("5000"));
}

#[test]
fn test_data_error_invalid_request() {
    let err = axon_data::DataError::InvalidRequest("bad params".into());
    assert!(err.to_string().contains("bad params"));
}

#[test]
fn test_data_error_unsupported_frequency() {
    let err = axon_data::DataError::UnsupportedFrequency("tick".into());
    assert!(err.to_string().contains("tick"));
}

#[test]
fn test_data_error_ipc_schema_mismatch() {
    let err = axon_data::DataError::IpcSchemaMismatch {
        expected: 5,
        actual: 3,
        expected_type: "tick".into(),
    };
    assert!(err.to_string().contains("5"));
    assert!(err.to_string().contains("3"));
}
