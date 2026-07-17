//! 流式策略 trait
//!
//! `StreamingEngine::on_market_event` 收到 tick 时,会调注入的 `StreamingStrategy::on_tick`
//! 拿到 `StrategyAction` 列表,然后按顺序执行(Submit → 撮合 / Cancel / Hold)。
//!
//! ## 设计要点
//!
//! - **`Send` 约束**:`StreamingEngine` 后续可放进 PyO3 `#[pyclass]`,需 `Send`
//! - **`&mut self`**:策略内部有状态(如 SMA 窗口、订单簿快照),允许可变更
//! - **单方法 `on_tick`**:每次 tick 调用一次,返回 actions 列表
//!   (允许一次返回多个 action,如"平仓 A + 开仓 B",执行按返回顺序)
//! - **不感知 portfolio**:`StrategyAction` 只描述"做什么",不持有 portfolio 引用,
//!   portfolio 状态由 `StreamingEngine` 统一管理(避免借用冲突)
//!
//! ## 不注入 strategy 的退化语义
//!
//! `StreamingEngine::new(mode)` 不带 strategy 时,`on_market_event` 仅更新
//! `portfolio.update_market_price(...)`,不调 `submit_order`。
//! 用户可绕过 strategy 自己在外层循环调 `submit_order`。
//!
//! 运行:`cargo test -p axon-backtest --lib streaming::strategy::`

use axon_core::order::Order;
use axon_core::types::Symbol;

/// 策略对单个 tick 的反应
///
/// 每次 `StreamingEngine::on_market_event` 收到 tick 后,按以下顺序处理 actions:
/// 1. `Submit(order)` → 调 L1MatchingEngine::submit(order),产生 fills
/// 2. `Cancel(order_id)` → 调 L1MatchingEngine::cancel(order_id)
/// 3. `Hold` → 跳过
///
/// ponytail:`enum` 三态够用,无需 bitflags(actions 通常很短,< 10 个)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrategyAction {
    /// 提交订单(走 `L1MatchingEngine::submit` 路径)
    Submit(Order),
    /// 取消已有挂单(走 `L1MatchingEngine::cancel` 路径)
    Cancel(u64),
    /// 观望(无操作)
    Hold,
}

/// 流式策略 trait
///
/// 实现方需:
/// - 维护自己的内部状态(SMA 窗口、订单簿快照、风控阈值等)
/// - 在 `on_tick` 中根据 `(symbol, price)` 决策,返回 `StrategyAction` 列表
/// - 自行分配 `Order.id`(用 `Order::new` 时显式传入,避免与 engine 内部 ID 冲突)
pub trait StreamingStrategy: Send {
    /// 处理 tick,返回应执行的动作列表
    ///
    /// # 参数
    ///
    /// - `symbol`:tick 对应的交易品种
    /// - `price`:tick 成交价(`tick.price.as_f64()`,已从 `Price` 提取)
    ///
    /// # 返回
    ///
    /// `Vec<StrategyAction>`,按执行顺序排列(先返回的先执行)。
    /// 长度为 0 表示"本 tick 不做任何操作"(等价于一个 `Hold`)。
    fn on_tick(&mut self, symbol: &Symbol, price: f64) -> Vec<StrategyAction>;
}

/// SMA 均线交叉策略
///
/// 维护 short/long 两个滑动窗口,当 short > long 时返回 Buy(开仓),
/// 否则返回 Hold。**简化**:不跟踪已有持仓,持续产生 Buy(实战应加 state)。
///
/// 用途:streaming 链路端到端验证 + 示例策略。
pub struct SmaCrossover {
    /// 短期均线窗口大小
    pub short_win: usize,
    /// 长期均线窗口大小
    pub long_win: usize,
    /// 价格滑动窗口(最多保留 `long_win` 个元素)
    pub closes: std::collections::VecDeque<f64>,
    /// 下一个可分配的订单 id(自增)
    pub next_order_id: u64,
}

impl SmaCrossover {
    /// 创建 SMA 均线交叉策略
    ///
    /// `short_win` 和 `long_win` 分别为短期和长期均线的窗口大小。
    /// 当 `short_win >= long_win` 时策略退化(永远 Hold)。
    pub fn new(short_win: usize, long_win: usize) -> Self {
        Self {
            short_win,
            long_win,
            closes: std::collections::VecDeque::with_capacity(long_win),
            next_order_id: 1,
        }
    }

    fn sma(&self, win: usize) -> Option<f64> {
        if self.closes.len() < win {
            return None;
        }
        let sum: f64 = self.closes.iter().rev().take(win).sum();
        Some(sum / win as f64)
    }
}

impl StreamingStrategy for SmaCrossover {
    fn on_tick(&mut self, symbol: &Symbol, price: f64) -> Vec<StrategyAction> {
        self.closes.push_back(price);
        if self.closes.len() > self.long_win {
            self.closes.pop_front();
        }
        let short = self.sma(self.short_win);
        let long = self.sma(self.long_win);
        match (short, long) {
            (Some(s), Some(l)) if s > l => {
                // T2.2: 用 Order::spot 替代 Order::new,symbol 拆分 base/quote
                let s_str = symbol.as_str();
                let (base, quote) = match s_str.split_once('-').or_else(|| s_str.split_once('/')) {
                    Some((b, q)) => (axon_core::types::Symbol::from(b), axon_core::types::Symbol::from(q)),
                    None => (symbol.clone(), axon_core::types::Symbol::from("USDT")),
                };
                let order = Order::spot(
                    self.next_order_id,
                    base,
                    quote,
                    axon_core::market::Side::Buy,
                    axon_core::order::OrderType::Market,
                    axon_core::types::Quantity::from_f64(0.1),
                    axon_core::order::TimeInForce::IOC,
                );
                self.next_order_id += 1;
                vec![StrategyAction::Submit(order)]
            }
            _ => vec![StrategyAction::Hold],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 简单"固定动作"策略(供测试用)
    struct FixedStrategy {
        /// 下次 on_tick 返回的 action
        next: Vec<StrategyAction>,
    }

    impl FixedStrategy {
        fn new(next: Vec<StrategyAction>) -> Self {
            Self { next }
        }
    }

    impl StreamingStrategy for FixedStrategy {
        fn on_tick(&mut self, _symbol: &Symbol, _price: f64) -> Vec<StrategyAction> {
            std::mem::take(&mut self.next)
        }
    }

    #[test]
    fn fixed_strategy_returns_next_actions_and_clears() {
        let mut s = FixedStrategy::new(vec![StrategyAction::Hold]);
        let sym = Symbol::from("BTC-USDT");
        let actions = s.on_tick(&sym, 100.0);
        assert_eq!(actions, vec![StrategyAction::Hold]);
        // 第二次应返回空(已 clear)
        let actions2 = s.on_tick(&sym, 101.0);
        assert!(actions2.is_empty());
    }

    #[test]
    fn sma_crossover_emits_buy_after_uptrend() {
        let mut s = SmaCrossover::new(2, 3);
        let sym = Symbol::from("BTC-USDT");
        // 喂 4 个递增 tick,触发 short(2) > long(3)
        for price in [100.0, 101.0, 102.0, 103.0] {
            let actions = s.on_tick(&sym, price);
            if let StrategyAction::Submit(order) = &actions[0] {
                assert_eq!(order.side, axon_core::market::Side::Buy);
                return; // 成功
            }
        }
        panic!("uptrend 序列应触发至少 1 次 Buy");
    }
}
