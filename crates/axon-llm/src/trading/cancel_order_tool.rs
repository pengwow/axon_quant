//! 撤单工具:按 order_id 撤销未成交订单。
//!
//! 风控:受 `RiskLimits::max_daily_cancels` 限制(进程内计数,重启清零)。
//! 走 `DailyCounter::increment_and_check`,与 `max_daily_orders` 模式一致。
//!
//! 不走 `RiskGate` 闸门(Stage D 闸门为下单专用)。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::tools::{Tool, ToolError};
use crate::trading::backend::TradingBackend;
use crate::trading::metrics::{RiskRule, TradingMetrics};
use crate::trading::safety::{DailyCounter, RiskLimits};
use crate::trading::types::CancelOrderArgs;

/// 撤单工具
pub struct CancelOrderTool {
    backend: Arc<dyn TradingBackend>,
    risk: RiskLimits,
    daily: Arc<DailyCounter>,
    /// Stage H:metrics 收集器(默认 `None`,零运行时开销)
    metrics: Option<Arc<TradingMetrics>>,
}

impl CancelOrderTool {
    /// 构造撤单工具
    pub fn new(
        backend: Arc<dyn TradingBackend>,
        risk: RiskLimits,
        daily: Arc<DailyCounter>,
    ) -> Self {
        Self {
            backend,
            risk,
            daily,
            metrics: None, // Stage H
        }
    }

    /// 启用 metrics 收集(Stage H)
    pub fn with_metrics(mut self, metrics: Arc<TradingMetrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// 埋点:风控拒绝(单日撤单超限)
    fn record_risk_block_metric(&self) {
        if let Some(m) = &self.metrics {
            m.record_risk_block(RiskRule::MaxDailyCancels, "direct");
        }
    }

    /// 埋点:撤单结果
    fn record_cancel_metric(&self, status: &str) {
        if let Some(m) = &self.metrics {
            m.record_cancel(status, "direct");
        }
    }
}

#[async_trait]
impl Tool for CancelOrderTool {
    fn name(&self) -> &str {
        "cancel_order"
    }

    fn description(&self) -> &str {
        "撤单工具:按 order_id 撤销未成交订单。受 RiskLimits::max_daily_cancels 限制。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "order_id": {
                    "type": "string",
                    "description": "要撤销的订单 ID"
                }
            },
            "required": ["order_id"]
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, ToolError> {
        // 1. 解析 arguments
        let args: CancelOrderArgs = serde_json::from_str(arguments)
            .map_err(|e| ToolError::InvalidArguments(e.to_string()))?;

        // 2. 撤单风控:单日撤单次数
        if let Some(max) = self.risk.max_daily_cancels
            && let Err(e) = self.daily.increment_and_check(max)
        {
            // Stage H:单日撤单超限埋点
            self.record_risk_block_metric();
            return Err(ToolError::ExecutionFailed(format!(
                "risk check failed: {}",
                e
            )));
        }

        // 3. 调后端撤单
        let ack = match self.backend.cancel_order(&args.order_id).await {
            Ok(a) => a,
            Err(e) => {
                // Stage H:后端失败埋点(status="Error")
                self.record_cancel_metric("Error");
                return Err(ToolError::ExecutionFailed(format!(
                    "backend cancel failed: {}",
                    e
                )));
            }
        };

        // 4. Stage H:成功埋点
        self.record_cancel_metric(&ack.status.0);

        // 5. 序列化回执
        serde_json::to_string(&ack)
            .map_err(|e| ToolError::ExecutionFailed(format!("serialize ack: {}", e)))
    }
}

// ── 测试 ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trading::mock::MockTradingBackend;
    use crate::trading::safety::{DailyCounter, SafetyMode};
    use crate::trading::types::{OrderAck, OrderKind, OrderSide, PlaceOrderArgs, TimeInForce};

    fn mk_args() -> PlaceOrderArgs {
        PlaceOrderArgs {
            symbol: "BTC-USDT".into(),
            side: OrderSide::Buy,
            quantity: 0.05,
            order_type: OrderKind::Limit,
            price: Some(50_000.0),
            stop_loss: None,
            take_profit: None,
            time_in_force: TimeInForce::GTC,
            extras: serde_json::Value::Null,
        }
    }

    /// 1. 正常路径返回 ack JSON
    #[tokio::test]
    async fn cancel_happy_path_returns_ack() {
        let m = Arc::new(MockTradingBackend::new());
        let ack = m.place_order(&mk_args()).await.unwrap();
        let tool = CancelOrderTool::new(
            m.clone(),
            RiskLimits::permissive(),
            Arc::new(DailyCounter::default()),
        );
        let out = tool
            .execute(&format!(r#"{{"order_id":"{}"}}"#, ack.order_id))
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["order_id"], ack.order_id);
        assert_eq!(parsed["status"], "Cancelled");
    }

    /// 2. 受 max_daily_cancels 限制:第 N+1 次返回 risk 错误
    #[tokio::test]
    async fn cancel_respects_max_daily_cancels() {
        let m = Arc::new(MockTradingBackend::new());
        // 下 3 笔,准备撤 3 笔
        let a1 = m.place_order(&mk_args()).await.unwrap();
        let a2 = m.place_order(&mk_args()).await.unwrap();
        let a3 = m.place_order(&mk_args()).await.unwrap();
        let tool = CancelOrderTool::new(
            m.clone(),
            RiskLimits {
                max_daily_cancels: Some(2),
                ..Default::default()
            },
            Arc::new(DailyCounter::default()),
        );
        // 前 2 次成功
        tool.execute(&format!(r#"{{"order_id":"{}"}}"#, a1.order_id))
            .await
            .unwrap();
        tool.execute(&format!(r#"{{"order_id":"{}"}}"#, a2.order_id))
            .await
            .unwrap();
        // 第 3 次超限
        let e = tool
            .execute(&format!(r#"{{"order_id":"{}"}}"#, a3.order_id))
            .await
            .unwrap_err();
        assert!(format!("{:?}", e).contains("risk check failed"));
    }

    /// 3. backend 错误透传(未知 ID)
    #[tokio::test]
    async fn cancel_propagates_backend_error() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = CancelOrderTool::new(
            m.clone(),
            RiskLimits::permissive(),
            Arc::new(DailyCounter::default()),
        );
        let e = tool
            .execute(r#"{"order_id":"DOES-NOT-EXIST"}"#)
            .await
            .unwrap_err();
        assert!(format!("{:?}", e).contains("backend cancel failed"));
    }

    /// 4. arguments 不是合法 JSON
    #[tokio::test]
    async fn cancel_invalid_json_returns_invalid_arguments() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = CancelOrderTool::new(
            m.clone(),
            RiskLimits::permissive(),
            Arc::new(DailyCounter::default()),
        );
        let e = tool.execute("not json").await.unwrap_err();
        assert!(matches!(e, ToolError::InvalidArguments(_)));
    }

    /// 5. 返回的 ack.order_id 与请求一致
    #[tokio::test]
    async fn cancel_unchanged_id() {
        let m = Arc::new(MockTradingBackend::new());
        let ack = m.place_order(&mk_args()).await.unwrap();
        let tool = CancelOrderTool::new(
            m.clone(),
            RiskLimits::permissive(),
            Arc::new(DailyCounter::default()),
        );
        let out = tool
            .execute(&format!(r#"{{"order_id":"{}"}}"#, ack.order_id))
            .await
            .unwrap();
        let parsed: OrderAck = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed.order_id, ack.order_id);
    }

    /// 6. RiskLimits.max_daily_cancels=None 时不报错(无限)
    #[tokio::test]
    async fn cancel_no_daily_counter_works() {
        let m = Arc::new(MockTradingBackend::new());
        let a1 = m.place_order(&mk_args()).await.unwrap();
        let a2 = m.place_order(&mk_args()).await.unwrap();
        let a3 = m.place_order(&mk_args()).await.unwrap();
        let tool = CancelOrderTool::new(
            m.clone(),
            RiskLimits::permissive(),
            Arc::new(DailyCounter::default()),
        );
        // 撤 3 笔全成功
        tool.execute(&format!(r#"{{"order_id":"{}"}}"#, a1.order_id))
            .await
            .unwrap();
        tool.execute(&format!(r#"{{"order_id":"{}"}}"#, a2.order_id))
            .await
            .unwrap();
        tool.execute(&format!(r#"{{"order_id":"{}"}}"#, a3.order_id))
            .await
            .unwrap();
        assert_eq!(*m.cancel_count.lock().unwrap(), 3);
    }

    /// 7. 撤单后 mock.cancel_count += 1
    #[tokio::test]
    async fn cancel_records_into_mock_count() {
        let m = Arc::new(MockTradingBackend::new());
        let ack = m.place_order(&mk_args()).await.unwrap();
        let tool = CancelOrderTool::new(
            m.clone(),
            RiskLimits::permissive(),
            Arc::new(DailyCounter::default()),
        );
        tool.execute(&format!(r#"{{"order_id":"{}"}}"#, ack.order_id))
            .await
            .unwrap();
        assert_eq!(*m.cancel_count.lock().unwrap(), 1);
    }

    // ── max_position_abs 测试(Stage F)─────────────────────

    /// max_position_abs:cancel 不走位置检查(极严 max_position_abs 也能 cancel)
    #[tokio::test]
    async fn cancel_does_not_check_max_position_abs() {
        use crate::trading::place_order_tool::PlaceOrderTool;

        let m = Arc::new(MockTradingBackend::new());
        let place_tool = PlaceOrderTool::new(
            m.clone(),
            SafetyMode::Direct,
            RiskLimits::permissive(),
            Arc::new(DailyCounter::default()),
        );
        // 下个 Buy 0.1
        let place_args = serde_json::json!({
            "symbol": "BTC-USDT", "side": "Buy", "quantity": 0.1,
            "order_type": "Limit", "price": 50_000.0
        })
        .to_string();
        let place_out = place_tool.execute(&place_args).await.unwrap();
        let place_ack: OrderAck = serde_json::from_str(&place_out).unwrap();

        // 用极严 max_position_abs=0.001 调 cancel
        let risk = RiskLimits {
            max_position_abs: Some(0.001),
            ..Default::default()
        };
        let cancel_tool = CancelOrderTool::new(m.clone(), risk, Arc::new(DailyCounter::default()));
        let cancel_args = serde_json::json!({"order_id": place_ack.order_id}).to_string();
        // 期望成功 cancel(cancel 不查 max_position_abs)
        let cancel_out = cancel_tool.execute(&cancel_args).await.unwrap();
        let cancel_ack: OrderAck = serde_json::from_str(&cancel_out).unwrap();
        assert_eq!(cancel_ack.status.0, "Cancelled");
    }
}
