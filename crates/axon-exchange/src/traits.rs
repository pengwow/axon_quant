use std::collections::HashMap;

use async_trait::async_trait;
use tokio::sync::mpsc;

use axon_core::Bar;

use crate::error::ExchangeError;
use crate::types::{
    AccountBalance, AccountInfo, DepthSnapshot, ExchangeId, FundingRate, LeverageBracket,
    LongShortRatio, MarginType, OpenInterest, Order, OrderId, Position, Symbol, Ticker, UserTrade,
    WsMessage,
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

    // === 杠杆/合约(Stage 4' D 新增)===
    // 现货交易所对以下方法返回 `ExchangeError::OrderRejected` 或类似语义错误;
    // Binance USDⓈ-M + OKX V5 适配器提供完整实现。

    /// 设置杠杆倍数(合约 1-125x)
    async fn set_leverage(&self, symbol: &str, leverage: u8) -> Result<(), ExchangeError>;

    /// 设置保证金模式(逐仓 / 全仓)
    async fn set_margin_type(
        &self,
        symbol: &str,
        margin_type: MarginType,
    ) -> Result<(), ExchangeError>;

    /// 获取杠杆分层(每个 symbol 的 max_notional 上限)
    async fn get_leverage_brackets(
        &self,
        symbol: &str,
    ) -> Result<Vec<LeverageBracket>, ExchangeError>;

    /// 设置持仓模式(`true`=对冲 hedge,`false`=单向 net)
    async fn set_position_mode(&self, hedge_mode: bool) -> Result<(), ExchangeError>;

    /// 获取资金费率(永续合约 8h 结算)
    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate, ExchangeError>;

    /// 获取完整账户信息(余额 + 未实现盈亏 + 保证金占用)
    async fn get_account_info(&self) -> Result<AccountInfo, ExchangeError>;

    /// 获取持仓量(未平仓合约数,市场情绪指标)
    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest, ExchangeError>;

    /// 获取多空账户比(主动买入/卖出成交量比,市场情绪指标)
    async fn get_long_short_ratio(&self, symbol: &str) -> Result<LongShortRatio, ExchangeError>;

    /// 获取历史 K 线（实盘启动时填充 KlineBuffer）
    ///
    /// Args:
    /// - `symbol`: 交易对
    /// - `interval`: K 线周期字符串（"1m", "5m", "1h", "4h", "1d"）
    /// - `limit`: 返回根数（交易所上限通常 1000）
    async fn fetch_ohlcv(
        &self,
        symbol: &Symbol,
        interval: &str,
        limit: usize,
    ) -> Result<Vec<Bar>, ExchangeError>;

    /// 拉取账户成交明细（REST 独立数据源，用于成交对账 / 断线补齐）
    ///
    /// 默认实现返回错误：adapter 不带成交通道（如 axon-llm 的 mock），
    /// 用到成交对账的消费方必须自行实现或显式不支持。
    ///
    /// Args:
    /// - `symbol`: 交易对
    /// - `since_ms`: 起始时间（毫秒，含）；不传则返回最近 `limit` 笔
    /// - `limit`: 返回条数上限（交易所通常 1000）
    async fn fetch_my_trades(
        &self,
        symbol: &Symbol,
        since_ms: Option<i64>,
        limit: u32,
    ) -> Result<Vec<UserTrade>, ExchangeError> {
        let _ = (symbol, since_ms, limit);
        Err(ExchangeError::ApiError {
            code: -1,
            message: "fetch_my_trades not implemented for this adapter".into(),
        })
    }
}
