//! LLM 调用成本跟踪
//!
//! 提供:
//! - [`ModelPricing`]:单模型定价(input/output 每 1M tokens USD 价格)
//! - [`CostTracker`]:累积多笔调用的 token + USD
//! - [`pricing_for`]:按模型名查表(未知返回 None,便于测试断言)

use crate::types::TokenUsage;
use std::collections::HashMap;
use std::sync::RwLock;

/// 单模型定价
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPricing {
    /// 每 1M input token 的 USD 价格
    pub input_per_million: f64,
    /// 每 1M output token 的 USD 价格
    pub output_per_million: f64,
}

impl ModelPricing {
    /// 按 TokenUsage 计算费用(USD)
    ///
    /// 公式:`prompt / 1e6 * input + completion / 1e6 * output`
    pub fn compute(&self, usage: &TokenUsage) -> f64 {
        (usage.prompt_tokens as f64 / 1_000_000.0) * self.input_per_million
            + (usage.completion_tokens as f64 / 1_000_000.0) * self.output_per_million
    }
}

/// 全局定价表(进程内单例,可通过 [`register_pricing`] 扩展)
static PRICING: RwLock<Option<HashMap<&'static str, ModelPricing>>> = RwLock::new(None);

/// 注册默认定价表(幂等)
fn ensure_default_pricing() {
    {
        let guard = PRICING.read().expect("pricing poisoned");
        if guard.is_some() {
            return;
        }
    }
    let mut guard = PRICING.write().expect("pricing poisoned");
    if guard.is_some() {
        return;
    }
    let mut map: HashMap<&'static str, ModelPricing> = HashMap::new();
    // DeepSeek 2026-06-12 定价
    map.insert(
        "deepseek-chat",
        ModelPricing {
            input_per_million: 0.14,
            output_per_million: 0.28,
        },
    );
    map.insert(
        "deepseek-coder",
        ModelPricing {
            input_per_million: 0.14,
            output_per_million: 0.28,
        },
    );
    // OpenAI 主流模型参考价(便于 e2e 测试)
    map.insert(
        "gpt-4o-mini",
        ModelPricing {
            input_per_million: 0.15,
            output_per_million: 0.60,
        },
    );
    map.insert(
        "gpt-4o",
        ModelPricing {
            input_per_million: 2.50,
            output_per_million: 10.00,
        },
    );
    *guard = Some(map);
}

/// 按模型名查表。未知模型返回 `None`(调用方决定 panic / fallback)
pub fn pricing_for(model: &str) -> Option<ModelPricing> {
    ensure_default_pricing();
    let guard = PRICING.read().expect("pricing poisoned");
    guard.as_ref().and_then(|m| m.get(model).copied())
}

/// 注册自定义定价(测试 / 私有部署用)
///
/// # 幂等性
/// 多次注册同 model 取最后一次的值(`HashMap::insert` 语义)。
///
/// # 不可撤销
/// 本函数无对应的 unregister 入口: 默认定价表 + 已注册的 model
/// 在进程生命周期内共存。测试间**禁止**通过 reset 破坏 PRICING
/// (历史上由 `reset_for_test` 提供,已删除以修复并行测试 flakiness)。
pub fn register_pricing(model: &'static str, pricing: ModelPricing) {
    ensure_default_pricing();
    let mut guard = PRICING.write().expect("pricing poisoned");
    guard
        .as_mut()
        .expect("default pricing initialized")
        .insert(model, pricing);
}

/// 累积多次调用的 token + USD 成本
#[derive(Debug, Default, Clone)]
pub struct CostTracker {
    /// 累计 token
    total_usage: TokenUsage,
    /// 累计 USD
    total_usd: f64,
}

impl CostTracker {
    /// 新建空 tracker
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一笔调用的 token
    ///
    /// 若已知 model 定价,同步累加 USD;否则只累加 token
    pub fn record(&mut self, usage: &TokenUsage, model: &str) {
        self.total_usage.add(*usage);
        if let Some(p) = pricing_for(model) {
            self.total_usd += p.compute(usage);
        }
    }

    /// 当前累计 token
    pub fn total_usage(&self) -> TokenUsage {
        self.total_usage
    }

    /// 当前累计 USD(只对已知模型累计)
    pub fn total_usd(&self) -> f64 {
        self.total_usd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_known_pricing() {
        let p = ModelPricing {
            input_per_million: 0.14,
            output_per_million: 0.28,
        };
        let u = TokenUsage::new(1000, 500);
        // 0.001 * 0.14 + 0.0005 * 0.28 = 0.00014 + 0.00014 = 0.00028
        let cost = p.compute(&u);
        assert!((cost - 0.00028).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn pricing_for_known_model() {
        assert!(pricing_for("deepseek-chat").is_some());
        assert!(pricing_for("unknown-model").is_none());
    }

    #[test]
    fn register_pricing_overrides() {
        // 不再 reset: register_pricing 自身是 HashMap::insert 语义,
        // 默认表(deepseek-chat / gpt-4o-mini 等)始终存在。
        // 显式断言默认表未受影响,作为 reset 已删除的回归保护。
        register_pricing(
            "custom-model",
            ModelPricing {
                input_per_million: 1.0,
                output_per_million: 2.0,
            },
        );
        // 自定义 model 生效
        let p = pricing_for("custom-model").unwrap();
        assert_eq!(p.input_per_million, 1.0);
        assert_eq!(p.output_per_million, 2.0);
        // 默认表不受影响(回归保护: reset_for_test 已被删除)
        assert!(pricing_for("deepseek-chat").is_some());
        assert!(pricing_for("gpt-4o-mini").is_some());
    }

    #[test]
    fn register_pricing_idempotent() {
        // 同一 model 多次 register: HashMap::insert 语义,最后一次胜出。
        // 这是 PRICING 不可 reset 约束的契约,作为未来重构者参考。
        register_pricing(
            "idem-model",
            ModelPricing {
                input_per_million: 1.0,
                output_per_million: 2.0,
            },
        );
        register_pricing(
            "idem-model",
            ModelPricing {
                input_per_million: 3.0,
                output_per_million: 4.0,
            },
        );
        let p = pricing_for("idem-model").unwrap();
        assert_eq!(p.input_per_million, 3.0);
        assert_eq!(p.output_per_million, 4.0);
    }

    #[test]
    fn cost_tracker_records_known_model() {
        let mut t = CostTracker::new();
        let u = TokenUsage::new(1000, 500);
        t.record(&u, "deepseek-chat");
        assert_eq!(t.total_usage().prompt_tokens, 1000);
        assert!(t.total_usd() > 0.0);
    }

    #[test]
    fn cost_tracker_unknown_model_just_records_tokens() {
        let mut t = CostTracker::new();
        let u = TokenUsage::new(1000, 500);
        t.record(&u, "unknown-model");
        assert_eq!(t.total_usage().prompt_tokens, 1000);
        assert_eq!(t.total_usd(), 0.0); // 未知模型不累计 USD
    }
}
