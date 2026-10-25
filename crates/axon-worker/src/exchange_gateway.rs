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
//! - 网关取走了 adapter 唯一的行情通道，`KernelPump` 就拿不到行情了。接 live 主
//!   回路时必须把消息 fan-out 成「行情给 `KernelPump` + 回报给网关」两路，否则
//!   长时间挂机时通道（容量 4096）会被行情塞满，adapter 的读循环会被阻塞。
//!
//! Binance 合约的 user data stream（`ORDER_TRADE_UPDATE`）已在 adapter 内实现，
//! 回报与行情共用同一条通道，因此上面的「单消费者」约束仍然成立。

use std::collections::HashMap;
use std::sync::Arc;

use axon_core::{Bar, Timestamp};
use axon_exchange::ExchangeAdapter;
use axon_exchange::types::{
    ExchangeId, Order as ExchangeOrder, OrderId as ExchangeOrderId,
    OrderStatus as ExchangeOrderStatus, OrderType as ExchangeOrderType, Side as ExchangeSide,
    Symbol as ExchangeSymbol, Ticker, TimeInForce, WsMessage,
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
    /// OMS 订单号 → 已回报的**累计**成交量（累计 → 增量的转换状态）
    last_filled: HashMap<String, Decimal>,
}

impl ExchangeGateway {
    /// 创建网关
    ///
    /// 必须在 runtime 之外调用：内部会创建 Runtime，并取走 adapter 的
    /// `market_data_rx`（单消费者通道，且实现里用 `blocking_lock`）。
    pub fn new(adapter: Box<dyn ExchangeAdapter>) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("构建 tokio 运行时失败: {e}"))?;
        // 同步上下文取通道：adapter 的 `market_data_rx` 用 `blocking_lock`，
        // 在 async 任务里调用会 panic，所以只能在这里取一次。
        let updates = adapter.market_data_rx();
        Ok(Self {
            adapter: Arc::new(Mutex::new(adapter)),
            runtime,
            updates: Some(updates),
            last_filled: HashMap::new(),
        })
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
        let Some(updates) = self.updates.as_mut() else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        // 非阻塞 drain：有回报就转，没有就返回空，绝不阻塞主回路。
        // `try_recv` 返回「空」或「断线」都意味着本轮没有更多回报。
        while let Ok(message) = updates.try_recv() {
            // 行情类消息属于 KernelPump 的通道，这里不重复消费（直接丢弃）。
            let WsMessage::OrderUpdate(update) = message else {
                continue;
            };
            if let Some(venue_update) = convert(&mut self.last_filled, update) {
                out.push(venue_update);
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
/// `trade_id` 用 `订单号:纳秒时间戳:本次增量` 合成：交易所回报不带成交编号，
/// 而 `LiveSink` 靠 `trade_id` 去重。这个三元组在「同一订单 + 同一纳秒 + 同一
/// 增量」上唯一 —— 重复投递得到完全相同的键，会被 sink 丢弃；代价是同一订单在
/// 同一纳秒内两笔等量成交会被误判为重复，方向是**宁可漏记不重复记账**，与
/// [`crate::live_sink`] 的硬约束一致。
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
    let trade_id = (fill_quantity > Decimal::ZERO)
        .then(|| format!("{order_id}:{}:{fill_quantity}", timestamp.nanos));
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
