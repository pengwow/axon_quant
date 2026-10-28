//! Live 执行出口：策略意图 → 同步网关 → 交易所回报
//!
//! `axon-kernel` 中 `ExecutionSink` 的方法是**同步**的，而 `axon_exchange::ExchangeAdapter` 是
//! async 的。内核不允许反过来依赖 exchange / tokio，所以这里只定义一个**最小
//! 同步网关** [`LiveOrderGateway`]：真实交易所接线（把 async adapter 桥成同步
//! gateway、WS 回报转 [`VenueUpdate`]）留到有 testnet 时再做。
//!
//! # 当前只到 gateway 抽象 + mock 验证
//!
//! 本模块**没有**接真实交易所：
//!
//! - 没有 `ExchangeAdapter → LiveOrderGateway` 的桥接实现（async → sync 需要
//!   独立线程或 `block_on`，与内核主回路的时序还没定）；
//! - 没有 WS `OrderUpdate` → [`VenueUpdate`] 的转换（`KernelPump` 目前把
//!   `OrderUpdate` 原样转成系统事件）；
//! - 没有真实余额 / 持仓对账，也没有崩溃恢复后的持仓校验。
//!
//! 因此下面的账务一律称「影子账本」：`OrderManager` 的订单状态机 + 成交流水是
//! 真的，资金与持仓只是按回报累计的近似值。
//!
//! # 三条与回测不同的硬约束
//!
//! 1. **绝不回流 `Event::Order(Submitted)`**：内核把 `Submitted` 交给
//!    `BacktestEngine::handle_submit`，会走合成撮合产生 fill。live 的真实成交
//!    由交易所回报驱动，再合成一次就是重复记账。这里只在撤单 / 拒单时发
//!    `Event::Order`（引擎只计数，不改持仓），成交统一走 `Event::Fill`。
//! 2. **目标调仓只下差额**：`SetTarget` 先按 `BacktestSink` 同一公式换算绝对
//!    数量（`ratio × equity / price`），再减去当前持仓，只把差额发给交易所。
//! 3. **同一 `trade_id` 只记账一次**：重复回报直接丢弃；缺 `trade_id` 的成交
//!    回报拒绝记账（无法去重时宁可漏记也不重复记账）。
//!
//! ponytail: 持仓取自 OMS 影子账本（`Portfolio`），不是交易所真实持仓；
//! `core.equity()` 仍是内核自己的账面权益，未与交易所对账。升级路径是 M5
//! testnet 阶段在 `poll` 里接余额 / 持仓快照并回灌 `KernelCore`，而不是在这里
//! 猜。

use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use axon_core::event::FillEvent;
use axon_core::types::{Price, Quantity};
use axon_core::{Event, Instrument, MarketDataPayload, OrderAction, OrderEvent, Timestamp, Trade};
use axon_kernel::{ExecutionSink, HostCtx, KernelCore, KernelError, OrderId, OrderIntent};
use axon_oms::{
    AccountingMode, Fill, Order as OmsOrder, OrderId as OmsOrderId, OrderManager,
    OrderStatus as OmsOrderStatus, OrderType as OmsOrderType, Position as OmsPosition,
    Side as OmsSide,
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};

/// 下单失败后的最大尝试次数（含首次）
///
/// 交易所超时不代表没收到单，所以重试必须复用同一个幂等键；次数封顶是防止
/// 网关持续故障时在主回路里无限重试。
const PLACE_ATTEMPTS: usize = 2;

/// 差额小于该阈值视为无需调仓（数量单位，不是名义额）
const MIN_ORDER_QUANTITY: f64 = 1e-9;

/// 交易所确认
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayAck {
    /// 交易所侧订单号（写进 OMS `order.meta`，供对账 / 排查）
    pub venue_order_id: String,
    /// 交易所受理后的初始状态（通常是 `Acknowledged`）
    pub status: OmsOrderStatus,
}

/// 交易所回报（gateway → sink 的最小统一形状）
///
/// 成交类回报用 `fill_quantity` 表达**本次增量**成交量；非成交类回报把它留成
/// `0`，只更新订单状态。真实交易所适配器的职责就是把 `OrderUpdate` 归一化成
/// 这个形状。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueUpdate {
    /// OMS 订单号（下单时由 sink 分配，gateway 原样回传）
    pub order_id: OmsOrderId,
    /// 回报后订单应处于的状态
    pub status: OmsOrderStatus,
    /// 本次增量成交量（正数）；`0` 表示非成交类回报
    pub fill_quantity: Decimal,
    /// 本次成交价（`fill_quantity > 0` 时有意义）
    pub fill_price: Decimal,
    /// 手续费（计价币，正数）
    pub fee: Decimal,
    /// 交易所成交编号；成交类回报必须提供，重复 `trade_id` 会被丢弃
    pub trade_id: Option<String>,
    /// 回报时间（纳秒）
    pub timestamp: Timestamp,
}

impl VenueUpdate {
    /// 编码为 journal 状态行（重演时由 [`Self::from_journal_state`] 还原）
    ///
    /// Decimal 一律编码成字符串：JSON 数字只有 f64 / u64，会丢精度，数量与
    /// 价格必须保住交易所口径的精确值。`status` 编码成 `{tag, detail}`——
    /// `OmsOrderStatus` 各变体载荷形状不同。
    pub fn to_journal_state(&self) -> serde_json::Value {
        let (tag, detail) = match &self.status {
            OmsOrderStatus::New => ("New", serde_json::json!({})),
            OmsOrderStatus::Submitted => ("Submitted", serde_json::json!({})),
            OmsOrderStatus::Acknowledged => ("Acknowledged", serde_json::json!({})),
            OmsOrderStatus::PartiallyFilled {
                filled_qty,
                avg_price,
            } => (
                "PartiallyFilled",
                serde_json::json!({
                    "filled_qty": filled_qty.to_string(),
                    "avg_price": avg_price.to_string(),
                }),
            ),
            OmsOrderStatus::Filled {
                filled_qty,
                avg_price,
            } => (
                "Filled",
                serde_json::json!({
                    "filled_qty": filled_qty.to_string(),
                    "avg_price": avg_price.to_string(),
                }),
            ),
            OmsOrderStatus::Cancelled { filled_qty } => (
                "Cancelled",
                serde_json::json!({ "filled_qty": filled_qty.to_string() }),
            ),
            OmsOrderStatus::Rejected { reason } => {
                ("Rejected", serde_json::json!({ "reason": reason }))
            }
        };
        serde_json::json!({
            "order_id": self.order_id.to_string(),
            "status": { "tag": tag, "detail": detail },
            "fill_quantity": self.fill_quantity.to_string(),
            "fill_price": self.fill_price.to_string(),
            "fee": self.fee.to_string(),
            "trade_id": self.trade_id,
            "timestamp": self.timestamp.nanos,
        })
    }

    /// [`Self::to_journal_state`] 的逆操作
    ///
    /// 字段缺失 / 订单号非法一律报错：重演宁可恢复失败，也不能静默错记账目。
    pub fn from_journal_state(state: &serde_json::Value) -> Result<Self, String> {
        let get_str = |key: &str| -> Result<String, String> {
            state
                .get(key)
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| format!("回报状态缺字段 {key}"))
        };
        let get_dec = |key: &str| -> Result<Decimal, String> {
            get_str(key)?
                .parse::<Decimal>()
                .map_err(|e| format!("{key} 不是合法 Decimal: {e}"))
        };
        let order_id = OmsOrderId::from_str(&get_str("order_id")?)
            .map_err(|e| format!("订单号不是合法 UUID: {e}"))?;
        let status_obj = state
            .get("status")
            .ok_or_else(|| "回报状态缺字段 status".to_string())?;
        let tag = status_obj
            .get("tag")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "status 缺 tag".to_string())?;
        let detail = status_obj
            .get("detail")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let detail_dec = |key: &str| -> Result<Decimal, String> {
            detail
                .get(key)
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("status.detail 缺字段 {key}"))
                .and_then(|s| {
                    s.parse::<Decimal>()
                        .map_err(|e| format!("{key} 不是合法 Decimal: {e}"))
                })
        };
        let status = match tag {
            "New" => OmsOrderStatus::New,
            "Submitted" => OmsOrderStatus::Submitted,
            "Acknowledged" => OmsOrderStatus::Acknowledged,
            "PartiallyFilled" => OmsOrderStatus::PartiallyFilled {
                filled_qty: detail_dec("filled_qty")?,
                avg_price: detail_dec("avg_price")?,
            },
            "Filled" => OmsOrderStatus::Filled {
                filled_qty: detail_dec("filled_qty")?,
                avg_price: detail_dec("avg_price")?,
            },
            "Cancelled" => OmsOrderStatus::Cancelled {
                filled_qty: detail_dec("filled_qty")?,
            },
            "Rejected" => OmsOrderStatus::Rejected {
                reason: detail
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "status.detail 缺字段 reason".to_string())?
                    .to_string(),
            },
            other => return Err(format!("未知状态 tag: {other}")),
        };
        let trade_id = match state.get("trade_id") {
            Some(v) if !v.is_null() => Some(
                v.as_str()
                    .ok_or_else(|| "trade_id 不是字符串".to_string())?
                    .to_string(),
            ),
            _ => None,
        };
        let timestamp = state
            .get("timestamp")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| "回报状态缺字段 timestamp".to_string())?;
        Ok(Self {
            order_id,
            status,
            fill_quantity: get_dec("fill_quantity")?,
            fill_price: get_dec("fill_price")?,
            fee: get_dec("fee")?,
            trade_id,
            timestamp: Timestamp::from_nanos(timestamp),
        })
    }
}

/// 同步下单网关
///
/// 只暴露内核主回路需要的三件事：下单、撤单、非阻塞拉回报。具体是 REST +
/// 轮询还是 WS 回调由实现决定，内核不感知。
pub trait LiveOrderGateway: Send {
    /// 提交一笔订单（`order.idempotency_key` 由 sink 填好，重试复用同一个键）
    fn place(&mut self, order: &OmsOrder) -> Result<GatewayAck, String>;
    /// 按 OMS 订单号撤单
    fn cancel(&mut self, id: &OmsOrderId) -> Result<(), String>;
    /// 非阻塞拉取交易所回报（内部可缓存；无回报时返回空）
    fn poll_updates(&mut self) -> Result<Vec<VenueUpdate>, String>;
    /// 关闭与交易所的连接（默认空实现；真实网关在此断开 WS / REST）
    fn disconnect(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// 崩溃恢复重演期的假网关
///
/// 重演时下沉意图会再次走到 `gateway.place`：首跑交易所同样受理了它们，影子
/// 账本必须复现同样的初始 ack，否则 OMS 状态机在后续成交回报处对不上。回报
/// 队列恒空——真实回报由 journal 经 `replay_external` 注回，不经过网关。
struct ReplayGateway {
    venue_seq: u64,
}

impl LiveOrderGateway for ReplayGateway {
    fn place(&mut self, _order: &OmsOrder) -> Result<GatewayAck, String> {
        self.venue_seq += 1;
        let venue_seq = self.venue_seq;
        Ok(GatewayAck {
            venue_order_id: format!("replay-{venue_seq}"),
            status: OmsOrderStatus::Acknowledged,
        })
    }

    fn cancel(&mut self, _id: &OmsOrderId) -> Result<(), String> {
        Ok(())
    }

    fn poll_updates(&mut self) -> Result<Vec<VenueUpdate>, String> {
        Ok(Vec::new())
    }
}

// RiskCheck / AllowAllRisk 已上移到 axon-kernel（PaperSink 与 LiveSink 共用同一
// pre-trade 接缝）；这里 re-export 保持 `axon_worker::live_sink::RiskCheck` 引用不破。
pub use axon_kernel::risk::{AllowAllRisk, RiskCheck};

/// sink 自己记下的订单元信息
///
/// 回报里只有 OMS 订单号，成交方向、品种、symbol 都要靠这张表还原；`OrderManager`
/// 没有按 id 读整单的接口，每帧 `snapshot()` 又会拷贝全量历史，不能用。
struct LiveOrder {
    kernel_id: u64,
    instrument: Instrument,
    side: OmsSide,
    symbol: String,
}

/// 实盘执行出口
pub struct LiveSink {
    gateway: Box<dyn LiveOrderGateway>,
    risk: Box<dyn RiskCheck>,
    oms: OrderManager,
    /// 已记账的交易所成交编号（去重，防重复记账）
    seen_trade_ids: HashSet<String>,
    /// OMS 订单号 → 下单时记下的元信息
    orders: HashMap<OmsOrderId, LiveOrder>,
    /// 内核侧订单号 → OMS 订单号（撤单入口用）
    kernel_index: HashMap<u64, OmsOrderId>,
    /// 内核侧订单号计数器（`Event::Order` / `Event::Fill` 的 id 空间）
    next_order_id: u64,
    /// `BarClosed` 收盘价，仅作为市价单在 OMS 记录里的参考价
    last_price: HashMap<Instrument, f64>,
    /// poll 内攒下的回报状态（内核 `drain_external` 落盘 journal 用）
    pending_update_states: Vec<serde_json::Value>,
    /// poll 拉到、**尚未应用**的回报（两段式落盘后再应用，见 `poll_staged`）
    pending_applies: Vec<VenueUpdate>,
}

impl LiveSink {
    /// 创建实盘 sink
    ///
    /// `shadow_cash` 是 OMS 影子账本的 USDT 初始余额。真实资金在交易所，影子
    /// 账本只用于订单 / 成交流水校验（`axon-oms` 的 `Portfolio` 把计价币硬编码
    /// 为 USDT）。传 `0` 会让所有买入成交被影子账本以「现金不足」拒绝。
    ///
    /// 账务模式默认 [`AccountingMode::Cash`]（现货口径）；合约账户必须用
    /// [`Self::with_accounting_mode`] 切到 [`AccountingMode::Wallet`]。
    pub fn new(gateway: Box<dyn LiveOrderGateway>, shadow_cash: Decimal) -> Self {
        let oms = OrderManager::new();
        oms.deposit("USDT", shadow_cash);
        Self {
            gateway,
            risk: Box::new(AllowAllRisk),
            oms,
            seen_trade_ids: HashSet::new(),
            orders: HashMap::new(),
            kernel_index: HashMap::new(),
            next_order_id: 1,
            last_price: HashMap::new(),
            pending_update_states: Vec::new(),
            pending_applies: Vec::new(),
        }
    }

    /// 构造崩溃恢复重演期的 sink
    ///
    /// `shadow_cash` 必须与首跑一致：恢复后的对账拿它当基线抓差异，静默改
    /// 初始资金只会把账目差异藏起来。
    pub fn for_replay(shadow_cash: Decimal) -> Self {
        Self::new(Box::new(ReplayGateway { venue_seq: 0 }), shadow_cash)
    }

    /// 注入 adopt 接管的初始持仓（构造后链式注入，OMS 持仓种子）
    ///
    /// fresh start 时由 [`crate::live_driver::LiveDriver::start`] 注入交易所残留持仓；resume 重演时
    /// 注入 manifest 读回的同一份，保证重演带仓起点与首跑一致。只动持仓不动 cash
    /// （cash 已由 `deposit(shadow_cash)` 钉好）。
    pub fn with_initial_positions(self, positions: Vec<OmsPosition>) -> Self {
        if !positions.is_empty() {
            self.oms.seed_initial_positions(positions);
        }
        self
    }

    /// 设置 OMS 影子账本的账务模式（默认 [`AccountingMode::Cash`]）
    ///
    /// **合约必须用 [`AccountingMode::Wallet`]**：合约开仓不扣名义价值、只占用
    /// 保证金，交易所钱包余额（`totalWalletBalance`）只受手续费与已实现盈亏
    /// 影响。按 `Cash` 的全额口径扣现金，现金对账必然偏差。
    pub fn with_accounting_mode(self, mode: AccountingMode) -> Self {
        self.oms.set_accounting_mode(mode);
        self
    }

    /// 运行时切换账务模式（语义同 [`Self::with_accounting_mode`]）
    pub fn set_accounting_mode(&mut self, mode: AccountingMode) {
        self.oms.set_accounting_mode(mode);
    }

    /// 替换风控实现（默认 [`AllowAllRisk`]）
    pub fn with_risk(mut self, risk: Box<dyn RiskCheck>) -> Self {
        self.risk = risk;
        self
    }

    /// 挂载真实网关（崩溃恢复续跑用）
    ///
    /// 重演期挂的是假网关（`for_replay` 的 `ReplayGateway`）：重演时宿主重决策
    /// 会走 `place`，绝不能打真单。对账 + 恢复护栏全部通过后，恢复编排才用本
    /// 方法换成真实网关继续跑——「护栏通过前不得调用」由 LiveDriver 状态机保证。
    pub fn set_gateway(&mut self, gateway: Box<dyn LiveOrderGateway>) {
        self.gateway = gateway;
    }

    /// 只读访问 OMS 账本（对账 / 冒烟测试用；不暴露可变句柄）
    pub fn oms(&self) -> &OrderManager {
        &self.oms
    }

    /// 退出前收尾：撤销所有未完成订单，然后关闭交易所连接
    ///
    /// 只撤非终态订单：`OrderManager::add_fill` 把订单改成 `Filled` 后仍留在活动集合里，
    /// 对已成交 / 已撤的订单再发一次撤单只会被交易所拒绝。
    ///
    /// 撤单失败（典型是订单刚好成交）只记录并继续撤下一笔，最后把所有失败原因
    /// 一起返回，调用方据此决定是否告警。连接关闭失败同样汇总在返回值里。
    pub fn shutdown(&mut self) -> Result<(), String> {
        let active: Vec<OmsOrderId> = self
            .oms
            .snapshot()
            .active_orders
            .iter()
            .filter(|(_, order)| !order.status.is_terminal())
            .map(|(id, _)| *id)
            .collect();
        let mut errors = Vec::new();
        for id in active {
            match self.gateway.cancel(&id) {
                Ok(()) => {
                    // 交易所撤单成功后才把 OMS 落到终态。
                    if let Err(err) = self.oms.cancel(id) {
                        errors.push(format!("{id}: {err}"));
                    }
                }
                Err(err) => errors.push(format!("{id}: {err}")),
            }
        }
        if let Err(err) = self.gateway.disconnect() {
            errors.push(format!("disconnect: {err}"));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    /// 当前持仓（数量，正多负空）
    ///
    /// 取自 OMS 影子账本：成交回报按回报方向累计，ack 之前为 0。
    fn position(&self, instrument: &Instrument) -> f64 {
        let symbol = symbol_of(instrument);
        self.oms
            .snapshot_positions()
            .iter()
            .find(|p| p.symbol == symbol)
            .and_then(|p| p.quantity.to_f64())
            .unwrap_or(0.0)
    }

    /// `SetTarget`：权益占比 → 绝对数量 → 只下差额
    fn submit_target(
        &mut self,
        instrument: &Instrument,
        target_ratio: f64,
        core: &mut KernelCore,
    ) -> Result<Vec<Event>, KernelError> {
        let Some(price) = core.last_price(instrument) else {
            tracing::debug!(instrument = ?instrument, "live sink: 无最新价，跳过目标调仓");
            return Ok(Vec::new());
        };
        let equity = core.equity();
        if price <= 0.0 || !price.is_finite() || equity <= 0.0 || !equity.is_finite() {
            tracing::debug!(
                instrument = ?instrument,
                price,
                equity,
                "live sink: 价格或权益非正，跳过目标调仓"
            );
            return Ok(Vec::new());
        }

        // 换算公式与 `BacktestSink::submit` 逐字相同：全链路只有这一个换算点。
        let target_quantity = target_ratio * equity / price;
        let delta = target_quantity - self.position(instrument);
        if delta.abs() < MIN_ORDER_QUANTITY {
            tracing::debug!(
                instrument = ?instrument,
                target_quantity,
                "live sink: 已到目标仓位，无需下单"
            );
            return Ok(Vec::new());
        }
        let side = if delta > 0.0 {
            OmsSide::Buy
        } else {
            OmsSide::Sell
        };
        self.place_order(instrument, side, delta.abs(), None)
    }

    /// 下一笔真实订单（市价）
    ///
    /// `limit_price` 为 `None` 时按市价单提交，OMS 记录里带最近收盘价作为参考。
    fn place_order(
        &mut self,
        instrument: &Instrument,
        side: OmsSide,
        quantity: f64,
        limit_price: Option<f64>,
    ) -> Result<Vec<Event>, KernelError> {
        let kernel_id = self.next_order_id;
        self.next_order_id += 1;
        // 幂等键在创建订单时固化：同一次下单的所有重试都复用它。
        let idempotency_key = format!("axon-live-{kernel_id}");

        let quantity = to_decimal(quantity)?;
        let price = match limit_price {
            Some(limit) => to_decimal(limit)?,
            None => self
                .last_price
                .get(instrument)
                .copied()
                .map(to_decimal)
                .transpose()?
                .unwrap_or(Decimal::ZERO),
        };
        let order_type = if limit_price.is_some() {
            OmsOrderType::Limit
        } else {
            OmsOrderType::Market
        };
        let symbol = symbol_of(instrument);

        let mut order = OmsOrder::new(symbol.clone(), side, order_type, quantity, price)
            .with_instrument(instrument.clone())
            .with_idempotency_key(idempotency_key);
        // 覆盖随机 id 为内核序号派生的确定性 id：journal 里的回报按 OMS 订单号
        // 匹配簿记，重演必须派生出与首跑相同的订单号才能对上回报序列。
        order.id = OmsOrderId::deterministic(kernel_id);
        let ack = self.place_with_retry(&order)?;
        order
            .meta
            .insert("venue_order_id".to_string(), ack.venue_order_id.clone());

        // 交易所已受理才进 OMS：否则 OMS 会留下一笔从未发出的订单。
        let oms_id = self
            .oms
            .submit(order)
            .map_err(|e| KernelError::Sink(format!("oms submit: {e}")))?;
        if ack.status != OmsOrderStatus::Submitted {
            self.oms
                .update_status(oms_id, ack.status)
                .map_err(|e| KernelError::Sink(format!("oms ack: {e}")))?;
        }
        self.orders.insert(
            oms_id,
            LiveOrder {
                kernel_id,
                instrument: instrument.clone(),
                side,
                symbol,
            },
        );
        self.kernel_index.insert(kernel_id, oms_id);

        // 故意不回流 `Event::Order(Submitted)`，见模块文档第 1 条硬约束。
        Ok(Vec::new())
    }

    /// 带重试的下单：失败后复用同一个 `OmsOrder`（同一幂等键）再试一次
    fn place_with_retry(&mut self, order: &OmsOrder) -> Result<GatewayAck, KernelError> {
        let mut last_error = String::new();
        for attempt in 1..=PLACE_ATTEMPTS {
            match self.gateway.place(order) {
                Ok(ack) => return Ok(ack),
                Err(err) => {
                    tracing::warn!(
                        attempt,
                        attempts = PLACE_ATTEMPTS,
                        idempotency_key = ?order.idempotency_key,
                        error = %err,
                        "live sink: 下单失败，复用幂等键重试"
                    );
                    last_error = err;
                }
            }
        }
        Err(KernelError::Sink(format!(
            "gateway place failed after {PLACE_ATTEMPTS} attempts: {last_error}"
        )))
    }

    /// 撤单：网关先撤，OMS 再落状态，最后回流一条 `Cancelled` 事件
    fn cancel_order(
        &mut self,
        kernel_id: OrderId,
        timestamp: Timestamp,
    ) -> Result<Vec<Event>, KernelError> {
        let Some(&oms_id) = self.kernel_index.get(&kernel_id) else {
            return Err(KernelError::Sink(format!(
                "cancel: 未知内核订单号 {kernel_id}"
            )));
        };
        self.gateway
            .cancel(&oms_id)
            .map_err(|e| KernelError::Sink(format!("gateway cancel: {e}")))?;
        self.oms
            .cancel(oms_id)
            .map_err(|e| KernelError::Sink(format!("oms cancel: {e}")))?;
        self.kernel_index.remove(&kernel_id);
        Ok(vec![Event::Order(OrderEvent {
            seq: 0,
            timestamp,
            order_id: kernel_id,
            action: OrderAction::Cancelled(kernel_id),
        })])
    }

    /// 把一条交易所回报落进 OMS 并转成内核事件
    ///
    /// 单条回报的问题（未知订单、非法迁移、重复成交）只告警跳过，不把整条
    /// 主回路打挂；网关本身拉取失败才是 `KernelError::Sink`（见 `poll`）。
    fn apply_update(&mut self, update: VenueUpdate, out: &mut Vec<Event>) {
        let Some(order) = self.orders.get(&update.order_id) else {
            tracing::warn!(
                order_id = %update.order_id,
                "live sink: 回报对应订单不在簿记中，丢弃"
            );
            return;
        };
        // 先把需要的字段拷出来，结束对 `orders` 的借用，下面才能改其它字段。
        let kernel_id = order.kernel_id;
        let side = order.side;
        let symbol = order.symbol.clone();
        let instrument = order.instrument.clone();

        if update.fill_quantity > Decimal::ZERO {
            self.apply_fill_update(update, kernel_id, side, symbol, instrument, out);
            return;
        }

        match self
            .oms
            .update_status(update.order_id, update.status.clone())
        {
            Ok(()) => {
                let action = match &update.status {
                    OmsOrderStatus::Cancelled { .. } => OrderAction::Cancelled(kernel_id),
                    OmsOrderStatus::Rejected { reason } => OrderAction::Rejected {
                        order_id: kernel_id,
                        reason: reason.clone(),
                    },
                    _ => return,
                };
                out.push(Event::Order(OrderEvent {
                    seq: 0,
                    timestamp: update.timestamp,
                    order_id: kernel_id,
                    action,
                }));
            }
            // 非法迁移（如 Filled → Submitted）在这里被状态机拒绝。
            Err(err) => tracing::warn!(
                order_id = %update.order_id,
                status = ?update.status,
                error = %err,
                "live sink: 非法订单状态迁移，拒绝回报"
            ),
        }
    }

    /// 成交类回报：`trade_id` 去重 → OMS 记账 → 回流 `Event::Fill`
    fn apply_fill_update(
        &mut self,
        update: VenueUpdate,
        kernel_id: u64,
        side: OmsSide,
        symbol: String,
        instrument: Instrument,
        out: &mut Vec<Event>,
    ) {
        let Some(trade_id) = update.trade_id.clone() else {
            // 没有 trade_id 就无法判断重复，宁可漏记也不重复记账。
            tracing::warn!(
                order_id = %update.order_id,
                "live sink: 成交回报缺 trade_id，拒绝记账"
            );
            return;
        };
        if !self.seen_trade_ids.insert(trade_id.clone()) {
            tracing::debug!(
                trade_id = %trade_id,
                "live sink: 重复成交回报，丢弃"
            );
            return;
        }
        if update.fill_quantity > Decimal::ZERO && update.fill_price == Decimal::ZERO {
            // 零价成交会把现金/权益账记歪（卖出无进账、买入白拿仓位）。仍按
            // 通道语义记账保持与交易所数量一致，靠 reconcile 纠正偏差，但必须告警。
            tracing::warn!(
                order_id = %update.order_id,
                trade_id = %trade_id,
                fill_quantity = %update.fill_quantity,
                "live sink: 成交价格为零（有成交量），本次按 0 记账"
            );
        }

        let signed_quantity = match side {
            OmsSide::Buy => update.fill_quantity,
            OmsSide::Sell => -update.fill_quantity,
        };
        let fill = Fill {
            fill_id: trade_id.clone(),
            symbol,
            instrument: Some(instrument),
            price: update.fill_price,
            quantity: signed_quantity,
            fee: update.fee,
            timestamp: to_utc(update.timestamp),
        };
        if let Err(err) = self.oms.add_fill(update.order_id, fill) {
            // 未记账就把 trade_id 放回去：交易所重投时还有机会落账。
            self.seen_trade_ids.remove(&trade_id);
            tracing::warn!(
                order_id = %update.order_id,
                trade_id = %trade_id,
                error = %err,
                "live sink: 成交流水记账失败，丢弃本次回报"
            );
            return;
        }

        let trade = Trade::new(
            update.timestamp,
            Price::from_f64(update.fill_price.to_f64().unwrap_or(0.0)),
            Quantity::from_f64(update.fill_quantity.to_f64().unwrap_or(0.0)),
            if side == OmsSide::Buy { kernel_id } else { 0 },
            if side == OmsSide::Sell { kernel_id } else { 0 },
        );
        out.push(Event::Fill(FillEvent::new(0, update.timestamp, trade)));
    }
}

impl ExecutionSink for LiveSink {
    fn submit(
        &mut self,
        intent: OrderIntent,
        core: &mut KernelCore,
        ctx: &HostCtx,
    ) -> Result<Vec<Event>, KernelError> {
        // 风控内联点：所有意图（含 SetTarget）先过检查，拒单不下发网关。
        self.risk
            .check(&intent, ctx)
            .map_err(|reason| KernelError::Sink(format!("risk rejected: {reason}")))?;

        let timestamp = core.frame_ts();
        match intent {
            OrderIntent::SetTarget {
                instrument,
                target_ratio,
            } => self.submit_target(&instrument, target_ratio, core),
            OrderIntent::Submit(spec) => {
                let side = match spec.side {
                    axon_core::market::Side::Buy => OmsSide::Buy,
                    axon_core::market::Side::Sell => OmsSide::Sell,
                };
                self.place_order(
                    &spec.instrument,
                    side,
                    spec.quantity.as_f64(),
                    spec.price.map(|price| price.as_f64()),
                )
            }
            OrderIntent::Cancel(kernel_id) => self.cancel_order(kernel_id, timestamp),
        }
    }

    fn on_market(&mut self, ev: &Event) -> Result<Vec<Event>, KernelError> {
        // 只记录带品种的收市价：Tick / 盘口载荷不带 instrument，归因需要
        // 「活跃品种」这类启发式，对 live 下单没有实质收益，等接 WS 泵时再说。
        if let Event::MarketData(md) = ev
            && let MarketDataPayload::BarClosed {
                instrument, bar, ..
            } = &md.payload
            && bar.close.as_f64() > 0.0
        {
            self.last_price
                .insert(instrument.clone(), bar.close.as_f64());
        }
        Ok(Vec::new())
    }

    fn poll(&mut self) -> Result<Vec<Event>, KernelError> {
        // 组合式：拉取入队 + 应用，两步连做。仅重演 / 非记录路径走这里；
        // 首跑 live 的记录路径走拆分的 poll_staged / apply_staged——journal
        // 写入插在两段之间，保证「已应用」的回报必然「已落盘」。
        self.poll_staged()?;
        self.apply_staged()
    }

    /// 两段式第一段：从网关拉回报，只解析入队、**不应用**，返回恒空。
    ///
    /// 被丢弃的回报（未知订单 / 非法迁移 / 去重）也照攒状态——它们是外部输入，
    /// 重演时交给 apply_update 重现同样的丢弃判定，journal 不替 sink 做决定。
    fn poll_staged(&mut self) -> Result<Vec<Event>, KernelError> {
        let updates = self
            .gateway
            .poll_updates()
            .map_err(|e| KernelError::Sink(format!("gateway poll: {e}")))?;
        for update in updates {
            self.pending_update_states.push(update.to_journal_state());
            self.pending_applies.push(update);
        }
        Ok(Vec::new())
    }

    /// 两段式第二段：应用已落盘的暂存回报并产出事件。
    ///
    /// 主回路先调 [`Self::take_update_states`]+journal 落盘，再调本方法：
    /// 中间崩溃时 journal 里至少有回报记录，重演不丢外部输入。
    fn apply_staged(&mut self) -> Result<Vec<Event>, KernelError> {
        let mut out = Vec::new();
        for update in std::mem::take(&mut self.pending_applies) {
            self.apply_update(update, &mut out);
        }
        Ok(out)
    }

    fn records_external_responses(&self) -> bool {
        true
    }

    fn take_update_states(&mut self) -> Vec<serde_json::Value> {
        std::mem::take(&mut self.pending_update_states)
    }

    fn replay_external(&mut self, state: &serde_json::Value) -> Result<Vec<Event>, KernelError> {
        let update = VenueUpdate::from_journal_state(state)
            .map_err(|e| KernelError::Sink(format!("回报状态还原失败: {e}")))?;
        let mut out = Vec::new();
        self.apply_update(update, &mut out);
        Ok(out)
    }

    fn finish_bar(&mut self, _core: &mut KernelCore) -> Result<Vec<Event>, KernelError> {
        // 目标调仓在 `submit` 里已经按差额落单；这里不能再走
        // `apply_effective_targets`，否则内核撮合引擎会再合成一次成交。
        Ok(Vec::new())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// `"BASE/QUOTE"` 形式的 symbol（与内核 `symbol_key` 同口径）
fn symbol_of(instrument: &Instrument) -> String {
    format!(
        "{}/{}",
        instrument.base().as_str(),
        instrument.quote().as_str()
    )
}

/// `f64` → `Decimal`（NaN / 溢出直接报错，不做静默截断）
///
/// 用 `from_f64`（最短十进制表示）而不是 `from_f64_retain`：交易所的
/// LOT_SIZE / PRICE_FILTER 按十进制字符串校验，`from_f64_retain` 会把 f64 的
/// 二进制尾差展开成 `0.0001000000000000000047…` 这样的数量，必然被判非法。
fn to_decimal(value: f64) -> Result<Decimal, KernelError> {
    Decimal::from_f64(value).ok_or_else(|| KernelError::Sink(format!("非法数量/价格: {value}")))
}

/// 纳秒时间戳 → `chrono::DateTime<Utc>`（OMS `Fill` 的时间字段）
fn to_utc(timestamp: Timestamp) -> DateTime<Utc> {
    DateTime::from_timestamp_nanos(timestamp.nanos)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axon_core::market::Side as CoreSide;
    use axon_core::types::{SpotInstrument, Symbol};
    use axon_kernel::{HostCtx, KernelConfig, OrderSpec};

    use super::*;

    /// 网关收到的一次下单（只留断言需要的字段）
    #[derive(Debug, Clone)]
    struct Placed {
        idempotency_key: Option<String>,
        side: OmsSide,
        quantity: Decimal,
    }

    struct MockGateway {
        placed: Arc<Mutex<Vec<Placed>>>,
        updates: Arc<Mutex<Vec<VenueUpdate>>>,
        /// `shutdown` 收尾时撤掉的 OMS 订单号
        cancelled: Arc<Mutex<Vec<OmsOrderId>>>,
        /// `shutdown` 是否调用过 `disconnect`
        disconnected: Arc<Mutex<bool>>,
        /// 前 N 次 `place` 返回错误（模拟传输超时）
        fail_remaining: usize,
        venue_seq: usize,
    }

    impl LiveOrderGateway for MockGateway {
        fn place(&mut self, order: &OmsOrder) -> Result<GatewayAck, String> {
            self.placed.lock().unwrap().push(Placed {
                idempotency_key: order.idempotency_key.clone(),
                side: order.side,
                quantity: order.quantity,
            });
            if self.fail_remaining > 0 {
                self.fail_remaining -= 1;
                return Err("mock: 传输超时".to_string());
            }
            self.venue_seq += 1;
            Ok(GatewayAck {
                venue_order_id: format!("venue-{}", self.venue_seq),
                status: OmsOrderStatus::Acknowledged,
            })
        }

        fn cancel(&mut self, id: &OmsOrderId) -> Result<(), String> {
            self.cancelled.lock().unwrap().push(*id);
            Ok(())
        }

        fn poll_updates(&mut self) -> Result<Vec<VenueUpdate>, String> {
            Ok(std::mem::take(&mut *self.updates.lock().unwrap()))
        }

        fn disconnect(&mut self) -> Result<(), String> {
            *self.disconnected.lock().unwrap() = true;
            Ok(())
        }
    }

    struct DenyAllRisk;

    impl RiskCheck for DenyAllRisk {
        fn check(&mut self, _intent: &OrderIntent, _ctx: &HostCtx) -> Result<(), String> {
            Err("单笔限额超限".to_string())
        }
    }

    /// mock 网关 + 测试需要观察的共享句柄
    struct MockHandles {
        gateway: Box<dyn LiveOrderGateway>,
        placed: Arc<Mutex<Vec<Placed>>>,
        updates: Arc<Mutex<Vec<VenueUpdate>>>,
        cancelled: Arc<Mutex<Vec<OmsOrderId>>>,
        disconnected: Arc<Mutex<bool>>,
    }

    fn mock(fail_remaining: usize) -> MockHandles {
        let placed = Arc::new(Mutex::new(Vec::new()));
        let updates = Arc::new(Mutex::new(Vec::new()));
        let cancelled = Arc::new(Mutex::new(Vec::new()));
        let disconnected = Arc::new(Mutex::new(false));
        let gateway = MockGateway {
            placed: placed.clone(),
            updates: updates.clone(),
            cancelled: cancelled.clone(),
            disconnected: disconnected.clone(),
            fail_remaining,
            venue_seq: 0,
        };
        MockHandles {
            gateway: Box::new(gateway),
            placed,
            updates,
            cancelled,
            disconnected,
        }
    }

    fn btc() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        })
    }

    fn spec(side: CoreSide, quantity: f64) -> OrderSpec {
        OrderSpec {
            instrument: btc(),
            side,
            quantity: Quantity::from_f64(quantity),
            price: Some(Price::from_f64(100.0)),
        }
    }

    fn core(initial_cash: f64) -> KernelCore {
        KernelCore::new(&KernelConfig {
            initial_cash,
            ..KernelConfig::default()
        })
    }

    fn filled_update(oms_id: OmsOrderId, trade_id: &str, quantity: Decimal) -> VenueUpdate {
        VenueUpdate {
            order_id: oms_id,
            status: OmsOrderStatus::Filled {
                filled_qty: quantity,
                avg_price: Decimal::from(100u32),
            },
            fill_quantity: quantity,
            fill_price: Decimal::from(100u32),
            fee: Decimal::ZERO,
            trade_id: Some(trade_id.to_string()),
            timestamp: Timestamp::from_nanos(1_000),
        }
    }

    /// 1) 幂等：首次下单失败后复用同一幂等键重试，OMS 只留一笔订单
    #[test]
    fn test_place_retry_reuses_idempotency_key() {
        let MockHandles {
            gateway, placed, ..
        } = mock(1);
        let mut sink = LiveSink::new(gateway, Decimal::from(1_000_000u32));
        let mut core = core(10_000.0);

        sink.submit(
            OrderIntent::Submit(spec(CoreSide::Buy, 1.0)),
            &mut core,
            &HostCtx::default(),
        )
        .expect("重试后应成功");

        let placed = placed.lock().unwrap();
        assert_eq!(placed.len(), 2, "首次失败后必须重试一次");
        assert_eq!(
            placed[0].idempotency_key, placed[1].idempotency_key,
            "重试必须复用同一个幂等键"
        );
        assert!(placed[0].idempotency_key.is_some());
        assert_eq!(sink.oms.history_count(), 1, "重试不得产生第二笔 OMS 订单");
        assert_eq!(sink.orders.len(), 1);
    }

    /// 2) 重复 `trade_id` 回报：OMS 只记账一次，只回流一条 Fill
    #[test]
    fn test_duplicate_trade_id_booked_once() {
        let MockHandles {
            gateway, updates, ..
        } = mock(0);
        let mut sink = LiveSink::new(gateway, Decimal::from(1_000_000u32));
        let mut core = core(10_000.0);
        sink.submit(
            OrderIntent::Submit(spec(CoreSide::Buy, 5.0)),
            &mut core,
            &HostCtx::default(),
        )
        .expect("下单应成功");
        let oms_id = *sink.orders.keys().next().expect("应有订单");

        let update = filled_update(oms_id, "trade-1", Decimal::from(5u32));
        updates.lock().unwrap().push(update.clone());
        updates.lock().unwrap().push(update);

        let events = sink.poll().expect("poll 不应失败");

        assert_eq!(events.len(), 1, "重复回报只能产出一条 Fill");
        assert!(matches!(events[0], Event::Fill(_)));
        assert_eq!(
            sink.oms.snapshot().order_history[0].fills.len(),
            1,
            "OMS 必须只记账一次"
        );
        assert_eq!(sink.position(&btc()), 5.0);
    }

    /// 3) 非法状态迁移（Filled → Submitted，等价于 Filled → Pending）：
    ///    拒绝、不 panic，OMS 状态保持不变
    #[test]
    fn test_invalid_transition_is_rejected() {
        let MockHandles {
            gateway, updates, ..
        } = mock(0);
        let mut sink = LiveSink::new(gateway, Decimal::from(1_000_000u32));
        let mut core = core(10_000.0);
        sink.submit(
            OrderIntent::Submit(spec(CoreSide::Buy, 5.0)),
            &mut core,
            &HostCtx::default(),
        )
        .expect("下单应成功");
        let oms_id = *sink.orders.keys().next().expect("应有订单");

        updates
            .lock()
            .unwrap()
            .push(filled_update(oms_id, "trade-1", Decimal::from(5u32)));
        sink.poll().expect("成交回报应能落账");

        updates.lock().unwrap().push(VenueUpdate {
            order_id: oms_id,
            status: OmsOrderStatus::Submitted,
            fill_quantity: Decimal::ZERO,
            fill_price: Decimal::ZERO,
            fee: Decimal::ZERO,
            trade_id: None,
            timestamp: Timestamp::from_nanos(2_000),
        });
        let events = sink.poll().expect("非法迁移不得打挂主回路");

        assert!(events.is_empty(), "非法迁移不产生事件");
        assert_eq!(
            sink.oms.get_order_status(oms_id),
            Some(OmsOrderStatus::Filled {
                filled_qty: Decimal::from(5u32),
                avg_price: Decimal::from(100u32),
            }),
            "状态机必须停在 Filled"
        );
    }

    /// 4) `SetTarget` 差额单：持仓 10、目标 15 → 只下 5
    #[test]
    fn test_set_target_places_only_the_difference() {
        let MockHandles {
            gateway,
            placed,
            updates,
            ..
        } = mock(0);
        let mut sink = LiveSink::new(gateway, Decimal::from(1_000_000u32));
        let mut core = core(15.0);
        core.update_last_price(&btc(), 1.0);

        // 先用一笔成交把影子账本持仓做成 10
        sink.submit(
            OrderIntent::Submit(spec(CoreSide::Buy, 10.0)),
            &mut core,
            &HostCtx::default(),
        )
        .expect("建仓下单应成功");
        let oms_id = *sink.orders.keys().next().expect("应有订单");
        updates.lock().unwrap().push(VenueUpdate {
            order_id: oms_id,
            status: OmsOrderStatus::Filled {
                filled_qty: Decimal::from(10u32),
                avg_price: Decimal::ZERO,
            },
            fill_quantity: Decimal::from(10u32),
            fill_price: Decimal::ZERO,
            fee: Decimal::ZERO,
            trade_id: Some("seed-fill".to_string()),
            timestamp: Timestamp::from_nanos(1_000),
        });
        sink.poll().expect("建仓成交应能落账");
        assert_eq!(sink.position(&btc()), 10.0);

        // 权益 15 / 价格 1 时 ratio=1.0 → 目标 15，差额 5
        sink.submit(
            OrderIntent::SetTarget {
                instrument: btc(),
                target_ratio: 1.0,
            },
            &mut core,
            &HostCtx::default(),
        )
        .expect("目标调仓应成功");

        let placed = placed.lock().unwrap();
        let rebalance = placed.last().expect("应有调仓单");
        assert_eq!(rebalance.quantity, Decimal::from(5u32), "只允许下差额");
        assert_eq!(rebalance.side, OmsSide::Buy);
        assert_eq!(placed.len(), 2, "建仓 1 笔 + 差额 1 笔");
    }

    /// 5) 风控拒绝 → `submit` 返回 Err，网关一次都没被调用
    #[test]
    fn test_risk_rejection_blocks_gateway() {
        let MockHandles {
            gateway, placed, ..
        } = mock(0);
        let mut sink =
            LiveSink::new(gateway, Decimal::from(1_000_000u32)).with_risk(Box::new(DenyAllRisk));
        let mut core = core(10_000.0);

        let err = sink
            .submit(
                OrderIntent::Submit(spec(CoreSide::Buy, 1.0)),
                &mut core,
                &HostCtx::default(),
            )
            .expect_err("风控拒绝必须返回错误");

        assert!(matches!(err, KernelError::Sink(_)), "错误类型应为 Sink");
        assert!(placed.lock().unwrap().is_empty(), "网关不得被调用");
        assert_eq!(sink.oms.history_count(), 0);
    }

    /// 6) 退出收尾：撤销所有未完成订单并关闭交易所连接
    #[test]
    fn test_shutdown_cancels_active_orders_and_disconnects() {
        let MockHandles {
            gateway,
            cancelled,
            disconnected,
            ..
        } = mock(0);
        let mut sink = LiveSink::new(gateway, Decimal::from(1_000_000u32));
        let mut core = core(10_000.0);
        sink.submit(
            OrderIntent::Submit(spec(CoreSide::Buy, 1.0)),
            &mut core,
            &HostCtx::default(),
        )
        .expect("下单应成功");
        let oms_id = *sink.orders.keys().next().expect("应有订单");

        sink.shutdown().expect("收尾应成功");

        assert_eq!(
            *cancelled.lock().unwrap(),
            vec![oms_id],
            "收尾必须撤销未完成订单"
        );
        assert!(*disconnected.lock().unwrap(), "收尾必须关闭连接");
        assert_eq!(sink.oms.active_count(), 0, "撤单后 OMS 不应留有活动订单");
    }

    /// 7) `f64` → `Decimal` 走最短表示：交易所按十进制字符串校验数量 / 价格
    #[test]
    fn test_to_decimal_uses_shortest_representation() {
        assert_eq!(to_decimal(0.0001).unwrap().to_string(), "0.0001");
        assert_eq!(to_decimal(0.1).unwrap().to_string(), "0.1");
        assert_eq!(to_decimal(50_000.5).unwrap().to_string(), "50000.5");
        assert!(to_decimal(f64::NAN).is_err(), "NaN 必须报错");
    }
}
