//! PlaceOrderTool:LLM 下单工具
//!
//! 行为按 `SafetyMode` 分支:
//! - `DryRun`(默认):tracing 日志 + 返回 `status="DryRun"` 的 OrderAck,backend 不被调
//! - `TwoPhase`:第一次返回 confirm_token,第二次带相同 token 才真发
//! - `Direct`:直接调 backend,无任何拦截
//!
//! 三种模式都先经过 `RiskLimits` 预检。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::json;
use tracing::info;

use crate::tools::{Tool, ToolError};
use crate::trading::backend::{TradingBackend, TradingError};
use crate::trading::safety::{PendingOrder, RiskLimits, SafetyMode};
use crate::trading::types::{OrderAck, OrderStatus, PlaceOrderArgs};

/// Place order 工具
pub struct PlaceOrderTool {
    /// 交易后端
    backend: Arc<dyn TradingBackend>,
    /// 安全模式
    mode: SafetyMode,
    /// 风控规则
    risk: RiskLimits,
    /// TwoPhase 模式下的待确认订单表(token → PendingOrder)
    pub(super) pending: Mutex<HashMap<String, PendingOrder>>,
}

impl PlaceOrderTool {
    /// 构造(DryRun 为默认安全模式)
    pub fn new(backend: Arc<dyn TradingBackend>, mode: SafetyMode, risk: RiskLimits) -> Self {
        Self {
            backend,
            mode,
            risk,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// 当前安全模式
    pub fn mode(&self) -> SafetyMode {
        self.mode
    }

    /// 调整安全模式(运行时切换 DryRun ↔ Direct 等)
    pub fn set_mode(&mut self, mode: SafetyMode) {
        self.mode = mode;
    }

    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

#[async_trait]
impl Tool for PlaceOrderTool {
    fn name(&self) -> &str {
        "place_order"
    }

    fn description(&self) -> &str {
        "下单工具(高阶语义,extras 字段可透传底层 Order 字段如 client_order_id)"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "symbol": {"type": "string", "description": "交易对,如 BTC-USDT"},
                "side": {"type": "string", "enum": ["Buy", "Sell"]},
                "quantity": {"type": "number", "description": "下单数量"},
                "order_type": {"type": "string", "enum": ["Limit", "Market"], "default": "Limit"},
                "price": {"type": "number", "description": "Limit 单必填"},
                "stop_loss": {"type": "number"},
                "take_profit": {"type": "number"},
                "time_in_force": {"type": "string", "enum": ["GTC", "IOC", "FOK"], "default": "GTC"},
                "extras": {"type": "object", "description": "兜底透传字段,用于 client_order_id 等底层 Order 字段"}
            },
            "required": ["symbol", "side", "quantity"]
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, ToolError> {
        let args: PlaceOrderArgs = serde_json::from_str(arguments)
            .map_err(|e| ToolError::InvalidArguments(format!("JSON 解析失败: {}", e)))?;

        // 1. 风控预检
        self.risk
            .check(&args)
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        // 2. 按模式分支
        match self.mode {
            SafetyMode::DryRun => {
                info!(?args, "[DRY-RUN] place_order would be sent");
                let ack = OrderAck {
                    order_id: "DRY-RUN".into(),
                    symbol: args.symbol,
                    side: args.side,
                    quantity: args.quantity,
                    status: OrderStatus("DryRun".into()),
                    timestamp_ms: Self::now_ms(),
                    confirm_token: None,
                };
                serde_json::to_string(&ack)
                    .map_err(|e| ToolError::ExecutionFailed(format!("序列化失败: {}", e)))
            }
            SafetyMode::Direct => self
                .backend
                .place_order(&args)
                .await
                .and_then(|a| {
                    serde_json::to_string(&a)
                        .map_err(|e| TradingError::Backend(format!("序列化失败: {}", e)))
                })
                .map_err(|e| ToolError::ExecutionFailed(e.to_string())),
            // TwoPhase 完整流程由 task 6 实现
            SafetyMode::TwoPhase => {
                // 第二次提交:LLM 须把第一次返回的 confirm_token 放进
                // PlaceOrderArgs.extras.confirm_token 后再次调用 place_order
                let supplied_token = args
                    .extras
                    .get("confirm_token")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                if let Some(t) = supplied_token {
                    // 第二次提交:从 pending map 取出待确认订单并下单
                    //
                    // 不变量:`pending` 总是以 `token.clone()` 作为 key 插入,
                    // 且 `PendingOrder.token == key`,所以 `remove(&t)` 返回的
                    // value 必然满足 `pending.token == t`,无需再比较。
                    let pending = self
                        .pending
                        .lock()
                        .expect("poisoned")
                        .remove(&t)
                        .ok_or_else(|| {
                            ToolError::ExecutionFailed(format!("未找到待确认订单: {}", t))
                        })?;
                    self.backend
                        .place_order(&pending.args)
                        .await
                        .and_then(|a| {
                            serde_json::to_string(&a)
                                .map_err(|e| TradingError::Backend(format!("序列化失败: {}", e)))
                        })
                        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
                } else {
                    // 第一次提交:暂存 + 返回 token
                    let token = uuid::Uuid::new_v4().to_string();
                    self.pending.lock().expect("poisoned").insert(
                        token.clone(),
                        PendingOrder {
                            args: args.clone(),
                            token: token.clone(),
                        },
                    );
                    let ack = OrderAck {
                        order_id: "PENDING".into(),
                        symbol: args.symbol,
                        side: args.side,
                        quantity: args.quantity,
                        status: OrderStatus("Pending".into()),
                        timestamp_ms: Self::now_ms(),
                        confirm_token: Some(token),
                    };
                    serde_json::to_string(&ack)
                        .map_err(|e| ToolError::ExecutionFailed(format!("序列化失败: {}", e)))
                }
            }
        }
    }
}

// ── 测试 ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trading::mock::{FailureInjector, MockTradingBackend};

    fn args_json(symbol: &str, qty: f64) -> String {
        serde_json::json!({
            "symbol": symbol,
            "side": "Buy",
            "quantity": qty,
            "order_type": "Limit",
            "price": 50_000.0,
        })
        .to_string()
    }

    /// 辅助构造器:替换 mock 后端的 failure_injector
    fn mock_with_failure(m: MockTradingBackend, fi: FailureInjector) -> MockTradingBackend {
        *m.failure_injector.lock().expect("poisoned") = fi;
        m
    }

    #[tokio::test]
    async fn dry_run_does_not_call_backend() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = PlaceOrderTool::new(m.clone(), SafetyMode::DryRun, RiskLimits::permissive());
        let s = tool.execute(&args_json("BTC-USDT", 0.1)).await.unwrap();
        let ack: OrderAck = serde_json::from_str(&s).unwrap();
        assert_eq!(ack.order_id, "DRY-RUN");
        assert_eq!(ack.status.0, "DryRun");
        assert_eq!(m.order_count(), 0); // backend 未被调
    }

    #[tokio::test]
    async fn direct_mode_calls_backend() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = PlaceOrderTool::new(m.clone(), SafetyMode::Direct, RiskLimits::permissive());
        let s = tool.execute(&args_json("BTC-USDT", 0.1)).await.unwrap();
        let ack: OrderAck = serde_json::from_str(&s).unwrap();
        assert_eq!(ack.order_id, "MOCK-1");
        assert_eq!(m.order_count(), 1);
    }

    #[tokio::test]
    async fn invalid_json_returns_invalid_arguments() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = PlaceOrderTool::new(m, SafetyMode::DryRun, RiskLimits::permissive());
        let e = tool.execute("not a json").await.unwrap_err();
        assert!(matches!(e, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn risk_rejection_blocks_backend() {
        let m = Arc::new(MockTradingBackend::new());
        let risk = RiskLimits {
            allowed_symbols: Some(vec!["ETH-USDT".into()]),
            ..Default::default()
        };
        let tool = PlaceOrderTool::new(m.clone(), SafetyMode::Direct, risk);
        let e = tool.execute(&args_json("BTC-USDT", 0.1)).await.unwrap_err();
        assert!(matches!(e, ToolError::ExecutionFailed(_)));
        assert_eq!(m.order_count(), 0);
    }

    #[tokio::test]
    async fn backend_error_propagates() {
        let m = Arc::new(mock_with_failure(
            MockTradingBackend::new(),
            FailureInjector::new().with_place_order_error("outage"),
        ));
        let tool = PlaceOrderTool::new(m, SafetyMode::Direct, RiskLimits::permissive());
        let e = tool.execute(&args_json("BTC-USDT", 0.1)).await.unwrap_err();
        assert!(matches!(e, ToolError::ExecutionFailed(_)));
    }

    #[tokio::test]
    async fn missing_required_field_returns_invalid_arguments() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = PlaceOrderTool::new(m, SafetyMode::DryRun, RiskLimits::permissive());
        // 缺 quantity
        let e = tool
            .execute(r#"{"symbol":"BTC-USDT","side":"Buy"}"#)
            .await
            .unwrap_err();
        assert!(matches!(e, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn name_and_description_and_schema() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = PlaceOrderTool::new(m, SafetyMode::DryRun, RiskLimits::permissive());
        assert_eq!(tool.name(), "place_order");
        assert!(tool.description().contains("下单"));
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["symbol"].is_object());
        assert!(schema["properties"]["extras"].is_object());
    }

    // ── TwoPhase 测试 ──────────────────────────────────────

    #[tokio::test]
    async fn two_phase_first_call_returns_pending_with_token() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = PlaceOrderTool::new(m.clone(), SafetyMode::TwoPhase, RiskLimits::permissive());
        let s = tool.execute(&args_json("BTC-USDT", 0.1)).await.unwrap();
        let ack: OrderAck = serde_json::from_str(&s).unwrap();
        assert_eq!(ack.order_id, "PENDING");
        assert_eq!(ack.status.0, "Pending");
        let token = ack.confirm_token.expect("token 应存在");
        assert_eq!(m.order_count(), 0); // 第一次不真发
        assert!(tool.pending.lock().expect("poisoned").contains_key(&token));
    }

    #[tokio::test]
    async fn two_phase_second_call_with_correct_token_executes() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = PlaceOrderTool::new(m.clone(), SafetyMode::TwoPhase, RiskLimits::permissive());
        let s1 = tool.execute(&args_json("BTC-USDT", 0.1)).await.unwrap();
        let ack1: OrderAck = serde_json::from_str(&s1).unwrap();
        let token = ack1.confirm_token.unwrap();

        // 第二次:把 token 放进 extras
        let args2 = serde_json::json!({
            "symbol": "BTC-USDT",
            "side": "Buy",
            "quantity": 0.1,
            "order_type": "Limit",
            "price": 50_000.0,
            "extras": {"confirm_token": token}
        })
        .to_string();
        let s2 = tool.execute(&args2).await.unwrap();
        let ack2: OrderAck = serde_json::from_str(&s2).unwrap();
        assert_eq!(ack2.order_id, "MOCK-1");
        assert_eq!(m.order_count(), 1);
        // pending 已消费
        assert!(!tool.pending.lock().expect("poisoned").contains_key(&token));
    }

    #[tokio::test]
    async fn two_phase_unknown_token_returns_no_pending() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = PlaceOrderTool::new(m.clone(), SafetyMode::TwoPhase, RiskLimits::permissive());
        let args2 = serde_json::json!({
            "symbol": "BTC-USDT",
            "side": "Buy",
            "quantity": 0.1,
            "order_type": "Limit",
            "price": 50_000.0,
            "extras": {"confirm_token": "fake-token-xxx"}
        })
        .to_string();
        let e = tool.execute(&args2).await.unwrap_err();
        assert!(matches!(e, ToolError::ExecutionFailed(_)));
        assert_eq!(m.order_count(), 0);
    }

    #[tokio::test]
    async fn two_phase_token_consumed_once() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = PlaceOrderTool::new(m.clone(), SafetyMode::TwoPhase, RiskLimits::permissive());
        let s1 = tool.execute(&args_json("BTC-USDT", 0.1)).await.unwrap();
        let token = serde_json::from_str::<OrderAck>(&s1)
            .unwrap()
            .confirm_token
            .unwrap();

        let args2 = serde_json::json!({
            "symbol": "BTC-USDT", "side": "Buy", "quantity": 0.1,
            "order_type": "Limit", "price": 50_000.0,
            "extras": {"confirm_token": token}
        })
        .to_string();
        tool.execute(&args2).await.unwrap();

        // 第三次:token 已被消费
        let e = tool.execute(&args2).await.unwrap_err();
        assert!(matches!(e, ToolError::ExecutionFailed(_)));
    }
}
