//! REFLEX 置信门控级联：SDM 直出 → 低置信兜底 → 异常兜底
//!
//! 硬约束：
//! - 三条路径返回同一个 [`CascadeDecision`]，不早退裸结构。
//! - 门控只使用 SDM 原始校准 confidence，不组合综合分。
//! - 低置信与异常都已耗费一次调用（异常可能未计费），usage 如实透传或置 None。

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;

use super::backend::{DecisionBackend, DecisionError};
use super::types::{
    Action, Answer, CascadeDecision, DecisionRequest, DecisionResponse, DecisionSource,
    DecisionUsage,
};

/// 动作键固定顺序
pub const ACTIONS: [&str; 3] = ["buy", "sell", "hold"];

/// 非对称门控：开仓阈值高于观望阈值（开仓判错代价更高）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CascadeConfig {
    /// buy/sell 放行阈值
    pub open_threshold: f64,
    /// hold 放行阈值
    pub hold_threshold: f64,
}

impl Default for CascadeConfig {
    fn default() -> Self {
        Self {
            open_threshold: 0.80,
            hold_threshold: 0.70,
        }
    }
}

/// 置信度是否达到该动作的放行阈值（边界值放行）
pub fn gate_allows(action: Action, confidence: f64, config: &CascadeConfig) -> bool {
    let threshold = match action {
        Action::Buy | Action::Sell => config.open_threshold,
        Action::Hold => config.hold_threshold,
    };
    confidence >= threshold
}

/// 兜底决策协议
#[async_trait]
pub trait FallbackDecider: Send + Sync {
    /// 返回 (动作, buy/sell/hold 概率分布, 置信度)
    async fn decide(
        &self,
        state: &serde_json::Value,
    ) -> Result<(Action, BTreeMap<String, f64>, f64), DecisionError>;
}

/// 确定性规则兜底：双均线偏离与近一根收益同向才开仓，否则 hold
pub struct RuleFallback;

impl RuleFallback {
    /// 构造
    pub fn new() -> Self {
        Self
    }
}

impl Default for RuleFallback {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl FallbackDecider for RuleFallback {
    async fn decide(
        &self,
        state: &serde_json::Value,
    ) -> Result<(Action, BTreeMap<String, f64>, f64), DecisionError> {
        let feat = |name: &str| -> Result<f64, DecisionError> {
            state
                .get("features")
                .and_then(|f| f.get(name))
                .and_then(|v| v.as_f64())
                .ok_or_else(|| DecisionError::Backend(format!("features.{name} missing")))
        };
        let sma = feat("sma5_vs_sma20")?;
        let ret = feat("ret_1")?;
        let action = if sma > 0.0 && ret > 0.0 {
            Action::Buy
        } else if sma < 0.0 && ret < 0.0 {
            Action::Sell
        } else {
            Action::Hold
        };
        let mut probs = BTreeMap::new();
        let (win, other) = (0.7_f64, 0.1_f64);
        match action {
            Action::Buy => {
                probs.insert("buy".to_string(), win);
                probs.insert("sell".to_string(), other);
                probs.insert("hold".to_string(), 0.2);
            }
            Action::Sell => {
                probs.insert("sell".to_string(), win);
                probs.insert("buy".to_string(), other);
                probs.insert("hold".to_string(), 0.2);
            }
            Action::Hold => {
                probs.insert("hold".to_string(), 0.5);
                probs.insert("buy".to_string(), 0.25);
                probs.insert("sell".to_string(), 0.25);
            }
        }
        let confidence = probs[action.as_str()];
        Ok((action, probs, confidence))
    }
}

/// 提取子问题信号（v1 全部 3 个 + v2 新增 3 个可数值化的）。
///
/// 仅离线归因，禁止参与门控。position_size 和 execution_urgency 是 Choice 类型，
/// 走 harness 层路由，不在这里做数值化提取。
fn extract_sub_signals(resp: &DecisionResponse) -> BTreeMap<String, f64> {
    let mut sub = BTreeMap::new();
    // v1 既有
    if let Some(Answer::Noul { noul }) = resp.answers.get("trend_confirms") {
        sub.insert("trend_confirms".to_string(), *noul);
    }
    if let Some(Answer::Noul { noul }) = resp.answers.get("risk_regime_bad") {
        sub.insert("risk_regime_bad".to_string(), *noul);
    }
    if let Some(Answer::Score { score, .. }) = resp.answers.get("signal_clarity") {
        sub.insert("signal_clarity".to_string(), *score);
    }
    // v2 新增可数值化的
    if let Some(Answer::Score { score, .. }) = resp.answers.get("volatility_regime") {
        sub.insert("volatility_regime".to_string(), *score);
    }
    if let Some(Answer::Noul { noul }) = resp.answers.get("drawdown_risk") {
        sub.insert("drawdown_risk".to_string(), *noul);
    }
    sub
}

/// 补齐 buy/sell/hold 三键；保持原始概率不归一化（校准值不可篡改）
fn ensure_action_keys(probs: &BTreeMap<String, f64>) -> BTreeMap<String, f64> {
    ACTIONS
        .iter()
        .map(|name| (*name, probs.get(*name).copied().unwrap_or(0.0)))
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

/// SDM（快决策）+ 兜底的级联层，返回统一出口
pub struct SdmCascadeDecisionLayer {
    backend: Arc<dyn DecisionBackend>,
    fallback: Arc<dyn FallbackDecider>,
    config: CascadeConfig,
}

impl SdmCascadeDecisionLayer {
    /// 构造
    pub fn new(
        backend: Arc<dyn DecisionBackend>,
        fallback: Arc<dyn FallbackDecider>,
        config: CascadeConfig,
    ) -> Self {
        Self {
            backend,
            fallback,
            config,
        }
    }

    /// 执行级联决策
    pub async fn decide(&self, req: &DecisionRequest) -> CascadeDecision {
        // 路径一：SDM 调用异常（网络/解析/replay miss），未拿到有效响应
        let resp = match self.backend.decide(req).await {
            Ok(r) => r,
            // ponytail: 兜底面收到宽到 DecisionError 全集，任何 SDM 侧失败都不能让交易循环崩掉；
            // 升级路径是按错误类型细分重试/熔断，当前阶段无此需求
            Err(e) => {
                return self
                    .use_fallback(
                        req,
                        format!("sdm_error: {}", e.kind()),
                        None,
                        BTreeMap::new(),
                    )
                    .await;
            }
        };

        let sub_signals = extract_sub_signals(&resp);
        let usage = resp.usage;

        // 路径二：响应结构无法支撑门控（主问题缺失/类型错/胜选项不在动作集）。
        // 一次性解构出（动作, 置信度, 概率分布）；概率需 clone 以构造 owned 出口。
        let (action, confidence, probabilities) = match resp.answers.get("next_direction") {
            Some(Answer::Choice {
                choice,
                confidence,
                probabilities,
            }) if Action::from_name(choice).is_some() => (
                Action::from_name(choice).expect("checked above"),
                *confidence,
                probabilities.clone(),
            ),
            _ => {
                return self
                    .use_fallback(
                        req,
                        "sdm_error: invalid_choice".to_string(),
                        Some(usage),
                        sub_signals,
                    )
                    .await;
            }
        };

        // 路径三：校准置信度未达非对称阈值
        if !gate_allows(action, confidence, &self.config) {
            return self
                .use_fallback(
                    req,
                    format!("low_confidence: {}={}", action.as_str(), confidence),
                    Some(usage),
                    sub_signals,
                )
                .await;
        }

        // 直出：概率只补缺键不归一化，保留 SDM 原始校准分布
        CascadeDecision {
            action,
            source: DecisionSource::SdmDirect,
            confidence,
            probabilities: ensure_action_keys(&probabilities),
            fallback_reason: None,
            sdm_usage: Some(usage),
            sub_signals,
        }
    }

    /// 走兜底；兜底自身失败时降级为安全 Hold，保证任何分支都返回合法结构
    async fn use_fallback(
        &self,
        req: &DecisionRequest,
        reason: String,
        usage: Option<DecisionUsage>,
        sub_signals: BTreeMap<String, f64>,
    ) -> CascadeDecision {
        match self.fallback.decide(&req.state).await {
            Ok((action, probabilities, confidence)) => CascadeDecision {
                action,
                source: DecisionSource::Fallback,
                confidence,
                probabilities,
                fallback_reason: Some(reason),
                sdm_usage: usage,
                sub_signals,
            },
            Err(e) => {
                let mut probabilities = BTreeMap::new();
                probabilities.insert("hold".to_string(), 1.0);
                probabilities.insert("buy".to_string(), 0.0);
                probabilities.insert("sell".to_string(), 0.0);
                CascadeDecision {
                    action: Action::Hold,
                    source: DecisionSource::Fallback,
                    confidence: 0.0,
                    probabilities,
                    fallback_reason: Some(format!("fallback_error: {}", e.kind())),
                    sdm_usage: usage,
                    sub_signals,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::types::DecisionResponse;
    use serde_json::json;

    struct FixedBackend {
        resp: Option<DecisionResponse>,
    }
    #[async_trait]
    impl DecisionBackend for FixedBackend {
        async fn decide(&self, _req: &DecisionRequest) -> Result<DecisionResponse, DecisionError> {
            self.resp
                .clone()
                .ok_or_else(|| DecisionError::Network("boom".into()))
        }
        fn model_name(&self) -> &str {
            "fixed"
        }
    }

    fn state() -> serde_json::Value {
        json!({"features": {"sma5_vs_sma20": 0.02, "ret_1": 0.01}})
    }

    fn choice_resp(choice: &str, conf: f64) -> DecisionResponse {
        let mut answers = BTreeMap::new();
        answers.insert(
            "next_direction".to_string(),
            Answer::Choice {
                choice: choice.to_string(),
                confidence: conf,
                probabilities: BTreeMap::from([
                    ("buy".to_string(), 0.1),
                    ("sell".to_string(), 0.1),
                    ("hold".to_string(), 0.8),
                ]),
            },
        );
        DecisionResponse {
            model: "m".into(),
            answers,
            usage: DecisionUsage {
                input_tokens: 5,
                cost_usd: 0.01,
            },
        }
    }

    fn req_with(resp: Option<DecisionResponse>) -> (Arc<dyn DecisionBackend>, DecisionRequest) {
        let backend: Arc<dyn DecisionBackend> = Arc::new(FixedBackend { resp });
        let req = DecisionRequest {
            state: state(),
            questions: BTreeMap::new(),
        };
        (backend, req)
    }

    #[tokio::test]
    async fn direct_path_when_confidence_clears_gate() {
        let (b, req) = req_with(Some(choice_resp("hold", 0.75)));
        let layer = SdmCascadeDecisionLayer::new(
            b,
            Arc::new(RuleFallback::new()),
            CascadeConfig::default(),
        );
        let d = layer.decide(&req).await;
        assert_eq!(d.source, DecisionSource::SdmDirect);
        assert_eq!(d.action, Action::Hold);
        assert!(d.fallback_reason.is_none());
        assert_eq!(d.sdm_usage.unwrap().cost_usd, 0.01);
        for k in ACTIONS {
            assert!(d.probabilities.contains_key(k));
        }
    }

    #[tokio::test]
    async fn low_confidence_uses_rule_fallback_but_keeps_usage() {
        let (b, req) = req_with(Some(choice_resp("buy", 0.5)));
        let layer = SdmCascadeDecisionLayer::new(
            b,
            Arc::new(RuleFallback::new()),
            CascadeConfig::default(),
        );
        let d = layer.decide(&req).await;
        assert_eq!(d.source, DecisionSource::Fallback);
        assert_eq!(d.action, Action::Buy); // 规则：sma>0 且 ret>0
        assert!(d.fallback_reason.unwrap().starts_with("low_confidence"));
        assert_eq!(d.sdm_usage.unwrap().input_tokens, 5);
    }

    #[tokio::test]
    async fn backend_error_falls_back_with_no_usage() {
        let (b, req) = req_with(None);
        let layer = SdmCascadeDecisionLayer::new(
            b,
            Arc::new(RuleFallback::new()),
            CascadeConfig::default(),
        );
        let d = layer.decide(&req).await;
        assert_eq!(d.source, DecisionSource::Fallback);
        assert_eq!(d.fallback_reason.as_deref(), Some("sdm_error: Network"));
        assert!(d.sdm_usage.is_none());
    }

    #[tokio::test]
    async fn invalid_choice_falls_back() {
        let (b, req) = req_with(Some(choice_resp("moon", 0.99)));
        let layer = SdmCascadeDecisionLayer::new(
            b,
            Arc::new(RuleFallback::new()),
            CascadeConfig::default(),
        );
        let d = layer.decide(&req).await;
        assert_eq!(
            d.fallback_reason.as_deref(),
            Some("sdm_error: invalid_choice")
        );
    }

    struct FailingFallback;
    #[async_trait]
    impl FallbackDecider for FailingFallback {
        async fn decide(
            &self,
            _s: &serde_json::Value,
        ) -> Result<(Action, BTreeMap<String, f64>, f64), DecisionError> {
            Err(DecisionError::Backend("down".into()))
        }
    }

    #[tokio::test]
    async fn fallback_failure_degrades_to_safe_hold() {
        let (b, req) = req_with(None);
        let layer =
            SdmCascadeDecisionLayer::new(b, Arc::new(FailingFallback), CascadeConfig::default());
        let d = layer.decide(&req).await;
        assert_eq!(d.action, Action::Hold);
        assert_eq!(
            d.fallback_reason.as_deref(),
            Some("fallback_error: Backend")
        );
    }

    /// choice_resp + v1 3 子信号 + v2 2 新增子信号（volatility_regime + drawdown_risk）
    fn full_v2_resp(choice: &str, conf: f64) -> DecisionResponse {
        let mut answers = BTreeMap::new();
        answers.insert(
            "next_direction".to_string(),
            Answer::Choice {
                choice: choice.to_string(),
                confidence: conf,
                probabilities: BTreeMap::from([
                    ("buy".to_string(), 0.7),
                    ("sell".to_string(), 0.1),
                    ("hold".to_string(), 0.2),
                ]),
            },
        );
        // v1 子信号
        answers.insert("trend_confirms".to_string(), Answer::Noul { noul: 0.85 });
        answers.insert("risk_regime_bad".to_string(), Answer::Noul { noul: 0.3 });
        answers.insert(
            "signal_clarity".to_string(),
            Answer::Score {
                score: 2.0,
                confidence: 0.8,
                probabilities: BTreeMap::from([
                    ("0".to_string(), 0.0),
                    ("1".to_string(), 0.1),
                    ("2".to_string(), 0.9),
                ]),
            },
        );
        // v2 新增可数值化子信号
        answers.insert(
            "volatility_regime".to_string(),
            Answer::Score {
                score: 1.0,
                confidence: 0.75,
                probabilities: BTreeMap::new(),
            },
        );
        answers.insert("drawdown_risk".to_string(), Answer::Noul { noul: 0.25 });
        DecisionResponse {
            model: "m".into(),
            answers,
            usage: DecisionUsage {
                input_tokens: 10,
                cost_usd: 0.02,
            },
        }
    }

    #[tokio::test]
    async fn v2_sub_signals_pass_through_to_cascade_decision() {
        let (b, req) = req_with(Some(full_v2_resp("buy", 0.9)));
        let layer = SdmCascadeDecisionLayer::new(
            b,
            Arc::new(RuleFallback::new()),
            CascadeConfig::default(),
        );
        let d = layer.decide(&req).await;
        // v1 3 个子信号都在
        assert_eq!(d.sub_signals.get("trend_confirms"), Some(&0.85));
        assert_eq!(d.sub_signals.get("risk_regime_bad"), Some(&0.3));
        assert_eq!(d.sub_signals.get("signal_clarity"), Some(&2.0));
        // v2 新增的 volatility_regime 和 drawdown_risk 也在
        assert_eq!(d.sub_signals.get("volatility_regime"), Some(&1.0));
        assert_eq!(d.sub_signals.get("drawdown_risk"), Some(&0.25));
    }
}
