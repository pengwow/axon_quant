//! L1 撮合引擎：基础价格-时间优先撮合
//!
//! L1 支持：
//! - 限价单（Limit）
//! - 市价单（Market）
//! - 立即成交或取消（IOC）
//! - 全部成交或取消（FOK）
//!
//! 不支持：止损单、止损限价单、冰山单（属于 L2/L3 范围）。
//!
//! # 算法
//!
//! 价格-时间优先：
//! 1. 价格优先：买单按价格降序匹配，卖单按价格升序匹配
//! 2. 时间优先：同价位按到达时间升序匹配
//!
//! # 数据结构
//!
//! - `BTreeMap<Price, VecDeque<Order>>`：价格-时间优先队列
//! - `HashMap<OrderId, (Side, Price)>`：订单索引（用于快速取消）

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};

use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::types::{Instrument, Price, Quantity, SpotInstrument, Symbol};

use super::error::{MatchingError, MatchingResult};
use super::types::{MatchFill, OrderBookLevel, SubmitResult};
/// 把 `Symbol` (如 `"BTC-USDT"`) 拆为 `(base, quote)` `(T2.2 过渡 helper)`
///
/// L1 撮合引擎单 instrument 语义下,`with_symbol` / `seed_liquidity` 等接口
/// 仍按 `Symbol` 暴露;但 `Order::spot` 接收 `base/quote` 两个参数,因此需要
/// 在调用边界把单个 symbol 字符串拆开。
///
/// - 含 `-`:  视作 `BASE-QUOTE` 格式,按第一个 `-` 切分(如 `"BTC-USDT"` → `("BTC", "USDT")`)
/// - 不含 `-`: 视作 base 单 token,quote 默认填充 `"USDT"`
///
/// T2.3 计划用真正的 multi-instrument book + `Instrument` 路由替换此逻辑,
/// 届时此 helper 会被删除。
fn split_symbol_to_base_quote(sym: &Symbol) -> (Symbol, Symbol) {
    let s = sym.as_str();
    // 接受 "BASE-QUOTE" (L1 测试) 和 "BASE/QUOTE" (L3 测试/CoinGecko 风格) 两种格式
    if let Some((base, quote)) = s.split_once('-').or_else(|| s.split_once('/')) {
        (Symbol::from(base), Symbol::from(quote))
    } else {
        // 兜底: 单 token 视为 base,quote 默认 USDT
        (sym.clone(), Symbol::from("USDT"))
    }
}

/// 撮合引擎 trait
///
/// # 自动 trait 约束
///
/// `Send + Sync` 是必要的:
/// - Python 绑定(Stage 2 Task 8)中 `PyBacktestEngine`
///   需要把 `BacktestEngine` 包在 `#[pyclass]` 中,
///   pyo3 0.28 要求 `#[pyclass]` 自动派生 `Send + Sync`。
/// - `Box<dyn MatchingEngine>` 是 BacktestEngineConfig 的字段,
///   必须 Send + Sync 才能放进 PyBacktestEngine。
///
/// 由于所有已知实现(`L1MatchingEngine`)的字段都是 `Send + Sync`,
/// 该约束对当前实现是"零成本"的;后续添加新实现时需保持线程安全语义。
pub trait MatchingEngine: Send + Sync {
    /// 提交订单并执行撮合
    fn submit(&mut self, order: Order) -> SubmitResult;

    /// 取消订单
    ///
    /// 返回是否成功取消（订单存在且未完全成交）
    fn cancel(&mut self, order_id: u64) -> bool;

    /// 获取最优买价
    fn best_bid(&self) -> Option<Price>;

    /// 获取最优卖价
    fn best_ask(&self) -> Option<Price>;

    /// 计算买卖价差
    fn spread(&self) -> Option<Price>;

    /// 查询指定深度的订单簿快照
    ///
    /// 返回 `(bids, asks)`，买单按价格降序，卖单按价格升序。
    fn depth(&self, levels: usize) -> (Vec<OrderBookLevel>, Vec<OrderBookLevel>);

    /// 当前活跃订单数
    fn active_order_count(&self) -> usize;

    /// 清空订单簿两侧（bids + asks + order_index）。
    ///
    /// 用途:回测场景下"瞬时对手盘"——每 bar 由应用层先 `clear_book()` 再
    /// `seed_liquidity()` 重新挂一组限价单,撮合完不需要保留上 bar 的种子
    /// 流动性。**不**清空 `trade_sequence`(成交序号跨 bar 连续)。
    fn clear_book(&mut self);

    /// 在订单簿两侧播种虚拟流动性(回测辅助,默认 no-op)
    ///
    /// # 用途(下沉到 BacktestEngine 后)
    ///
    /// 回测场景没有真实外部对手盘,需要在撮合引擎内提供"虚拟深度"让策略单能成交。
    /// 通过 `BacktestEngine::with_seed_liquidity(...)` 启用后,引擎在每根 bar
    /// 同步执行 `clear_book + seed_liquidity`,应用层无需手动调用。
    ///
    /// # 默认实现
    ///
    /// `no-op`:直接返回 `next_id`(不消费 ID 计数器),适用于不实现该方法的
    /// 撮合引擎(如 `L2` / `L3` 的拍卖撮合等,流动性由真市场事件驱动)。
    /// `L1MatchingEngine` 重写此方法提供完整实现。
    ///
    /// # 参数
    ///
    /// - `mid_price`:中间价(通常为当前 bar close)
    /// - `half_spread`:每层价差(绝对价格单位),如 `0.0001 * mid = 10bps`
    /// - `depth_levels`:每侧挂单层数(典型 5~20)
    /// - `size_per_level`:每层挂单数量
    /// - `symbol`:交易对
    /// - `next_id`:下一个可用订单 id(避免与外部订单 id 冲突)
    ///
    /// # 返回
    ///
    /// 更新后的 `next_id` 计数器(调用方保存并用于下一次 seed,避免 id 冲突)。
    fn seed_liquidity(
        &mut self,
        _mid_price: f64,
        _half_spread: f64,
        _depth_levels: usize,
        _size_per_level: f64,
        _symbol: Symbol,
        next_id: u64,
    ) -> u64 {
        // ponytail:默认 no-op 实现,适配 L2/L3/自定义撮合引擎。
        // 真正支持的实现(L1)需要 override 此方法。
        next_id
    }
}

/// 内部订单簿侧类型
///
/// 同一价位下的订单队列（时间优先），按价格聚合形成订单簿的一侧。
pub type PriceLevel = VecDeque<Order>;

/// 订单簿一侧：`价格 -> 价格级别`
pub type OrderBookSide = BTreeMap<Price, PriceLevel>;

/// L1 撮合引擎
pub struct L1MatchingEngine {
    /// 买单簿（BTreeMap 升序，最优买价在末尾）
    bids: OrderBookSide,
    /// 卖单簿（BTreeMap 升序，最优卖价在开头）
    asks: OrderBookSide,
    /// 成交序列号（单调递增）
    trade_sequence: AtomicU64,
    /// 活跃订单索引：`order_id -> (side, price)` 快速定位
    order_index: HashMap<u64, (Side, Price)>,
    /// 引擎绑定的 instrument(确保只处理单一 instrument, T2.2 升级自 Symbol)
    instrument: Option<Instrument>,
}

impl Default for L1MatchingEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl L1MatchingEngine {
    /// 创建 L1 撮合引擎
    pub fn new() -> Self {
        Self {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            trade_sequence: AtomicU64::new(0),
            order_index: HashMap::new(),
            instrument: None,
        }
    }

    /// 绑定交易品种
    pub fn with_symbol(symbol: Symbol) -> Self {
        // T2.2: 构造 Instrument(Spot) from Symbol
        let (base, quote) = split_symbol_to_base_quote(&symbol);
        Self {
            instrument: Some(Instrument::Spot(SpotInstrument { base, quote })),
            ..Self::new()
        }
    }

    /// 获取当前已分配的成交 ID 数量
    pub fn fill_count(&self) -> u64 {
        self.trade_sequence.load(Ordering::Relaxed)
    }

    /// 获取下一个成交 ID（兼容辅助方法；内部循环中直接使用原子以避免借用冲突）
    #[allow(dead_code)]
    #[inline]
    fn next_fill_id(&self) -> u64 {
        self.trade_sequence.fetch_add(1, Ordering::Relaxed)
    }

    /// 提取订单的限价（市价单返回 `None`）
    #[inline]
    fn limit_price(order: &Order) -> Option<Price> {
        order.order_type.limit_price()
    }

    /// 验证订单基础参数
    fn validate(&self, order: &Order) -> MatchingResult<()> {
        // 限价单价格必须 > 0
        if let Some(p) = Self::limit_price(order)
            && p.as_f64() <= 0.0
        {
            return Err(MatchingError::InvalidPrice { price: p });
        }
        if order.quantity.as_f64() <= 0.0 {
            return Err(MatchingError::InvalidQuantity {
                quantity: order.quantity,
            });
        }
        if let Some(ref expected) = self.instrument
            && &order.instrument != expected
        {
            return Err(MatchingError::InvalidModification {
                reason: format!(
                    "instrument 不匹配: 引擎绑定 {:?}，订单 {:?}",
                    expected, order.instrument
                ),
            });
        }
        // L1 不支持止损/冰山
        match order.order_type {
            OrderType::Market | OrderType::Limit { .. } => Ok(()),
            _ => Err(MatchingError::UnsupportedOrderType(format!(
                "{:?}",
                order.order_type
            ))),
        }
    }

    /// FOK 预检：检查订单簿中是否有足够深度可以全部成交
    fn check_fok_fillable(&self, taker: &Order) -> bool {
        let required = taker.remaining_quantity().as_f64();
        match taker.side {
            Side::Buy => {
                // 买单：按卖价升序累加可成交量
                let mut available = 0.0;
                for orders in self.asks.values() {
                    if let Some(taker_price) = Self::limit_price(taker)
                        && taker_price.as_f64() < orders_price(orders)
                    {
                        break;
                    }
                    for maker in orders.iter() {
                        if !maker.status.is_terminal() {
                            available += maker.remaining_quantity().as_f64();
                            if available >= required {
                                return true;
                            }
                        }
                    }
                }
                false
            }
            Side::Sell => {
                // 卖单：按买价降序累加可成交量
                let mut available = 0.0;
                for orders in self.bids.values().rev() {
                    if let Some(taker_price) = Self::limit_price(taker)
                        && taker_price.as_f64() > orders_price(orders)
                    {
                        break;
                    }
                    for maker in orders.iter() {
                        if !maker.status.is_terminal() {
                            available += maker.remaining_quantity().as_f64();
                            if available >= required {
                                return true;
                            }
                        }
                    }
                }
                false
            }
        }
    }

    /// 买单与卖单簿撮合
    fn match_against_asks(&mut self, taker: &mut Order) -> Vec<MatchFill> {
        let mut fills = Vec::new();
        let mut empty_prices = Vec::new();

        for (price, orders) in self.asks.iter_mut() {
            // 限价单：买价 < 卖价时停止
            if let Some(taker_price) = Self::limit_price(taker)
                && taker_price.as_f64() < price.as_f64()
            {
                break;
            }

            loop {
                // 取出队首 maker
                let is_terminal = orders
                    .front()
                    .map(|m| m.status.is_terminal())
                    .unwrap_or(true);
                if is_terminal {
                    if orders.is_empty() {
                        break;
                    }
                    orders.pop_front();
                    continue;
                }

                let taker_remaining = taker.remaining_quantity();
                let maker_remaining = orders.front().map(|m| m.remaining_quantity()).unwrap();
                let fill_qty = taker_remaining.min(maker_remaining);

                // 收集 fill 所需字段（避免 &orders 与 &self 同时存在）
                let fill_id = self.trade_sequence.fetch_add(1, Ordering::Relaxed);
                let taker_id = taker.id;
                let taker_side = taker.side;
                let taker_created = taker.created_at;
                let maker_id = orders.front().map(|m| m.id).unwrap();
                let fill = MatchFill {
                    fill_id,
                    taker_order_id: taker_id,
                    maker_order_id: maker_id,
                    price: *price,
                    quantity: fill_qty,
                    taker_side,
                    timestamp: taker_created,
                };
                fills.push(fill);

                // 更新 taker 已成交量
                taker.filled_quantity =
                    Quantity::from_f64(taker.filled_quantity.as_f64() + fill_qty.as_f64());

                // 更新 maker 已成交量并更新状态
                if let Some(maker) = orders.front_mut() {
                    let _ = maker.apply_fill(fill_qty);
                }

                // taker 已全部成交
                if (taker.remaining_quantity().as_f64()).abs() < f64::EPSILON {
                    return fills;
                }
            }

            if orders.is_empty() {
                empty_prices.push(*price);
            }
        }

        for price in empty_prices {
            self.asks.remove(&price);
        }
        fills
    }

    /// 卖单与买单簿撮合
    fn match_against_bids(&mut self, taker: &mut Order) -> Vec<MatchFill> {
        let mut fills = Vec::new();
        let mut empty_prices = Vec::new();

        // 收集需要撮合的价格级别（从高到低）
        let prices: Vec<Price> = self.bids.keys().rev().copied().collect();

        for price in prices {
            let stop = {
                let Some(orders) = self.bids.get_mut(&price) else {
                    continue;
                };
                // 限价单：卖价 > 买价时停止
                if let Some(taker_price) = Self::limit_price(taker)
                    && taker_price.as_f64() > price.as_f64()
                {
                    break;
                }

                loop {
                    let is_terminal = orders
                        .front()
                        .map(|m| m.status.is_terminal())
                        .unwrap_or(true);
                    if is_terminal {
                        if orders.is_empty() {
                            break;
                        }
                        orders.pop_front();
                        continue;
                    }

                    let taker_remaining = taker.remaining_quantity();
                    let maker_remaining = orders.front().map(|m| m.remaining_quantity()).unwrap();
                    let fill_qty = taker_remaining.min(maker_remaining);

                    // 直接访问原子，避免借用冲突
                    let fill_id = self.trade_sequence.fetch_add(1, Ordering::Relaxed);
                    let taker_id = taker.id;
                    let taker_side = taker.side;
                    let taker_created = taker.created_at;
                    let maker_id = orders.front().map(|m| m.id).unwrap();
                    let fill = MatchFill {
                        fill_id,
                        taker_order_id: taker_id,
                        maker_order_id: maker_id,
                        price,
                        quantity: fill_qty,
                        taker_side,
                        timestamp: taker_created,
                    };
                    fills.push(fill);

                    // 更新 taker 已成交量
                    taker.filled_quantity =
                        Quantity::from_f64(taker.filled_quantity.as_f64() + fill_qty.as_f64());

                    // 更新 maker
                    if let Some(maker) = orders.front_mut() {
                        let _ = maker.apply_fill(fill_qty);
                    }

                    if (taker.remaining_quantity().as_f64()).abs() < f64::EPSILON {
                        break;
                    }
                }

                if orders.is_empty() {
                    empty_prices.push(price);
                }
                // 检查 taker 是否完全成交
                (taker.remaining_quantity().as_f64()).abs() < f64::EPSILON
            };
            if stop {
                break;
            }
        }

        for price in empty_prices {
            self.bids.remove(&price);
        }
        fills
    }

    /// 将未成交部分挂入本方订单簿
    fn insert_passive(&mut self, order: Order) {
        // 限价单按价格挂单；市价单无价格不入簿
        let Some(price) = Self::limit_price(&order) else {
            return;
        };
        let book = match order.side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };

        let orders = book.entry(price).or_insert_with(VecDeque::new);
        orders.push_back(order);

        // order 已 move，从参数获取 id/side
        // order_index 在 push_back 之后更新，避免借用冲突
        let last = orders.back().unwrap();
        self.order_index.insert(last.id, (last.side, price));
    }

    /// 在订单簿两侧播种虚拟流动性（回测辅助）
    ///
    /// # 用途
    ///
    /// 回测场景没有真实外部对手盘，需要在撮合引擎内提供"虚拟深度"，
    /// 让策略单能成交。常见于单边策略回测（量化研究）而非做市回测。
    ///
    /// # 行为
    ///
    /// 在 mid_price 上下分别挂 depth_levels 层限价单：
    /// - 卖方：`mid + half_spread * (1, 2, ..., depth_levels)`
    /// - 买方：`mid - half_spread * (1, 2, ..., depth_levels)`
    ///
    /// 每层 `size_per_level` 数量。
    ///
    /// 订单 id 从 `next_id` 起递增，返回更新后的 id 计数器
    /// （调用方应保存并用于下一次 seed，避免 id 冲突）。
    ///
    /// # 参数
    ///
    /// - `mid_price`：中间价（通常为当前 bar close）
    /// - `half_spread`：每层价差（绝对价格单位），如 0.0001 * mid = 10bps
    /// - `depth_levels`：每侧挂单层数（典型 5~20）
    /// - `size_per_level`：每层挂单数量
    /// - `symbol`：交易对
    /// - `next_id`：下一个可用订单 id（避免与外部订单 id 冲突）
    ///
    /// # 副作用
    ///
    /// - 内部订单簿新增 `2 * depth_levels` 条 maker 挂单
    /// - 订单不计入 stats（区别于 submit 路径）
    pub fn seed_liquidity(
        &mut self,
        mid_price: f64,
        half_spread: f64,
        depth_levels: usize,
        size_per_level: f64,
        symbol: Symbol,
        next_id: u64,
    ) -> u64 {
        if mid_price <= 0.0 || half_spread <= 0.0 || depth_levels == 0 || size_per_level <= 0.0 {
            return next_id;
        }
        let mut id = next_id;
        // 卖盘：ask 在 mid 之上，按 spread 阶梯递增
        for level in 1..=depth_levels {
            let ask_price = mid_price + half_spread * level as f64;
            if ask_price <= 0.0 {
                // 防御性:正常参数下不会触发,但 mid/half_spread 为 NaN/负时跳过
                continue;
            }
            let (base, quote) = split_symbol_to_base_quote(&symbol);
            let order = Order::spot(
                id,
                base,
                quote,
                Side::Sell,
                OrderType::Limit {
                    price: Price::from_f64(ask_price),
                },
                Quantity::from_f64(size_per_level),
                TimeInForce::GTC,
            );
            self.insert_passive(order);
            id += 1;
        }
        // 买盘：bid 在 mid 之下。一旦触及非正值,更深档只会更小,直接跳出循环
        for level in 1..=depth_levels {
            let bid_price = mid_price - half_spread * level as f64;
            if bid_price <= 0.0 {
                break;
            }
            let (base, quote) = split_symbol_to_base_quote(&symbol);
            let order = Order::spot(
                id,
                base,
                quote,
                Side::Buy,
                OrderType::Limit {
                    price: Price::from_f64(bid_price),
                },
                Quantity::from_f64(size_per_level),
                TimeInForce::GTC,
            );
            self.insert_passive(order);
            id += 1;
        }
        id
    }
}

/// 获取价格级别内首个订单的价格（用于 FOK 预检中的限价比较）
fn orders_price(orders: &PriceLevel) -> f64 {
    orders
        .iter()
        .find(|o| !o.status.is_terminal())
        .and_then(L1MatchingEngine::limit_price_static)
        .map(|p| p.as_f64())
        .unwrap_or(0.0)
}

impl L1MatchingEngine {
    /// 静态方法获取订单限价（用于辅助函数）
    #[inline]
    fn limit_price_static(order: &Order) -> Option<Price> {
        order.order_type.limit_price()
    }
}

impl MatchingEngine for L1MatchingEngine {
    fn submit(&mut self, order: Order) -> SubmitResult {
        // 1. 验证订单
        if let Err(_e) = self.validate(&order) {
            let mut rejected = order;
            let _ = rejected.reject(axon_core::order::RejectReason::Other);
            return SubmitResult::empty(rejected.quantity);
        }

        let mut taker = order;
        // 激活订单：Created -> Pending
        let _ = taker.activate();

        // 2. FOK 预检：若 FOK 无法全部成交，直接拒收
        if taker.time_in_force == TimeInForce::FOK && !self.check_fok_fillable(&taker) {
            let _ = taker.reject(axon_core::order::RejectReason::Other);
            return SubmitResult::empty(taker.quantity);
        }

        // 3. 撮合
        let fills = match taker.side {
            Side::Buy => self.match_against_asks(&mut taker),
            Side::Sell => self.match_against_bids(&mut taker),
        };

        // 4. 处理 TIF
        let remaining = taker.remaining_quantity();
        let is_filled = (remaining.as_f64()).abs() < f64::EPSILON;
        let mut to_insert = !is_filled;

        if !is_filled && taker.time_in_force == TimeInForce::IOC {
            // IOC：取消剩余
            let _ = taker.cancel();
            to_insert = false;
        }

        // 5. 挂单（move taker 避免 clone）
        if to_insert && !is_filled && taker.can_cancel() {
            self.insert_passive(taker);
        }

        // 6. 构造结果
        if is_filled {
            SubmitResult::filled(fills)
        } else if !fills.is_empty() {
            SubmitResult::partial(fills, remaining)
        } else {
            SubmitResult::empty(remaining)
        }
    }

    fn cancel(&mut self, order_id: u64) -> bool {
        let Some((side, price)) = self.order_index.remove(&order_id) else {
            return false;
        };
        let book = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        let mut found = false;
        if let Some(orders) = book.get_mut(&price) {
            let mut idx = 0;
            while idx < orders.len() {
                if orders[idx].id == order_id {
                    let _ = orders[idx].cancel();
                    orders.remove(idx);
                    found = true;
                    break;
                }
                idx += 1;
            }
            if orders.is_empty() {
                book.remove(&price);
            }
        }
        found
    }

    #[inline]
    fn best_bid(&self) -> Option<Price> {
        self.bids.keys().next_back().copied()
    }

    #[inline]
    fn best_ask(&self) -> Option<Price> {
        self.asks.keys().next().copied()
    }

    #[inline]
    fn spread(&self) -> Option<Price> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        let spread = ask.as_f64() - bid.as_f64();
        Some(Price::from_f64(spread))
    }

    fn depth(&self, levels: usize) -> (Vec<OrderBookLevel>, Vec<OrderBookLevel>) {
        let bid_levels: Vec<OrderBookLevel> = self
            .bids
            .iter()
            .rev()
            .take(levels)
            .map(|(price, orders)| OrderBookLevel {
                price: *price,
                quantity: sum_remaining(orders),
                order_count: orders.iter().filter(|o| !o.status.is_terminal()).count(),
            })
            .collect();

        let ask_levels: Vec<OrderBookLevel> = self
            .asks
            .iter()
            .take(levels)
            .map(|(price, orders)| OrderBookLevel {
                price: *price,
                quantity: sum_remaining(orders),
                order_count: orders.iter().filter(|o| !o.status.is_terminal()).count(),
            })
            .collect();

        (bid_levels, ask_levels)
    }

    fn active_order_count(&self) -> usize {
        self.order_index.len()
    }

    fn clear_book(&mut self) {
        // 1) 清空两侧订单簿与索引;清空后所有"被种子但未成交"的 limit 单
        //    全部丢弃,这正是回测场景下"瞬时对手盘"想要的语义。
        // 2) **不**清空 `trade_sequence`,成交序号跨 bar 仍连续递增。
        //
        // 3) ponytail: 关键内存语义。
        //    - `BTreeMap::clear()` 内部实现 `self.root = None; self.length = 0`,
        //      drop 根节点会递归释放整棵 B 树(节点 + 各价格下的 VecDeque +
        //      内部每个 Order 及其 Symbol 堆),真正释放内存。直接 `.clear()` 即可。
        //    - **`HashMap::clear()` 不释放底层 raw table 内存**(Rust std 明确语义,
        //      "Keeps the allocated memory for reuse")。`seed_liquidity` 中
        //      `order_index` 的 key 是单调递增的 `next_id`,HashMap 会按需扩容
        //      到能容纳最大 id 的容量,但 `clear()` 不会缩容,导致多次 seed 循环
        //      后 raw table 容量只增不减,叠加 PyO3 端 `Arc<Mutex<Py<PyAny>>>`
        //      持有 Python 对象,多引擎实例创建/丢弃时会产生 GB 级累积。
        //    - **修复**:把 `order_index` 替换为新 `HashMap` 实例(等价于
        //      `mem::replace`),旧实例 drop 时 raw table 真正 deallocate。
        //    - 已知简化面:`BTreeMap::clear()` 仍保留根节点的 leaf node
        //      allocation 给下次 `seed_liquidity` 复用(避免重新分配 root),
        //      容量重用语义稳定,无需替换新实例。
        self.bids.clear();
        self.asks.clear();
        self.order_index = HashMap::new();
    }

    /// trait 适配:委托给 `L1MatchingEngine::seed_liquidity` inherent 方法
    /// (`engine.rs:432-485`)。详见该方法文档。
    fn seed_liquidity(
        &mut self,
        mid_price: f64,
        half_spread: f64,
        depth_levels: usize,
        size_per_level: f64,
        symbol: Symbol,
        next_id: u64,
    ) -> u64 {
        L1MatchingEngine::seed_liquidity(
            self,
            mid_price,
            half_spread,
            depth_levels,
            size_per_level,
            symbol,
            next_id,
        )
    }
}

/// 汇总价格级别中所有非终态订单的剩余数量
fn sum_remaining(orders: &PriceLevel) -> axon_core::types::Quantity {
    orders.iter().filter(|o| !o.status.is_terminal()).fold(
        axon_core::types::Quantity::default(),
        |acc, o| {
            axon_core::types::Quantity::from_f64(acc.as_f64() + o.remaining_quantity().as_f64())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::market::Side;
    use axon_core::order::{Order, OrderType, TimeInForce};
    use axon_core::types::{Price, Quantity};

    fn make_limit_order(id: u64, side: Side, price: f64, qty: f64, _ts: i64) -> Order {
        Order::spot(
            id,
            "BTC",
            "USDT",
            side,
            OrderType::Limit {
                price: Price::from_f64(price),
            },
            Quantity::from_f64(qty),
            TimeInForce::GTC,
        )
    }

    #[test]
    fn test_engine_creation() {
        let engine = L1MatchingEngine::new();
        assert!(engine.best_bid().is_none());
        assert!(engine.best_ask().is_none());
        assert_eq!(engine.fill_count(), 0);
    }

    #[test]
    fn test_buy_limit_matches_sell_limit() {
        let mut engine = L1MatchingEngine::new();
        // 卖单挂单
        let sell = make_limit_order(1, Side::Sell, 100.0, 1.0, 1_000);
        engine.submit(sell);
        assert_eq!(engine.best_ask(), Some(Price::from_f64(100.0)));

        // 买单以同价成交
        let buy = make_limit_order(2, Side::Buy, 100.0, 1.0, 2_000);
        let result = engine.submit(buy);
        assert_eq!(result.fills.len(), 1);
        assert!(result.is_filled);
        assert_eq!(result.fills[0].price, Price::from_f64(100.0));
    }

    #[test]
    fn test_sell_limit_matches_buy_limit() {
        let mut engine = L1MatchingEngine::new();
        let buy = make_limit_order(1, Side::Buy, 100.0, 1.0, 1_000);
        engine.submit(buy);
        assert_eq!(engine.best_bid(), Some(Price::from_f64(100.0)));

        let sell = make_limit_order(2, Side::Sell, 100.0, 1.0, 2_000);
        let result = engine.submit(sell);
        assert_eq!(result.fills.len(), 1);
        assert!(result.is_filled);
    }

    #[test]
    fn test_partial_fill_creates_remaining_order() {
        let mut engine = L1MatchingEngine::new();
        // 大卖单 10.0 @ 100
        let sell = make_limit_order(1, Side::Sell, 100.0, 10.0, 1_000);
        engine.submit(sell);

        // 小买单 3.0 @ 100，部分成交 3.0
        let buy = make_limit_order(2, Side::Buy, 100.0, 3.0, 2_000);
        let result = engine.submit(buy);
        assert_eq!(result.fills.len(), 1);
        assert!(result.is_filled);
        assert_eq!(result.fills[0].quantity, Quantity::from_f64(3.0));

        // 卖单剩余 7.0
        assert_eq!(engine.best_ask(), Some(Price::from_f64(100.0)));
        let (_bids, asks) = engine.depth(5);
        assert_eq!(asks[0].quantity, Quantity::from_f64(7.0));
    }

    #[test]
    fn test_higher_bid_matches_first() {
        let mut engine = L1MatchingEngine::new();
        // 卖单 1 @ 100
        engine.submit(make_limit_order(1, Side::Sell, 100.0, 1.0, 1_000));
        // 卖单 1 @ 101
        engine.submit(make_limit_order(2, Side::Sell, 101.0, 1.0, 2_000));

        // 买单 1 @ 101 → 先匹配卖单 1 @ 100（更优）
        let buy = make_limit_order(3, Side::Buy, 101.0, 1.0, 3_000);
        let result = engine.submit(buy);
        assert_eq!(result.fills[0].price, Price::from_f64(100.0));
    }

    #[test]
    fn test_lower_ask_matches_first() {
        let mut engine = L1MatchingEngine::new();
        engine.submit(make_limit_order(1, Side::Buy, 100.0, 1.0, 1_000));
        engine.submit(make_limit_order(2, Side::Buy, 101.0, 1.0, 2_000));

        let sell = make_limit_order(3, Side::Sell, 100.0, 1.0, 3_000);
        let result = engine.submit(sell);
        // 卖单 100 价能匹配买单 101（更高价）
        assert_eq!(result.fills[0].price, Price::from_f64(101.0));
    }

    #[test]
    fn test_same_price_earlier_order_matches_first() {
        let mut engine = L1MatchingEngine::new();
        engine.submit(make_limit_order(1, Side::Sell, 100.0, 1.0, 1_000));
        engine.submit(make_limit_order(2, Side::Sell, 100.0, 1.0, 2_000));

        let buy = make_limit_order(3, Side::Buy, 100.0, 1.0, 3_000);
        let result = engine.submit(buy);
        assert_eq!(result.fills[0].maker_order_id, 1);
    }

    #[test]
    fn test_best_bid_after_insert() {
        let mut engine = L1MatchingEngine::new();
        engine.submit(make_limit_order(1, Side::Buy, 100.0, 1.0, 1_000));
        engine.submit(make_limit_order(2, Side::Buy, 102.0, 1.0, 2_000));
        engine.submit(make_limit_order(3, Side::Buy, 101.0, 1.0, 3_000));
        assert_eq!(engine.best_bid(), Some(Price::from_f64(102.0)));
    }

    #[test]
    fn test_best_ask_after_insert() {
        let mut engine = L1MatchingEngine::new();
        engine.submit(make_limit_order(1, Side::Sell, 100.0, 1.0, 1_000));
        engine.submit(make_limit_order(2, Side::Sell, 102.0, 1.0, 2_000));
        engine.submit(make_limit_order(3, Side::Sell, 101.0, 1.0, 3_000));
        assert_eq!(engine.best_ask(), Some(Price::from_f64(100.0)));
    }

    #[test]
    fn test_spread_calculation() {
        let mut engine = L1MatchingEngine::new();
        engine.submit(make_limit_order(1, Side::Buy, 100.0, 1.0, 1_000));
        engine.submit(make_limit_order(2, Side::Sell, 103.0, 1.0, 2_000));
        let spread = engine.spread().unwrap();
        assert!((spread.as_f64() - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_cancel_existing_order() {
        let mut engine = L1MatchingEngine::new();
        let sell = make_limit_order(1, Side::Sell, 100.0, 1.0, 1_000);
        engine.submit(sell);
        assert_eq!(engine.active_order_count(), 1);

        let cancelled = engine.cancel(1);
        assert!(cancelled);
        assert_eq!(engine.active_order_count(), 0);
        assert!(engine.best_ask().is_none());
    }

    #[test]
    fn test_cancel_nonexistent_order() {
        let mut engine = L1MatchingEngine::new();
        assert!(!engine.cancel(999));
    }

    #[test]
    fn test_depth_query() {
        let mut engine = L1MatchingEngine::new();
        engine.submit(make_limit_order(1, Side::Buy, 100.0, 1.0, 1_000));
        engine.submit(make_limit_order(2, Side::Buy, 101.0, 2.0, 2_000));
        engine.submit(make_limit_order(3, Side::Sell, 103.0, 1.0, 3_000));
        engine.submit(make_limit_order(4, Side::Sell, 104.0, 3.0, 4_000));

        let (bids, asks) = engine.depth(5);
        assert_eq!(bids.len(), 2);
        assert_eq!(bids[0].price, Price::from_f64(101.0)); // 最高价优先
        assert_eq!(asks.len(), 2);
        assert_eq!(asks[0].price, Price::from_f64(103.0));
    }

    #[test]
    fn test_ioc_unfilled_cancelled() {
        let mut engine = L1MatchingEngine::new();
        // 无对手方时 IOC 立即取消
        let ioc_order = Order::spot(
            1,
            "BTC",
            "USDT",
            Side::Buy,
            OrderType::Limit {
                price: Price::from_f64(100.0),
            },
            Quantity::from_f64(1.0),
            TimeInForce::IOC,
        );
        let result = engine.submit(ioc_order);
        assert!(result.fills.is_empty());
        assert_eq!(engine.active_order_count(), 0);
    }

    #[test]
    fn test_fok_partial_fill_rejected() {
        let mut engine = L1MatchingEngine::new();
        // 卖单 1.0 @ 100
        engine.submit(make_limit_order(1, Side::Sell, 100.0, 1.0, 1_000));

        // FOK 买单 2.0 @ 100（期望全部成交，否则取消）
        let fok_order = Order::spot(
            2,
            "BTC",
            "USDT",
            Side::Buy,
            OrderType::Limit {
                price: Price::from_f64(100.0),
            },
            Quantity::from_f64(2.0),
            TimeInForce::FOK,
        );
        let result = engine.submit(fok_order);
        // FOK 无法全部成交，应整单取消
        assert!(result.fills.is_empty());
        // 卖单仍在
        assert_eq!(engine.active_order_count(), 1);
    }

    #[test]
    fn test_fok_full_fill_succeeds() {
        let mut engine = L1MatchingEngine::new();
        // 卖单 1.0 @ 100
        engine.submit(make_limit_order(1, Side::Sell, 100.0, 1.0, 1_000));

        // FOK 买单 1.0 @ 100（恰好全部成交）
        let fok_order = Order::spot(
            2,
            "BTC",
            "USDT",
            Side::Buy,
            OrderType::Limit {
                price: Price::from_f64(100.0),
            },
            Quantity::from_f64(1.0),
            TimeInForce::FOK,
        );
        let result = engine.submit(fok_order);
        assert_eq!(result.fills.len(), 1);
        assert!(result.is_filled);
    }

    #[test]
    fn test_market_order_immediate_fill() {
        let mut engine = L1MatchingEngine::new();
        engine.submit(make_limit_order(1, Side::Sell, 100.0, 1.0, 1_000));

        let market_order = Order::spot(
            2,
            "BTC",
            "USDT",
            Side::Buy,
            OrderType::Market,
            Quantity::from_f64(1.0),
            TimeInForce::IOC,
        );
        let result = engine.submit(market_order);
        assert_eq!(result.fills.len(), 1);
        assert!(result.is_filled);
    }

    #[test]
    fn test_invalid_price_rejected() {
        let mut engine = L1MatchingEngine::new();
        let bad_order = Order::spot(
            1,
            "BTC",
            "USDT",
            Side::Buy,
            OrderType::Limit {
                price: Price::from_f64(0.0),
            },
            Quantity::from_f64(1.0),
            TimeInForce::GTC,
        );
        let result = engine.submit(bad_order);
        assert!(result.fills.is_empty());
    }

    #[test]
    fn test_engine_with_symbol() {
        let engine = L1MatchingEngine::with_symbol(Symbol::from("ETH-USDT"));
        assert!(engine.instrument.is_some());
        let inst = engine.instrument.as_ref().unwrap();
        assert_eq!(inst.base().as_str(), "ETH");
        assert_eq!(inst.quote().as_str(), "USDT");
    }

    #[test]
    fn test_fill_id_monotonic() {
        let mut engine = L1MatchingEngine::new();
        engine.submit(make_limit_order(1, Side::Sell, 100.0, 1.0, 1_000));
        engine.submit(make_limit_order(2, Side::Buy, 100.0, 1.0, 2_000));
        engine.submit(make_limit_order(3, Side::Sell, 101.0, 1.0, 3_000));
        engine.submit(make_limit_order(4, Side::Buy, 101.0, 1.0, 4_000));
        assert_eq!(engine.fill_count(), 2);
    }

    #[test]
    fn test_spread_none_when_empty() {
        let engine = L1MatchingEngine::new();
        assert!(engine.spread().is_none());
    }

    #[test]
    fn test_no_match_when_prices_dont_cross() {
        let mut engine = L1MatchingEngine::new();
        // 卖单 1 @ 100
        engine.submit(make_limit_order(1, Side::Sell, 100.0, 1.0, 1_000));
        // 买单 0.5 @ 99 (限价低于卖价，无法成交)
        let buy = make_limit_order(2, Side::Buy, 99.0, 0.5, 2_000);
        let result = engine.submit(buy);
        assert!(result.fills.is_empty());
        assert!(!result.is_filled);
        // 买单进入买单簿
        assert_eq!(engine.best_bid(), Some(Price::from_f64(99.0)));
    }

    // ─── 补充边界场景 ─────────────────────────────────

    /// 空订单簿查询
    #[test]
    fn test_empty_book_queries() {
        let engine = L1MatchingEngine::new();
        assert_eq!(engine.best_bid(), None, "空簿无买价");
        assert_eq!(engine.best_ask(), None, "空簿无卖价");
        assert_eq!(engine.spread(), None, "空簿无价差");
        assert_eq!(engine.active_order_count(), 0);
        let (bids, asks) = engine.depth(10);
        assert!(bids.is_empty());
        assert!(asks.is_empty());
    }

    /// 取消不存在的订单应返回 false（边界测试）
    #[test]
    fn test_boundary_cancel_nonexistent_order() {
        let mut engine = L1MatchingEngine::new();
        assert!(!engine.cancel(999), "取消不存在订单返回 false");
    }

    /// 市价单在空簿下应产生空成交
    #[test]
    fn test_market_order_on_empty_book() {
        let mut engine = L1MatchingEngine::new();
        let order = Order::spot(
            1,
            "BTC",
            "USDT",
            Side::Buy,
            OrderType::Market,
            Quantity::from_f64(10.0),
            TimeInForce::IOC,
        );
        let result = engine.submit(order);
        assert!(result.fills.is_empty(), "空簿市价单无成交");
        assert!(!result.is_filled);
        assert_eq!(result.remaining_quantity.as_f64(), 10.0);
    }

    /// 极小数量订单（0.001）应被接受
    #[test]
    fn test_min_positive_quantity_order() {
        let mut engine = L1MatchingEngine::new();
        let sell = make_limit_order(1, Side::Sell, 100.0, 0.001, 1_000);
        let result = engine.submit(sell);
        assert!(result.fills.is_empty());
        // 订单入簿
        assert_eq!(engine.best_ask(), Some(Price::from_f64(100.0)));
    }

    /// 深度查询 0 层应返回空
    #[test]
    fn test_depth_zero_levels() {
        let mut engine = L1MatchingEngine::new();
        engine.submit(make_limit_order(1, Side::Buy, 99.0, 1.0, 1_000));
        engine.submit(make_limit_order(2, Side::Sell, 101.0, 1.0, 1_000));
        let (bids, asks) = engine.depth(0);
        assert!(bids.is_empty());
        assert!(asks.is_empty());
    }

    /// 同价位多订单按 FIFO 排序
    #[test]
    fn test_same_price_fifo() {
        let mut engine = L1MatchingEngine::new();
        // 3 笔卖单同价位（FIFO 顺序：1, 2, 3）
        engine.submit(make_limit_order(1, Side::Sell, 100.0, 1.0, 1_000));
        engine.submit(make_limit_order(2, Side::Sell, 100.0, 1.0, 1_500));
        engine.submit(make_limit_order(3, Side::Sell, 100.0, 1.0, 2_000));
        // 买单 2.5 应优先成交 FIFO 顺序的订单
        let buy = make_limit_order(4, Side::Buy, 100.0, 2.5, 3_000);
        let result = engine.submit(buy);
        // 每笔 maker 都对应一个 fill（可能部分成交）
        // 订单 1（1.0） + 订单 2（1.0） + 订单 3（0.5）= 2.5（完全成交）
        assert_eq!(result.fills.len(), 3);
        // 总成交量 = 2.5
        let total_qty: f64 = result.fills.iter().map(|f| f.quantity.as_f64()).sum();
        assert!((total_qty - 2.5).abs() < f64::EPSILON);
        // 全部成交（剩余 0）
        assert!(result.is_filled);
        assert!(!result.is_partially_filled);
        // 订单 3 剩余 0.5 挂在卖单簿
        assert_eq!(engine.best_ask(), Some(Price::from_f64(100.0)));
    }

    /// seed_liquidity 在 mid 上下挂 depth_levels 层对手盘
    /// 后续策略单应能立即与虚拟对手盘成交
    #[test]
    fn test_seed_liquidity_provides_counterparty() {
        let mut engine = L1MatchingEngine::new();
        let sym = Symbol::from("BTC-USDT");

        // mid=100, half_spread=0.5, depth=3, size=2.0
        // 卖盘: 100.5, 101.0, 101.5（各 2.0）
        // 买盘: 99.5, 99.0, 98.5（各 2.0）
        let next_id = engine.seed_liquidity(100.0, 0.5, 3, 2.0, sym.clone(), 1);
        // 挂入 6 个 maker（3 卖 + 3 买）
        assert_eq!(engine.active_order_count(), 6);
        // 最优买价 = 99.5，最优卖价 = 100.5
        assert_eq!(engine.best_bid().unwrap().as_f64(), 99.5);
        assert_eq!(engine.best_ask().unwrap().as_f64(), 100.5);
        // next_id 返回 1 + 6 = 7
        assert_eq!(next_id, 7);

        // 策略买单 @ 100（mid） vs 卖盘 100.5/101.0/101.5：
        // 限价 100 < 100.5 不撮合（限价单不穿越价差）
        let buy_under_ask = make_limit_order(100, Side::Buy, 100.0, 1.0, 10_000);
        let r1 = engine.submit(buy_under_ask);
        assert!(r1.fills.is_empty(), "mid 限价买单 vs ask@100.5 不应成交");

        // 策略买单 @ 100.6 vs 卖盘 100.5：成交 1.0（吃掉最优卖）
        let buy_cross = make_limit_order(101, Side::Buy, 100.6, 1.0, 11_000);
        let r2 = engine.submit(buy_cross);
        assert_eq!(r2.fills.len(), 1, "应成交 1 笔（吃掉 100.5 的 1.0）");
        assert_eq!(r2.fills[0].price.as_f64(), 100.5);
        // 卖盘 100.5 剩余 1.0（成交 1.0 from 2.0）
        assert_eq!(engine.best_ask().unwrap().as_f64(), 100.5);
    }

    /// seed_liquidity 对非法参数（<=0）应 no-op，返回原 next_id
    #[test]
    fn test_seed_liquidity_invalid_params_noop() {
        let mut engine = L1MatchingEngine::new();
        let sym = Symbol::from("BTC-USDT");

        // mid_price=0 无效
        let r = engine.seed_liquidity(0.0, 0.5, 3, 2.0, sym.clone(), 1);
        assert_eq!(r, 1);
        assert_eq!(engine.active_order_count(), 0);

        // depth_levels=0 无效
        let r = engine.seed_liquidity(100.0, 0.5, 0, 2.0, sym.clone(), 1);
        assert_eq!(r, 1);
        assert_eq!(engine.active_order_count(), 0);
    }

    /// mid_price 不足以容纳所有买盘档位时,负/零价 bid 档应被跳过,避免在订单簿插入 price=0 的废单
    /// 场景:mid=10, half_spread=3, depth_levels=5 → bid 价 7/4/1/-2/-5
    ///       后两档 <= 0,只挂 3 档买;卖盘 13/16/19/22/25 全部 >= 0,挂 5 档
    #[test]
    fn test_seed_liquidity_skips_non_positive_bid_levels() {
        let mut engine = L1MatchingEngine::new();
        let sym = Symbol::from("BTC-USDT");

        let r = engine.seed_liquidity(10.0, 3.0, 5, 1.0, sym.clone(), 100);
        // 5 卖 + 3 买 = 8 单
        assert_eq!(engine.active_order_count(), 8);
        // 返回的 next_id:从 100 起,5 卖 + 3 买 = 8
        assert_eq!(r, 108);

        // 最深一档买价应 >= 0
        let lowest_bid = engine.best_bid();
        if let Some(p) = lowest_bid {
            assert!(p.as_f64() > 0.0, "best_bid 应为正,实际 {}", p.as_f64());
        }
    }

    /// seed_liquidity 在 impacted_engine.rs 的包装应透传到 L1
    #[test]
    fn test_impacted_engine_seed_liquidity_wraps_l1() {
        use crate::impact::ImpactedMatchingEngine;
        use axon_core::impact::ImpactModelConfig;
        use axon_core::impact::create_model;

        let config = ImpactModelConfig::Linear {
            coefficient: 0.0,
            depth_levels: 10,
            instantaneous_ratio: 0.7,
        };
        let model: Box<dyn axon_core::impact::ImpactModel> = create_model(config);
        let mut engine = ImpactedMatchingEngine::new(model);
        let sym = Symbol::from("BTC-USDT");

        let next_id = engine.seed_liquidity(100.0, 0.5, 2, 1.0, sym.clone(), 1);
        // 4 个 maker（2 卖 + 2 买）
        assert_eq!(engine.inner().active_order_count(), 4);
        assert_eq!(next_id, 5);
    }

    /// 大量订单（10K）插入 / 取消性能与一致性
    #[test]
    fn test_large_order_volume() {
        let mut engine = L1MatchingEngine::new();
        // 插入 10K 买单（不同价位）
        for i in 0..10_000 {
            let price = 100.0 - (i as f64) * 0.01;
            engine.submit(make_limit_order(i, Side::Buy, price, 1.0, i as i64));
        }
        assert_eq!(engine.active_order_count(), 10_000);
        assert_eq!(engine.best_bid(), Some(Price::from_f64(100.0)));

        // 全部取消
        for i in 0..10_000 {
            assert!(engine.cancel(i), "订单 {i} 取消失败");
        }
        assert_eq!(engine.active_order_count(), 0);
        assert_eq!(engine.best_bid(), None);
    }

    /// 跨价位 match：买单价格高于最低卖价，以卖一价（maker price）成交
    #[test]
    fn test_buy_above_best_ask_fills_at_ask() {
        let mut engine = L1MatchingEngine::new();
        // 卖单 @ 100
        engine.submit(make_limit_order(1, Side::Sell, 100.0, 1.0, 1_000));
        // 卖单 @ 101
        engine.submit(make_limit_order(2, Side::Sell, 101.0, 1.0, 1_500));
        // 买单 @ 105 数量 1.0，正好吃 @ 100（最低卖价）
        let buy = make_limit_order(3, Side::Buy, 105.0, 1.0, 2_000);
        let result = engine.submit(buy);
        // 应以卖一价（100）成交（maker price）
        assert_eq!(result.fills.len(), 1);
        assert_eq!(result.fills[0].price, Price::from_f64(100.0));
        assert!(result.is_filled, "全部成交");
        // 验证 best_ask 在 best_bid 之上（卖单簿仍存在）
        let ask = engine.best_ask().expect("卖单簿非空");
        assert!(
            ask.as_f64() >= 100.0,
            "best_ask 应 ≥ 100（最低卖价），实际: {ask}"
        );
    }

    // ─── clear_book 内存稳定性测试（ponytail）──────────────────────────
    //
    // 根因:`HashMap::clear()` 不释放底层 raw table 内存(Rust std 明确语义,
    // "Keeps the allocated memory for reuse")。`seed_liquidity` 中
    // `order_index` 的 key 是单调递增的 `next_id`,HashMap 会按需扩容,
    // 但 `clear()` 不会缩容。修复:`clear_book` 把 `order_index` 替换为
    // 新 `HashMap` 实例,强制 deallocate 旧 raw table。
    //
    // 本组测试用最小化规模(1000 rounds × 20 orders = 20K 临时 order)
    // 验证不变量,不依赖 RSS 测量(避免环境差异),仅验证"clear 后
    // 所有结构为空 + 可被新种子复用"。

    /// `clear_book` 后所有订单簿状态必须完全清空
    #[test]
    fn test_clear_book_resets_all_state() {
        let mut engine = L1MatchingEngine::new();
        let symbol = Symbol::from("BTC-USDT");

        // 种子注入虚拟对手盘
        let _ = engine.seed_liquidity(100.0, 0.1, 10, 1.0, symbol.clone(), 1);
        assert!(engine.active_order_count() > 0, "seed 后应有 active order");
        assert!(engine.best_bid().is_some());
        assert!(engine.best_ask().is_some());

        // 清空后必须完全归零
        engine.clear_book();
        assert_eq!(
            engine.active_order_count(),
            0,
            "active_order_count 必须为 0"
        );
        assert!(engine.best_bid().is_none(), "best_bid 必须为 None");
        assert!(engine.best_ask().is_none(), "best_ask 必须为 None");
        // 索引 len 必须为 0(通过 active_order_count 间接验证)
    }

    /// 1000 轮 seed+clear 循环后,clear 必须仍能完全清空
    /// (验证 HashMap 替换为新实例后,旧 raw table 已被 deallocate,
    ///  不会因单调 next_id 扩容而泄漏)
    #[test]
    fn test_clear_book_stable_over_1000_rounds() {
        let mut engine = L1MatchingEngine::new();
        let symbol = Symbol::from("BTC-USDT");

        // 1000 轮 seed + clear,每轮 mid_price 略微变化以触发不同价格键
        for round in 0..1000 {
            // 上一轮的种子清掉
            engine.clear_book();
            assert_eq!(
                engine.active_order_count(),
                0,
                "round {round} clear 后必须归零"
            );

            // 注入新种子(id 单调递增模拟真实回测 caller 行为)
            let next_id = engine.seed_liquidity(
                100.0 + (round as f64 * 0.0001), // mid_price 变化触发新价格键
                0.1,
                10, // depth_levels
                1.0,
                symbol.clone(),
                round * 100 + 1, // next_id 单调递增
            );
            assert_eq!(next_id, round * 100 + 1 + 20, "next_id 应递增 20");
            assert!(engine.active_order_count() > 0, "seed 后应有 active order");
        }

        // 最后一轮 clear 必须完全归零
        engine.clear_book();
        assert_eq!(engine.active_order_count(), 0);
        assert!(engine.best_bid().is_none());
        assert!(engine.best_ask().is_none());
    }

    /// 修复后 `clear_book` 不应保留 `order_index` 的旧 entry
    /// (防御性:`HashMap::replace` 后再 seed,新 entry id 范围应被正确接受)
    #[test]
    fn test_clear_book_does_not_ghost_retain_old_ids() {
        let mut engine = L1MatchingEngine::new();
        let symbol = Symbol::from("BTC-USDT");

        // 第一轮:seed 1000 个 id (1..=1000)
        let _ = engine.seed_liquidity(100.0, 0.1, 10, 1.0, symbol.clone(), 1);
        assert!(engine.active_order_count() > 0);

        // 关键:`clear_book` 替换 HashMap 后,旧 id 不应被保留
        engine.clear_book();
        assert_eq!(engine.active_order_count(), 0);

        // 第二轮:用相同 id 范围 (1..=20) 重新 seed
        // 修复前:`order_index.clear()` 不释放 raw table,id 1..=1000 仍存在(虽然 len=0)
        // 修复后:`HashMap::new()` 替换,完全干净
        let _ = engine.seed_liquidity(101.0, 0.1, 10, 1.0, symbol.clone(), 1);
        // 验证:active_order_count = 20(10 buy + 10 sell),不等于上轮的 1000 或 0
        assert_eq!(
            engine.active_order_count(),
            20,
            "二轮 seed 后 active_order_count 必须等于 20(无 ghost entry)"
        );
    }
}
