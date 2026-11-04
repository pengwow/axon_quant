//! LLM 兜底：把 [`crate::backend::LLMBackend`] 包装成 cascade 的兜底决策器
//!
//! SDM 低置信/异常时升级到 LLM 自由文本推理；要求 LLM 仅输出 JSON
//! `{"action":"buy|sell|hold","confidence":0.0-1.0}`。解析/调用失败由
//! cascade 统一降级为安全 Hold（见 `SdmCascadeDecisionLayer::use_fallback`）。

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::backend::{LLMBackend, LLMError};
use crate::prompt::PromptTemplate;
use crate::types::Message;

use super::backend::DecisionError;
use super::cascade::{FallbackDecider, FallbackOutcome};
use super::types::Action;

/// LLM 兜底决策器
pub struct LlmFallback {
    backend: Arc<dyn LLMBackend>,
    prompt: PromptTemplate,
}

impl LlmFallback {
    /// 用默认 JSON 输出模板构造
    pub fn new(backend: Arc<dyn LLMBackend>) -> Self {
        Self {
            backend,
            prompt: Self::default_template(),
        }
    }

    /// 用自定义模板构造（user 中需含 `{state}` 占位符）
    pub fn with_prompt(backend: Arc<dyn LLMBackend>, prompt: PromptTemplate) -> Self {
        Self { backend, prompt }
    }

    /// 默认模板：state 摘要 + 三选一 + 强制 JSON
    pub fn default_template() -> PromptTemplate {
        PromptTemplate {
            system: "你是量化交易决策兜底器。只能输出 JSON，不要输出任何额外文字。\n\
                     输出格式：{\"action\":\"buy|sell|hold\",\"confidence\":0.0到1.0的数字}"
                .to_string(),
            user: "基于以下市场状态，决定下一根 bar 的方向（buy 做多 / sell 做空 / hold 观望）：\n{state}"
                .to_string(),
            few_shot: vec![],
        }
    }
}

/// LLMError → DecisionError（语义对齐）
pub(crate) fn map_llm_error(e: LLMError) -> DecisionError {
    match e {
        LLMError::Network(m) => DecisionError::Network(m),
        LLMError::Auth(m) => DecisionError::Auth(m),
        LLMError::RateLimited { retry_after } => DecisionError::RateLimited { retry_after },
        LLMError::Parse(m) => DecisionError::Parse(m),
        LLMError::ContextOverflow { needed, limit } => {
            DecisionError::Backend(format!("context window overflow: {needed} > {limit}"))
        }
        LLMError::MockExhausted => DecisionError::Backend("mock backend exhausted".into()),
        LLMError::Backend(m) => DecisionError::Backend(m),
    }
}

/// 从 LLM 文本中抽取 JSON 并解析为 (动作, 三键概率, 置信度)
pub(crate) fn parse_llm_content(
    content: &str,
) -> Result<(Action, BTreeMap<String, f64>, f64), DecisionError> {
    let start = content
        .find('{')
        .ok_or_else(|| DecisionError::Parse("no json object".into()))?;
    let end = content
        .rfind('}')
        .ok_or_else(|| DecisionError::Parse("no json object".into()))?;
    if end < start {
        return Err(DecisionError::Parse("malformed json object".into()));
    }
    let v: serde_json::Value = serde_json::from_str(&content[start..=end])
        .map_err(|e| DecisionError::Parse(e.to_string()))?;
    let action = v
        .get("action")
        .and_then(|x| x.as_str())
        .and_then(Action::from_name)
        .ok_or_else(|| DecisionError::Parse("missing/invalid action".into()))?;
    let confidence = v
        .get("confidence")
        .and_then(|x| x.as_f64())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    // 胜选项给置信度，其余概率均分剩余质量，保证三键齐全且和为 1
    let rest = (1.0 - confidence).max(0.0) / 2.0;
    let mut probs = BTreeMap::new();
    for name in ["buy", "sell", "hold"] {
        probs.insert(
            name.to_string(),
            if Action::from_name(name) == Some(action) {
                confidence
            } else {
                rest
            },
        );
    }
    Ok((action, probs, confidence))
}

#[async_trait]
impl FallbackDecider for LlmFallback {
    async fn decide(&self, state: &serde_json::Value) -> Result<FallbackOutcome, DecisionError> {
        let state_str = state.to_string();
        let vars = HashMap::from([("state", state_str.as_str())]);
        let user = self.prompt.render_user(&vars);
        let messages = vec![
            Message::system(self.prompt.system_with_few_shot()),
            Message::user(user),
        ];
        let resp = self
            .backend
            .complete(&messages)
            .await
            .map_err(map_llm_error)?;
        let thinking = resp.reasoning_content.clone();
        let content = resp
            .content
            .ok_or_else(|| DecisionError::Parse("llm returned empty content".into()))?;
        let (action, probabilities, confidence) = parse_llm_content(&content)?;
        Ok(FallbackOutcome {
            action,
            probabilities,
            confidence,
            raw: Some(content),
            thinking,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_json_inside_fence() {
        let text = "好的，结果是：\n```json\n{\"action\":\"sell\",\"confidence\":0.8}\n```";
        let (a, probs, c) = parse_llm_content(text).unwrap();
        assert_eq!(a, Action::Sell);
        assert!((c - 0.8).abs() < 1e-9);
        assert!((probs["buy"] - 0.1).abs() < 1e-9);
        assert!((probs["sell"] - 0.8).abs() < 1e-9);
        assert!((probs["hold"] - 0.1).abs() < 1e-9);
    }

    #[test]
    fn parse_rejects_bad_action() {
        assert!(parse_llm_content("{\"action\":\"x\",\"confidence\":0.5}").is_err());
        assert!(parse_llm_content("no json here").is_err());
    }

    // 需要 MockBackend（backends feature）
    #[cfg(feature = "backends")]
    #[tokio::test]
    async fn end_to_end_with_mock_backend() {
        use crate::backends::MockBackend;
        let llm: Arc<dyn LLMBackend> = Arc::new(MockBackend::text_only(
            "{\"action\":\"buy\",\"confidence\":0.66}",
        ));
        let fb = LlmFallback::new(llm);
        let outcome = fb
            .decide(&serde_json::json!({"features": {}}))
            .await
            .unwrap();
        assert_eq!(outcome.action, Action::Buy);
        assert!((outcome.confidence - 0.66).abs() < 1e-9);
        assert!(outcome.raw.is_some());
    }
}
