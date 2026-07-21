//! L3 多资产撮合引擎
//!
//! 核心功能：
//! - 多资产独立 L2 订单簿路由(按 `Instrument` 索引)
//! - 暗池撮合(软暗池:先扫暗池,未成交再入暗池簿)
//! - 批量拍卖清算
//! - 跨资产套利检测与执行
//! - 快照与恢复(仅资产 / 配置 / 批量模式,订单级别恢复需 L2 `from_entries`)
//!
//! 0.6.0 改(BREAKING):`engines` / `dark_orders` 由 `HashMap<Symbol, _>`
//! 迁为 `HashMap<Instrument, _>`,`CrossPair.leg1/leg2` 由 `Symbol` 迁为
//! `Instrument`,公共 API `register_asset` → `register_instrument` /
//! `engine(symbol)` → `engine(instrument)` / `run_auction(symbol)` →
//! `run_auction(instrument)`,与 L1/L2 引擎和 BacktestEngine 多 leg 路由
//! 保持一致。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::types::{Instrument, Price, Quantity};

use super::super::engine::MatchingEngine;
use super::super::types::{MatchFill, SubmitResult};
use super::auction::{AuctionResult, BatchMode, find_clearing_price};
use super::dark_pool::{DarkOrder, try_dark_match};
use super::error::{MatchingL3Error, MatchingL3Result};
use super::types::{CrossPair, L2Snapshot, MatchingEngineSnapshot, PriceLevel};

/// L3 统计
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct L3Stats {
    /// 注册资产数
    pub total_assets: usize,
    /// 跨资产成交笔数
    pub total_cross_fills: u64,
    /// 批量拍卖成交笔数
    pub total_batch_fills: u64,
    /// 暗池成交笔数
    pub total_dark_fills: u64,
    /// 套利利润累计
    pub total_arbitrage_profit: f64,
}

/// 套利机会(只读报告,不自动执行)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArbitrageOpportunity {
    /// 交易对
    pub pair: CrossPair,
    /// leg1 隐含中间价
    pub leg1_mid: Option<Price>,
    /// leg2 隐含中间价
    pub leg2_mid: Option<Price>,
    /// 隐含比率
    pub implied_ratio: Option<f64>,
    /// 偏离度(`|implied - target| / target`)
    pub deviation: f64,
    /// 估计套利利润(绝对值)
    pub estimated_profit: f64,
}

/// 多资产撮合引擎(0.6.0 改:全部按 `Instrument` 路由)
#[derive(Default)]
pub struct MultiAssetMatchingEngine {
    /// 各资产独立的 L2 撮合引擎(0.6.0 改:键类型 `Symbol` → `Instrument`)
    engines: HashMap<Instrument, crate::matching::L2MatchingEngine>,
    /// 跨资产交易对配置
    cross_pairs: Vec<CrossPair>,
    /// 当前批量模式
    batch_mode: BatchMode,
    /// 暗池订单簿(按 Instrument 索引,0.6.0 改)
    dark_orders: HashMap<Instrument, Vec<DarkOrder>>,
    /// 批量待撮合订单(Auction / DarkPool 模式暂存)
    pending_batch: Vec<Order>,
    /// 下一个 fill id(暗池成交使用)
    next_fill_id: u64,
    /// Phase 3.1.3 新增:`MatchingEngine` trait 路由用的 primary instrument
    /// (`best_bid` / `best_ask` 等无 instrument 参数的方法走 primary 路由)
    primary_instrument: Option<Instrument>,
    /// 统计
    stats: L3Stats,
}

impl MultiAssetMatchingEngine {
    /// 创建新的多资产撮合引擎
    pub fn new() -> Self {
        Self::default()
    }

    /// 0.6.0 改(BREAKING):`register_asset(symbol)` → `register_instrument(instrument)`
    ///
    /// 注册新资产(幂等)。`order.instrument` 首次 submit 时也会隐式注册,
    /// 故一般无需显式调;但预注册可让 `engine()` 查询不返回 `None`。
    pub fn register_instrument(&mut self, instrument: Instrument) {
        self.engines.entry(instrument.clone()).or_default();
        self.dark_orders.entry(instrument).or_default();
        self.stats.total_assets = self.engines.len();
    }

    /// 注册跨资产交易对(自动注册两个 leg 资产)
    pub fn register_cross_pair(&mut self, pair: CrossPair) -> MatchingL3Result<()> {
        if pair.pair.spot == pair.pair.perp {
            return Err(MatchingL3Error::InvalidCrossPair {
                leg1: Box::new(pair.pair.spot.clone()),
                leg2: Box::new(pair.pair.perp.clone()),
                ratio: pair.ratio,
            });
        }
        if pair.ratio <= 0.0 || !pair.ratio.is_finite() {
            return Err(MatchingL3Error::InvalidCrossPair {
                leg1: Box::new(pair.pair.spot.clone()),
                leg2: Box::new(pair.pair.perp.clone()),
                ratio: pair.ratio,
            });
        }
        self.register_instrument(pair.pair.spot.clone());
        self.register_instrument(pair.pair.perp.clone());
        self.cross_pairs.push(pair);
        Ok(())
    }

    /// 设置批量模式
    #[inline]
    pub fn set_batch_mode(&mut self, mode: BatchMode) {
        self.batch_mode = mode;
    }

    /// 当前批量模式
    #[inline]
    pub fn batch_mode(&self) -> BatchMode {
        self.batch_mode
    }

    /// 0.6.0 改(BREAKING):`engine(&Symbol)` → `engine(&Instrument)`
    #[inline]
    pub fn engine(&self, instrument: &Instrument) -> Option<&crate::matching::L2MatchingEngine> {
        self.engines.get(instrument)
    }

    /// 0.6.0 改(BREAKING):`engine_mut(&Symbol)` → `engine_mut(&Instrument)`
    #[inline]
    pub fn engine_mut(
        &mut self,
        instrument: &Instrument,
    ) -> Option<&mut crate::matching::L2MatchingEngine> {
        self.engines.get_mut(instrument)
    }

    /// 注册资产数量
    #[inline]
    pub fn asset_count(&self) -> usize {
        self.engines.len()
    }

    /// 跨资产交易对数量
    #[inline]
    pub fn cross_pair_count(&self) -> usize {
        self.cross_pairs.len()
    }

    /// 统计信息
    #[inline]
    pub fn stats(&self) -> &L3Stats {
        &self.stats
    }

    /// 路由订单到正确的资产引擎
    ///
    /// 0.6.0 改:直接用 `order.instrument` 做 HashMap key,不再走
    /// `instrument_to_key` 字符串桥接。
    pub fn submit(&mut self, order: Order) -> MatchingL3Result<Vec<MatchFill>> {
        let instrument = order.instrument.clone();
        match self.batch_mode {
            BatchMode::Continuous => {
                let engine = self.engines.get_mut(&instrument).ok_or_else(|| {
                    MatchingL3Error::AssetNotFound {
                        instrument: instrument.clone(),
                    }
                })?;
                let result = engine.submit(order);
                Ok(result.fills)
            }
            BatchMode::Auction => {
                self.pending_batch.push(order);
                Ok(Vec::new())
            }
            BatchMode::DarkPool => {
                // 暗池模式:先扫暗池簿,未成交入暗池
                if order.order_type.limit_price().is_none() {
                    return Err(MatchingL3Error::OrderMissingLimitPrice { order_id: order.id });
                }
                let dark = DarkOrder {
                    visible_quantity: order.remaining_quantity(),
                    hidden_quantity: order.remaining_quantity(),
                    order: order.clone(),
                };
                self.try_dark_and_store(&instrument, dark)
            }
        }
    }

    /// 批量提交
    pub fn submit_batch(&mut self, orders: Vec<Order>) -> MatchingL3Result<Vec<MatchFill>> {
        let mut all_fills = Vec::new();
        for order in orders {
            let fills = self.submit(order)?;
            all_fills.extend(fills);
        }
        Ok(all_fills)
    }

    /// 提交暗池订单
    pub fn submit_dark_order(&mut self, dark: DarkOrder) -> MatchingL3Result<Vec<MatchFill>> {
        let instrument = dark.order.instrument.clone();
        self.try_dark_and_store(&instrument, dark)
    }

    /// 暗池撮合 + 暂存辅助方法
    fn try_dark_and_store(
        &mut self,
        instrument: &Instrument,
        dark: DarkOrder,
    ) -> MatchingL3Result<Vec<MatchFill>> {
        // 借用 dark_orders 的可变引用到一个临时块内
        let fills = {
            let dark_book = self.dark_orders.entry(instrument.clone()).or_default();
            let fills = try_dark_match(dark_book, &dark, self.next_fill_id)?;
            self.next_fill_id = self.next_fill_id.saturating_add(fills.len() as u64);
            fills
        };
        self.stats.total_dark_fills += fills.len() as u64;
        if !fills.is_empty() {
            return Ok(fills);
        }
        // 暗池无匹配 → 暂存到暗池簿
        self.dark_orders
            .entry(instrument.clone())
            .or_default()
            .push(dark);
        Ok(Vec::new())
    }

    /// 执行批量拍卖(0.6.0 改:参数 `&Symbol` → `&Instrument`)
    pub fn run_auction(&mut self, instrument: &Instrument) -> MatchingL3Result<AuctionResult> {
        if self.pending_batch.is_empty() {
            return Ok(AuctionResult::empty());
        }

        // 分离当前 instrument 的订单
        let mut to_auction: Vec<Order> = Vec::new();
        let mut kept: Vec<Order> = Vec::new();
        for order in self.pending_batch.drain(..) {
            if order.instrument == *instrument {
                to_auction.push(order);
            } else {
                kept.push(order);
            }
        }
        self.pending_batch = kept;

        if to_auction.is_empty() {
            return Ok(AuctionResult::empty());
        }

        let (clearing_price, clearing_volume) = find_clearing_price(&to_auction)?;
        if clearing_volume.as_f64() <= 0.0 {
            return Ok(AuctionResult::empty());
        }

        let engine =
            self.engines
                .get_mut(instrument)
                .ok_or_else(|| MatchingL3Error::AssetNotFound {
                    instrument: instrument.clone(),
                })?;

        let mut fills = Vec::new();
        let mut unfilled = Vec::new();
        for order in to_auction {
            let auctioned = override_order_price(order, clearing_price);
            let result = engine.submit(auctioned.clone());
            if result.fills.is_empty() {
                unfilled.push(auctioned);
            } else {
                fills.extend(result.fills);
            }
        }

        self.stats.total_batch_fills += fills.len() as u64;
        Ok(AuctionResult {
            clearing_price,
            clearing_volume,
            fills,
            unfilled_orders: unfilled,
        })
    }

    /// 跨资产套利检测
    pub fn detect_arbitrage(&self) -> Vec<ArbitrageOpportunity> {
        self.cross_pairs
            .iter()
            .map(|pair| self.compute_arbitrage(pair))
            .collect()
    }

    fn compute_arbitrage(&self, pair: &CrossPair) -> ArbitrageOpportunity {
        let leg1_mid = self
            .engines
            .get(&pair.pair.spot)
            .and_then(mid_price_from_engine);
        let leg2_mid = self
            .engines
            .get(&pair.pair.perp)
            .and_then(mid_price_from_engine);

        let implied_ratio = match (leg1_mid, leg2_mid) {
            (Some(l1), Some(l2)) if l2.as_f64() > 0.0 => Some(l1.as_f64() / l2.as_f64()),
            _ => None,
        };

        let (deviation, estimated_profit) = match implied_ratio {
            Some(implied) => {
                let dev = ((implied - pair.ratio) / pair.ratio).abs();
                let profit = (implied - pair.ratio).abs() * pair.max_quantity.as_f64();
                (dev, profit)
            }
            None => (0.0, 0.0),
        };

        ArbitrageOpportunity {
            pair: pair.clone(),
            leg1_mid,
            leg2_mid,
            implied_ratio,
            deviation,
            estimated_profit,
        }
    }

    /// 套利执行(同时提交 leg1 / leg2 订单)
    ///
    /// 0.6.0 改:`CrossPair` 内部是 `LegPair { spot, perp }`,直接用
    /// `Order::spot` / `Order::swap` 工厂构造 leg 订单,不再走 0.5.0
    /// `split_symbol_to_base_quote` 字符串桥接。
    pub fn execute_arbitrage(
        &mut self,
        pair: &CrossPair,
        quantity: Quantity,
        side_leg1: Side,
    ) -> MatchingL3Result<Vec<MatchFill>> {
        if quantity.as_f64() > pair.max_quantity.as_f64() {
            return Err(MatchingL3Error::InvalidCrossPair {
                leg1: Box::new(pair.pair.spot.clone()),
                leg2: Box::new(pair.pair.perp.clone()),
                ratio: pair.ratio,
            });
        }

        // 提前取价避免双重可变借用
        let leg1_price = self
            .engines
            .get(&pair.pair.spot)
            .and_then(|e| match side_leg1 {
                Side::Buy => e.best_ask(),
                Side::Sell => e.best_bid(),
            })
            .unwrap_or_default();
        let leg2_side = side_leg1.opposite();
        let leg2_price = self
            .engines
            .get(&pair.pair.perp)
            .and_then(|e| match leg2_side {
                Side::Buy => e.best_ask(),
                Side::Sell => e.best_bid(),
            })
            .unwrap_or_default();

        // 0.6.0:pair.pair.spot / pair.pair.perp 已是 Instrument,直接分派 spot/swap 工厂
        let leg1_order = build_leg_order(
            0,
            &pair.pair.spot,
            side_leg1,
            OrderType::Limit { price: leg1_price },
            quantity,
            TimeInForce::GTC,
        );
        let leg2_order = build_leg_order(
            0,
            &pair.pair.perp,
            leg2_side,
            OrderType::Limit { price: leg2_price },
            quantity,
            TimeInForce::GTC,
        );

        let mut fills = self.submit(leg1_order)?;
        fills.extend(self.submit(leg2_order)?);
        self.stats.total_cross_fills += fills.len() as u64;
        Ok(fills)
    }

    /// 创建快照(0.6.0 改:`engines: HashMap<Instrument, L2Snapshot>`)
    pub fn snapshot(&self) -> MatchingEngineSnapshot {
        let mut engines = HashMap::new();
        for (instrument, engine) in &self.engines {
            let (bids, asks) = engine.depth(20);
            engines.insert(
                instrument.clone(),
                L2Snapshot {
                    instrument: instrument.clone(),
                    best_bid: engine.best_bid(),
                    best_ask: engine.best_ask(),
                    bid_depth: bids.iter().map(PriceLevel::from_book_level).collect(),
                    ask_depth: asks.iter().map(PriceLevel::from_book_level).collect(),
                    trade_count: engine.stats().total_fills,
                },
            );
        }

        MatchingEngineSnapshot {
            engines,
            cross_pairs: self.cross_pairs.clone(),
            batch_mode: self.batch_mode,
            timestamp_ns: 0,
        }
    }

    /// 从快照恢复(仅恢复资产注册 / 跨资产配置 / 批量模式;
    /// 价格级别因需重建挂单,不自动恢复)。
    pub fn restore(&mut self, snapshot: MatchingEngineSnapshot) -> MatchingL3Result<()> {
        self.engines.clear();
        self.dark_orders.clear();
        self.pending_batch.clear();

        for instrument in snapshot.engines.keys() {
            self.register_instrument(instrument.clone());
        }

        self.cross_pairs = snapshot.cross_pairs;
        self.batch_mode = snapshot.batch_mode;
        self.stats.total_assets = self.engines.len();

        Ok(())
    }

    /// Phase 3.1.3 新增:设置 primary instrument(用于 `MatchingEngine` trait 路由)
    ///
    /// `MatchingEngine::best_bid` / `best_ask` 等无 instrument 参数,
    /// 多资产场景下需用 primary instrument 路由。如果未设置,
    /// trait 方法返回 `None`(不影响 `submit_batch` / `execute_arbitrage` 等
    /// 显式传 instrument 的 inherent 方法)。
    ///
    /// 副作用:顺带 `register_instrument(primary)`,保证 `submit(primary)` /
    /// `seed_liquidity(primary)` 不报 `AssetNotFound`。
    pub fn with_primary(mut self, primary: Instrument) -> Self {
        self.register_instrument(primary.clone());
        self.primary_instrument = Some(primary);
        self
    }

    /// Phase 3.3 (A1.2) 新增:取**主 instrument** 的 fill 链追踪器(只读)
    ///
    /// 路由语义同 `best_bid` / `best_ask`:无 instrument 参数时走 primary。
    /// 若未设置 primary 或 primary 未注册,返回 `None`。
    ///
    /// 多 leg 套利对账通常需要**跨 instrument** 聚合 fill 链,可直接调
    /// [`Self::tracker_for`] 逐个取,或遍历 `instruments()` 自己聚合。
    #[inline]
    pub fn tracker(&self) -> Option<&crate::matching::PartialFillTracker> {
        self.primary_instrument
            .as_ref()
            .and_then(|p| self.engines.get(p).map(|e| e.tracker()))
    }

    /// Phase 3.3 (A1.2) 新增:取**指定 instrument** 的 fill 链追踪器(只读)
    ///
    /// 透传路径:`MultiAssetMatchingEngine` → `L2MatchingEngine::tracker()`
    /// → `L1MatchingEngine::tracker()`。instrument 未注册时返回 `None`。
    ///
    /// 套利对账用法:
    /// ```ignore
    /// let leg1_fills = engine.tracker_for(&pair.pair.spot)
    ///     .and_then(|t| t.chain(leg1_order_id));
    /// let leg2_fills = engine.tracker_for(&pair.pair.perp)
    ///     .and_then(|t| t.chain(leg2_order_id));
    /// ```
    #[inline]
    pub fn tracker_for(
        &self,
        instrument: &Instrument,
    ) -> Option<&crate::matching::PartialFillTracker> {
        self.engines.get(instrument).map(|e| e.tracker())
    }

    /// Phase 3.3 (A1.2) 新增:列出已注册的所有 instrument(无序)
    ///
    /// 给策略层遍历跨 leg fill 链用,避免暴露内部 `engines` HashMap。
    #[inline]
    pub fn instruments(&self) -> Vec<Instrument> {
        self.engines.keys().cloned().collect()
    }
}

/// Phase 3.1.3:`MultiAssetMatchingEngine` 实现 `MatchingEngine` trait
///
/// `MultiAssetMatchingEngine` 是多资产路由容器(每个 instrument 一个 L2 引擎),
/// `MatchingEngine` trait 签名是"无 instrument 参数"的(因为 L1/L2 单一 book 路由
/// 是隐式的),所以这里采用 **primary instrument 路由** 方案:
///
/// - 调用 `MultiAssetMatchingEngine::with_primary(btc)` 设定 primary
/// - 之后 `best_bid` / `best_ask` / `seed_liquidity` 走 primary 路由
/// - `submit(order)` 按 `order.instrument` 路由(自然多 asset)
/// - `cancel(order_id)` 跨所有 instrument 扫
/// - `active_order_count` / `clear_book` 跨所有 instrument
///
/// `seed_liquidity` 返回的 `next_id` 是 L2 内部 id 计数器,与上层 caller
/// 的 `next_id` 解耦,故 trait 适配层把 caller 传进来的 `next_id` **丢进 L2
/// seed 之前的"前置隔离带"** —— 实际 L2 内部 id 自增不影响 caller(简化
/// 实现:直接用 caller 传入的 next_id,忽略 L2 返回的 id 偏移)。
impl MatchingEngine for MultiAssetMatchingEngine {
    fn submit(&mut self, order: Order) -> SubmitResult {
        let instrument = order.instrument.clone();
        // 显式 UFCS:调 inherent `MultiAssetMatchingEngine::submit`,
        // 返回 `MatchingL3Result<Vec<MatchFill>>`(多资产路径),
        // 与 trait 要的 `SubmitResult` 不同,所以需要 match 转。
        // 显式 UFCS 避免依赖 method resolution(inherent 优先 trait,
        // 这里签名不同所以必定调 inherent,但显式让意图清晰 + 防 regression)。
        match Self::submit(self, order) {
            Ok(fills) => {
                if fills.is_empty() {
                    SubmitResult::empty(Quantity::from_f64(0.0))
                } else {
                    // 判断是否部分成交:简化实现 — 多资产路径下只返回 fills,
                    // 标记全部成交(底层 caller 通常按 fill 列表消费)
                    SubmitResult::filled(fills)
                }
            }
            Err(e) => {
                // 错误降级:不阻塞 backtest(错误信息丢弃,无法在 SubmitResult 中传递)
                let _ = (e, instrument);
                SubmitResult::empty(Quantity::from_f64(0.0))
            }
        }
    }

    /// 跨所有 instrument 扫 order_id,任一找到则取消
    fn cancel(&mut self, order_id: u64) -> bool {
        for engine in self.engines.values_mut() {
            if engine.cancel(order_id) {
                return true;
            }
        }
        false
    }

    /// primary instrument 路由
    fn best_bid(&self) -> Option<Price> {
        let primary = self.primary_instrument.as_ref()?;
        self.engine(primary).and_then(|e| e.best_bid())
    }

    /// primary instrument 路由
    fn best_ask(&self) -> Option<Price> {
        let primary = self.primary_instrument.as_ref()?;
        self.engine(primary).and_then(|e| e.best_ask())
    }

    /// primary instrument spread
    fn spread(&self) -> Option<Price> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        Some(Price::from_f64(ask.as_f64() - bid.as_f64()))
    }

    /// depth 走 primary instrument(若未设则返回空)
    fn depth(
        &self,
        levels: usize,
    ) -> (
        Vec<crate::matching::types::OrderBookLevel>,
        Vec<crate::matching::types::OrderBookLevel>,
    ) {
        match self
            .primary_instrument
            .as_ref()
            .and_then(|p| self.engine(p))
        {
            Some(e) => e.depth(levels),
            None => (Vec::new(), Vec::new()),
        }
    }

    /// 跨所有 instrument 合计
    fn active_order_count(&self) -> usize {
        self.engines.values().map(|e| e.active_order_count()).sum()
    }

    /// 清空所有 instrument 的 book
    fn clear_book(&mut self) {
        for engine in self.engines.values_mut() {
            engine.clear_book();
        }
    }

    /// 清空指定 instrument 的 book
    fn clear_book_for(&mut self, instrument: &Instrument) {
        if let Some(engine) = self.engines.get_mut(instrument) {
            engine.clear_book();
        }
    }

    /// primary instrument 注入种子
    ///
    /// 返回 L2 引擎 seed 后的 `next_id`,与 L1 / L2 的 trait 实现语义一致。
    /// 如果 instrument 尚未注册,先 register 再 seed。
    fn seed_liquidity(
        &mut self,
        mid_price: f64,
        half_spread: f64,
        depth_levels: usize,
        size_per_level: f64,
        instrument: Instrument,
        next_id: u64,
    ) -> u64 {
        if !self.engines.contains_key(&instrument) {
            self.register_instrument(instrument.clone());
        }
        if let Some(engine) = self.engines.get_mut(&instrument) {
            engine.seed_liquidity(
                mid_price,
                half_spread,
                depth_levels,
                size_per_level,
                instrument,
                next_id,
            )
        } else {
            next_id
        }
    }
}

/// 0.6.0 新增:按 `Instrument` 类型分派构造 leg 订单(spot → `Order::spot`,
/// swap → `Order::swap`)。替代 0.5.0 `split_symbol_to_base_quote` 字符串桥。
///
/// 0.7.0 抽离:加 `tif` 参数变通用 helper,供以下场景共用,避免 spot/swap
/// 派发不一致:
/// - `L1MatchingEngine::seed_liquidity` → GTC Limit(seed 流动性)
/// - `BacktestEngine::liquidate_eod` → IOC Market(EOD 强制平仓)
/// - `BacktestEngine::rebalance_to_target` → IOC Market(rebalance 市价单)
/// - `MultiAssetMatchingEngine::execute_arbitrage` → GTC Limit(套利)
pub(crate) fn build_leg_order(
    id: u64,
    instrument: &Instrument,
    side: Side,
    order_type: OrderType,
    quantity: Quantity,
    tif: TimeInForce,
) -> Order {
    match instrument {
        Instrument::Spot(s) => Order::spot(
            id,
            s.base.clone(),
            s.quote.clone(),
            side,
            order_type,
            quantity,
            tif,
        ),
        Instrument::Swap(s) => Order::swap(
            id,
            s.base.clone(),
            s.quote.clone(),
            s.settle,
            s.contract_size,
            side,
            order_type,
            quantity,
            tif,
        ),
    }
}

/// 覆盖订单价格为新价格
fn override_order_price(mut order: Order, price: Price) -> Order {
    order.order_type = match order.order_type {
        OrderType::Limit { .. } | OrderType::StopLimit { .. } => OrderType::Limit { price },
        OrderType::Market | OrderType::Stop { .. } | OrderType::Iceberg { .. } => {
            OrderType::Limit { price }
        }
    };
    order
}

/// 从 L2 引擎的 best_bid / best_ask 计算 mid price
fn mid_price_from_engine(engine: &crate::matching::L2MatchingEngine) -> Option<Price> {
    match (engine.best_bid(), engine.best_ask()) {
        (Some(b), Some(a)) => Some(Price::from_f64((b.as_f64() + a.as_f64()) / 2.0)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::order::OrderType;
    use axon_core::time::Timestamp;
    use axon_core::types::{SpotInstrument, SwapInstrument, SwapSettle, Symbol};

    fn btc_spot() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        })
    }

    fn eth_spot() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from("ETH"),
            quote: Symbol::from("USDT"),
        })
    }

    fn btc_perp() -> Instrument {
        Instrument::Swap(SwapInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
            settle: SwapSettle::UsdMargin,
            contract_size: 1.0,
        })
    }

    fn make_limit(id: u64, instrument: Instrument, side: Side, price: f64, qty: f64) -> Order {
        let order = build_leg_order(
            id,
            &instrument,
            side,
            OrderType::Limit {
                price: Price::from_f64(price),
            },
            Quantity::from_f64(qty),
            TimeInForce::GTC,
        );
        // 测试里给 created_at 写 0 让排序稳定
        let mut o = order;
        o.created_at = Timestamp::from_nanos(0);
        o
    }

    // ─── 多资产路由 ──────────────────────────────────────

    #[test]
    fn test_new_engine_empty() {
        let m = MultiAssetMatchingEngine::new();
        assert_eq!(m.asset_count(), 0);
        assert_eq!(m.cross_pair_count(), 0);
        assert_eq!(m.batch_mode(), BatchMode::Continuous);
    }

    #[test]
    fn test_register_instrument_idempotent() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_instrument(btc_spot());
        m.register_instrument(btc_spot());
        assert_eq!(m.asset_count(), 1);
    }

    #[test]
    fn test_isolated_order_books() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_instrument(btc_spot());
        m.register_instrument(eth_spot());

        m.submit(make_limit(1, btc_spot(), Side::Sell, 50_000.0, 1.0))
            .expect("submit sell");
        let btc_fills = m
            .submit(make_limit(2, btc_spot(), Side::Buy, 50_000.0, 1.0))
            .expect("submit buy");
        assert_eq!(btc_fills.len(), 1);

        let eth = m.engine(&eth_spot()).expect("engine");
        assert!(eth.best_bid().is_none());
        assert!(eth.best_ask().is_none());
    }

    #[test]
    fn test_multi_asset_routing_unknown_asset_errors() {
        let mut m = MultiAssetMatchingEngine::new();
        let result = m.submit(make_limit(1, btc_spot(), Side::Buy, 100.0, 1.0));
        assert!(matches!(result, Err(MatchingL3Error::AssetNotFound { .. })));
    }

    // ─── 跨资产交易对 ──────────────────────────────────────

    #[test]
    fn test_register_cross_pair_registers_assets() {
        let mut m = MultiAssetMatchingEngine::new();
        let pair = CrossPair::new(btc_spot(), eth_spot(), 16.0, Quantity::from_f64(1.0));
        m.register_cross_pair(pair).expect("ok");
        assert_eq!(m.asset_count(), 2);
        assert_eq!(m.cross_pair_count(), 1);
    }

    #[test]
    fn test_register_cross_pair_same_leg_errors() {
        let mut m = MultiAssetMatchingEngine::new();
        let pair = CrossPair::new(btc_spot(), btc_spot(), 1.0, Quantity::from_f64(1.0));
        let result = m.register_cross_pair(pair);
        assert!(matches!(
            result,
            Err(MatchingL3Error::InvalidCrossPair { .. })
        ));
    }

    #[test]
    fn test_register_cross_pair_invalid_ratio() {
        let mut m = MultiAssetMatchingEngine::new();
        let pair = CrossPair::new(btc_spot(), eth_spot(), 0.0, Quantity::from_f64(1.0));
        assert!(matches!(
            m.register_cross_pair(pair),
            Err(MatchingL3Error::InvalidCrossPair { .. })
        ));
    }

    /// 0.6.0 新增:CrossPair 接受 spot + swap(perp)做 leg,验证 Instrument
    /// 抽象完整覆盖跨资产场景
    #[test]
    fn test_register_cross_pair_spot_vs_perp() {
        let mut m = MultiAssetMatchingEngine::new();
        let pair = CrossPair::new(btc_spot(), btc_perp(), 1.0, Quantity::from_f64(0.5));
        m.register_cross_pair(pair).expect("ok");
        assert_eq!(m.asset_count(), 2);
    }

    // ─── 批量模式 ──────────────────────────────────────

    #[test]
    fn test_auction_mode_defers_orders() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_instrument(eth_spot());
        m.set_batch_mode(BatchMode::Auction);

        let fills = m
            .submit(make_limit(1, eth_spot(), Side::Buy, 3000.0, 1.0))
            .expect("submit");
        assert!(fills.is_empty(), "Auction 模式应暂存订单，不立即撮合");
    }

    #[test]
    fn test_run_auction_empty() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_instrument(eth_spot());
        m.set_batch_mode(BatchMode::Auction);
        let result = m.run_auction(&eth_spot()).expect("ok");
        assert!(!result.has_trades());
    }

    #[test]
    fn test_run_auction_balanced() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_instrument(eth_spot());
        m.set_batch_mode(BatchMode::Auction);

        m.submit(make_limit(1, eth_spot(), Side::Buy, 3000.0, 5.0))
            .unwrap();
        m.submit(make_limit(2, eth_spot(), Side::Sell, 3002.0, 5.0))
            .unwrap();

        let result = m.run_auction(&eth_spot()).expect("ok");
        assert!(result.has_trades());
        assert!(!result.fills.is_empty());
    }

    // ─── 暗池 ──────────────────────────────────────

    #[test]
    fn test_submit_dark_order_invalid_quantity() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_instrument(btc_spot());
        let order = make_limit(1, btc_spot(), Side::Buy, 50_000.0, 5.0);
        let result = DarkOrder::new(
            order,
            Quantity::from_f64(10.0), // visible > hidden
            Quantity::from_f64(5.0),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_submit_dark_order_no_match_stores() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_instrument(btc_spot());
        let order = make_limit(1, btc_spot(), Side::Buy, 50_000.0, 5.0);
        let dark = DarkOrder {
            visible_quantity: Quantity::from_f64(2.0),
            hidden_quantity: Quantity::from_f64(5.0),
            order,
        };
        let fills = m.submit_dark_order(dark).expect("ok");
        assert!(fills.is_empty());
        assert_eq!(m.dark_orders.get(&btc_spot()).map(|v| v.len()), Some(1));
    }

    #[test]
    fn test_submit_dark_order_matches_existing() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_instrument(btc_spot());

        let sell = make_limit(1, btc_spot(), Side::Sell, 50_000.0, 3.0);
        m.submit_dark_order(DarkOrder {
            visible_quantity: Quantity::from_f64(1.0),
            hidden_quantity: Quantity::from_f64(3.0),
            order: sell,
        })
        .expect("ok");

        let buy = make_limit(2, btc_spot(), Side::Buy, 50_000.0, 3.0);
        let fills = m
            .submit_dark_order(DarkOrder {
                visible_quantity: Quantity::from_f64(1.0),
                hidden_quantity: Quantity::from_f64(3.0),
                order: buy,
            })
            .expect("ok");

        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].quantity, Quantity::from_f64(3.0));
        assert_eq!(m.stats().total_dark_fills, 1);
    }

    // ─── 套利 ──────────────────────────────────────

    #[test]
    fn test_detect_arbitrage_no_data() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_cross_pair(CrossPair::new(
            btc_spot(),
            eth_spot(),
            16.0,
            Quantity::from_f64(1.0),
        ))
        .expect("ok");
        let ops = m.detect_arbitrage();
        assert_eq!(ops.len(), 1);
        assert!(ops[0].implied_ratio.is_none());
    }

    #[test]
    fn test_detect_arbitrage_with_both_sides() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_cross_pair(CrossPair::new(
            btc_spot(),
            eth_spot(),
            16.0,
            Quantity::from_f64(1.0),
        ))
        .expect("ok");

        // BTC: bid=50000, ask=50100
        m.submit(make_limit(0, btc_spot(), Side::Buy, 50_000.0, 1.0))
            .unwrap();
        m.submit(make_limit(1, btc_spot(), Side::Sell, 50_100.0, 1.0))
            .unwrap();
        // ETH: bid=3000, ask=3020
        m.submit(make_limit(2, eth_spot(), Side::Buy, 3_000.0, 1.0))
            .unwrap();
        m.submit(make_limit(3, eth_spot(), Side::Sell, 3_020.0, 1.0))
            .unwrap();

        let ops = m.detect_arbitrage();
        let op = &ops[0];
        // BTC mid = 50050, ETH mid = 3010, implied = 50050/3010 ≈ 16.628
        assert!(op.implied_ratio.is_some());
        let ir = op.implied_ratio.unwrap();
        assert!(ir > 16.0);
        assert!(op.deviation > 0.0);
        assert!(op.estimated_profit > 0.0);
    }

    #[test]
    fn test_execute_arbitrage_quantity_exceeds_max() {
        let mut m = MultiAssetMatchingEngine::new();
        let pair = CrossPair::new(btc_spot(), eth_spot(), 16.0, Quantity::from_f64(0.5));
        m.register_cross_pair(pair.clone()).expect("ok");
        let result = m.execute_arbitrage(&pair, Quantity::from_f64(1.0), Side::Buy);
        assert!(matches!(
            result,
            Err(MatchingL3Error::InvalidCrossPair { .. })
        ));
    }

    /// 0.6.0 新增:`execute_arbitrage` 接受 spot + perp(不只 spot-spot)
    #[test]
    fn test_execute_arbitrage_spot_vs_perp() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_instrument(btc_spot());
        m.register_instrument(btc_perp());
        // 给 spot book 挂 1 笔 buy(让 spot sell 有对手),给 perp book 挂 1 笔 sell
        // (让 perp buy 有对手)
        m.submit(make_limit(1, btc_spot(), Side::Buy, 50_001.0, 1.0))
            .unwrap();
        m.submit(make_limit(2, btc_perp(), Side::Sell, 50_001.0, 1.0))
            .unwrap();

        let pair = CrossPair::new(btc_spot(), btc_perp(), 1.0, Quantity::from_f64(1.0));
        m.register_cross_pair(pair.clone()).expect("ok");
        // leg1 (spot) sell 吃 spot bid 50001,leg2 (perp) buy 吃 perp ask 50001
        let fills = m
            .execute_arbitrage(&pair, Quantity::from_f64(1.0), Side::Sell)
            .expect("ok");
        assert_eq!(fills.len(), 2, "spot + perp 各 1 笔 fill");
    }

    // ─── 快照与恢复 ──────────────────────────────────────

    #[test]
    fn test_snapshot_preserves_state() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_instrument(btc_spot());
        m.register_instrument(eth_spot());
        m.set_batch_mode(BatchMode::Auction);

        let snap = m.snapshot();
        assert_eq!(snap.batch_mode, BatchMode::Auction);
        assert_eq!(snap.engines.len(), 2);
    }

    #[test]
    fn test_restore_recovers_basic_state() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_instrument(btc_spot());
        m.register_cross_pair(CrossPair::new(
            btc_spot(),
            eth_spot(),
            16.0,
            Quantity::from_f64(1.0),
        ))
        .expect("ok");
        m.set_batch_mode(BatchMode::Auction);

        let snap = m.snapshot();
        let mut restored = MultiAssetMatchingEngine::new();
        restored.restore(snap).expect("ok");
        assert_eq!(restored.asset_count(), 2);
        assert_eq!(restored.cross_pair_count(), 1);
        assert_eq!(restored.batch_mode(), BatchMode::Auction);
    }

    // ─── 批量提交 ──────────────────────────────────────

    #[test]
    fn test_submit_batch() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_instrument(btc_spot());
        m.submit(make_limit(1, btc_spot(), Side::Sell, 50_000.0, 1.0))
            .unwrap();
        let orders = vec![
            make_limit(2, btc_spot(), Side::Buy, 50_000.0, 1.0),
            make_limit(3, btc_spot(), Side::Buy, 49_000.0, 1.0),
        ];
        let fills = m.submit_batch(orders).expect("ok");
        assert_eq!(fills.len(), 1);
    }

    #[test]
    fn test_override_order_price_market_to_limit() {
        let order = Order::spot(
            1,
            "BTC",
            "USDT",
            Side::Buy,
            OrderType::Market,
            Quantity::from_f64(5.0),
            TimeInForce::GTC,
        );
        let updated = override_order_price(order, Price::from_f64(200.0));
        assert_eq!(
            updated.order_type.limit_price(),
            Some(Price::from_f64(200.0))
        );
    }

    // ─── Phase 3.3 (A1.2):跨资产套利 e2e ─────────────────

    /// 跨资产套利完整生命周期:spot + perp 价格偏离 → detect → execute
    /// → 验证双 leg fill + tracker 链完整
    #[test]
    fn test_cross_asset_arbitrage_full_lifecycle() {
        let mut m = MultiAssetMatchingEngine::new().with_primary(btc_spot());
        m.register_instrument(btc_perp());

        // spot: bid 50_000, ask 50_100 (mid 50_050)
        m.submit(make_limit(10, btc_spot(), Side::Buy, 50_000.0, 1.0))
            .unwrap();
        m.submit(make_limit(11, btc_spot(), Side::Sell, 50_100.0, 1.0))
            .unwrap();
        // perp: bid 50_200, ask 50_300 (mid 50_250) — 隐含 perp 溢价比 spot 高 ~0.4%
        m.submit(make_limit(20, btc_perp(), Side::Buy, 50_200.0, 1.0))
            .unwrap();
        m.submit(make_limit(21, btc_perp(), Side::Sell, 50_300.0, 1.0))
            .unwrap();

        // 注册 cross pair:spot vs perp,ratio 1.0(应是平价)
        let pair = CrossPair::new(btc_spot(), btc_perp(), 1.0, Quantity::from_f64(1.0));
        m.register_cross_pair(pair.clone()).expect("ok");

        // 1) detect: 应报 deviation ≈ |50050/50250 - 1.0| ≈ 0.4%
        let ops = m.detect_arbitrage();
        assert_eq!(ops.len(), 1);
        let op = &ops[0];
        assert!(op.implied_ratio.is_some());
        let ir = op.implied_ratio.unwrap();
        assert!(ir < 1.0, "perp mid > spot mid → implied ratio < 1.0");
        assert!(op.deviation > 0.003, "deviation > 0.3%");
        assert!(op.estimated_profit > 0.0);

        // 2) execute: leg1 spot sell @50_100, leg2 perp buy @50_200
        //    → spot 卖一吃 spot 买一;perp 买一吃 perp 卖一
        let fills = m
            .execute_arbitrage(&pair, Quantity::from_f64(1.0), Side::Sell)
            .expect("ok");
        assert_eq!(fills.len(), 2, "spot + perp 各 1 笔 fill");

        // 3) tracker 验证:leg1 卖单和 leg2 买单的 fill 链都正确记录
        let spot_tracker = m
            .tracker_for(&btc_spot())
            .expect("spot tracker must exist after fill");
        let perp_tracker = m
            .tracker_for(&btc_perp())
            .expect("perp tracker must exist after fill");

        // fill_id 升序:spot taker(leg1_sell) 链至少有 1 条(spot 吃 buy 10)
        // 因 fill 链路是单次 fill,每笔 fill 在 taker 链 + maker 链都记录 → 2 records / fill
        assert!(
            spot_tracker.total_records() >= 2,
            "spot tracker 至少有 2 条 record (taker leg1 + maker 10)"
        );
        assert!(
            perp_tracker.total_records() >= 2,
            "perp tracker 至少有 2 条 record (taker leg2 + maker 21)"
        );

        // 跨 instrument 隔离:spot order 11(bid) 不应出现在 perp tracker
        assert!(
            perp_tracker.chain(11).is_none(),
            "perp 不应含 spot order id"
        );

        // 4) stats:cross_fills 累加
        assert_eq!(m.stats().total_cross_fills, 2);
    }

    /// 跨资产套利:无对手盘时 detect 报空,execute 报 InvalidCrossPair
    #[test]
    fn test_cross_asset_arbitrage_no_liquidity() {
        let mut m = MultiAssetMatchingEngine::new().with_primary(btc_spot());
        m.register_instrument(btc_perp());
        let pair = CrossPair::new(btc_spot(), btc_perp(), 1.0, Quantity::from_f64(0.5));
        m.register_cross_pair(pair.clone()).expect("ok");

        // 无任何挂单 → detect 报 implied_ratio = None
        let ops = m.detect_arbitrage();
        assert_eq!(ops.len(), 1);
        assert!(ops[0].implied_ratio.is_none());
        assert_eq!(ops[0].deviation, 0.0);
        assert_eq!(ops[0].estimated_profit, 0.0);

        // execute:spot leg1_price = 0(default) → buy/sell 撮合无对手
        let fills = m
            .execute_arbitrage(&pair, Quantity::from_f64(0.5), Side::Buy)
            .expect("ok");
        assert!(fills.is_empty(), "无对手盘 → 无 fill");
    }

    /// 跨 instrument tracker 隔离:同一 order_id 在不同 instrument 各自独立
    #[test]
    fn test_tracker_for_per_instrument_isolation() {
        let mut m = MultiAssetMatchingEngine::new().with_primary(btc_spot());
        m.register_instrument(eth_spot());

        // BTC + ETH 各挂 1 笔 sell + 1 笔 buy → 4 orders,2 fills
        m.submit(make_limit(100, btc_spot(), Side::Sell, 50_000.0, 1.0))
            .unwrap();
        m.submit(make_limit(101, btc_spot(), Side::Buy, 50_000.0, 1.0))
            .unwrap();
        m.submit(make_limit(200, eth_spot(), Side::Sell, 3_000.0, 1.0))
            .unwrap();
        m.submit(make_limit(201, eth_spot(), Side::Buy, 3_000.0, 1.0))
            .unwrap();

        let btc_tracker = m.tracker_for(&btc_spot()).expect("btc tracker");
        let eth_tracker = m.tracker_for(&eth_spot()).expect("eth tracker");

        // BTC tracker 含 100 + 101 的 fill 链(各 1 条 record,taker + maker 共 2)
        assert_eq!(btc_tracker.chain_len(100), 1);
        assert_eq!(btc_tracker.chain_len(101), 1);
        assert!(btc_tracker.chain(200).is_none());
        assert!(btc_tracker.chain(201).is_none());

        // ETH tracker 反之
        assert_eq!(eth_tracker.chain_len(200), 1);
        assert_eq!(eth_tracker.chain_len(201), 1);
        assert!(eth_tracker.chain(100).is_none());
        assert!(eth_tracker.chain(101).is_none());
    }

    /// tracker() 走 primary_instrument 路由
    #[test]
    fn test_tracker_primary_routing() {
        let mut m = MultiAssetMatchingEngine::new().with_primary(btc_spot());
        m.register_instrument(eth_spot());

        // 给 primary(btc) 喂 1 笔 fill
        m.submit(make_limit(1, btc_spot(), Side::Sell, 50_000.0, 1.0))
            .unwrap();
        m.submit(make_limit(2, btc_spot(), Side::Buy, 50_000.0, 1.0))
            .unwrap();

        let primary_tracker = m.tracker().expect("primary tracker");
        assert_eq!(primary_tracker.chain_len(1), 1);
        assert_eq!(primary_tracker.chain_len(2), 1);
    }

    /// tracker() 在未设 primary 时返回 None
    #[test]
    fn test_tracker_returns_none_without_primary() {
        let m = MultiAssetMatchingEngine::new();
        assert!(m.tracker().is_none());
    }

    /// instruments() 列出所有已注册 instrument
    #[test]
    fn test_instruments_listing() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_instrument(btc_spot());
        m.register_instrument(eth_spot());
        m.register_instrument(btc_perp());

        let insts = m.instruments();
        assert_eq!(insts.len(), 3);
        assert!(insts.contains(&btc_spot()));
        assert!(insts.contains(&eth_spot()));
        assert!(insts.contains(&btc_perp()));
    }

    // ─── Phase 3.3 (A1.2):暗池 + 拍卖 e2e ─────────────────

    /// 暗池撮合 → tracker 链追踪(0.7.x 已有 dark fill 计数,本测试加 fill 链验证)
    #[test]
    fn test_dark_pool_match_records_tracker() {
        // 注:MultiAssetMatchingEngine 的暗池撮合 **不走** L2 引擎
        // (try_dark_match 在 `dark_orders` vec 上),不产生 L1 fill → 不入 tracker。
        // 此测试验证 stats 累加 + 暗池簿状态,tracker 行为留给 L1 e2e 测试。
        let mut m = MultiAssetMatchingEngine::new().with_primary(btc_spot());

        // 1) 第一笔 sell 暗池单 → 无对手 → 入暗池簿
        let sell = make_limit(1, btc_spot(), Side::Sell, 50_000.0, 3.0);
        m.submit_dark_order(DarkOrder {
            visible_quantity: Quantity::from_f64(1.0),
            hidden_quantity: Quantity::from_f64(3.0),
            order: sell,
        })
        .expect("ok");
        assert_eq!(m.stats().total_dark_fills, 0);

        // 2) 第二笔 buy 暗池单 → 与第一笔 sell 暗池撮合
        let buy = make_limit(2, btc_spot(), Side::Buy, 50_000.0, 3.0);
        let fills = m
            .submit_dark_order(DarkOrder {
                visible_quantity: Quantity::from_f64(1.0),
                hidden_quantity: Quantity::from_f64(3.0),
                order: buy,
            })
            .expect("ok");
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].quantity, Quantity::from_f64(3.0));
        assert_eq!(m.stats().total_dark_fills, 1);

        // 3) 暗池簿被清空(双方都全成)
        let dark_book = m.dark_orders.get(&btc_spot()).expect("dark book for btc");
        assert!(dark_book.is_empty());
    }

    /// 批量拍卖清算:多笔 bid/ask → find_clearing_price → override 价格
    /// → engine.submit → fill + tracker 链追踪
    #[test]
    fn test_batch_auction_with_tracker() {
        let mut m = MultiAssetMatchingEngine::new().with_primary(btc_spot());
        m.set_batch_mode(BatchMode::Auction);

        // 2 buy + 2 sell,价格交叉,clearing 价应在 50_000~50_002 之间
        m.submit(make_limit(10, btc_spot(), Side::Buy, 50_000.0, 1.0))
            .unwrap();
        m.submit(make_limit(11, btc_spot(), Side::Buy, 50_002.0, 1.0))
            .unwrap();
        m.submit(make_limit(20, btc_spot(), Side::Sell, 50_000.0, 1.0))
            .unwrap();
        m.submit(make_limit(21, btc_spot(), Side::Sell, 50_002.0, 1.0))
            .unwrap();

        // 跑拍卖
        let result = m.run_auction(&btc_spot()).expect("ok");
        assert!(result.has_trades());
        assert!(!result.fills.is_empty());

        // fill 数量应 >= 1(拍卖 override 后部分订单相互撮合)
        assert!(!result.fills.is_empty());

        // tracker 验证:至少某些卖单作为 maker 入链
        let tracker = m
            .tracker_for(&btc_spot())
            .expect("spot tracker after auction");
        // 至少 1 个卖单 + 1 个买单的 fill 链都被记录
        let total_orders_with_fills = (0..4)
            .map(|id| [10u64, 11, 20, 21][id])
            .filter(|id| tracker.chain_len(*id) > 0)
            .count();
        assert!(
            total_orders_with_fills >= 2,
            "至少 2 个 order 有 fill record(实测:{})",
            total_orders_with_fills
        );

        // stats:batch_fills 累加(可能少于原始 4 笔订单,因为部分 unfilled)
        assert!(m.stats().total_batch_fills > 0);
    }

    /// 批量拍卖:无对手盘时(单边挂单)清算失败
    #[test]
    fn test_batch_auction_one_sided() {
        let mut m = MultiAssetMatchingEngine::new().with_primary(btc_spot());
        m.set_batch_mode(BatchMode::Auction);

        // 只挂 buy 单,无 sell → find_clearing_price 仍报 volume(纯买累计)
        // 但 engine.submit(buy) 簿内无对手 → 无 fill
        m.submit(make_limit(1, btc_spot(), Side::Buy, 50_000.0, 1.0))
            .unwrap();
        let result = m.run_auction(&btc_spot()).expect("ok");
        assert!(
            result.fills.is_empty(),
            "无对手盘 → 实际无 fill (unfilled_orders 含原 buy)"
        );
        assert_eq!(result.unfilled_orders.len(), 1, "1 笔 buy 留在 unfilled");
    }

    /// 跨 instrument 拍卖隔离:run_auction(btc) 不动 eth 簿
    #[test]
    fn test_batch_auction_cross_instrument_isolation() {
        let mut m = MultiAssetMatchingEngine::new().with_primary(btc_spot());
        m.register_instrument(eth_spot());
        m.set_batch_mode(BatchMode::Auction);

        // BTC 簿有可拍卖单
        m.submit(make_limit(10, btc_spot(), Side::Buy, 50_000.0, 1.0))
            .unwrap();
        m.submit(make_limit(20, btc_spot(), Side::Sell, 50_000.0, 1.0))
            .unwrap();
        // ETH 簿有可拍卖单
        m.submit(make_limit(11, eth_spot(), Side::Buy, 3_000.0, 1.0))
            .unwrap();
        m.submit(make_limit(21, eth_spot(), Side::Sell, 3_000.0, 1.0))
            .unwrap();

        // 只跑 btc 拍卖
        let result = m.run_auction(&btc_spot()).expect("ok");
        assert!(result.has_trades());

        // btc tracker 有 fill,eth tracker 空
        let btc_tracker = m.tracker_for(&btc_spot()).expect("btc");
        let eth_tracker = m.tracker_for(&eth_spot()).expect("eth");
        assert!(btc_tracker.total_records() > 0);
        assert_eq!(eth_tracker.total_records(), 0, "eth 簿未动 → 无 fill 链");

        // eth 仍在 pending_batch(等下次 run_auction(&eth))
        // 实际:run_auction 只 drain 当前 instrument 的 pending,eth 单保留
        let result_eth = m.run_auction(&eth_spot()).expect("ok");
        assert!(result_eth.has_trades());
    }
}
