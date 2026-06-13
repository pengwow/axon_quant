use std::sync::Arc;

use axon_core::event::{Event, EventHandler, EventType};
use axon_core::portfolio::Portfolio;
use parking_lot::RwLock;

use crate::engine::RiskEngine;

pub struct RiskEventHandler {
    engine: Arc<dyn RiskEngine>,
    _portfolio: Arc<RwLock<Portfolio>>,
}

impl RiskEventHandler {
    pub fn new(engine: Arc<dyn RiskEngine>, portfolio: Arc<RwLock<Portfolio>>) -> Self {
        Self {
            engine,
            _portfolio: portfolio,
        }
    }
}

impl EventHandler for RiskEventHandler {
    fn on_event(&mut self, event: &Event) {
        if let Event::Fill(fill) = event {
            let pnl = fill.trade.turnover();
            self.engine.update_daily_pnl(pnl);
        }
    }

    fn event_types(&self) -> EventType {
        EventType::FILL
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DefaultRiskEngine;
    use crate::config::RiskConfig;

    #[test]
    fn test_handler_creation() {
        let engine = Arc::new(DefaultRiskEngine::new(RiskConfig::default()));
        let portfolio = Arc::new(RwLock::new(Portfolio::default()));
        let _handler = RiskEventHandler::new(engine, portfolio);
    }
}
