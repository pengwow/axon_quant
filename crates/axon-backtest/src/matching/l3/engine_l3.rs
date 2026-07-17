//! L3 多资产撮合引擎
//!
//! 核心功能：
//! - 多资产独立 L2 订单簿路由
//! - 暗池撮合（软暗池：先扫暗池，未成交再入暗池簿）
//! - 批量拍卖清算
//! - 跨资产套利检测与执行
//! - 快照与恢复（仅资产 / 配置 / 批量模式，订单级别恢复需 L2 `from_entries`）

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::types::{Instrument, Price, Quantity, Symbol};

use super::super::types::MatchFill;
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

/// 套利机会（只读报告，不自动执行）
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
    /// 偏离度（`|implied - target| / target`）
    pub deviation: f64,
    /// 估计套利利润（绝对值）
    pub estimated_profit: f64,
}

/// 多资产撮合引擎
#[derive(Default)]
pub struct MultiAssetMatchingEngine {
    /// 各资产独立的 L2 撮合引擎
    engines: HashMap<Symbol, crate::matching::L2MatchingEngine>,
    /// 跨资产交易对配置
    cross_pairs: Vec<CrossPair>,
    /// 当前批量模式
    batch_mode: BatchMode,
    /// 暗池订单簿（按资产）
    dark_orders: HashMap<Symbol, Vec<DarkOrder>>,
    /// 批量待撮合订单（Auction / DarkPool 模式暂存）
    pending_batch: Vec<Order>,
    /// 下一个 fill id（暗池成交使用）
    next_fill_id: u64,
    /// 统计
    stats: L3Stats,
}

impl MultiAssetMatchingEngine {
    /// 创建新的多资产撮合引擎
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册新资产（幂等）
    pub fn register_asset(&mut self, symbol: Symbol) {
        self.engines.entry(symbol.clone()).or_default();
        self.dark_orders.entry(symbol).or_default();
        self.stats.total_assets = self.engines.len();
    }

    /// 注册跨资产交易对（自动注册两个 leg 资产）
    pub fn register_cross_pair(&mut self, pair: CrossPair) -> MatchingL3Result<()> {
        if pair.leg1 == pair.leg2 {
            return Err(MatchingL3Error::InvalidCrossPair {
                leg1: pair.leg1.to_string(),
                leg2: pair.leg2.to_string(),
                ratio: pair.ratio,
            });
        }
        if pair.ratio <= 0.0 || !pair.ratio.is_finite() {
            return Err(MatchingL3Error::InvalidCrossPair {
                leg1: pair.leg1.to_string(),
                leg2: pair.leg2.to_string(),
                ratio: pair.ratio,
            });
        }
        self.register_asset(pair.leg1.clone());
        self.register_asset(pair.leg2.clone());
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

    /// 获取资产的 L2 引擎引用
    #[inline]
    pub fn engine(&self, symbol: &Symbol) -> Option<&crate::matching::L2MatchingEngine> {
        self.engines.get(symbol)
    }

    /// 获取资产的 L2 引擎可变引用
    #[inline]
    pub fn engine_mut(
        &mut self,
        symbol: &Symbol,
    ) -> Option<&mut crate::matching::L2MatchingEngine> {
        self.engines.get_mut(symbol)
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
    pub fn submit(&mut self, order: Order) -> MatchingL3Result<Vec<MatchFill>> {
        // T2.2: Order::symbol -> Order::instrument; 用 Instrument 反向构造 Symbol key
        let symbol = instrument_to_key(&order.instrument);
        match self.batch_mode {
            BatchMode::Continuous => {
                let engine = self.engines.get_mut(&symbol).ok_or_else(|| {
                    MatchingL3Error::AssetNotFound {
                        symbol: symbol.clone(),
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
                // 暗池模式：先扫暗池簿，未成交入暗池
                if order.order_type.limit_price().is_none() {
                    return Err(MatchingL3Error::OrderMissingLimitPrice { order_id: order.id });
                }
                let dark = DarkOrder {
                    visible_quantity: order.remaining_quantity(),
                    hidden_quantity: order.remaining_quantity(),
                    order: order.clone(),
                };
                self.try_dark_and_store(&symbol, dark)
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
        let symbol = instrument_to_key(&dark.order.instrument);
        self.try_dark_and_store(&symbol, dark)
    }

    /// 暗池撮合 + 暂存辅助方法
    fn try_dark_and_store(
        &mut self,
        symbol: &Symbol,
        dark: DarkOrder,
    ) -> MatchingL3Result<Vec<MatchFill>> {
        // 借用 dark_orders 的可变引用到一个临时块内
        let fills = {
            let dark_book = self.dark_orders.entry(symbol.clone()).or_default();
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
            .entry(symbol.clone())
            .or_default()
            .push(dark);
        Ok(Vec::new())
    }

    /// 执行批量拍卖
    pub fn run_auction(&mut self, symbol: &Symbol) -> MatchingL3Result<AuctionResult> {
        if self.pending_batch.is_empty() {
            return Ok(AuctionResult::empty());
        }

        // 分离当前 symbol 的订单
        let mut to_auction: Vec<Order> = Vec::new();
        let mut kept: Vec<Order> = Vec::new();
        for order in self.pending_batch.drain(..) {
            // T2.2: 通过 instrument_to_key 转换为标准 symbol 字符串再比较
            let order_key = instrument_to_key(&order.instrument);
            if order_key == *symbol {
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
                .get_mut(symbol)
                .ok_or_else(|| MatchingL3Error::AssetNotFound {
                    symbol: symbol.clone(),
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
        let leg1_mid = self.engines.get(&pair.leg1).and_then(mid_price_from_engine);
        let leg2_mid = self.engines.get(&pair.leg2).and_then(mid_price_from_engine);

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

    /// 套利执行（同时提交 leg1 / leg2 订单）
    pub fn execute_arbitrage(
        &mut self,
        pair: &CrossPair,
        quantity: Quantity,
        side_leg1: Side,
    ) -> MatchingL3Result<Vec<MatchFill>> {
        if quantity.as_f64() > pair.max_quantity.as_f64() {
            return Err(MatchingL3Error::InvalidCrossPair {
                leg1: pair.leg1.to_string(),
                leg2: pair.leg2.to_string(),
                ratio: pair.ratio,
            });
        }

        // 提前取价避免双重可变借用
        let leg1_price = self
            .engines
            .get(&pair.leg1)
            .and_then(|e| match side_leg1 {
                Side::Buy => e.best_ask(),
                Side::Sell => e.best_bid(),
            })
            .unwrap_or_default();
        let leg2_side = side_leg1.opposite();
        let leg2_price = self
            .engines
            .get(&pair.leg2)
            .and_then(|e| match leg2_side {
                Side::Buy => e.best_ask(),
                Side::Sell => e.best_bid(),
            })
            .unwrap_or_default();

        // T2.2: 套利 leg 订单用 Order::spot 构造;base/quote 从 Symbol 拆分
        let (leg1_base, leg1_quote) = split_symbol_to_base_quote(&pair.leg1);
        let (leg2_base, leg2_quote) = split_symbol_to_base_quote(&pair.leg2);
        let leg1_order = Order::spot(
            0,
            leg1_base,
            leg1_quote,
            side_leg1,
            OrderType::Limit { price: leg1_price },
            quantity,
            TimeInForce::GTC,
        );
        let leg2_order = Order::spot(
            0,
            leg2_base,
            leg2_quote,
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

    /// 创建快照
    pub fn snapshot(&self) -> MatchingEngineSnapshot {
        let mut engines = HashMap::new();
        for (symbol, engine) in &self.engines {
            let (bids, asks) = engine.depth(20);
            engines.insert(
                symbol.clone(),
                L2Snapshot {
                    symbol: symbol.clone(),
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

    /// 从快照恢复
    ///
    /// 仅恢复 **资产注册 / 跨资产配置 / 批量模式**；
    /// 价格级别（`bid_depth` / `ask_depth`）因需重建挂单，**不自动恢复**。
    /// 如需完整恢复，请使用 L2 引擎自身的 `from_entries` API。
    pub fn restore(&mut self, snapshot: MatchingEngineSnapshot) -> MatchingL3Result<()> {
        self.engines.clear();
        self.dark_orders.clear();
        self.pending_batch.clear();

        for symbol in snapshot.engines.keys() {
            self.register_asset(symbol.clone());
        }

        self.cross_pairs = snapshot.cross_pairs;
        self.batch_mode = snapshot.batch_mode;
        self.stats.total_assets = self.engines.len();

        Ok(())
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

/// 把 `Instrument` 序列化为 L3 HashMap key 使用的字符串 (transitional)
///
/// 当前 L3 内部 `engines: HashMap<Symbol, _>` / `dark_orders: HashMap<Symbol, _>`
/// 仍按 `Symbol` 暴露 (T3.x 才会换成 `HashMap<Instrument, _>`),所以订单入口
/// 处把 `Instrument` 序列化为 `"{base}/{quote}"` 形式的临时字符串用于 lookup。
///
/// 格式: `"{base}/{quote}"`,例如 `"BTC/USDT"`。与 L3 现有 `register_asset`
/// 接受的 `Symbol::from("BTC-USDT")` 不同:此处用 `/` 与 axon_quant position
/// key 习惯保持一致,T3.x 切换 key 类型时两端会一起迁移。
fn instrument_to_key(inst: &Instrument) -> Symbol {
    Symbol::from(format!(
        "{}/{}",
        inst.base().as_str(),
        inst.quote().as_str()
    ))
}

/// 把 `Symbol` (L3 用的 `"BTC-USDT"` 风格) 拆为 `(base, quote)` `(T2.2 过渡 helper)`
///
/// 接受 `-` 和 `/` 两种分隔符 (测试 fixture 风格不统一)。L3 `leg1/leg2` 是
/// `Symbol`,构造 `Order::spot` 时需要先 split 拿到 base/quote。
///
/// T3.x 切到 `Instrument` 直接做 key 后这个 helper 会被删除。
fn split_symbol_to_base_quote(sym: &Symbol) -> (Symbol, Symbol) {
    let s = sym.as_str();
    if let Some((base, quote)) = s.split_once('-').or_else(|| s.split_once('/')) {
        (Symbol::from(base), Symbol::from(quote))
    } else {
        (sym.clone(), Symbol::from("USDT"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::order::OrderType;
    use axon_core::time::Timestamp;
    use axon_core::types::Symbol;

    fn make_limit(id: u64, symbol: &str, side: Side, price: f64, qty: f64) -> Order {
        // T2.2: 把 "BASE/QUOTE" 拆成 base/quote 再用 Order::spot
        let parts: Vec<&str> = symbol.split('/').collect();
        let (base, quote) = match parts.as_slice() {
            [b, q] => (Symbol::from(*b), Symbol::from(*q)),
            _ => panic!("test symbol 格式必须是 BASE/QUOTE, 收到: {symbol}"),
        };
        Order::spot(
            id,
            base,
            quote,
            side,
            OrderType::Limit {
                price: Price::from_f64(price),
            },
            Quantity::from_f64(qty),
            TimeInForce::GTC,
        )
        .with_test_timestamp(Timestamp::from_nanos(0))
    }

    trait OrderTestHelpers {
        fn with_test_timestamp(self, ts: Timestamp) -> Self;
    }
    impl OrderTestHelpers for Order {
        fn with_test_timestamp(mut self, ts: Timestamp) -> Self {
            self.created_at = ts;
            self
        }
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
    fn test_register_asset_idempotent() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_asset("BTC/USDT".into());
        m.register_asset("BTC/USDT".into());
        assert_eq!(m.asset_count(), 1);
    }

    #[test]
    fn test_isolated_order_books() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_asset("BTC/USDT".into());
        m.register_asset("ETH/USDT".into());

        m.submit(make_limit(1, "BTC/USDT", Side::Sell, 50_000.0, 1.0))
            .expect("submit sell");
        let btc_fills = m
            .submit(make_limit(2, "BTC/USDT", Side::Buy, 50_000.0, 1.0))
            .expect("submit buy");
        assert_eq!(btc_fills.len(), 1);

        let eth = m.engine(&"ETH/USDT".into()).expect("engine");
        assert!(eth.best_bid().is_none());
        assert!(eth.best_ask().is_none());
    }

    #[test]
    fn test_multi_asset_routing_unknown_asset_errors() {
        let mut m = MultiAssetMatchingEngine::new();
        let result = m.submit(make_limit(1, "SOL/USDT", Side::Buy, 100.0, 1.0));
        assert!(matches!(result, Err(MatchingL3Error::AssetNotFound { .. })));
    }

    // ─── 跨资产交易对 ──────────────────────────────────────

    #[test]
    fn test_register_cross_pair_registers_assets() {
        let mut m = MultiAssetMatchingEngine::new();
        let pair = CrossPair::new(
            "BTC/USDT".into(),
            "ETH/USDT".into(),
            16.0,
            Quantity::from_f64(1.0),
        );
        m.register_cross_pair(pair).expect("ok");
        assert_eq!(m.asset_count(), 2);
        assert_eq!(m.cross_pair_count(), 1);
    }

    #[test]
    fn test_register_cross_pair_same_leg_errors() {
        let mut m = MultiAssetMatchingEngine::new();
        let pair = CrossPair::new(
            "BTC/USDT".into(),
            "BTC/USDT".into(),
            1.0,
            Quantity::from_f64(1.0),
        );
        let result = m.register_cross_pair(pair);
        assert!(matches!(
            result,
            Err(MatchingL3Error::InvalidCrossPair { .. })
        ));
    }

    #[test]
    fn test_register_cross_pair_invalid_ratio() {
        let mut m = MultiAssetMatchingEngine::new();
        let pair = CrossPair::new(
            "BTC/USDT".into(),
            "ETH/USDT".into(),
            0.0,
            Quantity::from_f64(1.0),
        );
        assert!(matches!(
            m.register_cross_pair(pair),
            Err(MatchingL3Error::InvalidCrossPair { .. })
        ));
    }

    // ─── 批量模式 ──────────────────────────────────────

    #[test]
    fn test_auction_mode_defers_orders() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_asset("ETH/USDT".into());
        m.set_batch_mode(BatchMode::Auction);

        let fills = m
            .submit(make_limit(1, "ETH/USDT", Side::Buy, 3000.0, 1.0))
            .expect("submit");
        assert!(fills.is_empty(), "Auction 模式应暂存订单，不立即撮合");
    }

    #[test]
    fn test_run_auction_empty() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_asset("ETH/USDT".into());
        m.set_batch_mode(BatchMode::Auction);
        let result = m.run_auction(&"ETH/USDT".into()).expect("ok");
        assert!(!result.has_trades());
    }

    #[test]
    fn test_run_auction_balanced() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_asset("ETH/USDT".into());
        m.set_batch_mode(BatchMode::Auction);

        m.submit(make_limit(1, "ETH/USDT", Side::Buy, 3000.0, 5.0))
            .unwrap();
        m.submit(make_limit(2, "ETH/USDT", Side::Sell, 3002.0, 5.0))
            .unwrap();

        let result = m.run_auction(&"ETH/USDT".into()).expect("ok");
        assert!(result.has_trades());
        assert!(!result.fills.is_empty());
    }

    // ─── 暗池 ──────────────────────────────────────

    #[test]
    fn test_submit_dark_order_invalid_quantity() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_asset("BTC/USDT".into());
        // 用 DarkOrder::new 验证（绕过结构体直接构造）
        let order = make_limit(1, "BTC/USDT", Side::Buy, 50_000.0, 5.0);
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
        m.register_asset("BTC/USDT".into());
        let order = make_limit(1, "BTC/USDT", Side::Buy, 50_000.0, 5.0);
        let dark = DarkOrder {
            visible_quantity: Quantity::from_f64(2.0),
            hidden_quantity: Quantity::from_f64(5.0),
            order,
        };
        let fills = m.submit_dark_order(dark).expect("ok");
        assert!(fills.is_empty());
        assert_eq!(
            m.dark_orders.get(&"BTC/USDT".into()).map(|v| v.len()),
            Some(1)
        );
    }

    #[test]
    fn test_submit_dark_order_matches_existing() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_asset("BTC/USDT".into());

        let sell = make_limit(1, "BTC/USDT", Side::Sell, 50_000.0, 3.0);
        m.submit_dark_order(DarkOrder {
            visible_quantity: Quantity::from_f64(1.0),
            hidden_quantity: Quantity::from_f64(3.0),
            order: sell,
        })
        .expect("ok");

        let buy = make_limit(2, "BTC/USDT", Side::Buy, 50_000.0, 3.0);
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
            "BTC/USDT".into(),
            "ETH/USDT".into(),
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
        // 套利需要买卖价均存在才能计算 mid
        let mut m = MultiAssetMatchingEngine::new();
        m.register_cross_pair(CrossPair::new(
            "BTC/USDT".into(),
            "ETH/USDT".into(),
            16.0,
            Quantity::from_f64(1.0),
        ))
        .expect("ok");

        // BTC: bid=50000, ask=50100
        m.submit(make_limit(0, "BTC/USDT", Side::Buy, 50_000.0, 1.0))
            .unwrap();
        m.submit(make_limit(1, "BTC/USDT", Side::Sell, 50_100.0, 1.0))
            .unwrap();
        // ETH: bid=3000, ask=3020
        m.submit(make_limit(2, "ETH/USDT", Side::Buy, 3_000.0, 1.0))
            .unwrap();
        m.submit(make_limit(3, "ETH/USDT", Side::Sell, 3_020.0, 1.0))
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
        let pair = CrossPair::new(
            "BTC/USDT".into(),
            "ETH/USDT".into(),
            16.0,
            Quantity::from_f64(0.5),
        );
        m.register_cross_pair(pair.clone()).expect("ok");
        let result = m.execute_arbitrage(&pair, Quantity::from_f64(1.0), Side::Buy);
        assert!(matches!(
            result,
            Err(MatchingL3Error::InvalidCrossPair { .. })
        ));
    }

    // ─── 快照与恢复 ──────────────────────────────────────

    #[test]
    fn test_snapshot_preserves_state() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_asset("BTC/USDT".into());
        m.register_asset("ETH/USDT".into());
        m.set_batch_mode(BatchMode::Auction);

        let snap = m.snapshot();
        assert_eq!(snap.batch_mode, BatchMode::Auction);
        assert_eq!(snap.engines.len(), 2);
    }

    #[test]
    fn test_restore_recovers_basic_state() {
        let mut m = MultiAssetMatchingEngine::new();
        m.register_asset("BTC/USDT".into());
        m.register_cross_pair(CrossPair::new(
            "BTC/USDT".into(),
            "ETH/USDT".into(),
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
        m.register_asset("BTC/USDT".into());
        m.submit(make_limit(1, "BTC/USDT", Side::Sell, 50_000.0, 1.0))
            .unwrap();
        let orders = vec![
            make_limit(2, "BTC/USDT", Side::Buy, 50_000.0, 1.0),
            make_limit(3, "BTC/USDT", Side::Buy, 49_000.0, 1.0),
        ];
        let fills = m.submit_batch(orders).expect("ok");
        assert_eq!(fills.len(), 1);
    }

    #[test]
    fn test_override_order_price_market_to_limit() {
        // 市价单被覆盖为限价单（用于批量拍卖）
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
}
