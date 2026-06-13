use std::collections::HashMap;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::error::ExchangeError;
use crate::types::{
    AccountBalance, DepthSnapshot, ExchangeId, Order, OrderId, Position, Symbol, Ticker, WsMessage,
};

#[async_trait]
pub trait ExchangeAdapter: Send + Sync {
    fn exchange_id(&self) -> ExchangeId;
    async fn connect(&mut self) -> Result<(), ExchangeError>;
    async fn disconnect(&mut self) -> Result<(), ExchangeError>;
    async fn subscribe(&mut self, symbols: &[Symbol]) -> Result<(), ExchangeError>;
    async fn send_order(&mut self, order: Order) -> Result<OrderId, ExchangeError>;
    async fn cancel_order(&mut self, order_id: OrderId) -> Result<(), ExchangeError>;
    async fn get_balance(&self) -> Result<HashMap<String, AccountBalance>, ExchangeError>;
    async fn get_positions(&self) -> Result<Vec<Position>, ExchangeError>;
    fn get_depth(&self, symbol: &Symbol) -> Option<DepthSnapshot>;
    fn get_ticker(&self, symbol: &Symbol) -> Option<Ticker>;
    fn market_data_rx(&self) -> mpsc::Receiver<WsMessage>;
}
