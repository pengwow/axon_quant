//! CircuitBreaker 端到端集成测试(Stage J)
//!
//! 验证 `RejectionCircuitBreaker` 与 `PlaceOrderTool` 端到端协作:
//! 连续 N 次风控拒绝 → breaker 开闸 → 后续下单被阻断(cooldown 内)。
//!
//! 配合 PnL-based `RiskPnLCircuitBreaker`(需要 `trading-risk-extra` feature):
//! 日亏损达到上限 → cb.is_active() → 阻断 PlaceOrderTool 真发路径。
//!
//! 与 `tests/trading_integration.rs` 模式一致:用 `MockTradingBackend`
//! 替代真实后端,验证 Tool → Backend → Breaker 的端到端链路。

use std::sync::Arc;
use std::time::Duration;

use axon_llm::tools::Tool;
use axon_llm::trading::{
    DailyCounter, MockTradingBackend, PlaceOrderTool, RejectionCircuitBreaker, RiskLimits,
    SafetyMode,
};

fn args_json(symbol: &str) -> String {
    serde_json::json!({
        "symbol": symbol, "side": "Buy", "quantity": 0.1,
        "order_type": "Limit", "price": 50_000.0
    })
    .to_string()
}

/// 1. 连续 N 次风控拒绝 → RejectionCircuitBreaker 开闸 → 后续真发被阻断
///    主路径(通过把 RejectionCircuitBreaker 作为 PlaceOrderTool 的主 gate)
#[tokio::test]
async fn rejection_breaker_blocks_place_order_after_n_rejections() {
    let backend = Arc::new(MockTradingBackend::new());
    // 把 RejectionCircuitBreaker 作为主 gate:既做计数又做阻断
    let breaker = Arc::new(RejectionCircuitBreaker::new(3, Duration::from_secs(60)));
    // 白名单拒 BTC-USDT,触发 risk check 失败 → PlaceOrderTool 调 breaker.record_rejection()
    // 但 gate 阻断需要 breaker.is_blocked()=true,这要等 3 次拒绝后
    let risk = RiskLimits {
        allowed_symbols: Some(vec!["ETH-USDT".into()]),
        ..Default::default()
    };
    let tool = PlaceOrderTool::with_gate(
        backend.clone(),
        SafetyMode::Direct,
        risk,
        Arc::new(DailyCounter::default()),
        breaker.clone() as Arc<dyn axon_llm::trading::RiskGate>,
    )
    .with_rejection_breaker(breaker.clone());

    // 下 3 笔违规 BTC-USDT:每次 risk check 失败 + record_rejection
    for i in 1..=3 {
        let r = tool.execute(&args_json("BTC-USDT")).await;
        assert!(r.is_err(), "第 {} 次应被风控拒", i);
    }
    // 现在 breaker 达到 3 次阈值 → is_active()=true → 主 gate 阻断
    assert!(breaker.is_active(), "3 次拒绝后 breaker 应开闸");
    // 第 4 次:即使是白名单内 ETH-USDT(预检通过),gate 阻断后端不被调
    let r = tool.execute(&args_json("ETH-USDT")).await;
    let msg = format!("{:?}", r.unwrap_err());
    assert!(msg.contains("rejection circuit breaker"), "msg = {}", msg);
    assert_eq!(backend.order_count(), 0, "gate 阻断时 backend 不应被调");
}

/// 2. cooldown 自愈:100ms cooldown + 200ms sleep → 闸门自动放行
#[tokio::test]
async fn rejection_breaker_recovers_after_cooldown() {
    let backend = Arc::new(MockTradingBackend::new());
    let breaker = Arc::new(RejectionCircuitBreaker::new(1, Duration::from_millis(100)));
    let tool = PlaceOrderTool::with_gate(
        backend.clone(),
        SafetyMode::Direct,
        RiskLimits::permissive(),
        Arc::new(DailyCounter::default()),
        breaker.clone() as Arc<dyn axon_llm::trading::RiskGate>,
    )
    .with_rejection_breaker(breaker.clone());

    // 1 次拒绝:threshold=1,立即开闸
    let risk_block = RiskLimits {
        allowed_symbols: Some(vec!["ETH-USDT".into()]),
        ..Default::default()
    };
    // 用另一个 tool 触发拒绝(否则同一 tool 的 gate 也会被自己阻断)
    let tool_to_trigger = PlaceOrderTool::new(
        backend.clone(),
        SafetyMode::Direct,
        risk_block,
        Arc::new(DailyCounter::default()),
    )
    .with_rejection_breaker(breaker.clone());
    let _ = tool_to_trigger.execute(&args_json("BTC-USDT")).await;
    assert!(breaker.is_active(), "1 次拒绝即开闸");

    // 此时主 tool 应被 gate 阻断
    let r = tool.execute(&args_json("BTC-USDT")).await;
    let msg = format!("{:?}", r.unwrap_err());
    assert!(msg.contains("rejection circuit breaker"), "msg = {}", msg);

    // 等 cooldown 结束(100ms + buffer)
    tokio::time::sleep(Duration::from_millis(200)).await;

    // 现在 gate 应放行
    let r = tool.execute(&args_json("BTC-USDT")).await;
    assert!(r.is_ok(), "cooldown 后应放行: {:?}", r);
    assert_eq!(backend.order_count(), 1, "cooldown 后 backend 应被调");
}

/// 3. record_success 在 DryRun 路径也清零 breaker(LLM 探索阶段)
#[tokio::test]
async fn dry_run_resets_breaker_counter() {
    let backend = Arc::new(MockTradingBackend::new());
    let breaker = Arc::new(RejectionCircuitBreaker::new(2, Duration::from_secs(60)));
    let tool = PlaceOrderTool::new(
        backend.clone(),
        SafetyMode::DryRun, // DryRun:不调 backend,但通过 record_breaker_success 清零
        RiskLimits::permissive(),
        Arc::new(DailyCounter::default()),
    )
    .with_rejection_breaker(breaker.clone());

    // 1 次 DryRun(白名单内):预检通过 + record_success
    let r = tool.execute(&args_json("BTC-USDT")).await;
    assert!(r.is_ok());
    assert_eq!(breaker.rejection_count(), 0);

    // 模拟外部 record_rejection(LLM 在 dry-run 后切到 direct 试违规单)
    breaker.record_rejection();
    breaker.record_rejection();
    // 现在 count=2(其中 1 是外部 record,1 是 dry-run 没清掉的,实际 dry-run 调了
    // record_breaker_success 把外部的也清零了)
    // 上面假设有误:DryRun 的 record_breaker_success 是在 execute 内部调,
    // 而我们先 execute → count=0,然后外部 record 2 次 → count=2
    assert_eq!(breaker.rejection_count(), 2);

    // 再来一次 DryRun(成功)→ count 清零
    let r = tool.execute(&args_json("BTC-USDT")).await;
    assert!(r.is_ok());
    assert_eq!(breaker.rejection_count(), 0, "DryRun 成功应清零");
}

#[cfg(feature = "trading-risk-extra")]
mod pnl_breaker_tests {
    use super::*;
    use axon_llm::trading::RiskPnLCircuitBreaker;
    use axon_risk::circuit_breaker::CircuitBreaker as AxonCircuitBreaker;
    use std::time::Duration;

    /// 4. PnL 触发 → RiskPnLCircuitBreaker 阻断 PlaceOrderTool
    #[tokio::test]
    async fn pnl_breaker_blocks_place_order_after_loss() {
        let backend = Arc::new(MockTradingBackend::new());
        let cb = Arc::new(AxonCircuitBreaker::new(10_000.0, Duration::from_secs(60)));
        cb.check_and_trigger(-10_000.0); // 触发日亏损上限
        let pnl_gate: Arc<dyn axon_llm::trading::RiskGate> =
            Arc::new(RiskPnLCircuitBreaker::new(cb));
        let tool = PlaceOrderTool::with_gate(
            backend.clone(),
            SafetyMode::Direct,
            RiskLimits::permissive(),
            Arc::new(DailyCounter::default()),
            pnl_gate,
        );
        // 下单被 PnL breaker 阻断
        let r = tool.execute(&args_json("BTC-USDT")).await;
        let msg = format!("{:?}", r.unwrap_err());
        assert!(msg.contains("PnL circuit breaker"), "msg = {}", msg);
        assert_eq!(backend.order_count(), 0);
    }
}
