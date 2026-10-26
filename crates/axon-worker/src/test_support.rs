//! crate 内测试共享的交易所 mock（仅 `#[cfg(test)]` 编译）
//!
//! 两个消费方：`exchange_gateway`（fan-out / 回报转换）与 `live_driver`
//! （崩溃恢复全链路演练）。设计原则是「测试可见的假交易所」：
//!
//! - 所有账本状态集中在 [`MockState`]，测试经 [`MockHandle`] 直接读写
//!   （adapter 被 move 进网关后不再持有具体类型，state Arc 是测试侧的
//!   唯一控制通道，这也顺便验证了网关与 REST 快照共享同一 adapter）；
//! - WS 消息由测试经 [`MockHandle::push_message`] 注入，模拟交易所推送；
//! - `send_order` 只记录不成交——成交必须由测试显式注入 `OrderUpdate`，
//!   时间序完全可预测，方便做崩溃恢复演练。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_decimal::Decimal;
use tokio::sync::mpsc;

use axon_core::Bar;
use axon_exchange::ExchangeAdapter;
use axon_exchange::error::ExchangeError;
use axon_exchange::types::{
    AccountBalance, AccountInfo, DepthSnapshot, ExchangeId, FundingRate, LeverageBracket,
    LongShortRatio, MarginType, OpenInterest, Order, OrderId, Position, PositionMode, Symbol,
    Ticker, UserTrade, WsMessage,
};

/// mock 的全部账本状态（测试经 [`MockHandle`] 直接读写）
pub(crate) struct MockState {
    /// REST 持仓快照（`get_positions` 返回）
    pub positions: Vec<Position>,
    /// REST 账户快照（`get_account_info` 返回；默认 10000 余额）
    pub account: AccountInfo,
    /// REST 成交流水（`fetch_my_trades` 返回；trade_id 必须与 WS 回报同源）
    pub trades: Vec<UserTrade>,
    /// 已受理订单（`send_order` 记录，供断言）
    pub sent_orders: Vec<Order>,
    /// REST 历史 K 线（`fetch_ohlcv` 返回；缺口补灌演练用）
    pub ohlcv: Vec<Bar>,
    /// 测试注入「交易所侧」消息的发送端
    message_tx: mpsc::Sender<WsMessage>,
}

/// 测试侧对 mock 账本的操作句柄
pub(crate) struct MockHandle(Arc<Mutex<MockState>>);

impl MockHandle {
    /// 加锁拿全部状态（直接改字段 / 读字段）
    pub(crate) fn state(&self) -> std::sync::MutexGuard<'_, MockState> {
        self.0.lock().unwrap()
    }

    /// 注入一条「交易所推送」消息
    ///
    /// 通道 256 容量远大于测试注入量；塞满 panic 属于测试自身 bug。
    pub(crate) fn push_message(&self, message: WsMessage) {
        self.state()
            .message_tx
            .try_send(message)
            .expect("mock 消息通道不应塞满");
    }
}

/// 造一个「adapter Arc + 测试句柄」对
///
/// adapter 交给网关（WS 流 + REST 快照都从它走），handle 留在测试里模拟
/// 交易所侧行为——两者看到的是同一份状态。
pub(crate) fn mock_exchange(
    exchange_id: ExchangeId,
) -> (
    Arc<tokio::sync::Mutex<Box<dyn ExchangeAdapter>>>,
    MockHandle,
) {
    let (message_tx, rx) = mpsc::channel(256);
    let state = Arc::new(Mutex::new(MockState {
        positions: Vec::new(),
        account: AccountInfo {
            total_balance: Decimal::from(10_000),
            available_balance: Decimal::from(10_000),
            unrealized_pnl: Decimal::ZERO,
            margin_used: Decimal::ZERO,
            initial_margin: Decimal::ZERO,
            maintenance_margin: Decimal::ZERO,
            position_mode: PositionMode::Net,
            as_of_ms: 0,
        },
        trades: Vec::new(),
        sent_orders: Vec::new(),
        ohlcv: Vec::new(),
        message_tx,
    }));
    let adapter = MockAdapter {
        exchange_id,
        market_data_rx: Mutex::new(Some(rx)),
        state: Arc::clone(&state),
    };
    (
        Arc::new(tokio::sync::Mutex::new(Box::new(adapter))),
        MockHandle(state),
    )
}

/// 最小可用的交易所适配器 mock
struct MockAdapter {
    exchange_id: ExchangeId,
    /// 行情 / 回报读端（`market_data_rx(&self)` 是借用，单次消费用内部可变性表达）
    market_data_rx: Mutex<Option<mpsc::Receiver<WsMessage>>>,
    /// 与 [`MockHandle`] 共享的账本
    state: Arc<Mutex<MockState>>,
}

#[async_trait]
impl ExchangeAdapter for MockAdapter {
    fn exchange_id(&self) -> ExchangeId {
        self.exchange_id
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

    async fn send_order(&mut self, order: Order) -> Result<OrderId, ExchangeError> {
        // 真实 adapter 只透传 client order id（交易所侧 orderId 被吞），mock 同样口径。
        let client_order_id = order.client_order_id;
        self.state.lock().unwrap().sent_orders.push(order);
        Ok(client_order_id)
    }

    async fn cancel_order(&mut self, _order_id: OrderId) -> Result<(), ExchangeError> {
        Ok(())
    }

    async fn get_balance(&self) -> Result<HashMap<String, AccountBalance>, ExchangeError> {
        Ok(HashMap::new())
    }

    async fn get_positions(&self) -> Result<Vec<Position>, ExchangeError> {
        Ok(self.state.lock().unwrap().positions.clone())
    }

    fn get_depth(&self, _symbol: &Symbol) -> Option<DepthSnapshot> {
        None
    }

    fn get_ticker(&self, _symbol: &Symbol) -> Option<Ticker> {
        None
    }

    fn market_data_rx(&self) -> mpsc::Receiver<WsMessage> {
        self.market_data_rx
            .lock()
            .unwrap()
            .take()
            .expect("market_data_rx 只能取一次（单消费者语义）")
    }

    // === 未用能力：返回显式错误，被误用到测试里立刻暴露 ===

    async fn set_leverage(&self, _symbol: &str, _leverage: u8) -> Result<(), ExchangeError> {
        Err(ExchangeError::ApiError {
            code: -1,
            message: "mock adapter 不支持杠杆".into(),
        })
    }

    async fn set_margin_type(
        &self,
        _symbol: &str,
        _margin_type: MarginType,
    ) -> Result<(), ExchangeError> {
        Err(ExchangeError::ApiError {
            code: -1,
            message: "mock adapter 不支持保证金模式".into(),
        })
    }

    async fn get_leverage_brackets(
        &self,
        _symbol: &str,
    ) -> Result<Vec<LeverageBracket>, ExchangeError> {
        Err(ExchangeError::ApiError {
            code: -1,
            message: "mock adapter 不支持杠杆分层".into(),
        })
    }

    async fn set_position_mode(&self, _hedge_mode: bool) -> Result<(), ExchangeError> {
        Err(ExchangeError::ApiError {
            code: -1,
            message: "mock adapter 不支持持仓模式".into(),
        })
    }

    async fn get_funding_rate(&self, _symbol: &str) -> Result<FundingRate, ExchangeError> {
        Err(ExchangeError::ApiError {
            code: -1,
            message: "mock adapter 不支持资金费率".into(),
        })
    }

    async fn get_account_info(&self) -> Result<AccountInfo, ExchangeError> {
        Ok(self.state.lock().unwrap().account.clone())
    }

    async fn get_open_interest(&self, _symbol: &str) -> Result<OpenInterest, ExchangeError> {
        Err(ExchangeError::ApiError {
            code: -1,
            message: "mock adapter 不支持持仓量".into(),
        })
    }

    async fn get_long_short_ratio(&self, _symbol: &str) -> Result<LongShortRatio, ExchangeError> {
        Err(ExchangeError::ApiError {
            code: -1,
            message: "mock adapter 不支持多空比".into(),
        })
    }

    async fn fetch_ohlcv(
        &self,
        _symbol: &Symbol,
        _interval: &str,
        _limit: usize,
    ) -> Result<Vec<Bar>, ExchangeError> {
        Ok(self.state.lock().unwrap().ohlcv.clone())
    }

    /// REST 逐笔成交：全量返回（`since` 过滤需要时测试侧再补）
    async fn fetch_my_trades(
        &self,
        _symbol: &Symbol,
        _since_ms: Option<i64>,
        _limit: u32,
    ) -> Result<Vec<UserTrade>, ExchangeError> {
        Ok(self.state.lock().unwrap().trades.clone())
    }
}
