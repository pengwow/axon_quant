//! Agent Swarm 模块
//!
//! 多 Agent 协作框架，支持投票共识和动态扩缩容。

pub mod agent;
pub mod agent_runner;
pub mod agents;
pub mod error;
pub mod market_data;
pub mod message;
pub mod orchestrator;
pub mod paper_trading;
pub mod vote;
