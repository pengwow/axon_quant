//! PyO3 绑定单元测试
//!
//! 注意：完整的 Python 端集成测试需要 `cargo test --features python` + Python 解释器。
//! 这里我们先做 Rust 侧的单元测试，验证 helper 函数与配置解析逻辑。

#![cfg(feature = "python")]

use crate::action::types::{Action, ActionSpace, TradingDirection};
use crate::observation::types::{FeatureSource, NormalizerType, ObservationSpace};
use crate::python::{
    env_error_to_py, parse_action, parse_action_space, parse_config, parse_market_data,
};
use pyo3::conversion::IntoPyObject;
use pyo3::types::{PyDict, PyDictMethods, PyList};
use pyo3::{Bound, Python};

// ──────────────────────────────────────────────
// parse_config
// ──────────────────────────────────────────────

#[test]
fn test_parse_config_default_when_empty() {
    pyo3::Python::initialize();
    Python::attach(|py| {
        let dict = PyDict::new(py);
        let config = parse_config(&dict).expect("empty dict should yield default config");
        // 默认值（与 EnvConfig::default() 对齐）
        assert!((config.initial_capital - 100_000.0).abs() < 1e-9);
        assert_eq!(config.max_steps, 1000);
        assert_eq!(config.symbol, "BTCUSDT");
        assert!(config.seed.is_none());
    });
}

#[test]
fn test_parse_config_overrides_individual_keys() {
    pyo3::Python::initialize();
    Python::attach(|py| {
        let dict = PyDict::new(py);
        dict.set_item("initial_capital", 250_000.0).unwrap();
        dict.set_item("transaction_cost", 0.0025).unwrap();
        dict.set_item("slippage", 0.001).unwrap();
        dict.set_item("max_steps", 500_usize).unwrap();
        dict.set_item("seed", 42_u64).unwrap();
        dict.set_item("symbol", "ETHUSDT").unwrap();
        dict.set_item("return_window", 64_usize).unwrap();

        let config = parse_config(&dict).unwrap();
        assert!((config.initial_capital - 250_000.0).abs() < 1e-9);
        assert!((config.transaction_cost - 0.0025).abs() < 1e-9);
        assert!((config.slippage - 0.001).abs() < 1e-9);
        assert_eq!(config.max_steps, 500);
        assert_eq!(config.seed, Some(42));
        assert_eq!(config.symbol, "ETHUSDT");
        assert_eq!(config.return_window, 64);
    });
}

#[test]
fn test_parse_config_ignores_unknown_keys() {
    // 未知键应被忽略，配置仍能成功解析
    pyo3::Python::initialize();
    Python::attach(|py| {
        let dict = PyDict::new(py);
        dict.set_item("initial_capital", 50_000.0).unwrap();
        dict.set_item("unknown_key", 123).unwrap();
        let config = parse_config(&dict).unwrap();
        assert!((config.initial_capital - 50_000.0).abs() < 1e-9);
    });
}

#[test]
fn test_parse_config_wrong_type_returns_error() {
    pyo3::Python::initialize();
    Python::attach(|py| {
        let dict = PyDict::new(py);
        // initial_capital 应该是 float
        dict.set_item("initial_capital", "not a number").unwrap();
        let result = parse_config(&dict);
        assert!(result.is_err());
    });
}

// ──────────────────────────────────────────────
// parse_action
// ──────────────────────────────────────────────

#[test]
fn test_parse_action_discrete_int() {
    pyo3::Python::initialize();
    Python::attach(|py| {
        // 离散动作 0..=5
        for idx in 0..=5_usize {
            let py_int = idx.into_pyobject(py).unwrap().unbind();
            let action =
                parse_action(py_int.bind(py)).expect("int should parse as discrete action");
            let expected = Action::discrete(idx);
            assert_eq!(
                action.action_type, expected.action_type,
                "discrete action index mismatch"
            );
        }
    });
}

#[test]
fn test_parse_action_continuous_list() {
    pyo3::Python::initialize();
    Python::attach(|py| {
        let values: Vec<f64> = vec![0.5, -0.3];
        let py_list = values.clone().into_pyobject(py).unwrap().unbind();
        let action =
            parse_action(py_list.bind(py)).expect("list should parse as continuous action");
        let expected = Action::continuous(values);
        assert_eq!(action.action_type, expected.action_type);
    });
}

#[test]
fn test_parse_action_invalid_type_returns_error() {
    pyo3::Python::initialize();
    Python::attach(|py| {
        let py_str = "buy".into_pyobject(py).unwrap().unbind();
        let result = parse_action(py_str.bind(py));
        assert!(result.is_err());
    });
}

// ──────────────────────────────────────────────
// parse_action_space
// ──────────────────────────────────────────────

#[test]
fn test_parse_action_space_continuous_default() {
    pyo3::Python::initialize();
    Python::attach(|py| {
        // 空 dict → 默认连续 [-1, 1]
        let dict = PyDict::new(py);
        let space = parse_action_space(&dict).unwrap();
        match space {
            ActionSpace::Continuous(c) => {
                assert!((c.min - (-1.0)).abs() < 1e-9);
                assert!((c.max - 1.0).abs() < 1e-9);
            }
            _ => panic!("expected continuous action space"),
        }
    });
}

#[test]
fn test_parse_action_space_continuous_custom_range() {
    pyo3::Python::initialize();
    Python::attach(|py| {
        let dict = PyDict::new(py);
        dict.set_item("type", "continuous").unwrap();
        dict.set_item("min", -2.0).unwrap();
        dict.set_item("max", 0.5).unwrap();
        let space = parse_action_space(&dict).unwrap();
        match space {
            ActionSpace::Continuous(c) => {
                assert!((c.min - (-2.0)).abs() < 1e-9);
                assert!((c.max - 0.5).abs() < 1e-9);
            }
            _ => panic!("expected continuous action space"),
        }
    });
}

#[test]
fn test_parse_action_space_discrete_long_only() {
    pyo3::Python::initialize();
    Python::attach(|py| {
        let dict = PyDict::new(py);
        dict.set_item("type", "discrete").unwrap();
        dict.set_item("n_quantity_bins", 3_usize).unwrap();
        dict.set_item("direction", "long_only").unwrap();
        let space = parse_action_space(&dict).unwrap();
        match space {
            ActionSpace::Discrete(d) => {
                assert_eq!(d.n_quantity_bins, 3);
                assert_eq!(d.direction, TradingDirection::LongOnly);
                assert_eq!(d.n, 1 + 3 * 2);
            }
            _ => panic!("expected discrete action space"),
        }
    });
}

#[test]
fn test_parse_action_space_discrete_both() {
    pyo3::Python::initialize();
    Python::attach(|py| {
        let dict = PyDict::new(py);
        dict.set_item("type", "discrete").unwrap();
        dict.set_item("n_quantity_bins", 2_usize).unwrap();
        let space = parse_action_space(&dict).unwrap();
        match space {
            ActionSpace::Discrete(d) => {
                assert_eq!(d.direction, TradingDirection::Both);
            }
            _ => panic!("expected discrete action space"),
        }
    });
}

#[test]
fn test_parse_action_space_discrete_missing_bins() {
    pyo3::Python::initialize();
    Python::attach(|py| {
        let dict = PyDict::new(py);
        dict.set_item("type", "discrete").unwrap();
        // 缺少 n_quantity_bins
        let result = parse_action_space(&dict);
        assert!(result.is_err());
    });
}

// ──────────────────────────────────────────────
// parse_market_data
// ──────────────────────────────────────────────

#[test]
fn test_parse_market_data_basic() {
    pyo3::Python::initialize();
    Python::attach(|py| {
        let bars: Vec<Bound<'_, PyDict>> = (0..3)
            .map(|i| {
                let d = PyDict::new(py);
                d.set_item("timestamp", i as u64).unwrap();
                d.set_item("open", 100.0 + i as f64).unwrap();
                d.set_item("high", 101.0 + i as f64).unwrap();
                d.set_item("low", 99.0 + i as f64).unwrap();
                d.set_item("close", 100.5 + i as f64).unwrap();
                d.set_item("volume", 1000.0).unwrap();
                d
            })
            .collect();
        let list = PyList::new(py, &bars).unwrap();
        let parsed = parse_market_data(&list).expect("should parse market data");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].timestamp, 0);
        assert!((parsed[0].open - 100.0).abs() < 1e-9);
        assert!((parsed[1].close - 101.5).abs() < 1e-9);
    });
}

#[test]
fn test_parse_market_data_missing_key_errors() {
    pyo3::Python::initialize();
    Python::attach(|py| {
        let d = PyDict::new(py);
        d.set_item("timestamp", 0_u64).unwrap();
        // 缺少 open / high / low / close / volume
        let list = PyList::new(py, &[d]).unwrap();
        let result = parse_market_data(&list);
        assert!(result.is_err());
    });
}

// ──────────────────────────────────────────────
// env_error_to_py
// ──────────────────────────────────────────────

#[test]
fn test_env_error_to_py_episode_done() {
    pyo3::Python::initialize();
    Python::attach(|py| {
        let err = crate::env::error::EnvError::EpisodeAlreadyDone(5);
        let py_err = env_error_to_py(err);
        // EpisodeAlreadyDone → PyStopIteration
        let exc_type = py_err.get_type(py);
        assert_eq!(exc_type.to_string(), "<class 'StopIteration'>");
    });
}

#[test]
fn test_env_error_to_py_invalid_action() {
    pyo3::Python::initialize();
    Python::attach(|py| {
        let err = crate::env::error::EnvError::InvalidAction("bad".to_string());
        let py_err = env_error_to_py(err);
        let exc_type = py_err.get_type(py);
        assert_eq!(exc_type.to_string(), "<class 'ValueError'>");
    });
}

#[test]
fn test_env_error_to_py_empty_data() {
    pyo3::Python::initialize();
    Python::attach(|py| {
        let err = crate::env::error::EnvError::EmptyMarketData;
        let py_err = env_error_to_py(err);
        let exc_type = py_err.get_type(py);
        assert_eq!(exc_type.to_string(), "<class 'ValueError'>");
    });
}

// ──────────────────────────────────────────────
// 默认特征配置（PyTradingEnv::new 内部使用）
// ──────────────────────────────────────────────

#[test]
fn test_feature_config_default_close_volume() {
    // 验证 PyTradingEnv::new 中使用的默认 feature 列表是可构造的
    let features = vec![
        crate::observation::types::FeatureConfig {
            name: "close".to_string(),
            source: FeatureSource::PriceField("close".to_string()),
            normalizer: NormalizerType::ZScore,
            clip_range: None,
        },
        crate::observation::types::FeatureConfig {
            name: "volume".to_string(),
            source: FeatureSource::VolumeField("volume".to_string()),
            normalizer: NormalizerType::None,
            clip_range: None,
        },
    ];
    let space = crate::observation::space::DefaultObservationSpace::new(1, features)
        .expect("default features should build valid observation space");
    assert_eq!(space.num_features(), 2);
}
