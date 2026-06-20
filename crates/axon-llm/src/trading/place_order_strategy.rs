//! PlaceOrderTool 执行策略(Strategy Pattern)
//!
//! 为每种 `SafetyMode` 提供独立的执行策略,消除 `execute` 方法中
//! 三大分支的重复代码(单日计数检查、闸门检查、metrics 埋点、错误处理)。
//!
//! 策略不持有 backend / pending / metrics 状态,而是从 `PlaceOrderTool`
//! 借用(通过 `pub(crate)` 访问方法),保证数据源单一。

use std::time::Instant;

use async_trait::async_trait;
use tracing::info;

use crate::tools::ToolError;
use crate::trading::place_order_tool::PlaceOrderTool;
use crate::trading::safety::{PendingOrder, SafetyMode};
use crate::trading::types::{OrderAck, OrderStatus, PlaceOrderArgs};

/// 下单执行策略 trait
///
/// 每种 `SafetyMode` 对应一个实现。`execute` 返回 `OrderAck`,
/// 序列化为 JSON 由 `PlaceOrderTool::execute` 统一处理。
#[async_trait]
pub trait OrderExecutionStrategy: Send + Sync {
    /// 执行下单,返回 `OrderAck`
    ///
    /// # 参数
    /// - `ctx`:所属的 `PlaceOrderTool`,用于访问 backend / pending / metrics
    /// - `args`:已通过风控预检的下单参数
    /// - `start`:`Instant`,用于 metrics 延迟统计
    async fn execute(
        &self,
        ctx: &PlaceOrderTool,
        args: &PlaceOrderArgs,
        start: Instant,
    ) -> Result<OrderAck, ToolError>;
}

// ── DryRun 策略 ──────────────────────────────────────────

/// DryRun 模式:不调 backend,返回 `status="DryRun"` 的 ack
///
/// 不消耗单日计数,不检查闸门(LLM 任意次 dry-run 探索)。
/// 预检通过时调用 `record_breaker_success()` 视为"健康行为"。
pub struct DryRunStrategy;

#[async_trait]
impl OrderExecutionStrategy for DryRunStrategy {
    async fn execute(
        &self,
        ctx: &PlaceOrderTool,
        args: &PlaceOrderArgs,
        start: Instant,
    ) -> Result<OrderAck, ToolError> {
        info!(?args, "[DRY-RUN] place_order would be sent");
        let ack = OrderAck {
            order_id: "DRY-RUN".into(),
            symbol: args.symbol.clone(),
            side: args.side,
            quantity: args.quantity,
            status: OrderStatus("DryRun".into()),
            timestamp_ms: PlaceOrderTool::now_ms(),
            confirm_token: None,
        };
        // Stage H:DryRun 也埋点(status="DryRun"),便于观测 dry-run 比例
        ctx.record_order_metric(
            &ack.symbol,
            ack.side,
            "DryRun",
            start.elapsed().as_nanos() as u64,
        );
        // Stage J:DryRun 预检通过 → 视为"健康行为",清零拒绝计数
        ctx.record_breaker_success();
        Ok(ack)
    }
}

// ── Direct 策略 ──────────────────────────────────────────

/// Direct 模式:真发前做单日计数 + 闸门检查,直接调后端下单
///
/// 成功后调 `record_breaker_success()`;后端错误不调
/// `record_rejection`(避免被错误地清零)。
pub struct DirectStrategy;

#[async_trait]
impl OrderExecutionStrategy for DirectStrategy {
    async fn execute(
        &self,
        ctx: &PlaceOrderTool,
        args: &PlaceOrderArgs,
        start: Instant,
    ) -> Result<OrderAck, ToolError> {
        // 真发前做单日计数检查
        ctx.check_daily_or_record()?;
        // Stage H:镜像当日计数
        ctx.set_daily_metric();
        // Stage D:闸门检查
        ctx.check_gate_or_record()?;

        match ctx.backend().place_order(args).await {
            Ok(ack) => {
                let status = ack.status.0.clone();
                ctx.record_order_metric(
                    &ack.symbol,
                    ack.side,
                    &status,
                    start.elapsed().as_nanos() as u64,
                );
                // Stage J:下单成功 → 清零拒绝计数
                ctx.record_breaker_success();
                Ok(ack)
            }
            Err(e) => {
                ctx.record_order_metric(
                    &args.symbol,
                    args.side,
                    "Error",
                    start.elapsed().as_nanos() as u64,
                );
                // Stage J:后端错误不调 record_success(保持计数,等 cooldown)
                Err(ToolError::ExecutionFailed(e.to_string()))
            }
        }
    }
}

// ── TwoPhase 策略 ────────────────────────────────────────

/// TwoPhase 模式:第一次返回 confirm_token,第二次带相同 token 才真发
///
/// - 第一次提交:仅暂存到 `ctx.pending`,不消耗单日计数、不检查闸门
/// - 第二次提交:真发前做单日计数 + 闸门检查,移除 pending 后调后端
pub struct TwoPhaseStrategy;

#[async_trait]
impl OrderExecutionStrategy for TwoPhaseStrategy {
    async fn execute(
        &self,
        ctx: &PlaceOrderTool,
        args: &PlaceOrderArgs,
        start: Instant,
    ) -> Result<OrderAck, ToolError> {
        let supplied_token = args
            .extras
            .get("confirm_token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        if let Some(t) = supplied_token {
            // 第二次提交:真发前做单日计数检查
            ctx.check_daily_or_record()?;
            ctx.set_daily_metric();
            // Stage D:闸门检查(仅真发路径)
            ctx.check_gate_or_record()?;

            let pending = ctx
                .pending_map()
                .lock()
                .expect("poisoned")
                .remove(&t)
                .ok_or_else(|| ToolError::ExecutionFailed(format!("未找到待确认订单: {}", t)))?;

            let latency_ns = start.elapsed().as_nanos() as u64;
            match ctx.backend().place_order(&pending.args).await {
                Ok(ack) => {
                    let status = ack.status.0.clone();
                    ctx.record_order_metric(&ack.symbol, ack.side, &status, latency_ns);
                    // Stage J:下单成功 → 清零拒绝计数
                    ctx.record_breaker_success();
                    Ok(ack)
                }
                Err(e) => {
                    ctx.record_order_metric(
                        &pending.args.symbol,
                        pending.args.side,
                        "Error",
                        latency_ns,
                    );
                    // Stage J:后端错误不调 record_success
                    Err(ToolError::ExecutionFailed(e.to_string()))
                }
            }
        } else {
            // 第一次提交:仅暂存,不计数(尚未真发)
            // 注意:第一次提交也不检查闸门,允许 LLM 发起 confirm 流程
            let token = uuid::Uuid::new_v4().to_string();
            ctx.pending_map().lock().expect("poisoned").insert(
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
                timestamp_ms: PlaceOrderTool::now_ms(),
                confirm_token: Some(token),
            };
            // Stage J:TwoPhase 第一次预检通过 → 视为"健康行为",清零拒绝计数
            ctx.record_breaker_success();
            Ok(ack)
        }
    }
}

// ── 策略选择 ──────────────────────────────────────────────

/// 根据 `SafetyMode` 选择对应的执行策略
pub fn select_strategy(mode: SafetyMode) -> Box<dyn OrderExecutionStrategy> {
    match mode {
        SafetyMode::DryRun => Box::new(DryRunStrategy),
        SafetyMode::Direct => Box::new(DirectStrategy),
        SafetyMode::TwoPhase => Box::new(TwoPhaseStrategy),
    }
}
