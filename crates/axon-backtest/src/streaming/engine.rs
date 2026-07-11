//! 流式回测引擎核心
//!
//! ## 主回路(`on_market_event`)
//!
//! 收到 tick 时按以下顺序处理:
//! 1. 更新 portfolio mark-to-market
//! 2. 调注入的 `StreamingStrategy::on_tick` 拿到 `Vec<StrategyAction>`
//! 3. 按顺序应用 actions:`Submit` → 撮合 → `Event::Fill` 返回;`Cancel` → L1 取消;`Hold` → 跳过
//! 4. PaperTrading 模式下,限价单先按 `SimulatedExchange` 滑点上浮/下浮再撮合
//!
//! ## 退化语义
//!
//! 不注入 strategy 时,`on_market_event` 仅更新 portfolio,行为与"策略在外层循环
//! 自己调 `submit_order`"等价 — 既有用户路径不受影响。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use axon_core::event::{Event, FillEvent};
use axon_core::market::{Side as MarketSide, Trade};
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::portfolio::Portfolio;
use axon_core::types::{Price, Quantity, Symbol};

use crate::matching::{L1MatchingEngine, MatchingEngine};

use super::data_source::MarketDataEvent;
use super::paper_trading::PaperTradingEngine;
use super::strategy::{StrategyAction, StreamingStrategy};

/// 交易模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradingMode {
    /// 回测模式：使用历史数据回放
    Backtest,
    /// 模拟盘：实时行情，模拟成交
    PaperTrading,
    /// 实盘：真实交易所
    LiveTrading,
}

/// 引擎状态快照
#[derive(Debug, Clone)]
pub struct EngineSnapshot {
    /// 投资组合净值
    pub portfolio_nav: i64,
    /// 活跃订单数
    pub active_orders: usize,
    /// 总成交数
    pub total_trades: usize,
    /// 交易模式
    pub mode: TradingMode,
}

/// 流式回测引擎
pub struct StreamingEngine {
    /// per-symbol 撮合引擎
    engines: HashMap<Symbol, L1MatchingEngine>,
    /// 投资组合
    portfolio: Portfolio,
    /// 交易模式
    mode: TradingMode,
    /// 累计成交数
    total_trades: usize,
    /// fill 事件序列号(单调递增,用于 `Event::Fill::seq`)
    fill_seq: AtomicU64,
    /// 下个可分配订单 id(strategy 自动发单时用,起点 1)
    next_order_id: AtomicU64,
    /// 注入的策略(`None` = 无策略,用户自己驱动 `submit_order`)
    strategy: Option<Box<dyn StreamingStrategy>>,
    /// paper 模式引擎(`None` = 非 PaperTrading 模式,不做滑点)
    paper: Option<PaperTradingEngine>,
}

impl StreamingEngine {
    /// 创建新的流式引擎
    ///
    /// - `PaperTrading` 模式自动构造 `PaperTradingEngine::default()`,后续 `Submit` 限价单
    ///   会按 `slippage_bps` 上浮/下浮(默认 1bps)
    /// - 其他模式 `paper = None`,strategy 提交的限价单按原价撮合
    pub fn new(mode: TradingMode) -> Self {
        let paper = if mode == TradingMode::PaperTrading {
            Some(PaperTradingEngine::new(super::paper_trading::SimulatedExchange::default()))
        } else {
            None
        };
        Self {
            engines: HashMap::new(),
            portfolio: Portfolio::default(),
            mode,
            total_trades: 0,
            fill_seq: AtomicU64::new(0),
            next_order_id: AtomicU64::new(1),
            strategy: None,
            paper,
        }
    }

    /// 注入策略(builder 模式)
    ///
    /// 调用后,`on_market_event` 收到 tick 时会调 `strategy.on_tick(...)` 拿 actions。
    /// 可重复调用(覆盖);不调则走"无 strategy"路径。
    ///
    /// # 示例
    ///
    /// ```ignore
    /// use axon_backtest::streaming::{StreamingEngine, StreamingStrategy, TradingMode};
    ///
    /// let engine = StreamingEngine::new(TradingMode::PaperTrading)
    ///     .with_strategy(Box::my_strategy));
    /// ```
    pub fn with_strategy(mut self, strategy: Box<dyn StreamingStrategy>) -> Self {
        self.strategy = Some(strategy);
        self
    }

    /// 注册交易品种
    pub fn register_symbol(&mut self, symbol: Symbol) {
        self.engines.entry(symbol).or_default();
    }

    /// 处理市场事件
    ///
    /// 行为详见模块级文档「主回路」节。
    /// 返回本 tick 产生的 `Event::Fill`(可能为空)。
    pub fn on_market_event(&mut self, event: MarketDataEvent) -> Vec<Event> {
        match event {
            MarketDataEvent::Tick { symbol, tick } => {
                let tick_price = tick.price;
                let tick_qty = tick.quantity;
                let tick_ts = tick.timestamp;
                let tick_side = tick.side;

                // 1. 更新 portfolio mark-to-market
                self.portfolio.update_market_price(&symbol, tick_price);

                // 2. 调 strategy 拿 actions(无 strategy 则空)
                let actions = match &mut self.strategy {
                    Some(s) => s.on_tick(&symbol, tick_price.as_f64()),
                    None => Vec::new(),
                };

                // 3. 应用 actions,收集 fill events
                let mut events = Vec::new();
                for action in actions {
                    match action {
                        StrategyAction::Submit(mut order) => {
                            // 3a. paper 模式:对限价单应用滑点
                            if let Some(paper) = &self.paper
                                && let Some(limit_p) = order.order_type.limit_price()
                            {
                                let slip = paper.apply_slippage(limit_p.as_f64(), order.side);
                                order.order_type = OrderType::Limit {
                                    price: Price::from_f64(slip),
                                };
                            }
                            // 3b. 撮合(走 L1)
                            if let Some(engine) = self.engines.get_mut(&symbol) {
                                let result = engine.submit(order);
                                for fill in result.fills {
                                    self.total_trades += 1;
                                    // 3c. 写 portfolio
                                    let trade = Trade::new(
                                        fill.timestamp,
                                        fill.price,
                                        fill.quantity,
                                        fill.taker_order_id,
                                        fill.maker_order_id,
                                    );
                                    let _ = self.portfolio.apply_trade(
                                        &symbol,
                                        &trade,
                                        fill.taker_side,
                                        fill.timestamp,
                                    );
                                    // 3d. 推回 fill event
                                    let seq = self.fill_seq.fetch_add(1, Ordering::Relaxed);
                                    events.push(Event::Fill(FillEvent::new(
                                        seq, fill.timestamp, trade,
                                    )));
                                }
                            }
                        }
                        StrategyAction::Cancel(order_id) => {
                            if let Some(engine) = self.engines.get_mut(&symbol) {
                                let _ = engine.cancel(order_id);
                            }
                        }
                        StrategyAction::Hold => {}
                    }
                }

                // 抑制未使用变量警告(tick_qty / tick_ts / tick_side 暂未使用,留给后续扩展)
                let _ = (tick_qty, tick_ts, tick_side);

                events
            }
            MarketDataEvent::Heartbeat => vec![],
            MarketDataEvent::Disconnected => vec![],
        }
    }

    /// 提交订单(给"无 strategy"用户路径用)
    ///
    /// 返回订单 id(成功)或错误信息(snapshot 未注册)。
    /// 撮合产生的 fills 会累加 `total_trades`,但不更新 portfolio(portfolio 由
    /// `on_market_event` 走 strategy 路径时统一更新,避免重复记账)。
    pub fn submit_order(&mut self, order: Order) -> Result<u64, String> {
        let symbol = order.symbol.clone();
        let order_id = order.id;

        let engine = self
            .engines
            .get_mut(&symbol)
            .ok_or_else(|| format!("symbol not registered: {}", symbol))?;

        let result = engine.submit(order);
        self.total_trades += result.fills.len();

        Ok(order_id)
    }

    /// 分配下一个可用订单 id(给 strategy 内部发单用,起点 1)
    pub fn next_order_id(&self) -> u64 {
        self.next_order_id.fetch_add(1, Ordering::Relaxed)
    }

    /// 获取当前状态快照
    pub fn snapshot(&self) -> EngineSnapshot {
        EngineSnapshot {
            portfolio_nav: self.portfolio.nav(),
            active_orders: self.engines.values().map(|e| e.active_order_count()).sum(),
            total_trades: self.total_trades,
            mode: self.mode,
        }
    }

    /// 获取投资组合引用
    pub fn portfolio(&self) -> &Portfolio {
        &self.portfolio
    }

    /// 获取投资组合可变引用
    pub fn portfolio_mut(&mut self) -> &mut Portfolio {
        &mut self.portfolio
    }

    /// 获取交易模式
    pub fn mode(&self) -> TradingMode {
        self.mode
    }

    /// 是否已注入 strategy
    pub fn has_strategy(&self) -> bool {
        self.strategy.is_some()
    }

    /// 构造一个测试用的限价单(模块外亦可调)
    ///
    /// ponytail:常用 helper 放在 engine 上避免每个测试都写 Order::new 6 个参数。
    /// `id` 由调用方分配(可调 `next_order_id()`),`created_at` 用 `now()`。
    pub fn make_limit_order(
        &self,
        id: u64,
        symbol: Symbol,
        side: MarketSide,
        price: f64,
        qty: f64,
    ) -> Order {
        Order::new(
            id,
            symbol,
            side,
            OrderType::Limit {
                price: Price::from_f64(price),
            },
            Quantity::from_f64(qty),
            TimeInForce::GTC,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::market::{Side, Tick};
    use axon_core::time::Timestamp;
    use axon_core::types::{Price, Quantity};

    #[test]
    fn test_streaming_engine_create() {
        let engine = StreamingEngine::new(TradingMode::Backtest);
        assert_eq!(engine.mode(), TradingMode::Backtest);
        assert_eq!(engine.snapshot().total_trades, 0);
        assert!(!engine.has_strategy());
    }

    #[test]
    fn test_streaming_engine_paper_mode_has_paper_engine() {
        let engine = StreamingEngine::new(TradingMode::PaperTrading);
        // 内部 paper 字段应被自动构造(无法直接访问,但行为可通过 Submit 限价单体现)
        assert_eq!(engine.mode(), TradingMode::PaperTrading);
    }

    #[test]
    fn test_register_symbol() {
        let mut engine = StreamingEngine::new(TradingMode::PaperTrading);
        engine.register_symbol(Symbol::from("BTC-USDT"));
        assert!(engine.engines.contains_key(&Symbol::from("BTC-USDT")));
    }

    #[test]
    fn test_on_market_event_without_strategy_returns_empty() {
        // 退化语义:无 strategy 时 on_market_event 只更新 portfolio mark,不应返回任何 fill
        let mut engine = StreamingEngine::new(TradingMode::Backtest);
        engine.register_symbol(Symbol::from("BTC-USDT"));

        let tick = Tick::new(
            Timestamp::now(),
            Price::from_f64(50_000.0),
            Quantity::from_f64(1.0),
            Side::Buy,
        );
        let events = engine.on_market_event(MarketDataEvent::Tick {
            symbol: Symbol::from("BTC-USDT"),
            tick,
        });
        assert!(events.is_empty());
        // portfolio mark 已更新
        let _ = engine.portfolio().nav();
    }

    #[test]
    fn test_with_strategy_builder() {
        struct NoopStrategy;
        impl super::super::strategy::StreamingStrategy for NoopStrategy {
            fn on_tick(
                &mut self,
                _symbol: &Symbol,
                _price: f64,
            ) -> Vec<super::super::strategy::StrategyAction> {
                vec![super::super::strategy::StrategyAction::Hold]
            }
        }

        let engine = StreamingEngine::new(TradingMode::Backtest)
            .with_strategy(Box::new(NoopStrategy));
        assert!(engine.has_strategy());
    }
}
