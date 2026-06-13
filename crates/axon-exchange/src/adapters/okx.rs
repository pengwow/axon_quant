use std::collections::HashMap;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::error::ExchangeError;
use crate::traits::ExchangeAdapter;
use crate::types::{
    AccountBalance, DepthSnapshot, ExchangeConfig, ExchangeId, Order, OrderId, Position, Symbol,
    Ticker, WsMessage,
};

pub struct OkxAdapter {
    _config: ExchangeConfig,
    _market_tx: mpsc::Sender<WsMessage>,
    _market_rx: Option<mpsc::Receiver<WsMessage>>,
}

impl OkxAdapter {
    pub fn new(config: ExchangeConfig) -> Self {
        let (market_tx, market_rx) = mpsc::channel(1024);
        Self {
            _config: config,
            _market_tx: market_tx,
            _market_rx: Some(market_rx),
        }
    }
}

#[async_trait]
impl ExchangeAdapter for OkxAdapter {
    fn exchange_id(&self) -> ExchangeId {
        ExchangeId::Okx
    }

    async fn connect(&mut self) -> Result<(), ExchangeError> {
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ExchangeError> {
        Ok(())
    }

    async fn subscribe(&mut self, _symbols: &[Symbol]) -> Result<(), ExchangeError> {
        Ok(())
    }

    async fn send_order(&mut self, _order: Order) -> Result<OrderId, ExchangeError> {
        Err(ExchangeError::ConnectionFailed("not connected".into()))
    }

    async fn cancel_order(&mut self, _order_id: OrderId) -> Result<(), ExchangeError> {
        Err(ExchangeError::ConnectionFailed("not connected".into()))
    }

    async fn get_balance(&self) -> Result<HashMap<String, AccountBalance>, ExchangeError> {
        Err(ExchangeError::ConnectionFailed("not connected".into()))
    }

    async fn get_positions(&self) -> Result<Vec<Position>, ExchangeError> {
        Err(ExchangeError::ConnectionFailed("not connected".into()))
    }

    fn get_depth(&self, _symbol: &Symbol) -> Option<DepthSnapshot> {
        None
    }

    fn get_ticker(&self, _symbol: &Symbol) -> Option<Ticker> {
        None
    }

    fn market_data_rx(&self) -> mpsc::Receiver<WsMessage> {
        let (_tx, rx) = mpsc::channel(1);
        rx
    }
}
