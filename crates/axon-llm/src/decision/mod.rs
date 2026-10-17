//! SDM（Structured Decision Model，结构化决策模型）决策层
//!
//! 与 [`crate::backend::LLMBackend`] 并列的模型后端能力：
//! - [`DecisionBackend`]：决策后端 trait
//! - [`StubDecisionBackend`]：确定性模拟
//! - [`ReplayDecisionBackend`]：指纹精确回放
//! - [`SdmHttpBackend`]：真实 HTTP（`decision-http` feature）
//! - [`SdmCascadeDecisionLayer`]：置信门控级联（直出 / 低置信兜底 / 异常兜底）

pub mod backend;
pub mod cascade;
pub mod fingerprint;
pub mod llm_fallback;
pub mod quant_questions;
pub mod replay;
pub mod stub;
pub mod types;

#[cfg(feature = "decision-http")]
pub mod http;

pub use backend::{DecisionBackend, DecisionError};
pub use cascade::{
    ACTIONS, CascadeConfig, FallbackDecider, RuleFallback, SdmCascadeDecisionLayer, gate_allows,
};
pub use llm_fallback::LlmFallback;
pub use replay::{REPLAY_MODEL, ReplayDecisionBackend};
pub use stub::{STUB_MODEL, StubDecisionBackend};
pub use types::{
    Action, Answer, CascadeDecision, ChoiceInstructions, Criterion, DecisionRequest,
    DecisionResponse, DecisionSource, DecisionUsage, Question,
};

#[cfg(feature = "decision-http")]
pub use http::{SdmHttpBackend, SdmHttpConfig};
