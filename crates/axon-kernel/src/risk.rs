//! 下单前风控钩子
//!
//! [`RiskCheck`] 是三环境统一的 pre-trade 风控接缝：paper 的
//! [`crate::sink::PaperSink`] 与 live 的 `axon-worker::live_sink::LiveSink`
//! 都在 `submit` 入口内联调用——返回 `Err(reason)` 即拒单，意图不会到达
//! 撮合 / 网关。原先定义在 `axon-worker::live_sink`（只覆盖 live），接
//! QuantCell B3 风控接线时上移到内核：`PaperSink` 也需要同一个钩子。
//! 回测 [`crate::sink::BacktestSink`] 保持无风控（历史结果一致性优先）。
//!
//! `Send` 约束理由同 [`crate::sink::ExecutionSink`]：pyo3 的 pyclass 强制
//! Send+Sync，实现者不得持有非 Send 数据。

use crate::host::{HostCtx, OrderIntent};

/// 下单前风控检查
///
/// 内联点固定在各 `ExecutionSink::submit` 入口，返回 `Err(reason)` 表示拒单。
pub trait RiskCheck: Send {
    /// 检查一个意图（只读账户视图在 `ctx` 上）
    fn check(&mut self, intent: &OrderIntent, ctx: &HostCtx) -> Result<(), String>;
}

/// 默认放行（无风控配置时用）
#[derive(Debug, Default)]
pub struct AllowAllRisk;

impl RiskCheck for AllowAllRisk {
    fn check(&mut self, _intent: &OrderIntent, _ctx: &HostCtx) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use axon_core::market::Side;
    use axon_core::types::{Instrument, Quantity, SpotInstrument, Symbol};

    use crate::host::OrderSpec;

    fn btc() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        })
    }

    fn submit_intent() -> OrderIntent {
        OrderIntent::Submit(OrderSpec {
            instrument: btc(),
            side: Side::Buy,
            quantity: Quantity::from_f64(1.0),
            price: None,
        })
    }

    /// 拒绝一切的测试桩
    struct RejectAll;

    impl RiskCheck for RejectAll {
        fn check(&mut self, _intent: &OrderIntent, _ctx: &HostCtx) -> Result<(), String> {
            Err("rejected by test".to_string())
        }
    }

    #[test]
    fn allow_all_passes_every_intent() {
        let mut risk = AllowAllRisk;
        risk.check(&submit_intent(), &HostCtx::default())
            .expect("AllowAllRisk 必须放行");
    }

    #[test]
    fn reject_reason_propagates_to_caller() {
        let mut risk = RejectAll;
        let err = risk
            .check(&submit_intent(), &HostCtx::default())
            .expect_err("RejectAll 必须拒绝");
        assert_eq!(err, "rejected by test");
    }
}
