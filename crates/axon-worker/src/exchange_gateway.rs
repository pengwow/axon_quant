//! async `ExchangeAdapter` → sync [`LiveOrderGateway`] 桥接（M5 testnet 阶段）
//!
//! 内核主回路是同步的（见 [`crate::live_sink`] 模块文档），而
//! [`axon_exchange::ExchangeAdapter`] 全是 async。本模块用一个内部 tokio
//! Runtime + `block_on` 把内核需要的三件事桥成同步：下单、撤单、拉回报。
//!
//! # 两条必须知道的约束
//!
//! 1. **只在同步上下文使用**：`place` / `cancel` / `poll_updates` 内部走
//!    `block_on` + `blocking_lock`，在 async 任务里调用会 panic。构造函数同理，
//!    必须在 runtime 之外调用（它会创建自己的 Runtime，并在此时取走 adapter 的
//!    单消费者行情通道 —— `market_data_rx` 内部用 `blocking_lock`，放在 async
//!    的 `connect` 里取会在 runtime 内 panic）。
//! 2. **累计量要转成增量**：交易所回报的是**累计**成交量，而
//!    [`VenueUpdate::fill_quantity`] 要求**本次增量**。转换状态存在 `last_filled`
//!    里，按 OMS 订单号累积；累计量没有增长的回报（重复投递、撤单回报）直接跳过。
//!
//! # 已知缺口（testnet 冒烟会暴露，接线前必须补）
//!
//! - `ExchangeAdapter::send_order` 只回传 client order id（交易所侧 orderId 被
//!   adapter 内部日志吞掉），所以 [`GatewayAck::venue_order_id`] 填的是 client
//!   order id；
//! - `axon_exchange::OrderUpdate` 的手续费来自合约回报的 `n` 字段（本笔增量），
//!   [`VenueUpdate::fee`] 直接透传；现货 `executionReport` 因返佣币可能是 BNB，
//!   adapter 填 0，现货影子账本仍会少记手续费；
//! - 「网关独占行情通道」已由 [`ExchangeGateway::with_market_data_fanout`] 解决：live 主
//!   回路接上 fan-out 通道后，`poll_updates` 把行情类消息转发给 [`crate::KernelPump`]，
//!   网关只留下回报；未接 fan-out（paper 等独立使用）时行情直接丢弃。
//!
//! 崩溃恢复编排（[`crate::live_driver`]）还需要拿 REST 快照对账，本模块把
//! `get_positions` / `get_account_info` / `fetch_my_trades` 包成了同步方法；
//! REST 快照与 WS 回报流共享同一个 adapter（[`ExchangeGateway::shared`]），不会多建连接。
//!
//! Binance 合约的 user data stream（`ORDER_TRADE_UPDATE`）已在 adapter 内实现，
//! 回报与行情共用同一条通道，因此上面的「单消费者」约束仍然成立。

use std::collections::HashMap;
use std::sync::Arc;

use axon_core::{Bar, Timestamp};
use axon_exchange::ExchangeAdapter;
use axon_exchange::types::{
    AccountInfo, ExchangeId, Order as ExchangeOrder, OrderId as ExchangeOrderId,
    OrderStatus as ExchangeOrderStatus, OrderType as ExchangeOrderType, Position,
    Side as ExchangeSide, Symbol as ExchangeSymbol, Ticker, TimeInForce, UserTrade, WsMessage,
};
use axon_oms::{
    Order as OmsOrder, OrderId as OmsOrderId, OrderStatus as OmsOrderStatus,
    OrderType as OmsOrderType, Side as OmsSide,
};
use rust_decimal::Decimal;
use tokio::runtime::Runtime;
use tokio::sync::{Mutex, mpsc};

use crate::live_sink::{GatewayAck, LiveOrderGateway, VenueUpdate};

/// 把 async 交易所适配器桥成内核要的同步网关
pub struct ExchangeGateway {
    adapter: Arc<Mutex<Box<dyn ExchangeAdapter>>>,
    runtime: Runtime,
    /// adapter 唯一的行情 / 回报通道（构造时取走，之后只在这里 drain）
    updates: Option<mpsc::Receiver<WsMessage>>,
    /// 行情 fan-out 通道（`with_market_data_fanout` 接上；KernelPump 在另一端消费）
    market_data_tx: Option<mpsc::Sender<WsMessage>>,
    /// OMS 订单号 → 已回报的**累计**成交量（累计 → 增量的转换状态）
    last_filled: HashMap<String, Decimal>,
}

impl ExchangeGateway {
    /// 创建网关
    ///
    /// 必须在 runtime 之外调用：内部会创建 Runtime，并取走 adapter 的
    /// `market_data_rx`（单消费者通道，且实现里用 `blocking_lock`）。
    pub fn new(adapter: Box<dyn ExchangeAdapter>) -> Result<Self, String> {
        Self::shared(Arc::new(Mutex::new(adapter)))
    }

    /// 从已有 adapter Arc 创建网关
    ///
    /// 崩溃恢复编排（`live_driver`）要同时拿 WS 回报流（本网关）和 REST 快照
    /// （对账），两者必须共享同一个 adapter，所以支持从外部传入 Arc。
    /// 同样必须在 runtime 之外调用（`blocking_lock` + 创建 Runtime）。
    pub fn shared(adapter: Arc<Mutex<Box<dyn ExchangeAdapter>>>) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("构建 tokio 运行时失败: {e}"))?;
        // 同步上下文取通道：adapter 的 `market_data_rx` 用 `blocking_lock`，
        // 在 async 任务里调用会 panic，所以只能在这里取一次。
        let updates = adapter.blocking_lock().market_data_rx();
        Ok(Self {
            adapter,
            runtime,
            updates: Some(updates),
            market_data_tx: None,
            last_filled: HashMap::new(),
        })
    }

    /// 接上行情 fan-out 通道（live 主回路必用）
    ///
    /// 接上后 `poll_updates` 会把行情类消息（Kline / Trade / Depth 等）转到
    /// 该通道，由 [`crate::KernelPump`] 消费；网关只保留回报。不接则行情直接
    /// 丢弃（paper 等独立使用网关的场景）。
    pub fn with_market_data_fanout(mut self, tx: mpsc::Sender<WsMessage>) -> Self {
        self.market_data_tx = Some(tx);
        self
    }

    /// 建立交易所连接（REST 探活 + 启动 WS 监督任务）
    ///
    /// `async` 是为了在已有 runtime 里直接复用 adapter 的连接逻辑；本模块的其它
    /// 方法都是同步的（内部 `block_on`）。
    pub async fn connect(&mut self) -> Result<(), String> {
        let mut adapter = self.adapter.lock().await;
        adapter
            .connect()
            .await
            .map_err(|e| format!("adapter connect: {e}"))
    }

    /// [`Self::connect`] 的同步包装（LiveDriver 等同步编排器在无 tokio
    /// 上下文的线程上调用）
    ///
    /// 不能直接 `block_on(self.connect())`：`connect` 的 future 借 `&mut self`，
    /// 而 `self.runtime` 位于 `&self` 字段上——receiver 双重借用整体冲突。先取
    /// guard 把字段投影抽出来，与 [`Self::subscribe`] 同型。
    pub fn connect_sync(&mut self) -> Result<(), String> {
        let mut adapter = self.adapter.blocking_lock();
        self.runtime
            .block_on(adapter.connect())
            .map_err(|e| format!("adapter connect: {e}"))
    }

    /// 订阅行情（WS 建立后才有实际效果；ticker / 成交推送依赖它）
    pub fn subscribe(&mut self, symbols: &[ExchangeSymbol]) -> Result<(), String> {
        let mut adapter = self.adapter.blocking_lock();
        self.runtime
            .block_on(adapter.subscribe(symbols))
            .map_err(|e| format!("adapter subscribe: {e}"))
    }

    /// 最新报价（读 adapter 的 WS ticker 缓存；未订阅或尚未收到推送时为 `None`）
    pub fn ticker(&self, symbol: &ExchangeSymbol) -> Option<Ticker> {
        self.adapter.blocking_lock().get_ticker(symbol)
    }

    /// 历史 K 线（同步包装 `adapter.fetch_ohlcv`）
    pub fn fetch_ohlcv(
        &self,
        symbol: &ExchangeSymbol,
        interval: &str,
        limit: usize,
    ) -> Result<Vec<Bar>, String> {
        let adapter = self.adapter.blocking_lock();
        self.runtime
            .block_on(adapter.fetch_ohlcv(symbol, interval, limit))
            .map_err(|e| format!("adapter fetch_ohlcv: {e}"))
    }

    /// 全部持仓快照（崩溃恢复对账用）
    pub fn positions(&self) -> Result<Vec<Position>, String> {
        let adapter = self.adapter.blocking_lock();
        self.runtime
            .block_on(adapter.get_positions())
            .map_err(|e| format!("adapter get_positions: {e}"))
    }

    /// 账户信息快照（钱包口径现金对账用；`cash = total_balance - unrealized_pnl`）
    pub fn account_info(&self) -> Result<AccountInfo, String> {
        let adapter = self.adapter.blocking_lock();
        self.runtime
            .block_on(adapter.get_account_info())
            .map_err(|e| format!("adapter get_account_info: {e}"))
    }

    /// 账户成交明细（成交对账用；对应 `fill_ids` 差异核对）
    pub fn fetch_my_trades(
        &self,
        symbol: &ExchangeSymbol,
        since_ms: Option<i64>,
        limit: u32,
    ) -> Result<Vec<UserTrade>, String> {
        let adapter = self.adapter.blocking_lock();
        self.runtime
            .block_on(adapter.fetch_my_trades(symbol, since_ms, limit))
            .map_err(|e| format!("adapter fetch_my_trades: {e}"))
    }

    /// 行情消息转入 fan-out 通道（poll_updates 用）
    ///
    /// 通道满 / 接收端已关闭直接丢弃：行情是时效信息，下一根 bar 会再来，
    /// 丢消息远好于阻塞 adapter 读循环。未接 fan-out 时不处理（独立网关用法）。
    fn forward_market_data(&mut self, message: WsMessage) {
        if let Some(tx) = self.market_data_tx.as_ref() {
            // Full / Closed 都按丢消息处理（Full = 下游没排空，Closed = 下游已撤）。
            let _ = tx.try_send(message);
        }
    }
}

impl LiveOrderGateway for ExchangeGateway {
    fn place(&mut self, order: &OmsOrder) -> Result<GatewayAck, String> {
        let mut adapter = self.adapter.blocking_lock();
        let exchange = adapter.exchange_id();
        let request = to_exchange_order(order, exchange)?;
        let accepted = self
            .runtime
            .block_on(adapter.send_order(request))
            .map_err(|e| format!("adapter send_order: {e}"))?;
        Ok(GatewayAck {
            // adapter 只回传 client order id（交易所侧 orderId 未透出），见模块文档。
            venue_order_id: accepted.to_string(),
            // REST 已受理，但 adapter 丢掉了响应里的初始状态，只能按「已确认」记账。
            status: OmsOrderStatus::Acknowledged,
        })
    }

    fn cancel(&mut self, id: &OmsOrderId) -> Result<(), String> {
        // OMS 订单号就是下单时用的 client order id，撤单直接按它反查。
        let mut adapter = self.adapter.blocking_lock();
        self.runtime
            .block_on(adapter.cancel_order(ExchangeOrderId(id.0)))
            .map_err(|e| format!("adapter cancel_order: {e}"))
    }

    fn poll_updates(&mut self) -> Result<Vec<VenueUpdate>, String> {
        // 先把通道 drain 到本地，再逐条分发：转发行情要再借 self（fan-out 通道），
        // 直接在 drain 循环里调方法会撞上首个 &mut 借用（单 tick 至多 4096 条）。
        let mut messages = Vec::new();
        if let Some(updates) = self.updates.as_mut() {
            while let Ok(message) = updates.try_recv() {
                messages.push(message);
            }
        }
        let mut out = Vec::new();
        for message in messages {
            match message {
                WsMessage::OrderUpdate(update) => {
                    if let Some(venue_update) = convert(&mut self.last_filled, update) {
                        out.push(venue_update);
                    }
                }
                // 行情类消息归 KernelPump（fan-out 通道），网关不消费。
                other => self.forward_market_data(other),
            }
        }
        Ok(out)
    }

    fn disconnect(&mut self) -> Result<(), String> {
        let mut adapter = self.adapter.blocking_lock();
        self.runtime
            .block_on(adapter.disconnect())
            .map_err(|e| format!("adapter disconnect: {e}"))
    }
}

/// 交易所回报 → [`VenueUpdate`]，累计成交量转成增量
///
/// `trade_id` 优先透传 adapter 归一后的原生成交编号；回报不带成交号的交易所
/// 用 `订单号:纳秒时间戳:本次增量` 合成：而 `LiveSink` 靠 `trade_id` 去重。
/// 这个三元组在「同一订单 + 同一纳秒 + 同一增量」上唯一 —— 重复投递得到
/// 完全相同的键，会被 sink 丢弃；代价是同一订单在同一纳秒内两笔等量成交会被
/// 误判为重复，方向是**宁可漏记不重复记账**，与 [`crate::live_sink`] 的硬约束
/// 一致。
fn convert(
    last_filled: &mut HashMap<String, Decimal>,
    update: axon_exchange::OrderUpdate,
) -> Option<VenueUpdate> {
    // client_order_id 解析失败时 adapter 会回落成 nil uuid，无法归因到 OMS 订单。
    if update.client_order_id.0.is_nil() {
        tracing::warn!("exchange gateway: 回报缺 client_order_id，丢弃");
        return None;
    }
    let order_id = OmsOrderId(update.client_order_id.0);
    let key = order_id.to_string();
    let previous = last_filled.get(&key).copied().unwrap_or(Decimal::ZERO);
    let delta = update.filled_qty - previous;
    let fill_quantity = if delta > Decimal::ZERO {
        last_filled.insert(key, update.filled_qty);
        delta
    } else {
        Decimal::ZERO
    };
    let timestamp = Timestamp::from_datetime(&update.timestamp);
    let trade_id = (fill_quantity > Decimal::ZERO).then(|| {
        // adapter 透出的原生成交号（如 Binance 合约 `binfut:{symbol}:{id}`）优先：
        // 与 REST userTrades 侧同源，成交对账两侧天然得到同一个键。回报不带
        // 成交号的交易所回退到旧合成格式。
        update
            .trade_id
            .clone()
            .unwrap_or_else(|| format!("{order_id}:{}:{fill_quantity}", timestamp.nanos))
    });
    Some(VenueUpdate {
        order_id,
        status: map_status(update.status),
        fill_quantity,
        // 交易所只给累计均价；单订单内价格差异通常极小，按均价记本次成交。
        fill_price: update.avg_price.unwrap_or(Decimal::ZERO),
        // `n` 已由 adapter 归一成「本笔增量」的手续费，可直接进钱包口径账本。
        fee: update.fee,
        trade_id,
        timestamp,
    })
}

/// OMS 订单 → 交易所订单
///
/// 现货口径：`instrument_id` 是 `"BTC/USDT"`，去掉 `/` 得到 Binance 的
/// `"BTCUSDT"`；合约 / OKX 的 symbol 口径不同，等接第二个交易所时再抽象。
fn to_exchange_order(order: &OmsOrder, exchange: ExchangeId) -> Result<ExchangeOrder, String> {
    let symbol = order.instrument_id.replace('/', "");
    if symbol.is_empty() {
        return Err(format!("订单 {} 缺 instrument_id", order.id));
    }
    let side = match order.side {
        OmsSide::Buy => ExchangeSide::Buy,
        OmsSide::Sell => ExchangeSide::Sell,
    };
    let order_type = match order.order_type {
        OmsOrderType::Limit => ExchangeOrderType::Limit,
        OmsOrderType::Market => ExchangeOrderType::Market,
        OmsOrderType::StopLoss => ExchangeOrderType::StopLoss,
        OmsOrderType::StopLimit => ExchangeOrderType::StopLimit,
    };
    // 市价单不能带价格：OMS 记录里的价格只是参考收盘价。
    let price = match order.order_type {
        OmsOrderType::Market => None,
        _ => Some(order.price),
    };
    Ok(ExchangeOrder {
        // OMS 订单号直接当 client order id：撤单 / 回报归因都靠它。
        client_order_id: ExchangeOrderId(order.id.0),
        symbol: ExchangeSymbol::new(symbol),
        side,
        order_type,
        price,
        quantity: order.quantity,
        time_in_force: TimeInForce::Gtc,
        exchange,
        meta: HashMap::new(),
    })
}

/// 交易所订单状态 → OMS 订单状态
fn map_status(status: ExchangeOrderStatus) -> OmsOrderStatus {
    match status {
        // `Pending` / `Sent` 都没进 OMS 的可见状态，按已提交处理。
        ExchangeOrderStatus::Pending | ExchangeOrderStatus::Sent => OmsOrderStatus::Submitted,
        ExchangeOrderStatus::Acknowledged => OmsOrderStatus::Acknowledged,
        ExchangeOrderStatus::PartiallyFilled {
            filled_qty,
            avg_price,
        } => OmsOrderStatus::PartiallyFilled {
            filled_qty,
            avg_price,
        },
        ExchangeOrderStatus::Filled {
            filled_qty,
            avg_price,
        } => OmsOrderStatus::Filled {
            filled_qty,
            avg_price,
        },
        ExchangeOrderStatus::Cancelled { filled_qty } => OmsOrderStatus::Cancelled { filled_qty },
        ExchangeOrderStatus::Rejected { reason } => OmsOrderStatus::Rejected { reason },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::mock_exchange;
    use axon_exchange::types::{Kline, OrderUpdate as ExchangeOrderUpdate};
    use chrono::Utc;

    fn sample_kline() -> WsMessage {
        WsMessage::Kline(Kline {
            symbol: ExchangeSymbol::new("BTCUSDT"),
            interval: "1m".into(),
            open: Decimal::from(100),
            high: Decimal::from(101),
            low: Decimal::from(99),
            close: Decimal::from(100),
            volume: Decimal::from(1),
            timestamp: Utc::now(),
            is_closed: true,
        })
    }

    fn sample_fill(order_id: ExchangeOrderId, filled_qty: i64) -> WsMessage {
        WsMessage::OrderUpdate(ExchangeOrderUpdate {
            order_id: "venue-1".into(),
            client_order_id: order_id,
            status: ExchangeOrderStatus::Filled {
                filled_qty: Decimal::from(filled_qty),
                avg_price: Decimal::from(100),
            },
            filled_qty: Decimal::from(filled_qty),
            avg_price: Some(Decimal::from(100)),
            fee: Decimal::ZERO,
            trade_id: Some(format!("binfut:BTCUSDT:{filled_qty}")),
            timestamp: Utc::now(),
        })
    }

    #[test]
    fn fanout_gives_market_data_to_pump_keeps_fills() {
        let (adapter, handle) = mock_exchange(ExchangeId::Binance);
        let (tx, mut rx) = mpsc::channel(16);
        let mut gateway = ExchangeGateway::shared(adapter)
            .unwrap()
            .with_market_data_fanout(tx);

        handle.push_message(sample_kline());
        handle.push_message(sample_fill(ExchangeOrderId::new(), 2));
        handle.push_message(WsMessage::Ping);

        let updates = gateway.poll_updates().unwrap();
        // 回报归网关，行情两条（Kline + Ping）进 fan-out 通道。
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].fill_quantity, Decimal::from(2));
        assert!(matches!(rx.try_recv().unwrap(), WsMessage::Kline(_)));
        assert!(matches!(rx.try_recv().unwrap(), WsMessage::Ping));
    }

    #[test]
    fn without_fanout_market_data_is_dropped() {
        let (adapter, handle) = mock_exchange(ExchangeId::Binance);
        let mut gateway = ExchangeGateway::shared(adapter).unwrap();

        handle.push_message(sample_kline());
        handle.push_message(sample_fill(ExchangeOrderId::new(), 1));

        let updates = gateway.poll_updates().unwrap();
        assert_eq!(updates.len(), 1);
        // 再 drain 一轮：行情已被丢弃，不残留。
        assert!(gateway.poll_updates().unwrap().is_empty());
    }

    #[test]
    fn rest_snapshots_read_shared_adapter() {
        let (adapter, handle) = mock_exchange(ExchangeId::Binance);
        let gateway = ExchangeGateway::shared(adapter).unwrap();
        let symbol = ExchangeSymbol::new("BTCUSDT");

        {
            let mut state = handle.state();
            state.positions.push(Position {
                symbol: symbol.clone(),
                side: ExchangeSide::Buy,
                quantity: Decimal::from(3),
                avg_entry_price: Decimal::from(100),
                unrealized_pnl: Decimal::from(5),
            });
            state.account.total_balance = Decimal::from(12_345);
            state.trades.push(UserTrade {
                trade_id: "binfut:BTCUSDT:373270969".into(),
                symbol: "BTCUSDT".into(),
                order_id: "venue-1".into(),
                side: ExchangeSide::Buy,
                price: Decimal::from(100),
                qty: Decimal::from(3),
                quote_qty: Decimal::from(300),
                commission: Decimal::ZERO,
                commission_asset: "USDT".into(),
                realized_pnl: Decimal::ZERO,
                time_ms: 0,
            });
        }

        let positions = gateway.positions().unwrap();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].quantity, Decimal::from(3));
        assert_eq!(
            gateway.account_info().unwrap().total_balance,
            Decimal::from(12_345)
        );
        let trades = gateway.fetch_my_trades(&symbol, None, 1000).unwrap();
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].trade_id, "binfut:BTCUSDT:373270969");
    }
}
