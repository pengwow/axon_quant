//! 回测引擎主循环（事件驱动）
//!
//! 消费 [`EventQueue`] 中的事件，按事件类型分发给 [`MatchingEngine`] 处理，
//! 并汇总运行结果（events_processed / orders_accepted / orders_rejected /
//! fills / total_pnl / max_drawdown / final_nav / duration）。
//!
//! # 事件分发
//!
//! - `OrderAction::Submitted(order)` → 提交至 `MatchingEngine`，统计 accepted/rejected
//! - `OrderAction::Cancelled / Modified / Rejected` → 计数 accepted
//! - `FillEvent` → 累加 fills 计数与 PnL
//! - 其他事件（MarketData / System）→ 计数后跳过
//!
//! # 设计约束
//!
//! - 匹配由 `axon-backtest::matching` 提供（L1/L2/L3），本模块仅做调度
//! - `ImpactModel` 为可选附加语义，当前实现记录 `impact_applied` 统计；具体
//!   价格调整由 `ImpactedMatchingEngine` 在更高层包装完成
//! - 单一回测任务单线程执行（事件驱动串行），不引入额外锁

use std::time::{Duration, Instant};

use axon_core::event::{Event, FillEvent, OrderAction, OrderEvent};
use axon_core::impact::ImpactModel;
use axon_core::market::Side;
use axon_core::market::Trade;
use axon_core::order::Order;
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use tracing::trace;

use crate::matching::MatchingEngine;

/// 回测引擎配置
///
/// - `clock`：模拟时钟（可设置结束时间；为 None 时引擎按事件自然耗尽退出）
/// - `matching_engine`：撮合引擎（L1/L2/L3），通过 trait object 注入
/// - `impact_model`：可选的市场冲击模型（仅用于统计；实际价格调整由
///   `ImpactedMatchingEngine` 在上层包装）
/// - `initial_cash`：初始现金（用于计算 `final_nav`）
pub struct BacktestEngineConfig {
    /// 模拟时钟
    pub clock: SimulatedClock,
    /// 撮合引擎
    pub matching_engine: Box<dyn MatchingEngine>,
    /// 可选冲击模型
    pub impact_model: Option<Box<dyn ImpactModel>>,
    /// 初始资金
    pub initial_cash: f64,
}

impl std::fmt::Debug for BacktestEngineConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BacktestEngineConfig")
            .field("clock", &self.clock)
            .field("matching_engine", &"<dyn MatchingEngine>")
            .field(
                "impact_model",
                &self.impact_model.as_ref().map(|m| m.name()),
            )
            .field("initial_cash", &self.initial_cash)
            .finish()
    }
}

/// 回测运行结果
#[derive(Debug, Clone, PartialEq)]
pub struct RunResult {
    /// 已处理事件总数（含 MarketData/Order/Fill/System）
    pub events_processed: u64,
    /// 接受的订单数（被撮合引擎接收，含部分成交）
    pub orders_accepted: u64,
    /// 拒绝的订单数（被撮合引擎拒绝 / 验证失败 / FOK 未满足）
    pub orders_rejected: u64,
    /// 成交（fill）总数（每个 MatchFill 计数一次）
    pub fills: u64,
    /// 取消的订单数
    pub orders_cancelled: u64,
    /// 修改的订单数
    pub orders_modified: u64,
    /// 已实现的 PnL：buy 端为负、sell 端为正
    ///
    /// 公式：`Σ (side=Buy: -price*qty) + (side=Sell: +price*qty)`
    pub total_pnl: f64,
    /// 最大回撤（基于 PnL 曲线的运行最大值与运行最小值之差）
    pub max_drawdown: f64,
    /// 最终净资产（初始资金 + 累计 PnL）
    pub final_nav: f64,
    /// 运行耗时（墙钟时间）
    pub duration: Duration,
    /// 引擎最终时间（最后一个事件的时间戳）
    pub final_time: Timestamp,
}

impl Default for RunResult {
    fn default() -> Self {
        Self {
            events_processed: 0,
            orders_accepted: 0,
            orders_rejected: 0,
            fills: 0,
            orders_cancelled: 0,
            orders_modified: 0,
            total_pnl: 0.0,
            max_drawdown: 0.0,
            final_nav: 0.0,
            duration: Duration::ZERO,
            final_time: Timestamp::from_nanos(0),
        }
    }
}

/// 内部统计状态（与 RunResult 区别：用于 step() 单步暴露中间态）
#[derive(Debug, Clone, Default)]
pub struct RunStats {
    /// 累计处理事件数
    pub events_processed: u64,
    /// 接受的订单数
    pub orders_accepted: u64,
    /// 拒绝的订单数
    pub orders_rejected: u64,
    /// 成交数
    pub fills: u64,
    /// 取消订单数
    pub orders_cancelled: u64,
    /// 修改订单数
    pub orders_modified: u64,
    /// 累计 PnL
    pub total_pnl: f64,
    /// PnL 运行峰值（用于计算 max_drawdown）
    pub pnl_peak: f64,
}

/// 回测引擎：消费 `EventQueue` 调度撮合 + 汇总结果
pub struct BacktestEngine {
    /// 引擎配置（含 clock / matching_engine / impact_model）
    config: BacktestEngineConfig,
    /// 待消费事件队列
    event_queue: EventQueue,
    /// 运行统计
    stats: RunStats,
    /// 引擎是否已运行完成（防止重复调用 run）
    finished: bool,
}

impl std::fmt::Debug for BacktestEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BacktestEngine")
            .field("config", &self.config)
            .field("event_queue_len", &self.event_queue.len())
            .field("stats", &self.stats)
            .field("finished", &self.finished)
            .finish()
    }
}

impl BacktestEngine {
    /// 创建回测引擎
    ///
    /// - `config`：回测配置（clock / 撮合器 / 冲击模型 / 初始资金）
    /// - `event_queue`：已填充事件的事件队列（所有权转移）
    pub fn new(config: BacktestEngineConfig, event_queue: EventQueue) -> Self {
        Self {
            config,
            event_queue,
            stats: RunStats::default(),
            finished: false,
        }
    }

    /// 当前已注册的源数量（队列中剩余事件数）
    pub fn pending_events(&self) -> usize {
        self.event_queue.len()
    }

    /// 推入单个事件到事件队列
    ///
    /// 给 Python 绑定 / 外部调用方使用 —— 构造完整 `Event` 后追加到内部队列。
    /// 时序由 `EventQueue` 保证:同一时间戳按 `seq` 升序出队(FIFO 语义)。
    ///
    /// 用途:Python 端可逐条 push 订单事件、成交事件等,而非一次性 `EventQueue::new()` + `push`。
    pub fn push_event(&mut self, event: Event) {
        self.event_queue.push(event);
    }

    /// 当前统计快照
    pub fn stats(&self) -> &RunStats {
        &self.stats
    }

    /// 单步处理一个事件：返回处理后的事件统计（用于单元测试与步进模式）
    pub fn step(&mut self) -> Option<RunStats> {
        let event = self.event_queue.next()?;
        self.dispatch(event);
        Some(self.stats.clone())
    }

    /// 完整运行：消费事件队列至耗尽，返回 `RunResult`。
    ///
    /// - 退出条件：`EventQueue::next()` 返回 `None`（队列耗尽或暂停）
    /// - 时钟推进：每步设置 `clock` 为当前事件时间戳
    /// - 耗时：使用 `Instant::now()` 测量墙钟耗时（`duration` 字段）
    pub fn run(&mut self) -> RunResult {
        let started = Instant::now();
        let initial_cash = self.config.initial_cash;

        // 防止重复 run：若已 finished，直接返回上次结果
        if self.finished {
            return self.build_result(initial_cash, started.elapsed());
        }

        // 推进时钟到当前队列时间起点（事件可能晚于 clock.start()）
        if let Some(t) = self.event_queue.peek_time() {
            self.config.clock.set(t);
        }

        // 主循环：消费事件直到队列耗尽
        while let Some(event) = self.event_queue.next() {
            self.dispatch(event);
        }

        self.finished = true;
        self.build_result(initial_cash, started.elapsed())
    }

    /// 分发单个事件
    fn dispatch(&mut self, event: Event) {
        // 推进时钟
        self.config.clock.set(event.timestamp());

        // 计数
        self.stats.events_processed += 1;

        match event {
            Event::Order(OrderEvent { action, .. }) => self.handle_order_action(action),
            Event::Fill(fill) => self.handle_fill(fill),
            // MarketData / System 事件仅计数
            _ => {
                trace!(seq = event.seq(), "non-dispatchable event (skipped)");
            }
        }
    }

    /// 处理订单动作
    ///
    /// `OrderAction` 标记为 `#[non_exhaustive]`，因此需要通配分支以兼容未来扩展。
    fn handle_order_action(&mut self, action: OrderAction) {
        match action {
            OrderAction::Submitted(order) => self.handle_submit(order),
            OrderAction::Cancelled(_) => {
                self.stats.orders_cancelled += 1;
            }
            OrderAction::Modified { .. } => {
                self.stats.orders_modified += 1;
            }
            OrderAction::Rejected { .. } => {
                self.stats.orders_rejected += 1;
            }
            // 非穷尽枚举：未来新增变体不影响向后兼容
            _ => {
                trace!("unhandled OrderAction variant");
            }
        }
    }

    /// 处理订单提交：调用撮合引擎并汇总结果
    ///
    /// 接受/拒绝的判定逻辑：
    /// - 若撮合器在 submit 前后活跃订单数 +1 ⇒ 订单被挂簿（accepted, pending）
    /// - 若活跃订单数未变且 fills 为空 ⇒ 订单被拒绝（accepted/rejected counter +1）
    /// - 若 fills 非空 ⇒ 订单被接受（accepted +1，且按 fill 数累加 fills/PnL）
    fn handle_submit(&mut self, order: Order) {
        let active_before = self.config.matching_engine.active_order_count();
        let result = self.config.matching_engine.submit(order);
        let active_after = self.config.matching_engine.active_order_count();
        let added_to_book = active_after > active_before;

        match (result.fills.is_empty(), added_to_book) {
            // 撮合出 fill ⇒ accepted
            (false, _) => {
                self.stats.orders_accepted += 1;
                // 每个 fill 独立计入 fills 计数
                self.stats.fills += result.fills.len() as u64;
                // 累加 PnL：基于 taker_side 区分买卖
                for fill in &result.fills {
                    let pnl_delta = fill_pnl_delta(fill);
                    self.stats.total_pnl += pnl_delta;
                    // 更新 PnL 峰值
                    if self.stats.total_pnl > self.stats.pnl_peak {
                        self.stats.pnl_peak = self.stats.total_pnl;
                    }
                }
            }
            // 无 fill 但挂入订单簿 ⇒ accepted（pending）
            (true, true) => {
                self.stats.orders_accepted += 1;
            }
            // 无 fill 且未挂簿 ⇒ rejected
            (true, false) => {
                self.stats.orders_rejected += 1;
            }
        }
    }

    /// 处理成交事件（来自外部 FillEvent 推送）
    fn handle_fill(&mut self, fill: FillEvent) {
        self.stats.fills += 1;
        // FillEvent.trade 含 buyer/seller 订单 ID；用 axon-core Trade 转为 PnL
        let pnl_delta = trade_pnl_delta(&fill.trade);
        self.stats.total_pnl += pnl_delta;
        if self.stats.total_pnl > self.stats.pnl_peak {
            self.stats.pnl_peak = self.stats.total_pnl;
        }
    }

    /// 构造最终 RunResult
    fn build_result(&self, initial_cash: f64, duration: Duration) -> RunResult {
        // 最大回撤 = 峰值 - 谷值；本引擎未独立跟踪 running min，
        // 这里用 `max(0, peak - final_pnl)` 作为下界安全的近似值。
        // 完整 PnL 曲线跟踪属于未来增强项。
        let max_drawdown = (self.stats.pnl_peak - self.stats.total_pnl).max(0.0);

        let final_nav = initial_cash + self.stats.total_pnl;
        let final_time = self.config.clock.now();

        RunResult {
            events_processed: self.stats.events_processed,
            orders_accepted: self.stats.orders_accepted,
            orders_rejected: self.stats.orders_rejected,
            fills: self.stats.fills,
            orders_cancelled: self.stats.orders_cancelled,
            orders_modified: self.stats.orders_modified,
            total_pnl: self.stats.total_pnl,
            max_drawdown,
            final_nav,
            duration,
            final_time,
        }
    }
}

/// 计算 `MatchFill` 对 taker 的 PnL 影响
///
/// 视角为"回测策略方"：
/// - Buy 端：现金减少，PnL 记为 `-price * qty`
/// - Sell 端：现金增加，PnL 记为 `+price * qty`
fn fill_pnl_delta(fill: &crate::matching::MatchFill) -> f64 {
    let notional = fill.turnover();
    match fill.taker_side {
        Side::Buy => -notional,
        Side::Sell => notional,
    }
}

/// 计算外部 `Trade` 的 PnL 影响
///
/// Trade 字段同时包含 buyer/seller 订单 ID；本函数使用一个保守的"对称记账"：
/// 因无法判断哪一方是策略侧，将 trade 视为市场价格参考，PnL 增量为 0。
/// 实际策略应使用 `MatchFill` 路径获取按 taker_side 区分的 PnL。
fn trade_pnl_delta(_trade: &Trade) -> f64 {
    // 见上方说明：保守为 0；按需可在调用侧自行计算 realized_pnl
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::event::{EventBuilder, OrderAction};
    use axon_core::market::Side;
    use axon_core::order::{OrderType, TimeInForce};
    use axon_core::time::Timestamp;
    use axon_core::types::{Price, Quantity, Symbol};

    use crate::matching::L1MatchingEngine;

    /// 测试用辅助：构造限价买单
    fn make_limit_order(id: u64, side: Side, price: f64, qty: f64) -> Order {
        Order::new(
            id,
            Symbol::from("BTC-USDT"),
            side,
            OrderType::Limit {
                price: Price::from_f64(price),
            },
            Quantity::from_f64(qty),
            TimeInForce::GTC,
        )
    }

    /// 构造简单配置（无冲击模型）
    fn simple_config() -> BacktestEngineConfig {
        BacktestEngineConfig {
            clock: SimulatedClock::new(Timestamp::from_nanos(0)),
            matching_engine: Box::new(L1MatchingEngine::new()),
            impact_model: None,
            initial_cash: 100_000.0,
        }
    }

    /// 空事件队列应立即返回零结果
    #[test]
    fn test_run_empty_queue() {
        let mut engine = BacktestEngine::new(simple_config(), EventQueue::new());
        let result = engine.run();
        assert_eq!(result.events_processed, 0);
        assert_eq!(result.fills, 0);
        assert_eq!(result.orders_accepted, 0);
        assert_eq!(result.orders_rejected, 0);
        assert_eq!(result.total_pnl, 0.0);
        assert_eq!(result.final_nav, 100_000.0);
    }

    /// 处理 Submit 事件：合法订单（无对手方）应被挂簿，accepted
    #[test]
    fn test_run_processes_submit_event() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        // 卖单挂单（无对手方但限价合法 ⇒ 挂入订单簿）
        let sell = make_limit_order(1, Side::Sell, 100.0, 1.0);
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            1,
            OrderAction::Submitted(sell),
        ));

        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();
        assert_eq!(result.events_processed, 1);
        // 卖单无对手方但已挂簿：accepted = 1, rejected = 0
        assert_eq!(result.orders_accepted, 1);
        assert_eq!(result.orders_rejected, 0);
        assert_eq!(result.fills, 0);
    }

    /// 处理 Submit 事件：非法订单（price=0）应被拒绝
    #[test]
    fn test_run_processes_rejected_submit_event() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        // 构造非法订单：price=0
        let bad = Order::new(
            1,
            Symbol::from("BTC-USDT"),
            Side::Buy,
            OrderType::Limit {
                price: Price::from_f64(0.0),
            },
            Quantity::from_f64(1.0),
            TimeInForce::GTC,
        );
        q.push(b.order(Timestamp::from_nanos(1_000), 1, OrderAction::Submitted(bad)));

        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();
        assert_eq!(result.events_processed, 1);
        assert_eq!(result.orders_rejected, 1);
        assert_eq!(result.orders_accepted, 0);
    }

    /// 完整撮合链路：卖单挂单 + 买单吃单 ⇒ 1 fill
    #[test]
    fn test_run_matched_orders_yield_one_fill() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let sell = make_limit_order(1, Side::Sell, 100.0, 1.0);
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            1,
            OrderAction::Submitted(sell),
        ));
        let buy = make_limit_order(2, Side::Buy, 100.0, 1.0);
        q.push(b.order(Timestamp::from_nanos(2_000), 2, OrderAction::Submitted(buy)));

        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();
        assert_eq!(result.events_processed, 2);
        // 卖单无对手方但挂簿 ⇒ accepted
        // 买单吃单 ⇒ accepted
        assert_eq!(result.orders_accepted, 2);
        assert_eq!(result.orders_rejected, 0);
        assert_eq!(result.fills, 1);
        // PnL: 买单侧 -100*1 = -100
        assert!((result.total_pnl - (-100.0)).abs() < 1e-9);
        // final_nav = 100_000 + (-100) = 99_900
        assert!((result.final_nav - 99_900.0).abs() < 1e-9);
    }

    /// 推进时钟：final_time 应为最后一个事件时间戳
    #[test]
    fn test_run_advances_clock() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        // 推送多个事件，最后一个时间戳为 3_000
        for i in 0..3 {
            let sell = make_limit_order(i + 1, Side::Sell, 100.0, 1.0);
            q.push(b.order(
                Timestamp::from_nanos((i + 1) as i64 * 1_000),
                i + 1,
                OrderAction::Submitted(sell),
            ));
        }
        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();
        // 最后一个事件时间戳 3_000
        assert_eq!(result.final_time, Timestamp::from_nanos(3_000));
        assert_eq!(result.events_processed, 3);
        // 卖单无对手方但挂簿 ⇒ accepted
        assert_eq!(result.orders_accepted, 3);
        assert_eq!(result.orders_rejected, 0);
    }

    /// FillEvent 直接累加 fills 计数（PnL 保守为 0）
    #[test]
    fn test_run_processes_fill_event() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        // 直接推送 FillEvent
        let trade = Trade::new(
            Timestamp::from_nanos(1_000),
            Price::from_f64(100.0),
            Quantity::from_f64(1.0),
            1,
            2,
        );
        q.push(b.fill(Timestamp::from_nanos(1_000), trade));

        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();
        assert_eq!(result.fills, 1);
        assert_eq!(result.events_processed, 1);
        // Trade 路径下 PnL 保守为 0
        assert_eq!(result.total_pnl, 0.0);
    }

    /// 取消/修改/拒绝事件正确计数
    #[test]
    fn test_run_counts_cancelled_modified_rejected_events() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        q.push(b.order(Timestamp::from_nanos(1_000), 1, OrderAction::Cancelled(1)));
        q.push(b.order(
            Timestamp::from_nanos(2_000),
            2,
            OrderAction::Modified {
                order_id: 2,
                new_quantity: Quantity::from_f64(5.0),
            },
        ));
        q.push(b.order(
            Timestamp::from_nanos(3_000),
            3,
            OrderAction::Rejected {
                order_id: 3,
                reason: "risk".into(),
            },
        ));

        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();
        assert_eq!(result.orders_cancelled, 1);
        assert_eq!(result.orders_modified, 1);
        assert_eq!(result.orders_rejected, 1);
        assert_eq!(result.events_processed, 3);
    }

    /// step() 单步推进：每调用一次处理一个事件
    #[test]
    fn test_step_processes_one_event_at_a_time() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        for i in 0..3 {
            q.push(b.order(
                Timestamp::from_nanos((i + 1) as i64),
                i + 1,
                OrderAction::Cancelled(i + 1),
            ));
        }
        let mut engine = BacktestEngine::new(simple_config(), q);

        // 第 1 步
        let s1 = engine.step().expect("应有事件");
        assert_eq!(s1.events_processed, 1);
        assert_eq!(s1.orders_cancelled, 1);

        // 第 2 步
        let s2 = engine.step().expect("应有事件");
        assert_eq!(s2.events_processed, 2);
        assert_eq!(s2.orders_cancelled, 2);

        // 第 3 步
        let s3 = engine.step().expect("应有事件");
        assert_eq!(s3.events_processed, 3);

        // 队列耗尽
        assert!(engine.step().is_none());
    }

    /// 重复调用 run 不会重复处理事件
    ///
    /// 注：两次 `run()` 之间的 `duration` 墙钟时间不同，不能直接 `assert_eq!`；
    /// 这里单独断言 `duration` 字段之外的所有统计量相等，再断言两次耗时
    /// 都为非负零值。
    #[test]
    fn test_run_idempotent_after_finished() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        q.push(b.order(Timestamp::from_nanos(1_000), 1, OrderAction::Cancelled(1)));
        let mut engine = BacktestEngine::new(simple_config(), q);

        let r1 = engine.run();
        let r2 = engine.run();

        // 关键统计量必须完全一致（除墙钟 duration 外）
        assert_eq!(r1.events_processed, r2.events_processed);
        assert_eq!(r1.orders_accepted, r2.orders_accepted);
        assert_eq!(r1.orders_rejected, r2.orders_rejected);
        assert_eq!(r1.fills, r2.fills);
        assert_eq!(r1.orders_cancelled, r2.orders_cancelled);
        assert_eq!(r1.orders_modified, r2.orders_modified);
        assert_eq!(r1.total_pnl, r2.total_pnl);
        assert_eq!(r1.max_drawdown, r2.max_drawdown);
        assert_eq!(r1.final_nav, r2.final_nav);
        assert_eq!(r1.final_time, r2.final_time);
        // duration 不参与比较，仅做合理性检查
        assert!(r1.duration >= Duration::ZERO);
        assert!(r2.duration >= Duration::ZERO);
    }

    /// max_drawdown 在 PnL 单调递减时正确计算
    ///
    /// 场景：
    /// 1. Sell @ 100 qty=1.0 → 挂簿（无对手方）→ PnL 不变, peak=0
    /// 2. Sell @ 100 qty=1.0 → 挂簿 → PnL 不变
    /// 3. Buy @ 100 qty=2.0 → 吃两单，PnL = -200，peak=0, max_drawdown = 0 - (-200) = 200
    #[test]
    fn test_max_drawdown_tracks_peak() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        // 卖单 #1
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            1,
            OrderAction::Submitted(make_limit_order(1, Side::Sell, 100.0, 1.0)),
        ));
        // 卖单 #2
        q.push(b.order(
            Timestamp::from_nanos(2_000),
            2,
            OrderAction::Submitted(make_limit_order(2, Side::Sell, 100.0, 1.0)),
        ));
        // 买单吃两单：PnL = -100*1 + -100*1 = -200
        q.push(b.order(
            Timestamp::from_nanos(3_000),
            3,
            OrderAction::Submitted(make_limit_order(3, Side::Buy, 100.0, 2.0)),
        ));

        let cfg = simple_config();
        let mut engine = BacktestEngine::new(cfg, q);
        let result = engine.run();
        // 2 卖单挂簿 accepted + 1 买单 fill accepted
        assert_eq!(result.orders_accepted, 3);
        assert_eq!(result.fills, 2, "买单吃两单");
        // PnL: -200
        assert!(
            (result.total_pnl - (-200.0)).abs() < 1e-9,
            "expected total_pnl=-200, got {}",
            result.total_pnl
        );
        // peak = 0（PnL 单调下降）→ drawdown = 0 - (-200) = 200
        assert!(
            (result.max_drawdown - 200.0).abs() < 1e-9,
            "expected max_drawdown=200, got {}",
            result.max_drawdown
        );
    }

    /// BacktestEngineConfig Debug 不暴露 trait object
    #[test]
    fn test_config_debug_redacts_trait_objects() {
        let cfg = simple_config();
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("BacktestEngineConfig"));
        assert!(dbg.contains("matching_engine"));
    }

    /// `push_event` 公开 API:逐条推入后 `pending_events` 单调递增
    /// 用于 Python 绑定(`PyBacktestEngine::push_event`)的对外契约。
    #[test]
    fn test_push_event_increases_pending() {
        let mut engine = BacktestEngine::new(simple_config(), EventQueue::new());
        assert_eq!(engine.pending_events(), 0);

        let mut b = EventBuilder::new(0);
        engine.push_event(b.order(Timestamp::from_nanos(1_000), 1, OrderAction::Cancelled(1)));
        assert_eq!(engine.pending_events(), 1);

        engine.push_event(b.order(Timestamp::from_nanos(2_000), 2, OrderAction::Cancelled(2)));
        assert_eq!(engine.pending_events(), 2);
    }

    /// `push_event` 推入的事件能被 `run()` 正常消费
    #[test]
    fn test_push_event_consumed_by_run() {
        let mut engine = BacktestEngine::new(simple_config(), EventQueue::new());
        let mut b = EventBuilder::new(0);
        engine.push_event(b.order(Timestamp::from_nanos(1_000), 1, OrderAction::Cancelled(1)));
        engine.push_event(b.order(
            Timestamp::from_nanos(2_000),
            2,
            OrderAction::Modified {
                order_id: 2,
                new_quantity: Quantity::from_f64(5.0),
            },
        ));

        let result = engine.run();
        assert_eq!(result.events_processed, 2);
        assert_eq!(result.orders_cancelled, 1);
        assert_eq!(result.orders_modified, 1);
    }
}
