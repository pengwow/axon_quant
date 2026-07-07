//! ExecutionAgent - 执行 Agent
//!
//! 0.3.0 P0 之前:execute_order 是纯内存模拟,`order_id = "order_{n}"`,未触达
//! `PlaceOrderTool` / `QueryPortfolioTool` / `TradingBackend`,所以"执行 Agent"
//! 只是空壳。
//!
//! 0.3.0 P0 之后:
//! - 持有 `Arc<PlaceOrderTool>` + `Arc<QueryPortfolioTool>`,由
//!   `ExecutionAgentConfig.tools` 在构造时注入
//! - `execute_order(&TradeOrder)` 走 `PlaceOrderTool.execute(...)` 真下单;
//!   `OrderAck` 转 `TradeResult` 后经 outbox 发出
//! - `query_portfolio(&self)` 走 `QueryPortfolioTool.execute(...)` 真查询,
//!   返回 `PortfolioSnapshot`
//! - 业务校验(余额不足 / 风控拒绝)由 `PlaceOrderTool` 内部完成,
//!   ExecutionAgent 只负责"消息 ↔ Tool"格式转换

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::swarm::agent::{AgentId, AgentRole, AgentStatus};
use crate::swarm::agent_runner::{DeclarativeAgentRunner, RunnerOutput};
use crate::swarm::error::SwarmError;
use crate::swarm::message::{AgentMessage, MessageContent, TradeOrder, TradeResult};
use crate::tools::Tool;
use crate::trading::place_order_tool::PlaceOrderTool;
use crate::trading::query_portfolio_tool::QueryPortfolioTool;
use crate::trading::types::{
    OrderAck, OrderKind, PlaceOrderArgs, PortfolioSnapshot, QueryPortfolioArgs, TimeInForce,
};
use crate::trading::types::OrderSide as TradingOrderSide;

use crate::swarm::message::OrderSide as SwarmOrderSide;

/// ExecutionAgent 工具集合(Stage K 工具 + 可选 CancelOrderTool 预留)
#[derive(Clone)]
pub struct TradingTools {
    /// 下单工具
    pub place_order: Arc<PlaceOrderTool>,
    /// 查询投资组合工具
    pub query_portfolio: Arc<QueryPortfolioTool>,
}

impl TradingTools {
    /// 构造(常用入口)
    pub fn new(
        place_order: Arc<PlaceOrderTool>,
        query_portfolio: Arc<QueryPortfolioTool>,
    ) -> Self {
        Self {
            place_order,
            query_portfolio,
        }
    }
}

/// ExecutionAgent 配置
pub struct ExecutionAgentConfig {
    /// 模拟延迟(毫秒)— 当前版本未使用,占位以备将来接真实 OMS 时延
    #[allow(dead_code)]
    pub simulated_latency_ms: u64,
    /// 滑点(基点)— 保留兼容字段,真实滑点由后端 / PlaceOrderTool 处理
    #[allow(dead_code)]
    pub slippage_bps: f64,
    /// 工具集合(`None` 表示 agent 退化为"模拟模式",只产生 order_id 字符串,
    /// 适用于无后端的 demo / 测试场景)
    pub tools: Option<TradingTools>,
}

impl Default for ExecutionAgentConfig {
    fn default() -> Self {
        Self {
            simulated_latency_ms: 10,
            slippage_bps: 5.0,
            tools: None,
        }
    }
}

impl ExecutionAgentConfig {
    /// 带工具的构造器
    pub fn with_tools(tools: TradingTools) -> Self {
        Self {
            simulated_latency_ms: 10,
            slippage_bps: 5.0,
            tools: Some(tools),
        }
    }
}

/// ExecutionAgent - 执行 Agent
pub struct ExecutionAgent {
    id: AgentId,
    status: AgentStatus,
    config: ExecutionAgentConfig,
    #[allow(dead_code)]
    inbox: mpsc::Receiver<AgentMessage>,
    outbox: mpsc::Sender<AgentMessage>,
    /// 已执行订单计数
    executed_count: usize,
    /// 已发送消息计数(测试可观察)
    sent_count: usize,
    /// 真发下单次数(经 PlaceOrderTool 成功调用 backend)
    placed_count: usize,
    /// 失败下单次数(风控 / 后端 / 参数错误)
    failed_count: usize,
    /// 投资组合查询次数
    queried_count: usize,
}

impl ExecutionAgent {
    /// 创建新的 ExecutionAgent
    pub fn new(
        id: AgentId,
        config: ExecutionAgentConfig,
        inbox: mpsc::Receiver<AgentMessage>,
        outbox: mpsc::Sender<AgentMessage>,
    ) -> Self {
        Self {
            id,
            status: AgentStatus::Idle,
            config,
            inbox,
            outbox,
            executed_count: 0,
            sent_count: 0,
            placed_count: 0,
            failed_count: 0,
            queried_count: 0,
        }
    }

    /// 获取 Agent ID
    pub fn id(&self) -> &AgentId {
        &self.id
    }

    /// 获取角色
    pub fn role(&self) -> AgentRole {
        AgentRole::Execution
    }

    /// 获取状态
    pub fn status(&self) -> AgentStatus {
        self.status
    }

    /// 获取已执行订单数
    pub fn executed_count(&self) -> usize {
        self.executed_count
    }

    /// 获取已发送消息数(测试 / 监控用)
    pub fn sent_count(&self) -> usize {
        self.sent_count
    }

    /// 真发下单成功次数
    pub fn placed_count(&self) -> usize {
        self.placed_count
    }

    /// 下单失败次数(风控 / 后端 / 参数错误)
    pub fn failed_count(&self) -> usize {
        self.failed_count
    }

    /// 投资组合查询次数
    pub fn queried_count(&self) -> usize {
        self.queried_count
    }

    /// TradeOrder → PlaceOrderArgs(供 PlaceOrderTool.execute 序列化使用)
    fn to_place_args(order: &TradeOrder) -> PlaceOrderArgs {
        let order_kind = match order.order_type.to_ascii_lowercase().as_str() {
            "market" => OrderKind::Market,
            // 未知 / "limit" 一律视为 Limit
            _ => OrderKind::Limit,
        };
        let side = match order.side {
            SwarmOrderSide::Buy => TradingOrderSide::Buy,
            SwarmOrderSide::Sell => TradingOrderSide::Sell,
        };
        PlaceOrderArgs {
            symbol: order.symbol.clone(),
            side,
            quantity: order.quantity,
            order_type: order_kind,
            price: order.price,
            stop_loss: None,
            take_profit: None,
            time_in_force: TimeInForce::GTC,
            extras: serde_json::Value::Null,
        }
    }

    /// 调 `PlaceOrderTool` 真下单(0.3.0 P0 起)
    ///
    /// 把 `TradeOrder` 序列化为 JSON,经 `Tool::execute` 走完整风控 / 闸门 / 策略;
    /// 成功时 `placed_count +1`,后端 / 风控 / 参数错误时 `failed_count +1`。
    /// 无 tools(模拟模式)时退化为返回伪造的 `order_id` 字符串。
    pub async fn place_order_via_tool(
        &mut self,
        order: &TradeOrder,
    ) -> Result<TradeResult, SwarmError> {
        self.status = AgentStatus::Executing;
        self.executed_count += 1;

        let Some(tools) = self.config.tools.as_ref() else {
            // 模拟模式:无 PlaceOrderTool,退化为伪 order_id(保持向后兼容)
            let result = TradeResult {
                order_id: format!("order_{}", self.executed_count),
                success: true,
                error: None,
            };
            self.status = AgentStatus::Idle;
            return Ok(result);
        };

        let args = Self::to_place_args(order);
        let args_json = serde_json::to_string(&args)
            .map_err(|e| SwarmError::Other(format!("序列化 PlaceOrderArgs 失败: {e}")))?;

        match tools.place_order.execute(&args_json).await {
            Ok(json) => {
                let ack: OrderAck = serde_json::from_str(&json).map_err(|e| {
                    SwarmError::Other(format!("反序列化 OrderAck 失败: {e} (raw={json})"))
                })?;
                self.placed_count += 1;
                self.status = AgentStatus::Idle;
                // DryRun / TwoPhase-pending 也算"成功"成功(返回了 OrderAck),
                // 由 Orchestrator 后续按 status 字段决定是否发 ExecutionRequest
                Ok(TradeResult {
                    order_id: ack.order_id,
                    success: true,
                    error: None,
                })
            }
            Err(e) => {
                self.failed_count += 1;
                self.status = AgentStatus::Idle;
                Ok(TradeResult {
                    order_id: format!("failed_{}", self.executed_count),
                    success: false,
                    error: Some(e.to_string()),
                })
            }
        }
    }

    /// 调 `QueryPortfolioTool` 真查询(0.3.0 P0 起)
    ///
    /// 可选按 `symbol` 过滤持仓(不影响 balance)。
    /// 无 tools 时返回 `SwarmError::Other`。
    pub async fn query_portfolio(
        &mut self,
        symbol: Option<&str>,
    ) -> Result<PortfolioSnapshot, SwarmError> {
        self.queried_count += 1;
        let Some(tools) = self.config.tools.as_ref() else {
            return Err(SwarmError::Other(
                "ExecutionAgent has no tools attached (mock mode)".into(),
            ));
        };
        let args = QueryPortfolioArgs {
            symbol: symbol.map(|s| s.to_string()),
        };
        let args_json = serde_json::to_string(&args)
            .map_err(|e| SwarmError::Other(format!("序列化 QueryPortfolioArgs 失败: {e}")))?;

        let json = tools
            .query_portfolio
            .execute(&args_json)
            .await
            .map_err(|e| SwarmError::Other(format!("QueryPortfolioTool 失败: {e}")))?;
        let snap: PortfolioSnapshot = serde_json::from_str(&json).map_err(|e| {
            SwarmError::Other(format!(
                "反序列化 PortfolioSnapshot 失败: {e} (raw={json})"
            ))
        })?;
        Ok(snap)
    }

    /// 处理消息
    pub async fn handle_message(&mut self, msg: AgentMessage) -> Result<(), SwarmError> {
        self.status = AgentStatus::Thinking;

        match msg.content {
            MessageContent::ExecutionRequest(order) => {
                let result = self.place_order_via_tool(&order).await?;

                // 发送执行结果
                let response = AgentMessage {
                    id: crate::swarm::message::MessageId::new(),
                    from: self.id.clone(),
                    to: msg.from,
                    correlation_id: msg.correlation_id,
                    content: MessageContent::ExecutionResult(result),
                    timestamp: chrono::Utc::now().timestamp(),
                };
                let _ = self.outbox.send(response).await;
                self.sent_count += 1;
                self.status = AgentStatus::Idle;
            }
            MessageContent::Heartbeat => {
                self.status = AgentStatus::Idle;
            }
            MessageContent::Shutdown => {
                self.status = AgentStatus::Failed;
            }
            _ => {
                self.status = AgentStatus::Idle;
            }
        }

        Ok(())
    }
}

#[async_trait]
impl DeclarativeAgentRunner for ExecutionAgent {
    fn id(&self) -> &AgentId {
        &self.id
    }
    fn role(&self) -> AgentRole {
        AgentRole::Execution
    }
    fn status(&self) -> AgentStatus {
        self.status
    }
    async fn run_step(&mut self, msg: AgentMessage) -> Result<RunnerOutput, SwarmError> {
        // `ExecutionRequest` 触发真下单,经 outbox 发出 `ExecutionResult`
        let forwarded = matches!(msg.content, MessageContent::ExecutionRequest(_)) as usize;
        self.handle_message(msg).await?;
        if forwarded > 0 {
            Ok(RunnerOutput::Forwarded { forwarded })
        } else {
            Ok(RunnerOutput::None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::agent::AgentId;
    use crate::trading::mock::MockTradingBackend;
    use crate::trading::safety::{DailyCounter, RiskLimits, SafetyMode};

    /// 辅助构造:Mock + Permissive + DryRun PlaceOrderTool / QueryPortfolioTool
    fn build_tools_dry_run() -> TradingTools {
        let backend = Arc::new(MockTradingBackend::new());
        let daily = Arc::new(DailyCounter::default());
        let place_order = Arc::new(PlaceOrderTool::new(
            backend.clone(),
            SafetyMode::DryRun,
            RiskLimits::permissive(),
            daily,
        ));
        let query_portfolio = Arc::new(QueryPortfolioTool::new(backend));
        TradingTools::new(place_order, query_portfolio)
    }

    /// 辅助构造:Direct 模式(真发)
    fn build_tools_direct(backend: Arc<MockTradingBackend>) -> TradingTools {
        let daily = Arc::new(DailyCounter::default());
        let place_order = Arc::new(PlaceOrderTool::new(
            backend.clone(),
            SafetyMode::Direct,
            RiskLimits::permissive(),
            daily,
        ));
        let query_portfolio = Arc::new(QueryPortfolioTool::new(backend));
        TradingTools::new(place_order, query_portfolio)
    }

    /// 辅助构造:TwoPhase 模式
    fn build_tools_two_phase(backend: Arc<MockTradingBackend>) -> TradingTools {
        let daily = Arc::new(DailyCounter::default());
        let place_order = Arc::new(PlaceOrderTool::new(
            backend.clone(),
            SafetyMode::TwoPhase,
            RiskLimits::permissive(),
            daily,
        ));
        let query_portfolio = Arc::new(QueryPortfolioTool::new(backend));
        TradingTools::new(place_order, query_portfolio)
    }

    fn mk_order(qty: f64) -> TradeOrder {
        TradeOrder {
            symbol: "BTC-USDT".into(),
            side: SwarmOrderSide::Buy,
            quantity: qty,
            order_type: "limit".into(),
            price: Some(50_000.0),
            reason: "Test".into(),
        }
    }

    #[test]
    fn test_execution_agent_creation() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("execution_0");
        let config = ExecutionAgentConfig::default();
        let agent = ExecutionAgent::new(id.clone(), config, rx, tx);

        assert_eq!(agent.id(), &id);
        assert_eq!(agent.role(), AgentRole::Execution);
        assert_eq!(agent.status(), AgentStatus::Idle);
        assert_eq!(agent.executed_count(), 0);
        assert_eq!(agent.placed_count(), 0);
        assert_eq!(agent.failed_count(), 0);
    }

    /// 无 tools:沿用原"模拟模式"语义,order_id = "order_{n}"
    #[tokio::test]
    async fn test_execution_agent_mock_mode_compat() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("execution_0");
        let config = ExecutionAgentConfig::default(); // tools = None
        let mut agent = ExecutionAgent::new(id, config, rx, tx);

        let result = agent.place_order_via_tool(&mk_order(0.1)).await.unwrap();
        assert!(result.success);
        assert_eq!(result.order_id, "order_1");
        assert_eq!(agent.executed_count(), 1);
        // 模拟模式不动 placed_count / failed_count
        assert_eq!(agent.placed_count(), 0);
        assert_eq!(agent.failed_count(), 0);
    }

    /// 1) DryRun 路径:PlaceOrderTool.execute 返回 DRY-RUN,backend 未被调,
    ///    placed_count +1,executed_count +1
    #[tokio::test]
    async fn test_execution_agent_dry_run_path() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("execution_0");
        let backend = Arc::new(MockTradingBackend::new());
        assert_eq!(backend.order_count(), 0);

        let config = ExecutionAgentConfig::with_tools(build_tools_dry_run());
        let mut agent = ExecutionAgent::new(id, config, rx, tx);

        let result = agent.place_order_via_tool(&mk_order(0.1)).await.unwrap();
        assert!(result.success);
        assert_eq!(result.order_id, "DRY-RUN");
        assert_eq!(agent.executed_count(), 1);
        assert_eq!(agent.placed_count(), 1);
        assert_eq!(agent.failed_count(), 0);
        // DryRun 不应触达 backend
        assert_eq!(backend.order_count(), 0);
    }

    /// 2) Direct 路径:PlaceOrderTool 真发到 backend,placed_count +1
    #[tokio::test]
    async fn test_execution_agent_direct_path() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("execution_0");
        let backend = Arc::new(MockTradingBackend::new());
        assert_eq!(backend.order_count(), 0);

        let config = ExecutionAgentConfig::with_tools(build_tools_direct(backend.clone()));
        let mut agent = ExecutionAgent::new(id, config, rx, tx);

        let result = agent.place_order_via_tool(&mk_order(0.1)).await.unwrap();
        assert!(result.success);
        assert_eq!(result.order_id, "MOCK-1");
        assert_eq!(agent.placed_count(), 1);
        // backend 真被调
        assert_eq!(backend.order_count(), 1);
    }

    /// 3) TwoPhase 路径:第一次 place_order 返回 PENDING,backend 未被调
    #[tokio::test]
    async fn test_execution_agent_two_phase_first_call() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("execution_0");
        let backend = Arc::new(MockTradingBackend::new());

        let config = ExecutionAgentConfig::with_tools(build_tools_two_phase(backend.clone()));
        let mut agent = ExecutionAgent::new(id, config, rx, tx);

        let result = agent.place_order_via_tool(&mk_order(0.1)).await.unwrap();
        // PENDING 也算 success=true(order_id 不为空)
        assert!(result.success);
        assert_eq!(result.order_id, "PENDING");
        assert_eq!(agent.placed_count(), 1);
        // 第一次不真发
        assert_eq!(backend.order_count(), 0);
    }

    /// 4) 余额不足 reject:Backend 拒绝时 ExecutionAgent 返回 success=false
    #[tokio::test]
    async fn test_execution_agent_rejects_insufficient_cash() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("execution_0");
        // 注入 place_order 错误,模拟"余额不足 / 后端拒绝"
        use crate::trading::mock::FailureInjector;
        let backend = Arc::new(MockTradingBackend::new());
        *backend.failure_injector.lock().expect("poisoned") = FailureInjector::new()
            .with_place_order_error("insufficient cash: need 1e9, have 10000");

        let config = ExecutionAgentConfig::with_tools(build_tools_direct(backend.clone()));
        let mut agent = ExecutionAgent::new(id, config, rx, tx);

        let result = agent.place_order_via_tool(&mk_order(0.1)).await.unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());
        assert!(result.error.as_ref().unwrap().contains("insufficient cash"));
        assert_eq!(agent.placed_count(), 0);
        assert_eq!(agent.failed_count(), 1);
    }

    /// 5) query_portfolio:走 QueryPortfolioTool,返回 balance + positions
    #[tokio::test]
    async fn test_execution_agent_query_portfolio() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("execution_0");
        let backend = Arc::new(MockTradingBackend::new());

        let config = ExecutionAgentConfig::with_tools(build_tools_direct(backend.clone()));
        let mut agent = ExecutionAgent::new(id, config, rx, tx);

        // 全量查询
        let snap = agent.query_portfolio(None).await.unwrap();
        assert_eq!(snap.balance.currencies.len(), 2);
        assert_eq!(snap.positions.len(), 1);
        assert_eq!(agent.queried_count(), 1);

        // symbol 过滤
        let snap2 = agent
            .query_portfolio(Some("BTC-USDT"))
            .await
            .unwrap();
        assert_eq!(snap2.positions.len(), 1);
        assert_eq!(snap2.positions[0].symbol, "BTC-USDT");
        assert_eq!(agent.queried_count(), 2);

        // 过滤一个不存在的 symbol → 0 持仓
        let snap3 = agent.query_portfolio(Some("ETH-USDT")).await.unwrap();
        assert_eq!(snap3.positions.len(), 0);
        // balance 不受 filter 影响
        assert_eq!(snap3.balance.currencies.len(), 2);
    }

    /// 6) 无 tools 时 query_portfolio 返回 SwarmError::Other
    #[tokio::test]
    async fn test_execution_agent_query_portfolio_without_tools() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("execution_0");
        let config = ExecutionAgentConfig::default();
        let mut agent = ExecutionAgent::new(id, config, rx, tx);

        let err = agent.query_portfolio(None).await.unwrap_err();
        assert!(matches!(err, SwarmError::Other(_)));
    }

    /// 7) handle_message 走 ExecutionRequest 路径:真发 + 经 outbox 发 ExecutionResult
    #[tokio::test]
    async fn test_execution_agent_runner_trait_impl() {
        // 双 channel 配对:inbox 由 agent 持有,outbox 由测试观察
        let (_tx_in, rx_in) = mpsc::channel(10);
        let (tx_out, mut rx_out) = mpsc::channel(10);
        let id = AgentId::from_string("execution_0");
        let backend = Arc::new(MockTradingBackend::new());

        let config = ExecutionAgentConfig::with_tools(build_tools_direct(backend.clone()));
        let mut agent = ExecutionAgent::new(id, config, rx_in, tx_out);

        let order = mk_order(0.1);
        let msg = AgentMessage {
            id: crate::swarm::message::MessageId::new(),
            from: AgentId::from_string("orchestrator"),
            to: AgentId::from_string("execution_0"),
            correlation_id: None,
            content: MessageContent::ExecutionRequest(order),
            timestamp: 1000,
        };
        let out = agent.run_step(msg).await.unwrap();
        assert!(matches!(out, RunnerOutput::Forwarded { forwarded: 1 }));
        assert_eq!(agent.sent_count(), 1);
        assert_eq!(agent.executed_count(), 1);
        assert_eq!(agent.placed_count(), 1);
        assert_eq!(agent.status(), AgentStatus::Idle);
        assert_eq!(backend.order_count(), 1);

        // outbox 收到一条 ExecutionResult
        let response = rx_out.try_recv().expect("outbox 应有 1 条消息");
        match response.content {
            MessageContent::ExecutionResult(tr) => {
                assert!(tr.success);
                assert_eq!(tr.order_id, "MOCK-1");
            }
            _ => panic!("期望 ExecutionResult, 收到 {:?}", response.content),
        }

        // Heartbeat → None,sent_count 不变
        let hb = AgentMessage {
            id: crate::swarm::message::MessageId::new(),
            from: AgentId::from_string("orchestrator"),
            to: AgentId::from_string("execution_0"),
            correlation_id: None,
            content: MessageContent::Heartbeat,
            timestamp: 1000,
        };
        let out = agent.run_step(hb).await.unwrap();
        assert!(matches!(out, RunnerOutput::None));
        assert_eq!(agent.sent_count(), 1);
    }
}
