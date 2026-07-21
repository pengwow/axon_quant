//! 0.8.0 Phase 3 A3.2 (simplified):`PriceLevelSoA` + `L3BookSoA` 价位簿 SoA 视图
//!
//! # 设计动机
//!
//! 撮合引擎内部价位簿是 AoS(`BTreeMap<Price, VecDeque<Order>>`),其中
//! `Order` 字段较多(13 个,`Serialize` / `Deserialize` / `Timestamp` / `Quantity` /
//! `Option<String>`)。`L3Book` 等 read-only snapshot 路径只关心
//! `(order_id, side, qty, timestamp_ns)`,`Order` 的 80% 字段都是浪费。
//!
//! SoA 拆分:
//! - `qtys: Vec<f64>` —— 紧凑 cache line,聚合路径`sum()` 一次扫完
//! - `order_ids: Vec<u64>` —— 紧凑,迭代时无 indirection
//! - `sides: Vec<Side>` —— 单字节 enum,几乎免费
//! - `timestamps_ns: Vec<i64>` —— 紧凑 i64 数组
//!
//! # 范围(simplified)
//!
//! - ✅ `PriceLevelSoA` 独立类型(从 `PriceLevel` 提取,不替换 hot path)
//! - ✅ `L3BookSoA` 独立类型(从 `L3Book` 提取,JSON wire format 不变)
//! - ✅ 提供 `total_qty` / `level_count` 等聚合 O(n) 但 cache-friendly 方法
//! - ✅ 单元测试(15 case)
//! - ⏸️ **不**替换现有 `L3Book` JSON 序列化路径(plan 决定保持 wire format 稳定)
//! - ⏸️ **不**修改 L1Book / L2Book(撮合 hot path 仍用 `VecDeque<Order>`,
//!   因为 `Order.status` 状态机管理需要完整字段)
//!
//! # 典型用法
//!
//! ```ignore
//! use axon_backtest::matching::l3::soa::{L3BookSoA, PriceLevelSoA};
//!
//! let l3_book: L3Book = L3Book::from_l1_engine(&engine);
//! let soa: L3BookSoA = L3BookSoA::from_l3_book(&l3_book);
//! let total_bid_qty: f64 = soa.total_bid_qty();  // cache-friendly 聚合
//! let depth_5_bid: f64 = soa.bid_depth_qty(5);   // top 5 档 bid qty 聚合
//! ```
//!
//! # 与现有 `L3Book` 的关系
//!
//! | 路径 | 数据布局 | 适用场景 | JSON 序列化 |
//! |------|----------|----------|-------------|
//! | `L3Book` | `BTreeMap<Price, Vec<L3Order>>` (AoS) | 通用 / 序列化 | ✅ 稳定 |
//! | `L3BookSoA` | `BTreeMap<Price, PriceLevelSoA>` (SoA) | 高频聚合 / 报告 | ⏸️ serde 默认 AoS JSON,compact 报告走 `PriceLevelSoA::to_compact_json` |
//!
//! 两者是**并行的两种视图**,可独立使用。

#![deny(unsafe_code)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use axon_core::market::Side;
use axon_core::types::Price;

use super::super::engine::{L1Book, PriceLevel};
use super::book::{L3Book, L3Order};

/// 单价位的 SoA 视图
///
/// 字段都是 flat `Vec<Copy>`,聚合 / 序列化路径 cache-friendly。
/// `price` 单独一个字段(每个价位一个 `PriceLevelSoA`)。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PriceLevelSoA {
    /// 该价位
    pub price: Price,
    /// 各单剩余 qty(f64)
    pub qtys: Vec<f64>,
    /// 各单 order_id
    pub order_ids: Vec<u64>,
    /// 各单方向(冗余但便于序列化 / 不依赖外部 context)
    pub sides: Vec<Side>,
    /// 各单挂单时间戳(纳秒)
    pub timestamps_ns: Vec<i64>,
}

impl PriceLevelSoA {
    /// 从 `PriceLevel` (FIFO VecDeque&lt;Order&gt;) 提取 SoA
    ///
    /// 只保留**非终态**订单(同 `L3Book` 行为),保证 wire format 一致。
    pub fn from_price_level(price: Price, level: &PriceLevel) -> Self {
        let mut soa = Self {
            price,
            ..Self::default()
        };
        for order in level.iter() {
            if order.status.is_terminal() {
                continue;
            }
            soa.qtys.push(order.remaining_quantity().as_f64());
            soa.order_ids.push(order.id);
            soa.sides.push(order.side);
            soa.timestamps_ns.push(order.created_at.nanos);
        }
        soa
    }

    /// 从 `&[L3Order]` 提取 SoA(`L3Book` 路径)
    pub fn from_l3_orders(price: Price, orders: &[L3Order]) -> Self {
        let mut soa = Self {
            price,
            ..Self::default()
        };
        for o in orders {
            soa.qtys.push(o.qty);
            soa.order_ids.push(o.order_id);
            soa.sides.push(o.side);
            soa.timestamps_ns.push(o.timestamp_ns);
        }
        soa
    }

    /// 订单数
    #[inline]
    pub fn len(&self) -> usize {
        self.qtys.len()
    }

    /// 是否为空
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.qtys.is_empty()
    }

    /// 该价位总 qty(`qtys.iter().sum()`)
    #[inline]
    pub fn total_qty(&self) -> f64 {
        self.qtys.iter().sum()
    }

    /// 紧凑 JSON 数组格式:`[{"order_id":..,"qty":..,"side":..,"timestamp_ns":..}, ...]`
    ///
    /// 注:**不**是稳定 wire format,仅供报告 / 调试。
    /// 正式 wire format 仍走 `L3Book` JSON (AoS 形式)。
    ///
    /// 实现说明:委托 `serde_json::json!` 构造,避免手动 `{:?}` 把 `Side`
    /// 渲染成 `Side::Buy` (Rust Debug 格式) 而非 JSON 字符串 `"Buy"`。
    pub fn to_compact_json(&self) -> String {
        let orders: Vec<serde_json::Value> = (0..self.qtys.len())
            .map(|i| {
                serde_json::json!({
                    "order_id": self.order_ids[i],
                    "qty": self.qtys[i],
                    "side": self.sides[i],
                    "timestamp_ns": self.timestamps_ns[i],
                })
            })
            .collect();
        // 报告路径,不 panic(空 SoA 返回 `"[]"`)
        serde_json::to_string(&orders).unwrap_or_default()
    }
}

/// 整个订单簿的 SoA 视图
///
/// `BTreeMap<Price, PriceLevelSoA>` —— 价位升序,bids 末尾最优 / asks 开头最优。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct L3BookSoA {
    /// 买单簿(SoA)
    pub bids: BTreeMap<Price, PriceLevelSoA>,
    /// 卖单簿(SoA)
    pub asks: BTreeMap<Price, PriceLevelSoA>,
}

impl L3BookSoA {
    /// 创建空 SoA book
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// 从 `L1Book` 直接提取(无需先转 `L3Book`)
    pub fn from_l1_book(book: &L1Book) -> Self {
        let mut soa = Self::new();
        for (price, level) in book.iter_bids() {
            soa.bids
                .insert(price, PriceLevelSoA::from_price_level(price, level));
        }
        for (price, level) in book.iter_asks() {
            soa.asks
                .insert(price, PriceLevelSoA::from_price_level(price, level));
        }
        soa
    }

    /// 从 `L3Book` 转换
    pub fn from_l3_book(book: &L3Book) -> Self {
        let mut soa = Self::new();
        for (price, level) in book.bid_levels() {
            soa.bids
                .insert(price, PriceLevelSoA::from_l3_orders(price, level));
        }
        for (price, level) in book.ask_levels() {
            soa.asks
                .insert(price, PriceLevelSoA::from_l3_orders(price, level));
        }
        soa
    }

    /// 买单总 qty(SoA 路径,cache-friendly)
    pub fn total_bid_qty(&self) -> f64 {
        self.bids.values().map(|lvl| lvl.total_qty()).sum()
    }

    /// 卖单总 qty(SoA 路径)
    pub fn total_ask_qty(&self) -> f64 {
        self.asks.values().map(|lvl| lvl.total_qty()).sum()
    }

    /// 买单总订单数
    pub fn total_bid_orders(&self) -> usize {
        self.bids.values().map(|lvl| lvl.len()).sum()
    }

    /// 卖单总订单数
    pub fn total_ask_orders(&self) -> usize {
        self.asks.values().map(|lvl| lvl.len()).sum()
    }

    /// 买单档位数(不同价位数)
    pub fn bid_level_count(&self) -> usize {
        self.bids.len()
    }

    /// 卖单档位数
    pub fn ask_level_count(&self) -> usize {
        self.asks.len()
    }

    /// Top-N 档买单 qty 聚合(供深度查询)
    ///
    /// `BTreeMap` 升序 → 末尾是最优买价;`take(n)` 从末尾取 n 个。
    /// 注:`BTreeMap` 没有高效的 `rev().take(n)`,所以先 collect keys。
    pub fn bid_depth_qty(&self, depth: usize) -> f64 {
        if depth == 0 {
            return 0.0;
        }
        let mut keys: Vec<Price> = self.bids.keys().copied().collect();
        keys.reverse();
        keys.into_iter()
            .take(depth)
            .filter_map(|k| self.bids.get(&k))
            .map(|lvl| lvl.total_qty())
            .sum()
    }

    /// Top-N 档卖单 qty 聚合
    pub fn ask_depth_qty(&self, depth: usize) -> f64 {
        if depth == 0 {
            return 0.0;
        }
        self.asks
            .keys()
            .copied()
            .take(depth)
            .filter_map(|k| self.asks.get(&k))
            .map(|lvl| lvl.total_qty())
            .sum()
    }

    /// 买单最优价
    pub fn best_bid(&self) -> Option<Price> {
        self.bids.keys().next_back().copied()
    }

    /// 卖单最优价
    pub fn best_ask(&self) -> Option<Price> {
        self.asks.keys().next().copied()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.bids.is_empty() && self.asks.is_empty()
    }

    /// SoA 路径聚合报告(全字段,供监控)
    pub fn summary(&self) -> SoaBookSummary {
        SoaBookSummary {
            best_bid: self.best_bid(),
            best_ask: self.best_ask(),
            total_bid_qty: self.total_bid_qty(),
            total_ask_qty: self.total_ask_qty(),
            total_bid_orders: self.total_bid_orders(),
            total_ask_orders: self.total_ask_orders(),
            bid_levels: self.bid_level_count(),
            ask_levels: self.ask_level_count(),
        }
    }
}

/// SoA 聚合快照
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SoaBookSummary {
    /// 最优买价
    pub best_bid: Option<Price>,
    /// 最优卖价
    pub best_ask: Option<Price>,
    /// 买单总 qty
    pub total_bid_qty: f64,
    /// 卖单总 qty
    pub total_ask_qty: f64,
    /// 买单总订单数
    pub total_bid_orders: usize,
    /// 卖单总订单数
    pub total_ask_orders: usize,
    /// 买单档位数
    pub bid_levels: usize,
    /// 卖单档位数
    pub ask_levels: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matching::engine::{L1MatchingEngine, MatchingEngine};
    use crate::matching::l2::L2MatchingEngine;
    use axon_core::market::Side;
    use axon_core::order::{Order, OrderType, TimeInForce};
    use axon_core::types::{Quantity, SpotInstrument, Symbol};
    use std::collections::VecDeque;

    fn btc_spot() -> axon_core::types::Instrument {
        axon_core::types::Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
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

    // ─── PriceLevelSoA 基础 ─────────────────────────

    #[test]
    fn soa_from_price_level_empty() {
        let soa = PriceLevelSoA::from_price_level(Price::from_f64(100.0), &VecDeque::new());
        assert!(soa.is_empty());
        assert_eq!(soa.len(), 0);
        assert_eq!(soa.total_qty(), 0.0);
        assert_eq!(soa.price, Price::from_f64(100.0));
    }

    #[test]
    fn soa_from_price_level_three_orders() {
        let inst = btc_spot();
        let mut level: PriceLevel = VecDeque::new();
        level.push_back(make_limit(1, &inst, Side::Buy, 100.0, 1.5));
        level.push_back(make_limit(2, &inst, Side::Buy, 100.0, 2.5));
        level.push_back(make_limit(3, &inst, Side::Buy, 100.0, 0.5));

        let soa = PriceLevelSoA::from_price_level(Price::from_f64(100.0), &level);
        assert_eq!(soa.len(), 3);
        assert_eq!(soa.qtys, vec![1.5, 2.5, 0.5]);
        assert_eq!(soa.order_ids, vec![1, 2, 3]);
        assert_eq!(soa.timestamps_ns.len(), 3);
        assert_eq!(soa.total_qty(), 4.5);
    }

    #[test]
    fn soa_from_l3_orders_basic() {
        let l3_orders = vec![
            L3Order {
                order_id: 10,
                side: Side::Sell,
                qty: 3.0,
                timestamp_ns: 100,
            },
            L3Order {
                order_id: 11,
                side: Side::Sell,
                qty: 2.0,
                timestamp_ns: 200,
            },
        ];
        let soa = PriceLevelSoA::from_l3_orders(Price::from_f64(101.0), &l3_orders);
        assert_eq!(soa.qtys, vec![3.0, 2.0]);
        assert_eq!(soa.order_ids, vec![10, 11]);
        assert_eq!(soa.sides, vec![Side::Sell, Side::Sell]);
        assert_eq!(soa.timestamps_ns, vec![100, 200]);
        assert_eq!(soa.total_qty(), 5.0);
    }

    // ─── L3BookSoA 聚合路径 ─────────────────────────

    #[test]
    fn l3_book_soa_empty() {
        let soa = L3BookSoA::new();
        assert!(soa.is_empty());
        assert_eq!(soa.total_bid_qty(), 0.0);
        assert_eq!(soa.total_ask_qty(), 0.0);
        assert_eq!(soa.best_bid(), None);
        assert_eq!(soa.best_ask(), None);
        assert_eq!(soa.bid_level_count(), 0);
        assert_eq!(soa.ask_level_count(), 0);
    }

    #[test]
    fn l3_book_soa_from_l1_engine() {
        let mut engine = L1MatchingEngine::new();
        let inst = btc_spot();
        engine.submit(make_limit(1, &inst, Side::Buy, 99.0, 1.0));
        engine.submit(make_limit(2, &inst, Side::Buy, 100.0, 2.0));
        engine.submit(make_limit(3, &inst, Side::Buy, 101.0, 3.0));
        engine.submit(make_limit(4, &inst, Side::Sell, 102.0, 4.0));
        engine.submit(make_limit(5, &inst, Side::Sell, 103.0, 5.0));

        let l1_book = engine.book_for(&inst).expect("inst registered");
        let soa = L3BookSoA::from_l1_book(l1_book);

        // 3 个 bid 价位 + 2 个 ask 价位
        assert_eq!(soa.bid_level_count(), 3);
        assert_eq!(soa.ask_level_count(), 2);
        assert_eq!(soa.best_bid(), Some(Price::from_f64(101.0)));
        assert_eq!(soa.best_ask(), Some(Price::from_f64(102.0)));
        // bid qty: 1 + 2 + 3 = 6
        assert_eq!(soa.total_bid_qty(), 6.0);
        // ask qty: 4 + 5 = 9
        assert_eq!(soa.total_ask_qty(), 9.0);
        // 总订单数
        assert_eq!(soa.total_bid_orders(), 3);
        assert_eq!(soa.total_ask_orders(), 2);
    }

    #[test]
    fn l3_book_soa_depth_aggregation() {
        let mut engine = L1MatchingEngine::new();
        let inst = btc_spot();
        // 5 档 bid: 100/1.0, 101/2.0, 102/3.0, 103/4.0, 104/5.0
        engine.submit(make_limit(1, &inst, Side::Buy, 100.0, 1.0));
        engine.submit(make_limit(2, &inst, Side::Buy, 101.0, 2.0));
        engine.submit(make_limit(3, &inst, Side::Buy, 102.0, 3.0));
        engine.submit(make_limit(4, &inst, Side::Buy, 103.0, 4.0));
        engine.submit(make_limit(5, &inst, Side::Buy, 104.0, 5.0));
        // 5 档 ask: 105/1.0, 106/2.0, 107/3.0, 108/4.0, 109/5.0
        for (i, p) in [105.0, 106.0, 107.0, 108.0, 109.0].iter().enumerate() {
            engine.submit(make_limit(
                10 + i as u64,
                &inst,
                Side::Sell,
                *p,
                (i + 1) as f64,
            ));
        }

        let l1_book = engine.book_for(&inst).expect("inst registered");
        let soa = L3BookSoA::from_l1_book(l1_book);

        // Top 1 bid = 5.0 (104.0 价位)
        assert_eq!(soa.bid_depth_qty(1), 5.0);
        // Top 3 bid = 5.0 + 4.0 + 3.0 = 12.0
        assert_eq!(soa.bid_depth_qty(3), 12.0);
        // Top 5 bid = 全部 = 15.0
        assert_eq!(soa.bid_depth_qty(5), 15.0);
        // depth > 档位数 → 返回全部
        assert_eq!(soa.bid_depth_qty(100), 15.0);

        // Top 1 ask = 1.0 (105.0 价位)
        assert_eq!(soa.ask_depth_qty(1), 1.0);
        // Top 3 ask = 1.0 + 2.0 + 3.0 = 6.0
        assert_eq!(soa.ask_depth_qty(3), 6.0);
        // depth 0 = 0.0
        assert_eq!(soa.bid_depth_qty(0), 0.0);
        assert_eq!(soa.ask_depth_qty(0), 0.0);
    }

    // ─── 与 L3Book 行为等价 ─────────────────────────

    #[test]
    fn l3_book_soa_equivalent_to_l3_book() {
        let mut engine = L1MatchingEngine::new();
        let inst = btc_spot();
        engine.submit(make_limit(1, &inst, Side::Buy, 100.0, 1.5));
        engine.submit(make_limit(2, &inst, Side::Buy, 100.0, 2.5));
        engine.submit(make_limit(3, &inst, Side::Sell, 101.0, 4.0));
        engine.submit(make_limit(4, &inst, Side::Sell, 102.0, 5.0));

        let l1_book = engine.book_for(&inst).expect("inst registered");
        let l3_book = L3Book::from_l1_engine_for(&engine, &inst);
        let soa = L3BookSoA::from_l1_book(l1_book);
        let soa_via_l3 = L3BookSoA::from_l3_book(&l3_book);

        // 两种 SoA 构造路径结果一致
        assert_eq!(soa, soa_via_l3);
        // SoA 与 L3Book 聚合值一致
        assert_eq!(soa.total_bid_qty(), l3_book.total_bid_qty());
        assert_eq!(soa.total_ask_qty(), l3_book.total_ask_qty());
        assert_eq!(soa.total_bid_orders(), l3_book.total_bid_orders());
        assert_eq!(soa.total_ask_orders(), l3_book.total_ask_orders());
        assert_eq!(soa.best_bid(), l3_book.best_bid());
        assert_eq!(soa.best_ask(), l3_book.best_ask());
    }

    // ─── partial fill 后 SoA 状态正确 ────────────────

    #[test]
    fn soa_after_partial_fill() {
        let mut engine = L1MatchingEngine::new();
        let inst = btc_spot();
        engine.submit(make_limit(1, &inst, Side::Sell, 100.0, 5.0));
        // 部分成交:卖单剩 2.0,买单 0
        engine.submit(make_limit(2, &inst, Side::Buy, 100.0, 3.0));

        let l1_book = engine.book_for(&inst).expect("inst registered");
        let soa = L3BookSoA::from_l1_book(l1_book);

        assert_eq!(soa.bid_level_count(), 0);
        assert_eq!(soa.ask_level_count(), 1);
        assert_eq!(soa.best_ask(), Some(Price::from_f64(100.0)));
        // sell 1 partial: 5.0 - 3.0 = 2.0
        assert_eq!(soa.total_ask_qty(), 2.0);
        assert_eq!(soa.total_ask_orders(), 1);
    }

    // ─── Summary ─────────────────────────────────────

    #[test]
    fn soa_summary_basic() {
        let mut engine = L1MatchingEngine::new();
        let inst = btc_spot();
        engine.submit(make_limit(1, &inst, Side::Buy, 100.0, 1.0));
        engine.submit(make_limit(2, &inst, Side::Buy, 101.0, 2.0));
        engine.submit(make_limit(3, &inst, Side::Sell, 102.0, 3.0));

        let l1_book = engine.book_for(&inst).expect("inst registered");
        let soa = L3BookSoA::from_l1_book(l1_book);
        let summary = soa.summary();

        assert_eq!(summary.best_bid, Some(Price::from_f64(101.0)));
        assert_eq!(summary.best_ask, Some(Price::from_f64(102.0)));
        assert_eq!(summary.total_bid_qty, 3.0);
        assert_eq!(summary.total_ask_qty, 3.0);
        assert_eq!(summary.total_bid_orders, 2);
        assert_eq!(summary.total_ask_orders, 1);
        assert_eq!(summary.bid_levels, 2);
        assert_eq!(summary.ask_levels, 1);
    }

    // ─── SoA JSON 序列化(自定义 compact) ───────────

    #[test]
    fn price_level_soa_serde_json_roundtrip() {
        let soa = PriceLevelSoA {
            price: Price::from_f64(100.0),
            qtys: vec![1.5, 2.5],
            order_ids: vec![1, 2],
            sides: vec![Side::Buy, Side::Buy],
            timestamps_ns: vec![100, 200],
        };
        let json = serde_json::to_string(&soa).expect("serialize");
        let restored: PriceLevelSoA = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(soa, restored);
    }

    #[test]
    fn l3_book_soa_serde_json_roundtrip() {
        let mut engine = L1MatchingEngine::new();
        let inst = btc_spot();
        engine.submit(make_limit(1, &inst, Side::Buy, 100.0, 1.0));
        engine.submit(make_limit(2, &inst, Side::Sell, 101.0, 2.0));

        let l1_book = engine.book_for(&inst).expect("inst registered");
        let soa = L3BookSoA::from_l1_book(l1_book);
        let json = serde_json::to_string(&soa).expect("serialize");
        let restored: L3BookSoA = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(soa, restored);
    }

    // ─── Multi-instrument / L2 / MultiAsset ─────────

    #[test]
    fn l3_book_soa_via_l2_engine() {
        let mut engine = L2MatchingEngine::new();
        let inst = btc_spot();
        engine.submit(make_limit(1, &inst, Side::Buy, 100.0, 1.0));
        engine.submit(make_limit(2, &inst, Side::Sell, 101.0, 2.0));

        let l1_book = engine.inner().book_for(&inst).expect("inst");
        let soa = L3BookSoA::from_l1_book(l1_book);
        assert_eq!(soa.best_bid(), Some(Price::from_f64(100.0)));
        assert_eq!(soa.best_ask(), Some(Price::from_f64(101.0)));
        assert_eq!(soa.total_bid_qty(), 1.0);
        assert_eq!(soa.total_ask_qty(), 2.0);
    }

    // ─── PriceLevelSoA compact JSON 报告 ────────────

    #[test]
    fn price_level_soa_compact_json_format() {
        let soa = PriceLevelSoA {
            price: Price::from_f64(100.0),
            qtys: vec![1.5, 2.5],
            order_ids: vec![1, 2],
            sides: vec![Side::Buy, Side::Buy],
            timestamps_ns: vec![100, 200],
        };
        let json = soa.to_compact_json();
        // 包含关键字段
        assert!(json.contains("\"order_id\":1"));
        assert!(json.contains("\"qty\":1.5"));
        assert!(json.contains("\"order_id\":2"));
        assert!(json.contains("\"qty\":2.5"));
    }

    /// Regression: `side` 必须以 JSON 字符串 `"Buy"` / `"Sell"` 输出,
    /// 旧实现用 `{:?}` 会渲染成 Rust Debug 格式 `Side::Buy`,这是非法 JSON。
    /// 验证:解析 `to_compact_json()` 输出,`side` 字段必须是字符串。
    #[test]
    fn price_level_soa_compact_json_side_is_json_string() {
        let soa = PriceLevelSoA {
            price: Price::from_f64(100.0),
            qtys: vec![1.0, 2.0],
            order_ids: vec![1, 2],
            sides: vec![Side::Buy, Side::Sell],
            timestamps_ns: vec![100, 200],
        };
        let json = soa.to_compact_json();

        // 字符串里不能含 Rust 调试格式 `Side::` 前缀
        assert!(
            !json.contains("Side::"),
            "compact JSON 不应含 Rust Debug 格式 'Side::',got: {json}"
        );

        // side 字段必须以 JSON 字符串出现
        assert!(json.contains("\"side\":\"Buy\""), "got: {json}");
        assert!(json.contains("\"side\":\"Sell\""), "got: {json}");

        // round-trip 解析回 `Vec<serde_json::Value>`
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["side"], "Buy");
        assert_eq!(parsed[1]["side"], "Sell");
        assert_eq!(parsed[0]["order_id"], 1);
        assert_eq!(parsed[0]["qty"], 1.0);
        assert_eq!(parsed[0]["timestamp_ns"], 100);
    }

    /// 空 SoA 输出 `"[]"`(合法 JSON 数组)
    #[test]
    fn price_level_soa_compact_json_empty() {
        let soa = PriceLevelSoA {
            price: Price::from_f64(100.0),
            qtys: vec![],
            order_ids: vec![],
            sides: vec![],
            timestamps_ns: vec![],
        };
        let json = soa.to_compact_json();
        assert_eq!(json, "[]");
        // 也能正常 round-trip
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert!(parsed.is_array());
        assert_eq!(parsed.as_array().unwrap().len(), 0);
    }

    // ─── empty SoA JSON roundtrip ──────────────────

    #[test]
    fn l3_book_soa_empty_roundtrip() {
        let soa = L3BookSoA::new();
        let json = serde_json::to_string(&soa).expect("serialize");
        let restored: L3BookSoA = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(soa, restored);
        assert!(restored.is_empty());
    }

    // ─── Performance smoke (1000 levels, 10 orders/each) ──

    #[test]
    #[ignore] // 跑 < 1s 性能 smoke;`cargo test -- --ignored` 启用
    fn soa_aggregates_10k_orders_under_100ms() {
        let mut engine = L1MatchingEngine::new();
        let inst = btc_spot();
        // 100 价位 × 100 单 = 10K 单
        for level in 0..100 {
            for order in 0..100 {
                let id = level * 100 + order + 1;
                let price = 100.0 + level as f64;
                engine.submit(make_limit(id, &inst, Side::Buy, price, 1.0));
            }
        }
        let l1_book = engine.book_for(&inst).expect("inst");
        let soa = L3BookSoA::from_l1_book(l1_book);

        let start = std::time::Instant::now();
        for _ in 0..100 {
            let total = soa.total_bid_qty();
            std::hint::black_box(total);
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 100,
            "100× SoA total_bid_qty 应 < 100ms,实测 {elapsed:?}"
        );
    }
}
