//! 改单工具:按 order_id 修改订单(改 price / quantity / stop_loss / take_profit)。
//!
//! **保留 order_id**,后端负责校验 symbol / side / order_type 与原单一致。
//! 风控:`RiskLimits::check(new_req)`(白名单 + 单笔金额)。
//!
//! 走 `DailyCounter` 但不增加 `max_daily_orders` / `max_daily_cancels` 计数
//! (纯 replace 不算 cancel 也不算 place)。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::tools::{Tool, ToolError};
use crate::trading::backend::{TradingBackend, TradingError};
use crate::trading::metrics::{RiskRule, TradingMetrics};
use crate::trading::safety::RiskLimits;
use crate::trading::types::ReplaceOrderArgs;

/// 改单工具
pub struct ReplaceOrderTool {
    backend: Arc<dyn TradingBackend>,
    risk: RiskLimits,
    /// Stage H:metrics 收集器(默认 `None`,零运行时开销)
    metrics: Option<Arc<TradingMetrics>>,
}

impl ReplaceOrderTool {
    /// 构造改单工具
    pub fn new(backend: Arc<dyn TradingBackend>, risk: RiskLimits) -> Self {
        Self {
            backend,
            risk,
            metrics: None, // Stage H
        }
    }

    /// 启用 metrics 收集(Stage H)
    pub fn with_metrics(mut self, metrics: Arc<TradingMetrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// 埋点:风控拒绝
    fn record_risk_block_metric(&self, err: &TradingError) {
        if let Some(m) = &self.metrics {
            m.record_risk_block(RiskRule::from_err_msg(&err.to_string()), "direct");
        }
    }

    /// 埋点:改单结果
    fn record_replace_metric(&self, status: &str) {
        if let Some(m) = &self.metrics {
            m.record_replace(status, "direct");
        }
    }
}

#[async_trait]
impl Tool for ReplaceOrderTool {
    fn name(&self) -> &str {
        "replace_order"
    }

    fn description(&self) -> &str {
        "改单工具:按 order_id 修改订单(price / quantity / stop_loss / take_profit),保留 order_id。受 RiskLimits::check 预检。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "order_id": { "type": "string", "description": "要修改的订单 ID" },
                "symbol": { "type": "string" },
                "side": { "type": "string", "enum": ["Buy", "Sell"] },
                "quantity": { "type": "number" },
                "order_type": { "type": "string", "enum": ["Limit", "Market"] },
                "price": { "type": ["number", "null"] },
                "stop_loss": { "type": ["number", "null"] },
                "take_profit": { "type": ["number", "null"] },
                "time_in_force": { "type": "string" },
                "extras": { "type": "object" }
            },
            "required": ["order_id", "symbol", "side", "quantity", "order_type"]
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, ToolError> {
        // 1. 解析 arguments
        let args: ReplaceOrderArgs = serde_json::from_str(arguments)
            .map_err(|e| ToolError::InvalidArguments(e.to_string()))?;

        // 2. 取当前持仓(fail-closed:get_positions 错误时拒单,与 PlaceOrderTool 一致)
        let positions = self
            .backend
            .get_positions()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("position fetch failed: {}", e)))?;

        // 3. 风控预检(白名单 / 单笔金额 / max_position_abs)
        //    用 new_req 不用 args,因为 LLM 通过 ReplaceOrderArgs::new_req 传完整新参数
        if let Err(e) = self.risk.check(&args.new_req, &positions) {
            // Stage H:埋点风控拒绝
            self.record_risk_block_metric(&e);
            return Err(ToolError::ExecutionFailed(format!(
                "risk check failed: {}",
                e
            )));
        }

        // 3. 调后端改单
        let ack = match self
            .backend
            .replace_order(&args.order_id, &args.new_req)
            .await
        {
            Ok(a) => a,
            Err(e) => {
                // Stage H:后端失败埋点(status="Error")
                self.record_replace_metric("Error");
                return Err(ToolError::ExecutionFailed(format!(
                    "backend replace failed: {}",
                    e
                )));
            }
        };

        // 4. Stage H:成功埋点
        self.record_replace_metric(&ack.status.0);

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

    /// 1. 正常路径返回 ack,order_id 不变
    #[tokio::test]
    async fn replace_happy_path_returns_ack_with_same_id() {
        let m = Arc::new(MockTradingBackend::new());
        let original = m.place_order(&mk_args()).await.unwrap();
        let tool = ReplaceOrderTool::new(m.clone(), RiskLimits::permissive());

        let args_json = format!(
            r#"{{"order_id":"{}","new_req":{{"symbol":"BTC-USDT","side":"Buy","quantity":0.2,"order_type":"Limit","price":51000.0}}}}"#,
            original.order_id
        );
        let out = tool.execute(&args_json).await.unwrap();
        let parsed: OrderAck = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed.order_id, original.order_id);
        assert_eq!(parsed.quantity, 0.2);
    }

    /// 2. 调后端的 new_req 字段与传入一致
    #[tokio::test]
    async fn replace_keeps_symbol_and_side() {
        let m = Arc::new(MockTradingBackend::new());
        let original = m.place_order(&mk_args()).await.unwrap();
        let tool = ReplaceOrderTool::new(m.clone(), RiskLimits::permissive());

        let args_json = format!(
            r#"{{"order_id":"{}","new_req":{{"symbol":"BTC-USDT","side":"Buy","quantity":0.1,"order_type":"Limit","price":50000.0}}}}"#,
            original.order_id
        );
        tool.execute(&args_json).await.unwrap();
        let orders = m.orders.lock().unwrap();
        let updated = orders
            .iter()
            .find(|o| o.order_id == original.order_id)
            .unwrap();
        assert_eq!(updated.symbol, "BTC-USDT");
        assert_eq!(updated.side, OrderSide::Buy);
    }

    /// 3. new_req.symbol 不在白名单时返回 risk 错误
    #[tokio::test]
    async fn replace_respects_risk_whitelist() {
        let m = Arc::new(MockTradingBackend::new());
        let original = m.place_order(&mk_args()).await.unwrap();
        let tool = ReplaceOrderTool::new(
            m.clone(),
            RiskLimits {
                allowed_symbols: Some(vec!["ETH-USDT".into()]),
                ..Default::default()
            },
        );

        let args_json = format!(
            r#"{{"order_id":"{}","new_req":{{"symbol":"BTC-USDT","side":"Buy","quantity":0.1,"order_type":"Limit","price":50000.0}}}}"#,
            original.order_id
        );
        let e = tool.execute(&args_json).await.unwrap_err();
        assert!(format!("{:?}", e).contains("risk check failed"));
    }

    /// 4. new_req 单笔金额超限时返回 risk 错误
    #[tokio::test]
    async fn replace_respects_risk_notional() {
        let m = Arc::new(MockTradingBackend::new());
        let original = m.place_order(&mk_args()).await.unwrap();
        let tool = ReplaceOrderTool::new(
            m.clone(),
            RiskLimits {
                max_order_notional: Some(1_000.0),
                ..Default::default()
            },
        );

        // 0.5 * 50_000 = 25_000 > 1_000
        let args_json = format!(
            r#"{{"order_id":"{}","new_req":{{"symbol":"BTC-USDT","side":"Buy","quantity":0.5,"order_type":"Limit","price":50000.0}}}}"#,
            original.order_id
        );
        let e = tool.execute(&args_json).await.unwrap_err();
        assert!(format!("{:?}", e).contains("risk check failed"));
    }

    /// 5. backend 错误透传
    #[tokio::test]
    async fn replace_propagates_backend_error() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = ReplaceOrderTool::new(m.clone(), RiskLimits::permissive());
        let args_json = r#"{"order_id":"DOES-NOT-EXIST","new_req":{"symbol":"BTC-USDT","side":"Buy","quantity":0.1,"order_type":"Limit","price":50000.0}}"#;
        let e = tool.execute(args_json).await.unwrap_err();
        assert!(format!("{:?}", e).contains("backend replace failed"));
    }

    /// 6. arguments 不是合法 JSON
    #[tokio::test]
    async fn replace_invalid_json_returns_invalid_arguments() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = ReplaceOrderTool::new(m.clone(), RiskLimits::permissive());
        let e = tool.execute("not json").await.unwrap_err();
        assert!(matches!(e, ToolError::InvalidArguments(_)));
    }

    // ── max_position_abs 测试(Stage F)─────────────────────

    /// max_position_abs:replace 改 quantity 扩大持仓 → ToolError,backend 不被调
    #[tokio::test]
    async fn replace_with_larger_quantity_blocks() {
        use crate::trading::place_order_tool::PlaceOrderTool;

        let m = Arc::new(MockTradingBackend::new());
        let risk = RiskLimits {
            max_position_abs: Some(0.5),
            ..Default::default()
        };
        // 第一步:先下一个 Buy 0.1 BTC,获取 order_id
        let place_tool = PlaceOrderTool::new(
            m.clone(),
            SafetyMode::Direct,
            RiskLimits::permissive(),
            Arc::new(DailyCounter::default()),
        );
        let place_args = serde_json::json!({
            "symbol": "BTC-USDT", "side": "Buy", "quantity": 0.1,
            "order_type": "Limit", "price": 50_000.0
        })
        .to_string();
        let place_out = place_tool.execute(&place_args).await.unwrap();
        let place_ack: OrderAck = serde_json::from_str(&place_out).unwrap();
        assert_eq!(place_ack.order_id, "MOCK-1");
        assert_eq!(m.order_count(), 1);

        // 第二步:用 replace 改 quantity 到 0.5(持仓 0.1,projected=0.6 > 0.5)
        let replace_tool = ReplaceOrderTool::new(m.clone(), risk);
        let replace_args = serde_json::json!({
            "order_id": place_ack.order_id,
            "new_req": {
                "symbol": "BTC-USDT", "side": "Buy", "quantity": 0.5,
                "order_type": "Limit", "price": 50_000.0
            }
        })
        .to_string();
        let e = replace_tool.execute(&replace_args).await.unwrap_err();
        assert!(matches!(e, ToolError::ExecutionFailed(_)));
        // backend 未被二次调
        assert_eq!(m.order_count(), 1);
    }

    // ── Stage H: metrics 集成测试 ──

    /// 成功改单埋点 trading_replaces_total{status, mode}
    #[tokio::test]
    async fn replace_records_metrics_on_success() {
        use crate::trading::metrics::TradingMetrics;
        let m = Arc::new(MockTradingBackend::new());
        let original = m.place_order(&mk_args()).await.unwrap();
        let metrics = Arc::new(TradingMetrics::new());
        let tool = ReplaceOrderTool::new(m.clone(), RiskLimits::permissive())
            .with_metrics(metrics.clone());
        let args_json = format!(
            r#"{{"order_id":"{}","new_req":{{"symbol":"BTC-USDT","side":"Buy","quantity":0.2,"order_type":"Limit","price":51000.0}}}}"#,
            original.order_id
        );
        tool.execute(&args_json).await.unwrap();
        let snap = metrics.snapshot_filtered("trading_replaces_total");
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].value, 1.0);
        assert_eq!(snap[0].labels.get("status"), Some(&"Replaced".to_string()));
        assert_eq!(snap[0].labels.get("mode"), Some(&"direct".to_string()));
    }

    /// 风控拒绝埋点 trading_risk_blocks_total{rule, mode}
    #[tokio::test]
    async fn replace_records_risk_block_metric() {
        use crate::trading::metrics::TradingMetrics;
        let m = Arc::new(MockTradingBackend::new());
        let original = m.place_order(&mk_args()).await.unwrap();
        let metrics = Arc::new(TradingMetrics::new());
        let risk = RiskLimits {
            allowed_symbols: Some(vec!["ETH-USDT".into()]),
            ..Default::default()
        };
        let tool = ReplaceOrderTool::new(m.clone(), risk).with_metrics(metrics.clone());
        let args_json = format!(
            r#"{{"order_id":"{}","new_req":{{"symbol":"BTC-USDT","side":"Buy","quantity":0.1,"order_type":"Limit","price":50000.0}}}}"#,
            original.order_id
        );
        let _ = tool.execute(&args_json).await;
        let snap = metrics.snapshot_filtered("trading_risk_blocks_total");
        assert_eq!(snap.len(), 1);
        assert_eq!(
            snap[0].labels.get("rule"),
            Some(&"allowed_symbols".to_string())
        );
    }

    /// 后端失败埋点 trading_replaces_total{status="Error"}
    #[tokio::test]
    async fn replace_records_backend_error_metric() {
        use crate::trading::metrics::TradingMetrics;
        let m = Arc::new(MockTradingBackend::new());
        let metrics = Arc::new(TradingMetrics::new());
        let tool = ReplaceOrderTool::new(m.clone(), RiskLimits::permissive())
            .with_metrics(metrics.clone());
        let args_json = r#"{"order_id":"DOES-NOT-EXIST","new_req":{"symbol":"BTC-USDT","side":"Buy","quantity":0.1,"order_type":"Limit","price":50000.0}}"#;
        let _ = tool.execute(args_json).await;
        let snap = metrics.snapshot_filtered("trading_replaces_total");
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].value, 1.0);
        assert_eq!(snap[0].labels.get("status"), Some(&"Error".to_string()));
    }

    /// 默认 metrics=None 时,执行不 panic
    #[tokio::test]
    async fn replace_without_metrics_does_not_panic() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = ReplaceOrderTool::new(m.clone(), RiskLimits::permissive());
        let out = tool
            .execute(
                r#"{"order_id":"DOES-NOT-EXIST","new_req":{"symbol":"BTC-USDT","side":"Buy","quantity":0.1,"order_type":"Limit","price":50000.0}}"#,
            )
            .await;
        // 错误路径(未知 order_id),但 metrics=None 不应 panic
        assert!(out.is_err());
    }
}
