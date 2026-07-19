//! L3 完整可见订单簿视图
//!
//! # 设计动机
//!
//! 撮合引擎(L1 / L2 / Impacted / MultiAsset)的内部订单簿是"活的状态机",
//! 持有 `Order` 完整字段 + 状态转换器,不适合做对外稳定 wire format 或报告
//! 数据。`L3Book` 提供一个**只读稳定视图**:
//!
//! - 不持有 `Order` 引用,字段精简(`order_id` / `side` / `qty` / `timestamp_ns`)
//! - 序列化稳定,跨进程/跨语言/跨版本兼容
//! - 与撮合引擎**解耦**,撮合引擎可以替换或重构而不破坏下游消费者
//!
//! # 典型用法
//!
//! ```ignore
//! use axon_backtest::matching::engine::L1MatchingEngine;
//! use axon_backtest::matching::l3::book::L3Book;
//!
//! let mut engine = L1MatchingEngine::new();
//! // ... 撮合若干订单 ...
//!
//! // 从撮合引擎生成稳定视图
//! let book = L3Book::from_l1_engine(&engine);
//!
//! // 用于报告 / 监控 / 序列化
//! let json = serde_json::to_string(&book).unwrap();
//! ```
//!
//! # 数据结构
//!
//! `BTreeMap<Price, Vec<L3Order>>` —— 价位升序存储,价位内 `Vec` 保持 FIFO
//! (与撮合引擎一致)。`best_bid` / `best_ask` 走 BTreeMap 的 first/last key。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use axon_core::market::Side;
use axon_core::types::Price;

use crate::matching::engine::{L1Book, L1MatchingEngine};
use crate::matching::l2::L2MatchingEngine;
use crate::matching::l3::engine_l3::MultiAssetMatchingEngine;

/// L3 订单:精简的 wire format 视图
///
/// 只保留外部消费者需要的最小字段集:
/// - `order_id`:订单唯一 ID
/// - `side`:买卖方向
/// - `qty`:剩余未成交数量
/// - `timestamp_ns`:挂单时间戳(纳秒)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L3Order {
    /// 订单 ID
    pub order_id: u64,
    /// 买卖方向
    pub side: Side,
    /// 剩余未成交数量
    pub qty: f64,
    /// 挂单时间戳(纳秒)
    pub timestamp_ns: i64,
}

impl L3Order {
    /// 从 `Order` 构造 L3 视图(只读拷贝,无状态机)
    pub fn from_order(order: &axon_core::order::Order) -> Self {
        Self {
            order_id: order.id,
            side: order.side,
            qty: order.remaining_quantity().as_f64(),
            timestamp_ns: order.created_at.nanos,
        }
    }
}

/// L3 完整订单簿视图
///
/// `BTreeMap<Price, Vec<L3Order>>` —— 价位升序,价位内 FIFO(`Vec` 顺序即
/// 撮合引擎内的排队顺序)。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct L3Book {
    /// 买单簿(价位升序,最优买价在末尾)
    pub bids: BTreeMap<Price, Vec<L3Order>>,
    /// 卖单簿(价位升序,最优卖价在开头)
    pub asks: BTreeMap<Price, Vec<L3Order>>,
}

impl L3Book {
    /// 创建空 L3Book
    pub fn new() -> Self {
        Self::default()
    }

    /// 从 L1 引擎的**全部** instrument book 聚合(跨所有 asset)
    ///
    /// 注:多 asset 场景下,各 instrument 价格可能不同,聚合后 bids/asks
    /// 会混在一起;通常 `from_l1_engine_for(&instrument)` 更常用。
    pub fn from_l1_engine(engine: &L1MatchingEngine) -> Self {
        let mut book = Self::new();
        for (instrument, l1_book) in engine.iter_books() {
            book.merge_l1_book(l1_book, instrument);
        }
        book
    }

    /// 从 L1 引擎的**指定** instrument book 构造
    pub fn from_l1_engine_for(
        engine: &L1MatchingEngine,
        instrument: &axon_core::types::Instrument,
    ) -> Self {
        let mut book = Self::new();
        if let Some(l1_book) = engine.book_for(instrument) {
            book.merge_l1_book(l1_book, instrument);
        }
        book
    }

    /// 把一个 `L1Book` 合并进当前 L3Book(跨 instrument 聚合)
    fn merge_l1_book(&mut self, l1_book: &L1Book, _instrument: &axon_core::types::Instrument) {
        // bids: BTreeMap 升序 → best_bid 在最后;L3Book 同理
        for (price, level) in l1_book.iter_bids() {
            let entry = self.bids.entry(price).or_default();
            for order in level.iter() {
                if !order.status.is_terminal() {
                    entry.push(L3Order::from_order(order));
                }
            }
        }
        for (price, level) in l1_book.iter_asks() {
            let entry = self.asks.entry(price).or_default();
            for order in level.iter() {
                if !order.status.is_terminal() {
                    entry.push(L3Order::from_order(order));
                }
            }
        }
    }

    /// 从 L2 引擎构造(单 book 路由,L2 内部所有 instrument 共享)
    ///
    /// 注:L2 包装 L1,无 instrument 隔离,直接复用 L1 提取。
    pub fn from_l2_engine(engine: &L2MatchingEngine) -> Self {
        Self::from_l1_engine(engine.inner())
    }

    /// 从 MultiAsset 引擎的**指定** instrument book 构造
    pub fn from_multi_asset(
        engine: &MultiAssetMatchingEngine,
        instrument: &axon_core::types::Instrument,
    ) -> Self {
        let mut book = Self::new();
        if let Some(l2_engine) = engine.engine(instrument) {
            book.merge_l1_book(
                l2_engine
                    .inner()
                    .book_for(instrument)
                    .unwrap_or_else(|| panic!("L2 engine missing book for instrument")),
                instrument,
            );
        }
        book
    }

    /// 最优买价(买单簿最高价)
    pub fn best_bid(&self) -> Option<Price> {
        self.bids.keys().next_back().copied()
    }

    /// 最优卖价(卖单簿最低价)
    pub fn best_ask(&self) -> Option<Price> {
        self.asks.keys().next().copied()
    }

    /// 中间价(`(best_bid + best_ask) / 2`)
    pub fn mid_price(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(b), Some(a)) => Some((b.as_f64() + a.as_f64()) / 2.0),
            _ => None,
        }
    }

    /// 买卖价差(`best_ask - best_bid`)
    pub fn spread(&self) -> Option<Price> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        Some(Price::from_f64(ask.as_f64() - bid.as_f64()))
    }

    /// 买单总数量
    pub fn total_bid_qty(&self) -> f64 {
        self.bids
            .values()
            .flat_map(|v| v.iter())
            .map(|o| o.qty)
            .sum()
    }

    /// 卖单总数量
    pub fn total_ask_qty(&self) -> f64 {
        self.asks
            .values()
            .flat_map(|v| v.iter())
            .map(|o| o.qty)
            .sum()
    }

    /// 买单总订单数
    pub fn total_bid_orders(&self) -> usize {
        self.bids.values().map(|v| v.len()).sum()
    }

    /// 卖单总订单数
    pub fn total_ask_orders(&self) -> usize {
        self.asks.values().map(|v| v.len()).sum()
    }

    /// 取指定方向的指定价位订单(返回切片)
    pub fn orders_at(&self, side: Side, price: Price) -> &[L3Order] {
        let book = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };
        book.get(&price).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// 买单价位迭代(升序)
    pub fn bid_levels(&self) -> impl Iterator<Item = (Price, &[L3Order])> {
        self.bids.iter().map(|(p, v)| (*p, v.as_slice()))
    }

    /// 卖单价位迭代(升序)
    pub fn ask_levels(&self) -> impl Iterator<Item = (Price, &[L3Order])> {
        self.asks.iter().map(|(p, v)| (*p, v.as_slice()))
    }

    /// 是否为空(无 bid 无 ask)
    pub fn is_empty(&self) -> bool {
        self.bids.is_empty() && self.asks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matching::engine::MatchingEngine;
    use axon_core::market::Side;
    use axon_core::order::{Order, OrderType, TimeInForce};
    use axon_core::types::{Price, Quantity, SpotInstrument, Symbol};

    fn btc_spot() -> axon_core::types::Instrument {
        axon_core::types::Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        })
    }

    fn eth_spot() -> axon_core::types::Instrument {
        axon_core::types::Instrument::Spot(SpotInstrument {
            base: Symbol::from("ETH"),
            quote: Symbol::from("USDT"),
        })
    }

    fn make_limit(
        id: u64,
        instrument: &axon_core::types::Instrument,
        side: Side,
        price: f64,
        qty: f64,
    ) -> Order {
        let base = instrument.base().clone();
        let quote = instrument.quote().clone();
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
    }

    // ─── 空 book ──────────────────────────────────────

    #[test]
    fn empty_book_has_no_prices() {
        let book = L3Book::new();
        assert!(book.best_bid().is_none());
        assert!(book.best_ask().is_none());
        assert_eq!(book.mid_price(), None);
        assert_eq!(book.spread(), None);
        assert_eq!(book.total_bid_qty(), 0.0);
        assert_eq!(book.total_ask_qty(), 0.0);
        assert_eq!(book.total_bid_orders(), 0);
        assert_eq!(book.total_ask_orders(), 0);
        assert!(book.is_empty());
    }

    // ─── 单 instrument 撮合后视图 ─────────────────────

    #[test]
    fn from_l1_engine_after_orders() {
        let mut engine = L1MatchingEngine::new();
        let inst = btc_spot();
        engine.submit(make_limit(1, &inst, Side::Buy, 100.0, 1.0));
        engine.submit(make_limit(2, &inst, Side::Buy, 101.0, 2.0));
        engine.submit(make_limit(3, &inst, Side::Sell, 102.0, 1.5));
        engine.submit(make_limit(4, &inst, Side::Sell, 103.0, 3.0));

        let book = L3Book::from_l1_engine_for(&engine, &inst);

        // 4 笔订单,无成交
        assert_eq!(book.total_bid_orders(), 2);
        assert_eq!(book.total_ask_orders(), 2);
        // 价位精确
        assert_eq!(book.best_bid(), Some(Price::from_f64(101.0)));
        assert_eq!(book.best_ask(), Some(Price::from_f64(102.0)));
        // 数量精确
        assert_eq!(book.total_bid_qty(), 3.0); // 1 + 2
        assert_eq!(book.total_ask_qty(), 4.5); // 1.5 + 3
    }

    #[test]
    fn from_l1_engine_with_partial_fill() {
        let mut engine = L1MatchingEngine::new();
        let inst = btc_spot();
        // 卖单 100, 1.0
        engine.submit(make_limit(1, &inst, Side::Sell, 100.0, 1.0));
        // 买单 100, 0.3 → 部分成交后卖单剩 0.7
        let buy = make_limit(2, &inst, Side::Buy, 100.0, 0.3);
        engine.submit(buy);

        let book = L3Book::from_l1_engine_for(&engine, &inst);
        // 卖单剩 0.7,买单 0(全部成交)
        assert_eq!(book.best_ask(), Some(Price::from_f64(100.0)));
        assert_eq!(book.total_ask_qty(), 0.7);
        assert_eq!(book.best_bid(), None);
    }

    // ─── orders_at / levels ──────────────────────────

    #[test]
    fn orders_at_returns_correct_slice() {
        let mut engine = L1MatchingEngine::new();
        let inst = btc_spot();
        engine.submit(make_limit(1, &inst, Side::Buy, 100.0, 1.0));
        engine.submit(make_limit(2, &inst, Side::Buy, 100.0, 2.0));
        engine.submit(make_limit(3, &inst, Side::Buy, 100.0, 3.0));
        engine.submit(make_limit(4, &inst, Side::Buy, 101.0, 4.0));

        let book = L3Book::from_l1_engine_for(&engine, &inst);
        let orders_100 = book.orders_at(Side::Buy, Price::from_f64(100.0));
        assert_eq!(orders_100.len(), 3, "100 价位应有 3 单");
        // FIFO 顺序
        assert_eq!(orders_100[0].order_id, 1);
        assert_eq!(orders_100[1].order_id, 2);
        assert_eq!(orders_100[2].order_id, 3);

        let orders_101 = book.orders_at(Side::Buy, Price::from_f64(101.0));
        assert_eq!(orders_101.len(), 1);
        assert_eq!(orders_101[0].order_id, 4);

        // 不存在的价位返回空切片
        let empty = book.orders_at(Side::Buy, Price::from_f64(200.0));
        assert!(empty.is_empty());
    }

    #[test]
    fn bid_ask_levels_iterate_sorted() {
        let mut engine = L1MatchingEngine::new();
        let inst = btc_spot();
        engine.submit(make_limit(1, &inst, Side::Buy, 99.0, 1.0));
        engine.submit(make_limit(2, &inst, Side::Buy, 100.0, 1.0));
        engine.submit(make_limit(3, &inst, Side::Buy, 101.0, 1.0));
        engine.submit(make_limit(4, &inst, Side::Sell, 102.0, 1.0));
        engine.submit(make_limit(5, &inst, Side::Sell, 103.0, 1.0));

        let book = L3Book::from_l1_engine_for(&engine, &inst);
        let bid_prices: Vec<f64> = book.bid_levels().map(|(p, _)| p.as_f64()).collect();
        assert_eq!(bid_prices, vec![99.0, 100.0, 101.0]);
        let ask_prices: Vec<f64> = book.ask_levels().map(|(p, _)| p.as_f64()).collect();
        assert_eq!(ask_prices, vec![102.0, 103.0]);
    }

    // ─── 多 instrument 隔离 ──────────────────────────

    #[test]
    fn multi_asset_isolated_books() {
        let mut engine = MultiAssetMatchingEngine::new().with_primary(btc_spot());
        engine.register_instrument(eth_spot());

        // BTC 价 100
        let btc = btc_spot();
        let _ = engine.submit(make_limit(1, &btc, Side::Buy, 100.0, 1.0));
        // ETH 价 200
        let eth = eth_spot();
        let _ = engine.submit(make_limit(2, &eth, Side::Buy, 200.0, 1.0));

        // 取 BTC book 不应含 ETH
        let btc_book = L3Book::from_multi_asset(&engine, &btc);
        assert_eq!(btc_book.best_bid(), Some(Price::from_f64(100.0)));
        assert_eq!(btc_book.total_bid_orders(), 1);

        let eth_book = L3Book::from_multi_asset(&engine, &eth);
        assert_eq!(eth_book.best_bid(), Some(Price::from_f64(200.0)));
        assert_eq!(eth_book.total_bid_orders(), 1);
    }

    // ─── L2 engine ────────────────────────────────────

    #[test]
    fn from_l2_engine_works() {
        let mut engine = L2MatchingEngine::new();
        let inst = btc_spot();
        engine.submit(make_limit(1, &inst, Side::Buy, 100.0, 1.0));
        engine.submit(make_limit(2, &inst, Side::Sell, 101.0, 1.0));

        let book = L3Book::from_l2_engine(&engine);
        assert_eq!(book.best_bid(), Some(Price::from_f64(100.0)));
        assert_eq!(book.best_ask(), Some(Price::from_f64(101.0)));
    }

    // ─── 序列化稳定性 ───────────────────────────────

    #[test]
    fn serializes_to_json() {
        let mut engine = L1MatchingEngine::new();
        let inst = btc_spot();
        engine.submit(make_limit(1, &inst, Side::Buy, 100.0, 1.0));

        let book = L3Book::from_l1_engine_for(&engine, &inst);
        let json = serde_json::to_string(&book).expect("serialize ok");
        assert!(json.contains("100"), "JSON should contain price 100");
        assert!(json.contains("order_id"));

        // 反序列化恢复
        let restored: L3Book = serde_json::from_str(&json).expect("deserialize ok");
        assert_eq!(restored.best_bid(), Some(Price::from_f64(100.0)));
        assert_eq!(restored.total_bid_orders(), 1);
    }

    // ─── L3Order 字段精确性 ──────────────────────────

    #[test]
    fn l3_order_fields_precise() {
        let mut engine = L1MatchingEngine::new();
        let inst = btc_spot();
        engine.submit(make_limit(42, &inst, Side::Buy, 100.0, 5.0));

        let book = L3Book::from_l1_engine_for(&engine, &inst);
        let orders = book.orders_at(Side::Buy, Price::from_f64(100.0));
        assert_eq!(orders.len(), 1);
        let o = &orders[0];
        assert_eq!(o.order_id, 42);
        assert_eq!(o.side, Side::Buy);
        assert!((o.qty - 5.0).abs() < 1e-9);
        // timestamp_ns 来自 Order.created_at(Order::spot 默认当前时间,> 0)
        assert!(o.timestamp_ns > 0);
    }

    // ─── mid_price / spread ──────────────────────────

    #[test]
    fn mid_price_and_spread() {
        let mut engine = L1MatchingEngine::new();
        let inst = btc_spot();
        engine.submit(make_limit(1, &inst, Side::Buy, 100.0, 1.0));
        engine.submit(make_limit(2, &inst, Side::Sell, 102.0, 1.0));

        let book = L3Book::from_l1_engine_for(&engine, &inst);
        assert_eq!(book.mid_price(), Some(101.0));
        assert_eq!(book.spread().unwrap().as_f64(), 2.0);
    }

    // ─── L3Book::is_empty ────────────────────────────

    #[test]
    fn is_empty_consistency() {
        let mut engine = L1MatchingEngine::new();
        let inst = btc_spot();
        assert!(L3Book::from_l1_engine_for(&engine, &inst).is_empty());

        engine.submit(make_limit(1, &inst, Side::Buy, 100.0, 1.0));
        assert!(!L3Book::from_l1_engine_for(&engine, &inst).is_empty());
    }
}
