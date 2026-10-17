//! SDM 决策协议类型
//!
//! 字段名严格对齐 SDM 协议（`type/instructions/criteria`；
//! 答案 `noul/choice/score/confidence/probabilities`），录制 JSON 与真实响应零转换。
//! [`CascadeDecision`] 是 cascade 所有分支的唯一返回类型。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::backend::DecisionError;

/// 三种交易动作；概率 dict 的键恒为这三个
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    /// 做多
    Buy,
    /// 做空
    Sell,
    /// 观望
    Hold,
}

impl Action {
    /// 协议字符串
    pub fn as_str(&self) -> &'static str {
        match self {
            Action::Buy => "buy",
            Action::Sell => "sell",
            Action::Hold => "hold",
        }
    }

    /// 从协议字符串解析
    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "buy" => Action::Buy,
            "sell" => Action::Sell,
            "hold" => Action::Hold,
            _ => return None,
        })
    }
}

/// cascade 决策来源
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSource {
    /// SDM 直出（置信度达阈值）
    SdmDirect,
    /// 兜底（低置信 / 异常）
    Fallback,
}

/// 单次决策的真实计费信息（成本测算只允许引用这两个字段）
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DecisionUsage {
    /// 输入 token 数
    pub input_tokens: u64,
    /// 本次调用费用（美元）
    pub cost_usd: f64,
}

/// choice 问题的 instructions（对象形态）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChoiceInstructions {
    /// 问题正文
    pub question: String,
    /// 关注点
    pub focus: String,
}

/// choice 问题单个选项的判定标准
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Criterion {
    /// 该选项的描述
    pub what: String,
}

/// 决策问题（内部 tag = `type`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Question {
    /// 是/否命题
    Noul {
        /// 命题说明
        instructions: String,
    },
    /// 有界单选
    Choice {
        /// 问题说明（对象）
        instructions: ChoiceInstructions,
        /// 各选项标准
        criteria: BTreeMap<String, Criterion>,
    },
    /// 有序量表
    Score {
        /// 问题说明（字符串）
        instructions: String,
        /// 各等级描述（索引即等级）
        criteria: Vec<String>,
    },
}

impl Question {
    /// 构造最小可用的 blank 问题（给测试 / 快速原型用）
    ///
    /// kind 是 "choice" / "noul" / "score"。字段全部空值，StubDecisionBackend
    /// 只做 contains_key 检查所以够用。
    pub fn blank(kind: &str) -> Self {
        match kind {
            "choice" => Self::Choice {
                instructions: ChoiceInstructions {
                    question: String::new(),
                    focus: String::new(),
                },
                criteria: BTreeMap::new(),
            },
            "noul" => Self::Noul {
                instructions: String::new(),
            },
            "score" => Self::Score {
                instructions: String::new(),
                criteria: Vec::new(),
            },
            other => panic!("Question::blank: unknown kind '{other}' (use choice|noul|score)"),
        }
    }
}

/// 单个问题的答案（内部 tag = `type`，结构体变体保证 serde 稳定）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Answer {
    /// 是/否命题：noul 为命题成立的校准概率 0–1
    Noul {
        /// 命题成立概率
        noul: f64,
    },
    /// 有界单选：胜选项 + 校准置信 + 全选项概率分布
    Choice {
        /// 胜选项
        choice: String,
        /// 校准置信度
        confidence: f64,
        /// 全选项概率
        probabilities: BTreeMap<String, f64>,
    },
    /// 有序量表：概率加权等级 + 各等级概率
    Score {
        /// 概率加权得分
        score: f64,
        /// 置信度
        confidence: f64,
        /// 各等级概率
        probabilities: BTreeMap<String, f64>,
    },
}

/// 决策请求；state 为领域自定义 JSON，decision 层不解释
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionRequest {
    /// 领域状态（含 features/window 等）
    pub state: serde_json::Value,
    /// 类型化问题表
    pub questions: BTreeMap<String, Question>,
}

/// SDM 单次响应
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionResponse {
    /// 模型标识
    pub model: String,
    /// 各问题答案
    pub answers: BTreeMap<String, Answer>,
    /// 计费信息
    pub usage: DecisionUsage,
}

impl DecisionResponse {
    /// 从原始 JSON 解析；缺字段/未知类型返回 [`DecisionError::Parse`]，不做静默兜底
    pub fn from_value(v: serde_json::Value) -> Result<Self, DecisionError> {
        serde_json::from_value(v).map_err(|e| DecisionError::Parse(e.to_string()))
    }
}

/// cascade 统一出口（直出/低置信/异常三分支同构）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CascadeDecision {
    /// 最终动作
    pub action: Action,
    /// 来源
    pub source: DecisionSource,
    /// 置信度
    pub confidence: f64,
    /// buy/sell/hold 三键恒在
    pub probabilities: BTreeMap<String, f64>,
    /// source=Fallback 时必填原因
    pub fallback_reason: Option<String>,
    /// SDM 计费（异常未拿到响应时为 None）
    pub sdm_usage: Option<DecisionUsage>,
    /// 子问题信号，仅归因，禁止参与门控
    pub sub_signals: BTreeMap<String, f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_full_response_three_answer_kinds() {
        let raw = json!({
            "model": "sdm-stub",
            "answers": {
                "next_direction": {
                    "type": "choice",
                    "choice": "buy",
                    "confidence": 0.812345,
                    "probabilities": {"buy": 0.7, "hold": 0.2, "sell": 0.1}
                },
                "trend_confirms": {"type": "noul", "noul": 0.66},
                "signal_clarity": {
                    "type": "score", "score": 2.0,
                    "confidence": 0.8,
                    "probabilities": {"0": 0.1, "1": 0.1, "2": 0.8}
                }
            },
            "usage": {"input_tokens": 123, "cost_usd": 0.0045}
        });
        let resp = DecisionResponse::from_value(raw).expect("parse");
        assert_eq!(resp.model, "sdm-stub");
        assert_eq!(resp.usage.input_tokens, 123);
        assert!((resp.usage.cost_usd - 0.0045).abs() < 1e-12);
        match resp.answers.get("next_direction").unwrap() {
            Answer::Choice {
                choice, confidence, ..
            } => {
                assert_eq!(choice, "buy");
                assert!((confidence - 0.812345).abs() < 1e-9);
            }
            other => panic!("expected choice, got {other:?}"),
        }
        assert!(matches!(
            resp.answers.get("trend_confirms"),
            Some(Answer::Noul { .. })
        ));
    }

    #[test]
    fn reject_unknown_answer_type() {
        let raw = json!({"model":"m","answers":{"q":{"type":"weird"}},"usage":{"input_tokens":0,"cost_usd":0.0}});
        let err = DecisionResponse::from_value(raw).expect_err("should fail");
        assert!(matches!(err, DecisionError::Parse(_)), "got {err:?}");
    }

    #[test]
    fn action_roundtrip_and_serde_source() {
        assert_eq!(Action::Buy.as_str(), "buy");
        assert_eq!(Action::from_name("sell"), Some(Action::Sell));
        assert_eq!(Action::from_name("nope"), None);
        assert_eq!(
            serde_json::to_string(&DecisionSource::SdmDirect).unwrap(),
            "\"sdm_direct\""
        );
        assert_eq!(
            serde_json::to_string(&DecisionSource::Fallback).unwrap(),
            "\"fallback\""
        );
    }

    #[test]
    fn request_questions_deserialize_python_shape() {
        let raw = json!({
            "state": {"features": {"ret_1": 0.01}},
            "questions": {
                "next_direction": {
                    "type": "choice",
                    "instructions": {"question": "q", "focus": "f"},
                    "criteria": {"buy": {"what": "up"}, "sell": {"what": "down"}, "hold": {"what": "flat"}}
                },
                "trend_confirms": {"type": "noul", "instructions": "confirm?"},
                "signal_clarity": {"type": "score", "instructions": "clarity?", "criteria": ["a", "b", "c"]}
            }
        });
        let req: DecisionRequest = serde_json::from_value(raw).expect("req");
        assert_eq!(req.questions.len(), 3);
        assert!(matches!(
            req.questions.get("trend_confirms"),
            Some(Question::Noul { .. })
        ));
    }
}
