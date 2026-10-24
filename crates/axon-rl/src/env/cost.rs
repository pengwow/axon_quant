//! 交易成本模型
//!
//! 训练（`Executor`）与内核推理/回测共用这一份费率与滑点口径，保证两侧成本项
//! 完全一致（统一内核设计 6.5：成本差异必须为零）。

use crate::action::converter::OrderSide;
use crate::env::config::EnvConfig;

/// 交易成本模型
///
/// V1 只支持 taker 单费率 + 比例滑点（设计 4.11：不做 maker/taker 区分与
/// 排队位置模型）。新增档位时在这里扩展，不要在每个调用点各写一遍乘法。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostModel {
    /// 手续费率（按成交名义价值计）
    pub fee_rate: f64,
    /// 滑点比例（买价上浮 / 卖价下压）
    pub slippage: f64,
}

impl CostModel {
    /// 从环境配置提取成本参数
    pub fn from_env(cfg: &EnvConfig) -> Self {
        Self {
            fee_rate: cfg.transaction_cost,
            slippage: cfg.slippage,
        }
    }

    /// 手续费（名义价值 × 费率）
    pub fn fee(&self, notional: f64) -> f64 {
        notional * self.fee_rate
    }

    /// 含滑点的成交价
    ///
    /// 买入上浮、卖出下压，方向由 `side` 决定，和撮合对手价无关——这是训练侧
    /// 简化模型的口径，内核 `BacktestSink` 走真实 L1 撮合，成本项靠同一份
    /// `fee_rate` 对齐。
    pub fn fill_price(&self, side: OrderSide, price: f64) -> f64 {
        match side {
            OrderSide::Buy => price * (1.0 + self.slippage),
            OrderSide::Sell => price * (1.0 - self.slippage),
        }
    }
}
