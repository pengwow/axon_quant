//! 流式回测引擎核心

use std::collections::HashMap;

use axon_core::event::Event;
use axon_core::order::{Order, OrderId};
use axon_core::portfolio::Portfolio;
use axon_core::types::Symbol;

use crate::matching::{L1MatchingEngine, MatchingEngine};

use super::data_source::MarketDataEvent;

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
    engines: HashMap<Symbol, L1MatchingEngine>,
    portfolio: Portfolio,
    mode: TradingMode,
    total_trades: usize,
}

impl StreamingEngine {
    /// 创建新的流式引擎
    pub fn new(mode: TradingMode) -> Self {
        Self {
            engines: HashMap::new(),
            portfolio: Portfolio::default(),
            mode,
            total_trades: 0,
        }
    }

    /// 注册交易品种
    pub fn register_symbol(&mut self, symbol: Symbol) {
        self.engines.entry(symbol).or_default();
    }

    /// 处理市场事件
    pub fn on_market_event(&mut self, event: MarketDataEvent) -> Vec<Event> {
        match event {
            MarketDataEvent::Tick { symbol, tick } => {
                // 更新投资组合中的市场价格
                self.portfolio.update_market_price(&symbol, tick.price);

                // 如果有对应的撮合引擎，处理待定订单
                if let Some(_engine) = self.engines.get_mut(&symbol) {
                    // TODO: 触发待定订单的撮合
                }

                vec![]
            }
            MarketDataEvent::Heartbeat => vec![],
            MarketDataEvent::Disconnected => vec![],
        }
    }

    /// 提交订单
    pub fn submit_order(&mut self, order: Order) -> Result<OrderId, String> {
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
    }

    #[test]
    fn test_register_symbol() {
        let mut engine = StreamingEngine::new(TradingMode::PaperTrading);
        engine.register_symbol(Symbol::from("BTC-USDT"));
        assert!(engine.engines.contains_key(&Symbol::from("BTC-USDT")));
    }

    #[test]
    fn test_on_market_event() {
        let mut engine = StreamingEngine::new(TradingMode::Backtest);
        engine.register_symbol(Symbol::from("BTC-USDT"));

        let tick = Tick::new(
            Timestamp::now(),
            Price::from_f64(50000.0),
            Quantity::from_f64(1.0),
            Side::Buy,
        );

        let events = engine.on_market_event(MarketDataEvent::Tick {
            symbol: Symbol::from("BTC-USDT"),
            tick,
        });
        assert!(events.is_empty()); // 目前返回空事件
    }
}
