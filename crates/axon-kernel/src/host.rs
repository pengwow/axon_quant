//! 策略 Host 与订单意图
//!
//! 对内核而言，规则策略、RL 推理、LLM 信号策略都是同一种东西：**收到收市
//! bar 后同步吐出一组 `OrderIntent`**。因此 trait 故意设计成同步的，不做 async
//! （每 bar 一次 runtime 切换的开销不值得，见设计文档 4.4）。

use std::collections::HashMap;

use axon_core::{Instrument, Price, Quantity, Side};

use crate::event::BarClosedEvent;

/// 内核侧订单号（M0 用裸 `u64`）
///
/// M0 不持有订单状态机，只需要一个能回指意图的句柄。M5 接入 live 时替换为
/// `axon_oms::OrderId`（带幂等语义），届时本别名删除，不做并行保留。
pub type OrderId = u64;

/// 下单规格（最小自定义结构）
///
/// 刻意不复用 `axon_core::Order`：那是账务/撮合层的数据结构（含订单类型、
/// 有效期、状态机字段），而这里只表达「策略想下什么单」，由 sink 负责翻译。
#[derive(Debug, Clone, PartialEq)]
pub struct OrderSpec {
    /// 交易品种
    pub instrument: Instrument,
    /// 买卖方向
    pub side: Side,
    /// 数量（正数，方向由 `side` 表达）
    pub quantity: Quantity,
    /// 限价；`None` 表示市价（由 sink 按自己的撮合口径解释）
    pub price: Option<Price>,
}

/// 策略输出意图
#[derive(Debug, Clone, PartialEq)]
pub enum OrderIntent {
    /// 指定数量下单
    Submit(OrderSpec),
    /// 撤单（按内核侧订单号）
    Cancel(OrderId),
    /// 目标仓位调仓
    ///
    /// `target_ratio` 是**权益占比**，取值 `[-1, 1]`（`1.0` = 满仓多头，
    /// `-1.0` = 满仓空头，`0.0` = 平仓）。从占比换算成绝对数量是 sink 的
    /// 唯一职责，全链路只允许这一个换算点。
    SetTarget {
        /// 交易品种
        instrument: Instrument,
        /// 目标权益占比 `∈ [-1, 1]`
        target_ratio: f64,
    },
}

/// Host 决策上下文（只读视图）
///
/// 四个字段由账务本体在 per-bar 编排中回填（M1）。M0 保持默认空值。
#[derive(Debug, Clone, Default)]
pub struct HostCtx {
    /// 当前持仓（数量，正多负空）
    pub positions: HashMap<Instrument, f64>,
    /// 账户权益（计价币）
    pub equity: f64,
    /// 各品种最新盯市价
    pub last_price: HashMap<Instrument, f64>,
}

/// 策略宿主
///
/// 契约（必须遵守）：
///
/// - `on_event` 只在**收市**事件被调用，一 bar 一次；进行中 tick / 未收市 bar
///   不会触发决策。
/// - 返回**空 `Vec<OrderIntent>` 表示「维持上次生效的目标」**，不是平仓；
///   平仓必须显式返回 `OrderIntent::SetTarget { target_ratio: 0.0 }`。
pub trait StrategyHost {
    /// Host 名称（日志 / 诊断用）
    fn name(&self) -> &str;

    /// 收市事件决策入口
    ///
    /// 同步调用：故意不用 async，避免每 bar 一次 runtime 切换（设计文档 4.4）。
    fn on_event(&mut self, ev: &BarClosedEvent<'_>, ctx: &HostCtx) -> Vec<OrderIntent>;
}
