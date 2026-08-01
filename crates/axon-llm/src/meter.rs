//! Token 计量装饰器
//!
//! [`TokenMeter`] 以装饰器模式包裹任意 [`LLMBackend`],透明累加 input/output token,
//! 支持可选预算预警(`TokenBudget`)。对上层 agent 完全透明。
//!
//! ```rust,ignore
//! let metered = TokenMeter::new(Box::new(backend), Some(budget));
//! // 传给 ReActAgent,agent 无感知
//! let report = metered.report();
//! ```

use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;

use crate::backend::{LLMBackend, LLMError, ToolDefinition};
use crate::types::{LLMResponse, Message, TokenUsage};

#[cfg(feature = "backends")]
use crate::backends::cost::pricing_for;

/// Token 预算配置
#[derive(Debug, Clone)]
pub struct TokenBudget {
    /// 最大 input token（所有调用累计）
    pub max_input_tokens: u64,
    /// 最大 output token（所有调用累计）
    pub max_output_tokens: u64,
    /// 预警阈值（0.0-1.0），达到预算的该比例时触发 warning
    pub warn_threshold: f32,
}

impl Default for TokenBudget {
    fn default() -> Self {
        Self {
            max_input_tokens: 1_000_000,
            max_output_tokens: 500_000,
            warn_threshold: 0.8,
        }
    }
}

/// Token 汇总报告
#[derive(Debug, Clone, Default)]
pub struct TokenReport {
    /// 累计 input token
    pub input_tokens: u64,
    /// 累计 output token
    pub output_tokens: u64,
    /// 累计总 token
    pub total_tokens: u64,
    /// 调用次数
    pub call_count: u64,
    /// 估算费用（USD）
    pub estimated_cost_usd: f64,
    /// 是否超预算
    pub over_budget: bool,
}

/// Token 计量装饰器
///
/// 包裹任意 `LLMBackend`，透传 `complete` / `complete_with_tools`，
/// 同时以原子计数器累加 token 使用量。
pub struct TokenMeter {
    inner: Box<dyn LLMBackend>,
    input_tokens: AtomicU64,
    output_tokens: AtomicU64,
    call_count: AtomicU64,
    /// 累计费用（用 f64 的 bit 表示做原子操作）
    cost_bits: AtomicU64,
    budget: Option<TokenBudget>,
    /// 用于 cost 估算的 model 名
    #[cfg_attr(not(feature = "backends"), allow(dead_code))]
    model: String,
    /// 是否已触发过 warning（避免重复 warn）
    warned: AtomicU64,
}

impl TokenMeter {
    /// 构造计量装饰器
    ///
    /// - `inner`: 被包裹的 backend
    /// - `budget`: 可选预算配置
    /// - `model`: 模型名（用于 cost 估算）
    pub fn new(inner: Box<dyn LLMBackend>, budget: Option<TokenBudget>, model: String) -> Self {
        Self {
            inner,
            input_tokens: AtomicU64::new(0),
            output_tokens: AtomicU64::new(0),
            call_count: AtomicU64::new(0),
            cost_bits: AtomicU64::new(0),
            budget,
            model,
            warned: AtomicU64::new(0),
        }
    }

    /// 获取当前汇总报告
    pub fn report(&self) -> TokenReport {
        let input = self.input_tokens.load(Ordering::Relaxed);
        let output = self.output_tokens.load(Ordering::Relaxed);
        let calls = self.call_count.load(Ordering::Relaxed);
        let cost = f64::from_bits(self.cost_bits.load(Ordering::Relaxed));
        let over_budget = self.is_over_budget(input, output);
        TokenReport {
            input_tokens: input,
            output_tokens: output,
            total_tokens: input + output,
            call_count: calls,
            estimated_cost_usd: cost,
            over_budget,
        }
    }

    /// 重置计数器
    pub fn reset(&self) {
        self.input_tokens.store(0, Ordering::Relaxed);
        self.output_tokens.store(0, Ordering::Relaxed);
        self.call_count.store(0, Ordering::Relaxed);
        self.cost_bits.store(0, Ordering::Relaxed);
        self.warned.store(0, Ordering::Relaxed);
    }

    /// 内部：记录一次调用的 token 使用
    fn record(&self, usage: &TokenUsage) {
        self.input_tokens
            .fetch_add(usage.prompt_tokens as u64, Ordering::Relaxed);
        self.output_tokens
            .fetch_add(usage.completion_tokens as u64, Ordering::Relaxed);
        self.call_count.fetch_add(1, Ordering::Relaxed);

        // cost 估算
        #[cfg(feature = "backends")]
        {
            if let Some(pricing) = pricing_for(&self.model) {
                let cost = pricing.compute(usage);
                // 原子累加 f64（CAS loop）
                loop {
                    let current = self.cost_bits.load(Ordering::Relaxed);
                    let new_val = f64::from_bits(current) + cost;
                    if self
                        .cost_bits
                        .compare_exchange_weak(
                            current,
                            new_val.to_bits(),
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        )
                        .is_ok()
                    {
                        break;
                    }
                }
            }
        }

        // 预算预警
        self.check_budget();
    }

    /// 内部：检查预算并触发 warning
    fn check_budget(&self) {
        let Some(budget) = &self.budget else {
            return;
        };
        // 已 warn 过则不重复
        if self.warned.load(Ordering::Relaxed) != 0 {
            return;
        }
        let input = self.input_tokens.load(Ordering::Relaxed);
        let output = self.output_tokens.load(Ordering::Relaxed);
        let threshold = budget.warn_threshold as f64;
        let input_ratio = input as f64 / budget.max_input_tokens as f64;
        let output_ratio = output as f64 / budget.max_output_tokens as f64;
        if input_ratio >= threshold || output_ratio >= threshold {
            self.warned.store(1, Ordering::Relaxed);
            tracing::warn!(
                input_tokens = input,
                output_tokens = output,
                max_input = budget.max_input_tokens,
                max_output = budget.max_output_tokens,
                "TokenMeter: token usage approaching budget limit ({:.0}% threshold)",
                threshold * 100.0
            );
        }
    }

    /// 内部：是否超预算
    fn is_over_budget(&self, input: u64, output: u64) -> bool {
        match &self.budget {
            Some(b) => input > b.max_input_tokens || output > b.max_output_tokens,
            None => false,
        }
    }
}

#[async_trait]
impl LLMBackend for TokenMeter {
    async fn complete(&self, messages: &[Message]) -> Result<LLMResponse, LLMError> {
        let resp = self.inner.complete(messages).await?;
        self.record(&resp.token_usage);
        Ok(resp)
    }

    async fn complete_with_tools(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse, LLMError> {
        let resp = self.inner.complete_with_tools(messages, tools).await?;
        self.record(&resp.token_usage);
        Ok(resp)
    }

    fn context_window_size(&self) -> usize {
        self.inner.context_window_size()
    }
}

#[cfg(test)]
#[cfg(feature = "backends")]
mod tests {
    use super::*;
    use crate::backends::MockBackend;

    fn mock_with_usage(prompt: usize, completion: usize) -> MockBackend {
        let resp = LLMResponse {
            content: Some("hello".into()),
            tool_calls: None,
            token_usage: TokenUsage::new(prompt, completion),
            finish_reason: crate::types::FinishReason::Stop,
        };
        MockBackend::with_responses(vec![resp])
    }

    #[tokio::test]
    async fn meter_counts_tokens() {
        let mock = mock_with_usage(100, 50);
        let meter = TokenMeter::new(Box::new(mock), None, "unknown".into());

        let resp = meter.complete(&[Message::user("hi")]).await.unwrap();
        assert_eq!(resp.content.as_deref(), Some("hello"));

        let report = meter.report();
        assert_eq!(report.input_tokens, 100);
        assert_eq!(report.output_tokens, 50);
        assert_eq!(report.total_tokens, 150);
        assert_eq!(report.call_count, 1);
        assert!(!report.over_budget);
    }

    #[tokio::test]
    async fn meter_accumulates_multiple_calls() {
        let r1 = LLMResponse {
            content: Some("a".into()),
            tool_calls: None,
            token_usage: TokenUsage::new(10, 5),
            finish_reason: crate::types::FinishReason::Stop,
        };
        let r2 = LLMResponse {
            content: Some("b".into()),
            tool_calls: None,
            token_usage: TokenUsage::new(20, 10),
            finish_reason: crate::types::FinishReason::Stop,
        };
        let mock = MockBackend::with_responses(vec![r1, r2]);
        let meter = TokenMeter::new(Box::new(mock), None, "unknown".into());

        meter.complete(&[Message::user("1")]).await.unwrap();
        meter.complete(&[Message::user("2")]).await.unwrap();

        let report = meter.report();
        assert_eq!(report.input_tokens, 30);
        assert_eq!(report.output_tokens, 15);
        assert_eq!(report.call_count, 2);
    }

    #[tokio::test]
    async fn meter_budget_over_detection() {
        let mock = mock_with_usage(900, 100);
        let budget = TokenBudget {
            max_input_tokens: 1000,
            max_output_tokens: 500,
            warn_threshold: 0.8,
        };
        let meter = TokenMeter::new(Box::new(mock), Some(budget), "unknown".into());
        meter.complete(&[Message::user("hi")]).await.unwrap();

        let report = meter.report();
        // 900 < 1000, not over yet
        assert!(!report.over_budget);

        // 模拟超预算：直接 store
        meter.input_tokens.store(1001, Ordering::Relaxed);
        let report = meter.report();
        assert!(report.over_budget);
    }

    #[tokio::test]
    async fn meter_reset_clears_counters() {
        let mock = mock_with_usage(100, 50);
        let meter = TokenMeter::new(Box::new(mock), None, "unknown".into());
        meter.complete(&[Message::user("hi")]).await.unwrap();
        meter.reset();

        let report = meter.report();
        assert_eq!(report.input_tokens, 0);
        assert_eq!(report.output_tokens, 0);
        assert_eq!(report.call_count, 0);
    }

    #[test]
    fn token_budget_default() {
        let b = TokenBudget::default();
        assert_eq!(b.max_input_tokens, 1_000_000);
        assert_eq!(b.max_output_tokens, 500_000);
        assert!((b.warn_threshold - 0.8).abs() < f32::EPSILON);
    }
}
