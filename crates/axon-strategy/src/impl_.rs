//! Strategy trait 内置实现：
//! - SdmStrategyImpl   — 快路径（DecisionBackend + Cascade + 可选 Harness）
//! - RuleStrategyImpl  — 纯规则兜底
//! - SwarmRouteStrategyImpl — 快慢决策路由（SDM 直出 + 低置信升级）

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use axon_core::harness_types::{AgentIntent, TaskContext};
use axon_harness::{Adjudication, HarnessBridge};
use axon_llm::decision::*;
use serde::Deserialize;
use serde_json::Value;

#[cfg(feature = "python")]
use axon_core::market::bar::Bar;
#[cfg(feature = "python")]
use pyo3::Python;
#[cfg(feature = "python")]
use pyo3::types::PyAnyMethods;

use crate::trait_::{BarContext, Strategy, StrategyDecision, StrategyError};

// ──────────────────────────────────────────────────────────────────────
// 1. SdmStrategyImpl — SDM 快路径 + Cascade + 可选 Harness
// ──────────────────────────────────────────────────────────────────────

/// SDM 策略配置（可 JSON 序列化下发）
#[derive(Debug, Clone, Deserialize)]
pub struct SdmStrategyConfig {
    pub name: String,
    pub symbol: String,
    pub cost_threshold: f64,
    #[serde(default)]
    pub open_threshold: f64,
    #[serde(default)]
    pub hold_threshold: f64,
    /// 问题模板版本：v1（4 问题）/ v2（8 问题）
    #[serde(default = "default_version")]
    pub features_version: String,
}

fn default_version() -> String {
    "v2".into()
}

/// SDM 快路径策略
///
/// 把 DecisionBackend + SdmCascadeDecisionLayer + (可选) HarnessBridge
/// 串成一个 Strategy。决策链路：SDM → Cascade gate → (可选) Harness → Action。
///
/// Harness 接入后，decide() 在 Cascade 出结果后会构造 AgentIntent/TaskContext
/// 让 HarnessBridge adjudicate。默认 None 时跳过（回测场景一般不需要 Harness）。
pub struct SdmStrategyImpl {
    name: String,
    symbol: String,
    cascade: Arc<SdmCascadeDecisionLayer>,
    questions: BTreeMap<String, Question>,
    #[allow(dead_code)]
    features_version: String,
    /// 可选风控桥（回测时传 None；实盘接 CircuitBreaker/PositionGuard）
    harness: Option<Arc<HarnessBridge>>,
}

impl SdmStrategyImpl {
    pub fn new(
        config: &SdmStrategyConfig,
        backend: Arc<dyn DecisionBackend>,
        questions_raw: BTreeMap<String, Value>,
    ) -> Result<Self, String> {
        Self::with_harness(config, backend, questions_raw, None)
    }

    /// 完整构造（带可选 HarnessBridge）
    pub fn with_harness(
        config: &SdmStrategyConfig,
        backend: Arc<dyn DecisionBackend>,
        questions_raw: BTreeMap<String, Value>,
        harness: Option<Arc<HarnessBridge>>,
    ) -> Result<Self, String> {
        let cascade = Arc::new(SdmCascadeDecisionLayer::new(
            backend,
            Arc::new(RuleFallback::new()) as Arc<dyn FallbackDecider>,
            CascadeConfig {
                open_threshold: config.open_threshold.max(0.01),
                hold_threshold: config.hold_threshold.max(0.01),
            },
        ));
        let questions: BTreeMap<String, Question> = questions_raw
            .into_iter()
            .map(|(k, v)| serde_json::from_value::<Question>(v).map(|q| (k, q)))
            .collect::<Result<_, _>>()
            .map_err(|e| format!("invalid question schema: {e}"))?;
        Ok(Self {
            name: config.name.clone(),
            symbol: config.symbol.clone(),
            cascade,
            questions,
            features_version: config.features_version.clone(),
            harness,
        })
    }
}

#[async_trait]
impl Strategy for SdmStrategyImpl {
    fn name(&self) -> &str {
        &self.name
    }
    fn symbol(&self) -> &str {
        &self.symbol
    }

    async fn decide(&self, ctx: &BarContext<'_>) -> Result<StrategyDecision, StrategyError> {
        // 1) SDM → Cascade gate（Backend 失败已被 Cascade 内部捕获并转为 Fallback）
        let req = DecisionRequest {
            state: ctx.state.clone(),
            questions: self.questions.clone(),
        };
        let cascade = self.cascade.decide(&req).await;
        let decision = StrategyDecision::from_sdm(cascade);

        // 2) 可选：Harness 治理层（CircuitBreaker / PositionGuard）
        // ponytail:回测时 harness=None 直接跳过；实盘接 HarnessBridge 后才 adjudicate
        if let Some(h) = &self.harness {
            let intent = AgentIntent {
                action: format!("{:?}", decision.action).to_lowercase(),
                tool: None,
                params: Value::Null,
                confidence: decision.confidence,
                reasoning: decision.reason.clone().unwrap_or_default(),
                estimated_tokens: 0,
            };
            let task_ctx = TaskContext {
                step: ctx.bar_index as u32,
                tokens_used: 0,
                task_description: format!("bar {} symbol {}", ctx.bar_index, ctx.symbol),
                current_agent: self.name.clone(),
                started_at: ctx.timestamp_ns,
                metadata: Value::Null,
            };
            match h.adjudicate(&intent, &task_ctx) {
                Adjudication::Approved => {}
                Adjudication::Rejected(reason) | Adjudication::NeedRevision(reason) => {
                    return Err(StrategyError::HarnessRejected { reason });
                }
                Adjudication::CircuitBreak => {
                    return Err(StrategyError::HarnessRejected {
                        reason: "circuit breaker triggered".into(),
                    });
                }
            }
        }

        Ok(decision)
    }
}

// ──────────────────────────────────────────────────────────────────────
// 2. RuleStrategyImpl — 纯规则兜底
// ──────────────────────────────────────────────────────────────────────

/// 纯规则策略（双均线 + 近一根收益同向才开仓）
///
/// 用途：DecisionBackend 还没接通时的保底；或 SwarmRouteStrategyImpl 的慢路径兜底。
pub struct RuleStrategyImpl {
    name: String,
    symbol: String,
}

impl RuleStrategyImpl {
    pub fn new(name: impl Into<String>, symbol: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            symbol: symbol.into(),
        }
    }
}

#[async_trait]
impl Strategy for RuleStrategyImpl {
    fn name(&self) -> &str {
        &self.name
    }
    fn symbol(&self) -> &str {
        &self.symbol
    }

    async fn decide(&self, ctx: &BarContext<'_>) -> Result<StrategyDecision, StrategyError> {
        let feat = |name: &str| -> f64 {
            ctx.state
                .get("features")
                .and_then(|f| f.get(name))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0)
        };
        let sma = feat("sma5_vs_sma20");
        let ret = feat("ret_1");
        let action = if sma > 0.0 && ret > 0.0 {
            Action::Buy
        } else if sma < 0.0 && ret < 0.0 {
            Action::Sell
        } else {
            Action::Hold
        };
        Ok(StrategyDecision::from_rule(action))
    }
}

// ──────────────────────────────────────────────────────────────────────
// 3. SwarmRouteStrategyImpl — 快慢决策路由
// ──────────────────────────────────────────────────────────────────────

/// 快慢路由策略：每根 bar 先跑 SDM（快路径，70ms 级），低置信度时升级
/// 到另一个 Strategy（LLM 慢路径 / 规则兜底）。
///
/// 关键：SDM 的 sub_signals 直接传给慢路径 Strategy——LLM 不需要重新计算
/// volatility_regime / drawdown_risk 等指标，信息复用不浪费。
pub struct SwarmRouteStrategyImpl {
    name: String,
    symbol: String,
    fast: Arc<dyn Strategy>,
    slow: Arc<dyn Strategy>,
    /// 低于此置信度升级到慢路径
    upgrade_threshold: f64,
}

impl SwarmRouteStrategyImpl {
    pub fn new(
        name: impl Into<String>,
        symbol: impl Into<String>,
        fast: Arc<dyn Strategy>,
        slow: Arc<dyn Strategy>,
        upgrade_threshold: f64,
    ) -> Self {
        Self {
            name: name.into(),
            symbol: symbol.into(),
            fast,
            slow,
            upgrade_threshold,
        }
    }
}

#[async_trait]
impl Strategy for SwarmRouteStrategyImpl {
    fn name(&self) -> &str {
        &self.name
    }
    fn symbol(&self) -> &str {
        &self.symbol
    }

    async fn decide(&self, ctx: &BarContext<'_>) -> Result<StrategyDecision, StrategyError> {
        // 快路径：每根 bar 必跑
        let fast = self.fast.decide(ctx).await?;

        // 置信度达标 → 直接返回，升级零开销
        if fast.confidence >= self.upgrade_threshold {
            return Ok(fast);
        }

        // 低置信度 → 升级到慢路径
        // ponytail: 不把 fast.sub_signals 重新塞进 state——slow Strategy 可以独立重算
        // 或未来通过 BarContext 扩展字段传递 sub_signals。当前版本保持两策略解耦。
        let slow = self.slow.decide(ctx).await?;
        Ok(slow)
    }
}

// ──────────────────────────────────────────────────────────────────────
// 4. PythonStrategyImpl — Python 策略适配器（桥接 Python on_bar/decide）
// ──────────────────────────────────────────────────────────────────────

/// Python 策略适配器：把 Python 对象的 `decide(bar_dict)` / `on_bar(bar_dict)` 方法
/// 桥接到 Rust `Strategy` trait，让 QuantCell 自定义 Python 策略能跑在 Rust BacktestRunner 上。
///
/// Python 侧契约：
/// - 方法名：`decide` 或 `on_bar`（优先 decide）
/// - 入参：`bar: dict`（含 timestamp/open/high/low/close/volume）
/// - 返回：dict 或对象，含 `action_type`("buy"/"sell"/"hold")/`confidence`/`target_position`
#[cfg(feature = "python")]
pub struct PythonStrategyImpl {
    name: String,
    symbol: String,
    py_obj: pyo3::Py<pyo3::PyAny>,
}

#[cfg(feature = "python")]
impl PythonStrategyImpl {
    pub fn new(
        name: impl Into<String>,
        symbol: impl Into<String>,
        py_obj: pyo3::Py<pyo3::PyAny>,
    ) -> Self {
        Self {
            name: name.into(),
            symbol: symbol.into(),
            py_obj,
        }
    }

    /// 把 Rust Bar 转成 Python dict（timestamp 用纳秒 int）
    fn bar_to_py_dict<'py>(
        &self,
        py: pyo3::Python<'py>,
        bar: &Bar,
    ) -> pyo3::PyResult<pyo3::Bound<'py, pyo3::types::PyDict>> {
        let d = pyo3::types::PyDict::new(py);
        d.set_item("timestamp", bar.timestamp.nanos)?;
        d.set_item("open", bar.open.as_f64())?;
        d.set_item("high", bar.high.as_f64())?;
        d.set_item("low", bar.low.as_f64())?;
        d.set_item("close", bar.close.as_f64())?;
        d.set_item("volume", bar.volume.as_f64())?;
        d.set_item("symbol", &self.symbol)?;
        Ok(d)
    }
}

#[cfg(feature = "python")]
#[async_trait]
impl Strategy for PythonStrategyImpl {
    fn name(&self) -> &str {
        &self.name
    }

    fn symbol(&self) -> &str {
        &self.symbol
    }

    async fn decide(&self, ctx: &BarContext<'_>) -> Result<StrategyDecision, StrategyError> {
        // 获取 GIL，同步调用 Python 策略方法（回测是 block_on，无并发 GIL 争用）
        Python::attach(|py| -> Result<StrategyDecision, StrategyError> {
            let bar_dict = self
                .bar_to_py_dict(py, ctx.bar)
                .map_err(|e| StrategyError::Logic(format!("bar→dict 失败: {e}")))?;

            // 逐 bar state（因子/上下文）注入：非 Null 时以 `bar["state"]` 传给 Python 策略，
            // 供适配层映射到 StrategyContext.features 等。
            if !ctx.state.is_null() {
                let state_str = serde_json::to_string(ctx.state)
                    .map_err(|e| StrategyError::Logic(format!("state 序列化失败: {e}")))?;
                let state_py = py
                    .import("json")
                    .and_then(|m| m.call_method1("loads", (state_str,)))
                    .map_err(|e| StrategyError::Logic(format!("state→dict 失败: {e}")))?;
                bar_dict
                    .set_item("state", state_py)
                    .map_err(|e| StrategyError::Logic(format!("state 注入 bar 失败: {e}")))?;
            }

            // 优先 decide，回退 on_bar
            let bound = self.py_obj.bind(py);
            let method = bound
                .getattr("decide")
                .or_else(|_| bound.getattr("on_bar"))
                .map_err(|e| StrategyError::Logic(format!("策略无 decide/on_bar 方法: {e}")))?;

            let ret = method
                .call1((bar_dict,))
                .map_err(|e| StrategyError::Logic(format!("Python 策略调用失败: {e}")))?;

            // 解析返回：支持 dict 或带属性的对象
            let get_str = |key: &str| -> Option<String> {
                let v = ret.getattr(key).or_else(|_| ret.get_item(key)).ok()?;
                // extract::<String> 只认原生 Python str；官方 Action 的
                // action_type getter 返回 PyActionType pyclass 枚举（__str__
                // 映射 "buy"/"sell"/"hold"），extract 失败会静默降级成 Hold
                // 导致引擎里该策略永不成交，因此先 extract 再回退 str()。
                v.extract::<String>()
                    .or_else(|_| v.str().and_then(|s| s.extract::<String>()))
                    .ok()
            };
            let get_f64 = |key: &str| -> Option<f64> {
                ret.getattr(key)
                    .and_then(|v| v.extract::<f64>())
                    .or_else(|_| ret.get_item(key).and_then(|v| v.extract::<f64>()))
                    .ok()
            };

            let action_type = get_str("action_type").unwrap_or_else(|| "hold".to_string());
            let confidence = get_f64("confidence").unwrap_or(0.0);
            let target = get_f64("target_position").unwrap_or(0.0);

            let action = match action_type.as_str() {
                "buy" => Action::Buy,
                "sell" => Action::Sell,
                _ => Action::Hold,
            };

            Ok(StrategyDecision::from_rule(action)
                .with_confidence(confidence)
                .with_target_position(target))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::market::bar::Bar;
    use axon_core::time::Timestamp;
    use axon_core::types::{Price, Quantity};
    use serde_json::json;

    /// 测试用静态 Bar（RuleStrategyImpl 不读 bar 数据，只看 state.features）
    fn sample_bar() -> Bar {
        Bar {
            timestamp: Timestamp {
                nanos: 1713300000000000000,
            },
            open: Price::from_f64(100.0),
            high: Price::from_f64(101.0),
            low: Price::from_f64(99.0),
            close: Price::from_f64(100.5),
            volume: Quantity::from_f64(1000.0),
        }
    }

    fn sample_ctx<'a>(state: &'a Value) -> BarContext<'a> {
        // ponytail: Box::leak 让 bar 活到 'static，测试用可接受
        let bar: &'static Bar = Box::leak(Box::new(sample_bar()));
        BarContext {
            symbol: "BTCUSDT",
            bar_index: 42,
            timestamp_ns: 1713300000000000000,
            bar,
            state,
        }
    }

    #[tokio::test]
    async fn rule_strategy_follows_sma_ret_direction() {
        let s = RuleStrategyImpl::new("rule1", "BTCUSDT");

        // 同向正 → buy
        let state = json!({"features": {"sma5_vs_sma20": 0.02, "ret_1": 0.01}});
        let d = s.decide(&sample_ctx(&state)).await.unwrap();
        assert_eq!(d.action, Action::Buy);
        assert_eq!(d.source, "rule");

        // 同向负 → sell
        let state = json!({"features": {"sma5_vs_sma20": -0.02, "ret_1": -0.01}});
        let d = s.decide(&sample_ctx(&state)).await.unwrap();
        assert_eq!(d.action, Action::Sell);

        // 矛盾 → hold
        let state = json!({"features": {"sma5_vs_sma20": 0.02, "ret_1": -0.01}});
        let d = s.decide(&sample_ctx(&state)).await.unwrap();
        assert_eq!(d.action, Action::Hold);
    }

    #[tokio::test]
    async fn swarm_route_upgrades_to_slow_when_low_confidence() {
        // fast: 永远返回 hold, confidence=0.5（低于 0.7 阈值）
        struct AlwaysLowConf;
        #[async_trait]
        impl Strategy for AlwaysLowConf {
            fn name(&self) -> &str {
                "low"
            }
            fn symbol(&self) -> &str {
                "X"
            }
            async fn decide(&self, _: &BarContext<'_>) -> Result<StrategyDecision, StrategyError> {
                Ok(StrategyDecision {
                    action: Action::Hold,
                    confidence: 0.5,
                    target_position: None,
                    source: "low".into(),
                    probabilities: None,
                    sub_signals: BTreeMap::new(),
                    reason: None,
                })
            }
        }

        // slow: 永远返回 buy
        struct AlwaysBuy;
        #[async_trait]
        impl Strategy for AlwaysBuy {
            fn name(&self) -> &str {
                "buyer"
            }
            fn symbol(&self) -> &str {
                "X"
            }
            async fn decide(&self, _: &BarContext<'_>) -> Result<StrategyDecision, StrategyError> {
                Ok(StrategyDecision::from_rule(Action::Buy))
            }
        }

        let route = SwarmRouteStrategyImpl::new(
            "route1",
            "BTCUSDT",
            Arc::new(AlwaysLowConf),
            Arc::new(AlwaysBuy),
            0.7, // 置信度 < 0.7 就升级
        );
        let state = json!({"features": {}});
        let d = route.decide(&sample_ctx(&state)).await.unwrap();
        assert_eq!(
            d.action,
            Action::Buy,
            "should have upgraded to slow strategy"
        );
    }

    #[tokio::test]
    async fn swarm_route_returns_fast_when_confidence_high() {
        struct AlwaysHighConf;
        #[async_trait]
        impl Strategy for AlwaysHighConf {
            fn name(&self) -> &str {
                "high"
            }
            fn symbol(&self) -> &str {
                "X"
            }
            async fn decide(&self, _: &BarContext<'_>) -> Result<StrategyDecision, StrategyError> {
                Ok(StrategyDecision {
                    action: Action::Sell,
                    confidence: 0.95,
                    target_position: None,
                    source: "high".into(),
                    probabilities: None,
                    sub_signals: BTreeMap::new(),
                    reason: None,
                })
            }
        }
        struct NeverCalled;
        #[async_trait]
        impl Strategy for NeverCalled {
            fn name(&self) -> &str {
                "never"
            }
            fn symbol(&self) -> &str {
                "X"
            }
            async fn decide(&self, _: &BarContext<'_>) -> Result<StrategyDecision, StrategyError> {
                panic!("slow should not be called");
            }
        }

        let route = SwarmRouteStrategyImpl::new(
            "route2",
            "BTCUSDT",
            Arc::new(AlwaysHighConf),
            Arc::new(NeverCalled),
            0.7,
        );
        let state = json!({"features": {}});
        let d = route.decide(&sample_ctx(&state)).await.unwrap();
        assert_eq!(d.action, Action::Sell);
    }
}
