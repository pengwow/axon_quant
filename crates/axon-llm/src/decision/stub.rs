//! 确定性模拟决策后端（model = "sdm-stub"）
//!
//! ponytail: 用极简线性强度 x（双均线偏离 + 短周期收益，按波动率归一）驱动 softmax；
//! 天花板是不模拟真实 SDM 的判断偏差，用途仅限打通链路与 CI。升级路径是录制真实响应。

use async_trait::async_trait;
use serde_json::json;

use super::backend::{DecisionBackend, DecisionError};
use super::types::{DecisionRequest, DecisionResponse};

/// 模型标识
pub const STUB_MODEL: &str = "sdm-stub";

/// 确定性模拟后端
pub struct StubDecisionBackend;

impl StubDecisionBackend {
    /// 构造
    pub fn new() -> Self {
        Self
    }
}

impl Default for StubDecisionBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn softmax(values: [f64; 3]) -> [f64; 3] {
    let top = values[0].max(values[1]).max(values[2]);
    let exps = values.map(|v| (v - top).exp());
    let total: f64 = exps.iter().sum();
    exps.map(|v| v / total)
}

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

fn clamp(x: f64, lo: f64, hi: f64) -> f64 {
    lo.max(hi.min(x))
}

fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

fn feature(state: &serde_json::Value, name: &str) -> Result<f64, DecisionError> {
    state
        .get("features")
        .and_then(|f| f.get(name))
        .and_then(|v| v.as_f64())
        .ok_or_else(|| DecisionError::Backend(format!("state.features.{name} missing")))
}

/// 安全读取可选特征（v2 新增特征在 v1 问题模板下可能不存在）
fn feature_opt(state: &serde_json::Value, name: &str) -> f64 {
    feature(state, name).unwrap_or(50.0)
}

#[async_trait]
impl DecisionBackend for StubDecisionBackend {
    async fn decide(&self, req: &DecisionRequest) -> Result<DecisionResponse, DecisionError> {
        let f = &req.state;
        let vol = feature(f, "vol_20")?.max(1e-9);
        let sma = feature(f, "sma5_vs_sma20")?;
        let ret = feature(f, "ret_1")?;
        let volz = feature(f, "volume_z")?;

        let x = clamp(sma / vol, -3.0, 3.0) + 0.5 * clamp(ret / vol, -2.0, 2.0);
        // 顺序固定：buy, sell, hold
        let probs = softmax([x, -x, -x.abs() * 1.5 + 0.5]);
        let top_idx = if probs[0] >= probs[1] && probs[0] >= probs[2] {
            0
        } else if probs[1] >= probs[2] {
            1
        } else {
            2
        };
        let names = ["buy", "sell", "hold"];
        let choice_name = names[top_idx];
        let top_p = round6(probs[top_idx]);
        let choice_probs = json!({
            "buy": round6(probs[0]),
            "sell": round6(probs[1]),
            "hold": round6(probs[2]),
        });

        // 清晰度等级跟随胜选项概率
        let level = if top_p >= 0.7 {
            2
        } else if top_p >= 0.5 {
            1
        } else {
            0
        };
        let mut score_probs: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
        for i in 0..3 {
            score_probs.insert(i.to_string(), json!(0.0));
        }
        score_probs.insert(level.to_string(), json!(0.8));
        let lower = (level - 1).max(0);
        let upper = (level + 1).min(2);
        for idx in [lower, upper] {
            let cur = score_probs[&idx.to_string()].as_f64().unwrap_or(0.0);
            score_probs.insert(idx.to_string(), json!(cur + 0.1));
        }

        // —— 先构建 v1 全部 4 个答案 —— //
        let mut answers = json!({
            "next_direction": {
                "type": "choice",
                "choice": choice_name,
                "confidence": top_p,
                "probabilities": choice_probs
            },
            "trend_confirms": {"type": "noul", "noul": round6(sigmoid(x.abs()))},
            "risk_regime_bad": {"type": "noul", "noul": round6(sigmoid(volz.abs() - 1.5))},
            "signal_clarity": {
                "type": "score",
                "score": level as f64,
                "confidence": 0.8,
                "probabilities": serde_json::Value::Object(score_probs)
            }
        });

        // —— v2 新增 4 个，仅在 questions 里存在时才生成 —— //
        let qs = &req.questions;
        let ans_map = answers.as_object_mut().expect("answers must be object");

        if qs.contains_key("position_size") {
            ans_map.insert(
                "position_size".into(),
                mock_position_size(choice_name, top_p),
            );
        }
        if qs.contains_key("volatility_regime") {
            ans_map.insert(
                "volatility_regime".into(),
                mock_volatility_regime(feature_opt(f, "vol_20")),
            );
        }
        if qs.contains_key("drawdown_risk") {
            ans_map.insert("drawdown_risk".into(), mock_drawdown_risk(f));
        }
        if qs.contains_key("execution_urgency") {
            ans_map.insert("execution_urgency".into(), mock_execution_urgency(x));
        }

        let raw = json!({
            "model": STUB_MODEL,
            "answers": answers,
            "usage": {"input_tokens": 0, "cost_usd": 0.0}
        });
        DecisionResponse::from_value(raw)
    }

    fn model_name(&self) -> &str {
        STUB_MODEL
    }
}

// ── v2 mock 子函数（与 Python StubDecisionClient 同构） ── //

fn mock_position_size(direction: &str, confidence: f64) -> serde_json::Value {
    let (choice, probs_obj) = if direction == "hold" {
        (
            "0pct",
            json!({"0pct": 0.8, "25pct": 0.15, "50pct": 0.05, "100pct": 0.0}),
        )
    } else if confidence < 0.5 {
        (
            "25pct",
            json!({"0pct": 0.1, "25pct": 0.7, "50pct": 0.18, "100pct": 0.02}),
        )
    } else if confidence < 0.7 {
        (
            "50pct",
            json!({"0pct": 0.02, "25pct": 0.1, "50pct": 0.75, "100pct": 0.13}),
        )
    } else {
        (
            "100pct",
            json!({"0pct": 0.0, "25pct": 0.05, "50pct": 0.2, "100pct": 0.75}),
        )
    };
    let conf = probs_obj[choice].as_f64().unwrap_or(0.0);
    json!({
        "type": "choice",
        "choice": choice,
        "confidence": round6(conf),
        "probabilities": round_probs(probs_obj)
    })
}

fn mock_volatility_regime(vol_20: f64) -> serde_json::Value {
    let (score, probs_obj) = if vol_20 < 0.005 {
        (0, json!({"0": 0.85, "1": 0.14, "2": 0.01}))
    } else if vol_20 <= 0.02 {
        (1, json!({"0": 0.1, "1": 0.8, "2": 0.1}))
    } else {
        (2, json!({"0": 0.01, "1": 0.14, "2": 0.85}))
    };
    let conf = probs_obj[&score.to_string()].as_f64().unwrap_or(0.0);
    json!({
        "type": "score",
        "score": score as f64,
        "confidence": round6(conf),
        "probabilities": round_probs(probs_obj)
    })
}

fn mock_drawdown_risk(state: &serde_json::Value) -> serde_json::Value {
    let rsi = feature_opt(state, "rsi_14");
    let c20 = feature_opt(state, "close_vs_sma20");
    let base = if rsi > 70.0 || rsi < 30.0 { 0.7 } else { 0.3 };
    let base = if c20.abs() > 0.05 {
        (base + 0.15_f64).min(0.9)
    } else {
        base
    };
    json!({"type": "noul", "noul": round6(base)})
}

fn mock_execution_urgency(x: f64) -> serde_json::Value {
    let ax = x.abs();
    let (choice, probs_obj) = if ax > 2.0 {
        (
            "immediate",
            json!({"immediate": 0.8, "within_60s": 0.18, "cancel": 0.02}),
        )
    } else if ax > 1.0 {
        (
            "within_60s",
            json!({"immediate": 0.15, "within_60s": 0.75, "cancel": 0.1}),
        )
    } else {
        (
            "cancel",
            json!({"immediate": 0.02, "within_60s": 0.2, "cancel": 0.78}),
        )
    };
    let conf = probs_obj[choice].as_f64().unwrap_or(0.0);
    json!({
        "type": "choice",
        "choice": choice,
        "confidence": round6(conf),
        "probabilities": round_probs(probs_obj)
    })
}

/// 把 JSON Object 的概率值全部 round 到 6 位
fn round_probs(obj: serde_json::Value) -> serde_json::Value {
    let mut map = obj.as_object().cloned().unwrap_or_default();
    for (_k, v) in map.iter_mut() {
        if let Some(f) = v.as_f64() {
            *v = json!(round6(f));
        }
    }
    serde_json::Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::super::types::{Answer, Question};
    use super::*;
    use std::collections::BTreeMap;

    fn request_with(sma: f64, ret: f64) -> DecisionRequest {
        DecisionRequest {
            state: json!({
                "features": {
                    "ret_1": ret, "ret_5": ret, "vol_20": 0.01,
                    "sma5_vs_sma20": sma, "volume_z": 0.2
                }
            }),
            questions: BTreeMap::new(),
        }
    }

    /// 按实际类型表构造 v2 全问题集
    fn v2_full_questions() -> BTreeMap<String, Question> {
        [
            ("next_direction", Question::blank("choice")),
            ("trend_confirms", Question::blank("noul")),
            ("risk_regime_bad", Question::blank("noul")),
            ("signal_clarity", Question::blank("score")),
            ("position_size", Question::blank("choice")),
            ("volatility_regime", Question::blank("score")),
            ("drawdown_risk", Question::blank("noul")),
            ("execution_urgency", Question::blank("choice")),
        ]
        .into_iter()
        .map(|(k, q)| (k.to_string(), q))
        .collect()
    }

    /// 只含 v1 的 4 个问题
    fn v1_only_questions() -> BTreeMap<String, Question> {
        [
            ("next_direction", Question::blank("choice")),
            ("trend_confirms", Question::blank("noul")),
            ("risk_regime_bad", Question::blank("noul")),
            ("signal_clarity", Question::blank("score")),
        ]
        .into_iter()
        .map(|(k, q)| (k.to_string(), q))
        .collect()
    }

    #[tokio::test]
    async fn strong_up_market_picks_buy_and_is_deterministic() {
        let b = StubDecisionBackend::new();
        let r1 = b.decide(&request_with(0.02, 0.01)).await.unwrap();
        let r2 = b.decide(&request_with(0.02, 0.01)).await.unwrap();
        assert_eq!(r1, r2);
        assert_eq!(b.model_name(), STUB_MODEL);
        match r1.answers.get("next_direction").unwrap() {
            Answer::Choice { choice, .. } => assert_eq!(choice, "buy"),
            other => panic!("{other:?}"),
        }
        assert_eq!(r1.usage.cost_usd, 0.0);
    }

    #[tokio::test]
    async fn missing_feature_is_backend_error() {
        let req = DecisionRequest {
            state: json!({"features": {}}),
            questions: BTreeMap::new(),
        };
        let err = StubDecisionBackend::new()
            .decide(&req)
            .await
            .expect_err("fail");
        assert!(matches!(err, DecisionError::Backend(_)));
    }

    #[tokio::test]
    async fn v2_questions_produce_all_eight_answers() {
        // 包含全部 8 个 v2 问题 + 完整特征集
        let features = json!({
            "ret_1": 0.01, "ret_5": 0.02, "vol_20": 0.008,
            "sma5_vs_sma20": 0.02, "volume_z": 0.5,
            "close_vs_sma20": 0.01, "rsi_14": 65.0, "volume_ratio": 1.2
        });
        let req = DecisionRequest {
            state: json!({"features": features}),
            questions: v2_full_questions(),
        };
        let resp = StubDecisionBackend::new().decide(&req).await.unwrap();

        // v1 4 个必须都在
        for k in [
            "next_direction",
            "trend_confirms",
            "risk_regime_bad",
            "signal_clarity",
        ] {
            assert!(resp.answers.contains_key(k), "missing v1 key: {k}");
        }
        // v2 4 个也必须在
        for k in [
            "position_size",
            "volatility_regime",
            "drawdown_risk",
            "execution_urgency",
        ] {
            assert!(resp.answers.contains_key(k), "missing v2 key: {k}");
        }

        // 类型检查：v2 新增的 4 个类型正确
        assert!(matches!(
            resp.answers.get("position_size").unwrap(),
            Answer::Choice { .. }
        ));
        assert!(matches!(
            resp.answers.get("volatility_regime").unwrap(),
            Answer::Score { .. }
        ));
        assert!(matches!(
            resp.answers.get("drawdown_risk").unwrap(),
            Answer::Noul { .. }
        ));
        assert!(matches!(
            resp.answers.get("execution_urgency").unwrap(),
            Answer::Choice { .. }
        ));
    }

    #[tokio::test]
    async fn v1_questions_only_do_not_produce_v2_answers() {
        // 只给 v1 的 4 个问题，v2 问题不在 questions 里 → 不应该输出 v2 答案
        let features = json!({
            "ret_1": 0.01, "ret_5": 0.02, "vol_20": 0.01,
            "sma5_vs_sma20": 0.02, "volume_z": 0.5
        });
        let req = DecisionRequest {
            state: json!({"features": features}),
            questions: v1_only_questions(),
        };
        let resp = StubDecisionBackend::new().decide(&req).await.unwrap();

        for k in [
            "position_size",
            "volatility_regime",
            "drawdown_risk",
            "execution_urgency",
        ] {
            assert!(
                !resp.answers.contains_key(k),
                "v1-only request should NOT have v2 key: {k}"
            );
        }
    }
}
