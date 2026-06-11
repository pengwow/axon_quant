//! PyO3 绑定（feature = `python`）
//!
//! 将 `TradingEnv` 暴露给 Python，遵循 Gymnasium 风格 API。
//! 启用方式：`cargo build -p axon-rl --features python`
//!
//! ## Python 用法
//!
//! ```python
//! import axon_rl
//!
//! market_data = [
//!     {"timestamp": i, "open": 100.0, "high": 100.5, "low": 99.5, "close": 100.0, "volume": 1000.0}
//!     for i in range(50)
//! ]
//!
//! env = axon_rl.TradingEnv(
//!     config={"initial_capital": 100_000.0, "max_steps": 20},
//!     action_space={"type": "continuous", "min": -1.0, "max": 1.0},
//!     market_data=market_data,
//!     reward="pnl",
//! )
//!
//! obs = env.reset()
//! obs, reward, done, truncated, info = env.step([0.0])  # 连续动作
//! ```
//!
//! 注：允许 `unsafe_op_in_unsafe_fn` lint 是 pyo3 0.22 宏在 Rust 2024
//! edition 下的兼容需要（pyo3 0.23+ 已自动处理）。

#![allow(unsafe_op_in_unsafe_fn)]
// pyo3 0.22 的 `#[pymethods]` 宏在 Rust 2024 edition 下会展开出
// `PyResult -> PyResult` 的冗余转换，升到 pyo3 0.23+ 可移除。
#![allow(clippy::useless_conversion)]

use pyo3::exceptions::{PyStopIteration, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::action::types::{Action, ActionSpace, ContinuousActionSpace, DiscreteActionSpace};
use crate::env::config::EnvConfig;
use crate::env::error::EnvError;
use crate::env::trading_env::TradingEnv;
use crate::env::types::{EnvInfo, MarketBar};
use crate::observation::space::DefaultObservationSpace;
use crate::observation::types::{FeatureConfig, FeatureSource, NormalizerType};
use crate::reward::pnl::PnLReward;
use crate::reward::sharpe::{RiskAdjustedType, SharpeReward};

// ──────────────────────────────────────────────
// 类型转换辅助
// ──────────────────────────────────────────────

/// `EnvInfo` → Python dict
pub fn env_info_to_dict<'py>(info: &EnvInfo, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("portfolio_value", info.portfolio_value)?;
    dict.set_item("trades_executed", info.trades_executed)?;
    dict.set_item("transaction_costs", info.transaction_costs)?;
    dict.set_item("current_step", info.current_step)?;
    dict.set_item("done", info.done)?;
    dict.set_item("initial_capital", info.initial_capital)?;
    Ok(dict)
}

/// Python action → Rust `Action`
pub fn parse_action(action: &Bound<'_, PyAny>) -> PyResult<Action> {
    // 1. int → 离散动作
    if let Ok(idx) = action.extract::<usize>() {
        return Ok(Action::discrete(idx));
    }
    // 2. list[float] → 连续动作
    if let Ok(values) = action.extract::<Vec<f64>>() {
        return Ok(Action::continuous(values));
    }
    Err(PyTypeError::new_err(
        "action must be int (discrete) or list[float] (continuous)",
    ))
}

/// Python dict → `EnvConfig`
pub fn parse_config(dict: &Bound<'_, PyDict>) -> PyResult<EnvConfig> {
    let mut config = EnvConfig::default();
    if let Ok(Some(v)) = dict.get_item("initial_capital") {
        config.initial_capital = v.extract()?;
    }
    if let Ok(Some(v)) = dict.get_item("transaction_cost") {
        config.transaction_cost = v.extract()?;
    }
    if let Ok(Some(v)) = dict.get_item("slippage") {
        config.slippage = v.extract()?;
    }
    if let Ok(Some(v)) = dict.get_item("max_steps") {
        config.max_steps = v.extract()?;
    }
    if let Ok(Some(v)) = dict.get_item("seed") {
        config.seed = Some(v.extract()?);
    }
    if let Ok(Some(v)) = dict.get_item("symbol") {
        config.symbol = v.extract()?;
    }
    if let Ok(Some(v)) = dict.get_item("return_window") {
        config.return_window = v.extract()?;
    }
    Ok(config)
}

/// Python list → `Vec<MarketBar>`
pub fn parse_market_data(list: &Bound<'_, pyo3::types::PyList>) -> PyResult<Vec<MarketBar>> {
    let mut bars = Vec::with_capacity(list.len());
    for item in list.iter() {
        let dict = item.cast::<PyDict>()?;
        let get_f64 = |key: &str| -> PyResult<f64> {
            dict.get_item(key)?
                .ok_or_else(|| PyValueError::new_err(format!("missing key: {key}")))?
                .extract()
        };
        let get_u64 = |key: &str| -> PyResult<u64> {
            dict.get_item(key)?
                .ok_or_else(|| PyValueError::new_err(format!("missing key: {key}")))?
                .extract()
        };
        bars.push(MarketBar::new(
            get_u64("timestamp")?,
            get_f64("open")?,
            get_f64("high")?,
            get_f64("low")?,
            get_f64("close")?,
            get_f64("volume")?,
        ));
    }
    Ok(bars)
}

/// Rust 错误 → Python 异常
pub fn env_error_to_py(err: EnvError) -> PyErr {
    match err {
        EnvError::EpisodeAlreadyDone(_) | EnvError::DataExhausted(_, _) => {
            PyStopIteration::new_err(err.to_string())
        }
        EnvError::InvalidAction(_)
        | EnvError::EmptyMarketData
        | EnvError::ActionError(_)
        | EnvError::ObservationError(_)
        | EnvError::RewardError(_) => PyValueError::new_err(err.to_string()),
    }
}

/// 解析 Python 动作空间 dict
pub fn parse_action_space(dict: &Bound<'_, PyDict>) -> PyResult<ActionSpace> {
    let type_str: String = dict
        .get_item("type")
        .ok()
        .flatten()
        .map(|v| v.extract().unwrap_or_else(|_| "continuous".to_string()))
        .unwrap_or_else(|| "continuous".to_string());

    match type_str.as_str() {
        "discrete" => {
            let n_bins: usize = dict
                .get_item("n_quantity_bins")?
                .ok_or_else(|| {
                    PyValueError::new_err("discrete action_space needs n_quantity_bins")
                })?
                .extract()?;
            let direction_str: String = dict
                .get_item("direction")
                .ok()
                .flatten()
                .map(|v| v.extract().unwrap_or_else(|_| "both".to_string()))
                .unwrap_or_else(|| "both".to_string());
            let direction = match direction_str.as_str() {
                "long_only" => crate::action::types::TradingDirection::LongOnly,
                "short_only" => crate::action::types::TradingDirection::ShortOnly,
                _ => crate::action::types::TradingDirection::Both,
            };
            Ok(ActionSpace::Discrete(DiscreteActionSpace::new(
                n_bins, direction,
            )))
        }
        _ => {
            let min: f64 = dict
                .get_item("min")
                .ok()
                .flatten()
                .map(|v| v.extract().unwrap_or(-1.0))
                .unwrap_or(-1.0);
            let max: f64 = dict
                .get_item("max")
                .ok()
                .flatten()
                .map(|v| v.extract().unwrap_or(1.0))
                .unwrap_or(1.0);
            Ok(ActionSpace::Continuous(ContinuousActionSpace::new(
                min, max,
            )))
        }
    }
}

// ──────────────────────────────────────────────
// PyTradingEnv
// ──────────────────────────────────────────────

/// Python 可见的 `TradingEnv` 包装
#[pyclass(name = "TradingEnv")]
pub struct PyTradingEnv {
    inner: TradingEnv,
}

#[pymethods]
impl PyTradingEnv {
    /// 创建新环境
    ///
    /// # Arguments
    /// * `config` - dict，配置参数（可选）
    /// * `action_space` - dict，动作空间定义（可选，默认连续 `[-1, 1]`）
    /// * `market_data` - list[dict]，行情数据（OHLCV），必填
    /// * `reward` - str，奖励函数类型（"pnl" / "sharpe" / "sortino"，默认 "pnl"）
    #[new]
    #[pyo3(signature = (config=None, action_space=None, market_data=None, reward=None))]
    fn new(
        config: Option<&Bound<'_, PyDict>>,
        action_space: Option<&Bound<'_, PyDict>>,
        market_data: Option<&Bound<'_, pyo3::types::PyList>>,
        reward: Option<&str>,
    ) -> PyResult<Self> {
        let env_config = match config {
            Some(c) => parse_config(c)?,
            None => EnvConfig::default(),
        };

        let action_space = match action_space {
            Some(d) => parse_action_space(d)?,
            None => ActionSpace::Continuous(ContinuousActionSpace::new(-1.0, 1.0)),
        };

        // 默认观测空间：close + volume，window=1
        let obs_features = vec![
            FeatureConfig {
                name: "close".to_string(),
                source: FeatureSource::PriceField("close".to_string()),
                normalizer: NormalizerType::ZScore,
                clip_range: None,
            },
            FeatureConfig {
                name: "volume".to_string(),
                source: FeatureSource::VolumeField("volume".to_string()),
                normalizer: NormalizerType::None,
                clip_range: None,
            },
        ];
        let observation_space = DefaultObservationSpace::new(1, obs_features)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;

        let reward_fn: Box<dyn crate::reward::RewardFn> = match reward.unwrap_or("pnl") {
            "pnl" => Box::new(PnLReward::default()),
            "sharpe" => Box::new(SharpeReward::default()),
            "sortino" => Box::new(SharpeReward {
                reward_type: RiskAdjustedType::Sortino,
                ..Default::default()
            }),
            other => {
                return Err(PyValueError::new_err(format!(
                    "unknown reward type: {other}"
                )));
            }
        };

        let bars = match market_data {
            Some(l) => parse_market_data(l)?,
            None => return Err(PyValueError::new_err("market_data is required")),
        };

        let inner = TradingEnv::new(
            env_config,
            action_space,
            Box::new(observation_space),
            reward_fn,
            bars,
        )
        .map_err(env_error_to_py)?;

        Ok(Self { inner })
    }

    /// 重置环境，返回初始观测 dict
    fn reset<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let obs = self.inner.reset().map_err(env_error_to_py)?;
        let dict = PyDict::new(py);
        dict.set_item("features", obs.features.clone())?;
        dict.set_item("feature_names", obs.feature_names.clone())?;
        dict.set_item("timestamp", obs.timestamp)?;
        Ok(dict)
    }

    /// 执行一步，返回 `(observation, reward, terminated, truncated, info)`
    fn step<'py>(
        &mut self,
        py: Python<'py>,
        action: &Bound<'_, PyAny>,
    ) -> PyResult<Bound<'py, pyo3::types::PyTuple>> {
        let parsed = parse_action(action)?;
        let (obs, reward, done, info) = self.inner.step(&parsed).map_err(env_error_to_py)?;

        let obs_dict = PyDict::new(py);
        obs_dict.set_item("features", obs.features.clone())?;
        obs_dict.set_item("feature_names", obs.feature_names.clone())?;
        obs_dict.set_item("timestamp", obs.timestamp)?;

        let info_dict = env_info_to_dict(&info, py)?;

        pyo3::types::PyTuple::new(
            py,
            [
                obs_dict.into_any(),
                reward.into_pyobject(py)?.into_any(),
                done.into_pyobject(py)?.to_owned().into_any(),
                py.None().into_bound(py),
                info_dict.into_any(),
            ],
        )
    }

    /// 渲染环境状态
    fn render(&self) -> PyResult<String> {
        Ok(self.inner.render())
    }

    /// 关闭环境（保留接口兼容 Gymnasium）
    fn close(&mut self) -> PyResult<()> {
        Ok(())
    }

    /// 当前步
    #[getter]
    fn current_step(&self) -> usize {
        self.inner.current_step()
    }

    /// 是否已结束
    #[getter]
    fn done(&self) -> bool {
        self.inner.is_done()
    }

    /// 组合市值
    #[getter]
    fn portfolio_value(&self) -> f64 {
        self.inner.portfolio().portfolio_value
    }

    /// 信息 dict
    #[getter]
    fn info<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        env_info_to_dict(&self.inner.info(), py)
    }

    fn __repr__(&self) -> String {
        format!(
            "TradingEnv(step={}, done={}, portfolio={:.2})",
            self.inner.current_step(),
            self.inner.is_done(),
            self.inner.portfolio().portfolio_value,
        )
    }
}

// ──────────────────────────────────────────────
// Python 模块入口
// ──────────────────────────────────────────────

/// `axon_rl` Python 模块
#[pymodule]
pub fn axon_rl(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("VERSION", env!("CARGO_PKG_VERSION"))?;
    m.add_class::<PyTradingEnv>()?;
    Ok(())
}

#[cfg(test)]
mod tests;
