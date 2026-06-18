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
use std::time::Instant;

use async_trait::async_trait;
use serde_json::json;
use tracing::info;

use crate::tools::{Tool, ToolError};
use crate::trading::backend::{TradingBackend, TradingError};
use crate::trading::metrics::{RiskRule, TradingMetrics};
use crate::trading::safety::{
    AlwaysOpenGate, DailyCounter, PendingOrder, RiskGate, RiskLimits, SafetyMode,
};
use crate::trading::types::{OrderAck, OrderSide, OrderStatus, PlaceOrderArgs};

/// Place order 工具
pub struct PlaceOrderTool {
    /// 交易后端
    backend: Arc<dyn TradingBackend>,
    /// 安全模式
    mode: SafetyMode,
    /// 风控规则
    risk: RiskLimits,
    /// 进程内单日订单计数器(用于 `max_daily_orders` 规则)
    daily: Arc<DailyCounter>,
    /// 风控闸门(Stage D / Stage J 简化版)
    ///
    /// 在 TwoPhase 第二次 / Direct 真发订单前调用 `is_blocked()`,
    /// 返回 `Some(reason)` 时阻断下单并返回 `TradingError::RiskRejected`。
    /// DryRun 不调用闸门(允许 LLM 任意次 dry-run 探索)。
    /// 默认 `AlwaysOpenGate`(永远放行),保持向后兼容。
    gate: Arc<dyn RiskGate>,
    /// TwoPhase 模式下的待确认订单表(token → PendingOrder)
    pub(super) pending: Mutex<HashMap<String, PendingOrder>>,
    /// Stage H:metrics 收集器(默认 `None`,零运行时开销)
    metrics: Option<Arc<TradingMetrics>>,
}

impl PlaceOrderTool {
    /// 构造(DryRun 为默认安全模式)
    ///
    /// `daily` 由调用方共享(允许多个 tool 共享同一计数器),
    /// 即使 `risk.max_daily_orders == None` 也会持续计数(便于 observability)。
    ///
    /// 风控闸门使用 `AlwaysOpenGate`(永远放行),保持 Stage D 之前的
    /// 行为完全一致。如需接入熔断器,使用 [`PlaceOrderTool::with_gate`]。
    pub fn new(
        backend: Arc<dyn TradingBackend>,
        mode: SafetyMode,
        risk: RiskLimits,
        daily: Arc<DailyCounter>,
    ) -> Self {
        Self {
            backend,
            mode,
            risk,
            daily,
            gate: Arc::new(AlwaysOpenGate),
            pending: Mutex::new(HashMap::new()),
            metrics: None, // Stage H:默认无 metrics
        }
    }

    /// 构造(带风控闸门,Stage D 新增)
    ///
    /// 与 [`PlaceOrderTool::new`] 行为一致,但允许指定自定义 `RiskGate`。
    /// 真发路径(TwoPhase 第二次 / Direct)会在下单前调用 `gate.is_blocked()`。
    pub fn with_gate(
        backend: Arc<dyn TradingBackend>,
        mode: SafetyMode,
        risk: RiskLimits,
        daily: Arc<DailyCounter>,
        gate: Arc<dyn RiskGate>,
    ) -> Self {
        Self {
            backend,
            mode,
            risk,
            daily,
            gate,
            pending: Mutex::new(HashMap::new()),
            metrics: None, // Stage H:默认无 metrics
        }
    }

    /// 启用 metrics 收集(Stage H)
    ///
    /// 链式构造。`metrics = None`(默认)时所有 `record_*` 调用跳过,
    /// 运行时单分支预测开销近零。
    pub fn with_metrics(mut self, metrics: Arc<TradingMetrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// 当前安全模式 → 静态字符串 label(用于 metrics tag)
    fn mode_str(&self) -> &'static str {
        match self.mode {
            SafetyMode::DryRun => "dry_run",
            SafetyMode::Direct => "direct",
            SafetyMode::TwoPhase => "two_phase",
        }
    }

    /// OrderSide → 静态字符串 label
    fn side_str(side: OrderSide) -> &'static str {
        match side {
            OrderSide::Buy => "Buy",
            OrderSide::Sell => "Sell",
        }
    }

    /// 镜像 DailyCounter 当前计数(Stage H metrics 用)
    fn set_daily_metric(&self) {
        if let Some(m) = &self.metrics {
            m.set_daily_orders_count(self.daily.today_count() as f64);
        }
    }

    /// 埋点:风控拒绝
    fn record_risk_block_metric(&self, err: &TradingError) {
        if let Some(m) = &self.metrics {
            m.record_risk_block(RiskRule::from_err_msg(&err.to_string()), self.mode_str());
        }
    }

    /// 埋点:风控闸门阻断
    fn record_gate_block_metric(&self) {
        if let Some(m) = &self.metrics {
            m.record_gate_block(self.mode_str());
        }
    }

    /// 埋点:下单结果(成功 / 失败统一入口)
    fn record_order_metric(&self, symbol: &str, side: OrderSide, status: &str, latency_ns: u64) {
        if let Some(m) = &self.metrics {
            m.record_order(
                symbol,
                Self::side_str(side),
                status,
                self.mode_str(),
                latency_ns,
            );
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

    /// 运行时切换风控闸门(Stage D 新增)
    ///
    /// 用于在连续亏损后启用熔断器,或在 LLM agent 完成初期探索后切换到严格闸门。
    /// 切换立即生效,下一次真发路径调用即使用新闸门。
    pub fn set_gate(&mut self, gate: Arc<dyn RiskGate>) {
        self.gate = gate;
    }

    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    /// 单日订单计数预检:仅在 `risk.max_daily_orders` 配置时启用
    fn check_daily(&self) -> Result<(), ToolError> {
        if let Some(max) = self.risk.max_daily_orders {
            self.daily
                .increment_and_check(max)
                .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        }
        Ok(())
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
        let start = Instant::now();
        let args: PlaceOrderArgs = serde_json::from_str(arguments)
            .map_err(|e| ToolError::InvalidArguments(format!("JSON 解析失败: {}", e)))?;

        // 1. 取当前持仓(fail-closed:get_positions 错误时拒单)
        //    与 Stage F 一致:三模式 DryRun/Direct/TwoPhase 统一加位置预检,
        //    LLM agent 在 dry-run 阶段就感知"超过持仓上限"信号。
        let positions = self
            .backend
            .get_positions()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("position fetch failed: {}", e)))?;

        // 2. 风控预检(白名单 / 单笔金额 / max_position_abs)
        if let Err(e) = self.risk.check(&args, &positions) {
            // Stage H:埋点风控拒绝
            self.record_risk_block_metric(&e);
            return Err(ToolError::ExecutionFailed(e.to_string()));
        }

        // 3. 按模式分支
        match self.mode {
            SafetyMode::DryRun => {
                info!(?args, "[DRY-RUN] place_order would be sent");
                let ack = OrderAck {
                    order_id: "DRY-RUN".into(),
                    symbol: args.symbol.clone(),
                    side: args.side,
                    quantity: args.quantity,
                    status: OrderStatus("DryRun".into()),
                    timestamp_ms: Self::now_ms(),
                    confirm_token: None,
                };
                // Stage H:DryRun 也埋点(status="DryRun"),便于观测 dry-run 比例
                self.record_order_metric(
                    &ack.symbol,
                    ack.side,
                    "DryRun",
                    start.elapsed().as_nanos() as u64,
                );
                serde_json::to_string(&ack)
                    .map_err(|e| ToolError::ExecutionFailed(format!("序列化失败: {}", e)))
            }
            SafetyMode::Direct => {
                // 真发前做单日计数检查
                if let Err(e) = self.check_daily() {
                    // Stage H:单日超限属风控
                    self.record_risk_block_metric(&TradingError::RiskRejected(e.to_string()));
                    return Err(e);
                }
                // Stage H:镜像当日计数
                self.set_daily_metric();
                // Stage D:闸门检查(DryRun 不检查,Direct / TwoPhase 真发前检查)
                if let Some(reason) = self.gate.is_blocked() {
                    self.record_gate_block_metric();
                    return Err(ToolError::ExecutionFailed(format!(
                        "gate blocked: {}",
                        reason
                    )));
                }
                match self.backend.place_order(&args).await {
                    Ok(ack) => {
                        let status = ack.status.0.clone();
                        let out = serde_json::to_string(&ack).map_err(|e| {
                            ToolError::ExecutionFailed(format!("序列化失败: {}", e))
                        })?;
                        self.record_order_metric(
                            &ack.symbol,
                            ack.side,
                            &status,
                            start.elapsed().as_nanos() as u64,
                        );
                        Ok(out)
                    }
                    Err(e) => {
                        self.record_order_metric(
                            &args.symbol,
                            args.side,
                            "Error",
                            start.elapsed().as_nanos() as u64,
                        );
                        Err(ToolError::ExecutionFailed(e.to_string()))
                    }
                }
            }
            SafetyMode::TwoPhase => {
                let supplied_token = args
                    .extras
                    .get("confirm_token")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                if let Some(t) = supplied_token {
                    // 第二次提交:真发前做单日计数检查
                    if let Err(e) = self.check_daily() {
                        self.record_risk_block_metric(&TradingError::RiskRejected(e.to_string()));
                        return Err(e);
                    }
                    self.set_daily_metric();
                    // Stage D:闸门检查(仅真发路径)
                    if let Some(reason) = self.gate.is_blocked() {
                        self.record_gate_block_metric();
                        return Err(ToolError::ExecutionFailed(format!(
                            "gate blocked: {}",
                            reason
                        )));
                    }
                    let pending = self
                        .pending
                        .lock()
                        .expect("poisoned")
                        .remove(&t)
                        .ok_or_else(|| {
                            ToolError::ExecutionFailed(format!("未找到待确认订单: {}", t))
                        })?;
                    match self.backend.place_order(&pending.args).await {
                        Ok(ack) => {
                            let status = ack.status.0.clone();
                            let out = serde_json::to_string(&ack).map_err(|e| {
                                ToolError::ExecutionFailed(format!("序列化失败: {}", e))
                            })?;
                            self.record_order_metric(
                                &ack.symbol,
                                ack.side,
                                &status,
                                start.elapsed().as_nanos() as u64,
                            );
                            Ok(out)
                        }
                        Err(e) => {
                            self.record_order_metric(
                                &pending.args.symbol,
                                pending.args.side,
                                "Error",
                                start.elapsed().as_nanos() as u64,
                            );
                            Err(ToolError::ExecutionFailed(e.to_string()))
                        }
                    }
                } else {
                    // 第一次提交:仅暂存,不计数(尚未真发)
                    // 注意:第一次提交也不检查闸门,允许 LLM 发起 confirm 流程
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
                        symbol: args.symbol.clone(),
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

    fn daily() -> Arc<DailyCounter> {
        Arc::new(DailyCounter::default())
    }

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
        let tool = PlaceOrderTool::new(
            m.clone(),
            SafetyMode::DryRun,
            RiskLimits::permissive(),
            daily(),
        );
        let s = tool.execute(&args_json("BTC-USDT", 0.1)).await.unwrap();
        let ack: OrderAck = serde_json::from_str(&s).unwrap();
        assert_eq!(ack.order_id, "DRY-RUN");
        assert_eq!(ack.status.0, "DryRun");
        assert_eq!(m.order_count(), 0); // backend 未被调
    }

    #[tokio::test]
    async fn direct_mode_calls_backend() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = PlaceOrderTool::new(
            m.clone(),
            SafetyMode::Direct,
            RiskLimits::permissive(),
            daily(),
        );
        let s = tool.execute(&args_json("BTC-USDT", 0.1)).await.unwrap();
        let ack: OrderAck = serde_json::from_str(&s).unwrap();
        assert_eq!(ack.order_id, "MOCK-1");
        assert_eq!(m.order_count(), 1);
    }

    #[tokio::test]
    async fn invalid_json_returns_invalid_arguments() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = PlaceOrderTool::new(m, SafetyMode::DryRun, RiskLimits::permissive(), daily());
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
        let tool = PlaceOrderTool::new(m.clone(), SafetyMode::Direct, risk, daily());
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
        let tool = PlaceOrderTool::new(m, SafetyMode::Direct, RiskLimits::permissive(), daily());
        let e = tool.execute(&args_json("BTC-USDT", 0.1)).await.unwrap_err();
        assert!(matches!(e, ToolError::ExecutionFailed(_)));
    }

    #[tokio::test]
    async fn missing_required_field_returns_invalid_arguments() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = PlaceOrderTool::new(m, SafetyMode::DryRun, RiskLimits::permissive(), daily());
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
        let tool = PlaceOrderTool::new(m, SafetyMode::DryRun, RiskLimits::permissive(), daily());
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
        let tool = PlaceOrderTool::new(
            m.clone(),
            SafetyMode::TwoPhase,
            RiskLimits::permissive(),
            daily(),
        );
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
        let tool = PlaceOrderTool::new(
            m.clone(),
            SafetyMode::TwoPhase,
            RiskLimits::permissive(),
            daily(),
        );
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
        let tool = PlaceOrderTool::new(
            m.clone(),
            SafetyMode::TwoPhase,
            RiskLimits::permissive(),
            daily(),
        );
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
        let tool = PlaceOrderTool::new(
            m.clone(),
            SafetyMode::TwoPhase,
            RiskLimits::permissive(),
            daily(),
        );
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

    /// 验证 max_daily_orders 限制:超过阈值后直接拒绝
    #[tokio::test]
    async fn max_daily_orders_blocks_excess() {
        let m = Arc::new(MockTradingBackend::new());
        let risk = RiskLimits {
            max_daily_orders: Some(2),
            ..Default::default()
        };
        let tool = PlaceOrderTool::new(m.clone(), SafetyMode::Direct, risk, daily());

        tool.execute(&args_json("BTC-USDT", 0.01)).await.unwrap();
        tool.execute(&args_json("BTC-USDT", 0.01)).await.unwrap();
        assert_eq!(m.order_count(), 2);

        let e = tool
            .execute(&args_json("BTC-USDT", 0.01))
            .await
            .unwrap_err();
        assert!(matches!(e, ToolError::ExecutionFailed(_)));
        assert_eq!(m.order_count(), 2); // 第三次被风控拦截
    }

    /// DryRun 不计入每日订单数
    #[tokio::test]
    async fn dry_run_does_not_consume_daily_quota() {
        let m = Arc::new(MockTradingBackend::new());
        let risk = RiskLimits {
            max_daily_orders: Some(1),
            ..Default::default()
        };
        let tool = PlaceOrderTool::new(m.clone(), SafetyMode::DryRun, risk, daily());
        // DryRun 多次,但不消耗每日计数
        for _ in 0..3 {
            tool.execute(&args_json("BTC-USDT", 0.01)).await.unwrap();
        }
        assert_eq!(m.order_count(), 0);
    }

    // ── RiskGate 测试(Stage D)─────────────────────────────

    /// 测试用阻断闸门(返回固定 reason)
    struct BlockedGate {
        reason: String,
    }
    impl RiskGate for BlockedGate {
        fn is_blocked(&self) -> Option<String> {
            Some(self.reason.clone())
        }
    }

    /// Direct 模式:闸门放行 → 正常下单
    #[tokio::test]
    async fn gate_open_lets_order_through() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = PlaceOrderTool::with_gate(
            m.clone(),
            SafetyMode::Direct,
            RiskLimits::permissive(),
            daily(),
            Arc::new(AlwaysOpenGate),
        );
        let s = tool.execute(&args_json("BTC-USDT", 0.1)).await.unwrap();
        let ack: OrderAck = serde_json::from_str(&s).unwrap();
        assert_eq!(ack.order_id, "MOCK-1");
        assert_eq!(m.order_count(), 1);
    }

    /// Direct 模式:闸门阻断 → 返回 ToolError::ExecutionFailed,backend 不被调
    #[tokio::test]
    async fn gate_blocked_direct_mode() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = PlaceOrderTool::with_gate(
            m.clone(),
            SafetyMode::Direct,
            RiskLimits::permissive(),
            daily(),
            Arc::new(BlockedGate {
                reason: "circuit breaker open".into(),
            }),
        );
        let e = tool.execute(&args_json("BTC-USDT", 0.1)).await.unwrap_err();
        assert!(matches!(e, ToolError::ExecutionFailed(_)));
        // reason 包含阻断原因
        let msg = format!("{:?}", e);
        assert!(msg.contains("circuit breaker open"), "msg = {}", msg);
        // backend 未被调
        assert_eq!(m.order_count(), 0);
    }

    /// TwoPhase 第二次提交被闸门阻断
    #[tokio::test]
    async fn gate_blocked_two_phase_second() {
        let m = Arc::new(MockTradingBackend::new());
        // 第一次先 open gate(AlwaysOpenGate)→ 拿到 token
        let tool = Arc::new(tokio::sync::Mutex::new(PlaceOrderTool::with_gate(
            m.clone(),
            SafetyMode::TwoPhase,
            RiskLimits::permissive(),
            daily(),
            Arc::new(AlwaysOpenGate),
        )));
        let s1 = tool
            .lock()
            .await
            .execute(&args_json("BTC-USDT", 0.1))
            .await
            .unwrap();
        let ack1: OrderAck = serde_json::from_str(&s1).unwrap();
        let token = ack1.confirm_token.unwrap();

        // 运行时切换到阻断闸门
        tool.lock().await.set_gate(Arc::new(BlockedGate {
            reason: "after-trigger".into(),
        }));

        // 第二次带 token 提交,期望被阻断
        let args2 = serde_json::json!({
            "symbol": "BTC-USDT", "side": "Buy", "quantity": 0.1,
            "order_type": "Limit", "price": 50_000.0,
            "extras": {"confirm_token": token}
        })
        .to_string();
        let e = tool.lock().await.execute(&args2).await.unwrap_err();
        assert!(matches!(e, ToolError::ExecutionFailed(_)));
        // 注意:被阻断的请求会消耗 daily counter + 移除 pending,但 backend 未被调
        // 这一点是为了避免 daily counter 被打爆后 daily 失效,是有意为之。
        // 此处只验证 backend 未被调
        assert_eq!(m.order_count(), 0);
    }

    /// DryRun 模式:闸门即使阻断也不影响(LLM 仍可探索)
    #[tokio::test]
    async fn gate_does_not_block_dry_run() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = PlaceOrderTool::with_gate(
            m.clone(),
            SafetyMode::DryRun,
            RiskLimits::permissive(),
            daily(),
            Arc::new(BlockedGate {
                reason: "should not block".into(),
            }),
        );
        let s = tool.execute(&args_json("BTC-USDT", 0.1)).await.unwrap();
        let ack: OrderAck = serde_json::from_str(&s).unwrap();
        assert_eq!(ack.status.0, "DryRun");
        assert_eq!(m.order_count(), 0);
    }

    /// set_gate 运行时切换立即生效
    #[tokio::test]
    async fn set_gate_swaps_at_runtime() {
        let m = Arc::new(MockTradingBackend::new());
        let mut tool = PlaceOrderTool::with_gate(
            m.clone(),
            SafetyMode::Direct,
            RiskLimits::permissive(),
            daily(),
            Arc::new(AlwaysOpenGate),
        );
        // 第一次:open → 通过
        tool.execute(&args_json("BTC-USDT", 0.1)).await.unwrap();
        assert_eq!(m.order_count(), 1);
        // 切换为阻断
        tool.set_gate(Arc::new(BlockedGate {
            reason: "runtime swap".into(),
        }));
        // 第二次:被阻断
        let e = tool.execute(&args_json("BTC-USDT", 0.1)).await.unwrap_err();
        assert!(matches!(e, ToolError::ExecutionFailed(_)));
        assert_eq!(m.order_count(), 1); // backend 仅被调一次
    }

    // ── max_position_abs 测试(Stage F)─────────────────────

    /// max_position_abs:Buy 后持仓超过上限 → ToolError,backend 不被调
    #[tokio::test]
    async fn place_order_blocks_when_projected_position_exceeds_max() {
        let m = Arc::new(MockTradingBackend::new());
        // mock 默认持仓 BTC-USDT 0.1,max_position_abs=0.5
        // Buy 0.5 → projected = 0.1 + 0.5 = 0.6 > 0.5 → 拒
        let risk = RiskLimits {
            max_position_abs: Some(0.5),
            ..Default::default()
        };
        let tool = PlaceOrderTool::new(m.clone(), SafetyMode::Direct, risk, daily());
        let e = tool.execute(&args_json("BTC-USDT", 0.5)).await.unwrap_err();
        assert!(matches!(e, ToolError::ExecutionFailed(_)));
        // backend 未被调
        assert_eq!(m.order_count(), 0);
    }

    /// max_position_abs:Sell 减少持仓 → 正常下单
    #[tokio::test]
    async fn place_order_allows_sell_when_reduces_position() {
        let m = Arc::new(MockTradingBackend::new());
        // mock 默认持仓 BTC-USDT 0.1,max_position_abs=0.5
        // Sell 0.05 → projected = 0.1 - 0.05 = 0.05 < 0.5 → 放行
        let risk = RiskLimits {
            max_position_abs: Some(0.5),
            ..Default::default()
        };
        let tool = PlaceOrderTool::new(m.clone(), SafetyMode::Direct, risk, daily());
        let args_sell = serde_json::json!({
            "symbol": "BTC-USDT", "side": "Sell", "quantity": 0.05,
            "order_type": "Limit", "price": 50_000.0
        })
        .to_string();
        let s = tool.execute(&args_sell).await.unwrap();
        let ack: OrderAck = serde_json::from_str(&s).unwrap();
        assert_eq!(ack.order_id, "MOCK-1");
        assert_eq!(m.order_count(), 1);
    }

    /// max_position_abs:DryRun 也走位置预检(LLM 早感知超过持仓上限)
    #[tokio::test]
    async fn place_order_dry_run_still_respects_max_position_abs() {
        let m = Arc::new(MockTradingBackend::new());
        let risk = RiskLimits {
            max_position_abs: Some(0.5),
            ..Default::default()
        };
        let tool = PlaceOrderTool::new(m.clone(), SafetyMode::DryRun, risk, daily());
        let e = tool.execute(&args_json("BTC-USDT", 0.5)).await.unwrap_err();
        assert!(matches!(e, ToolError::ExecutionFailed(_)));
        // DryRun backend 也不被调(预检阶段就拒)
        assert_eq!(m.order_count(), 0);
    }

    // ── Stage H: metrics 集成测试 ──

    #[tokio::test]
    async fn place_order_records_metrics_on_success() {
        use crate::trading::metrics::TradingMetrics;
        let m = Arc::new(MockTradingBackend::new());
        let metrics = Arc::new(TradingMetrics::new());
        let tool = PlaceOrderTool::new(
            m.clone(),
            SafetyMode::Direct,
            RiskLimits::permissive(),
            daily(),
        )
        .with_metrics(metrics.clone());
        tool.execute(&args_json("BTC-USDT", 0.1)).await.unwrap();
        let snap = metrics.snapshot_filtered("trading_orders_total");
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].value, 1.0);
        assert_eq!(snap[0].labels.get("side"), Some(&"Buy".to_string()));
    }

    #[tokio::test]
    async fn place_order_records_risk_block_metric() {
        use crate::trading::metrics::TradingMetrics;
        let m = Arc::new(MockTradingBackend::new());
        let metrics = Arc::new(TradingMetrics::new());
        let risk = RiskLimits {
            allowed_symbols: Some(vec!["ETH-USDT".into()]),
            ..Default::default()
        };
        let tool = PlaceOrderTool::new(m.clone(), SafetyMode::Direct, risk, daily())
            .with_metrics(metrics.clone());
        let _ = tool.execute(&args_json("BTC-USDT", 0.1)).await;
        let snap = metrics.snapshot_filtered("trading_risk_blocks_total");
        assert_eq!(snap.len(), 1);
        assert_eq!(
            snap[0].labels.get("rule"),
            Some(&"allowed_symbols".to_string())
        );
    }

    #[tokio::test]
    async fn place_order_without_metrics_does_not_panic() {
        // 默认 None 行为:不埋点,执行成功
        let m = Arc::new(MockTradingBackend::new());
        let tool = PlaceOrderTool::new(
            m.clone(),
            SafetyMode::Direct,
            RiskLimits::permissive(),
            daily(),
        );
        let out = tool.execute(&args_json("BTC-USDT", 0.1)).await.unwrap();
        assert!(out.contains("order_id"));
    }
}
