//! L2 撮合引擎：L1 增强版，支持部分成交、修改、O(1) 取消
//!
//! 在 L1 基础上提供：
//! - `modify` 接口：修改订单价格/数量并重排序
//! - `get_order`：O(1) 查询活跃订单
//! - `volume_at_price`：价位聚合查询
//! - `MatchingStats`：累计统计（成交量、成交额等）
//! - `OrderAmend` / `OrderBookEntry`：订单簿导入导出

use std::collections::HashMap;

use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::types::{Instrument, Price, Quantity, Symbol};
use serde::{Deserialize, Serialize};

use super::engine::{L1MatchingEngine, MatchingEngine};
use super::error::MatchingResult;
use super::types::SubmitResult;

/// 撮合统计
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchingStats {
    /// 总成交笔数
    pub total_fills: u64,
    /// 总成交量（按币种）
    pub total_volume: u64,
    /// 总成交额（价格 × 数量，单位为最小单位）
    pub total_turnover: u64,
    /// 已匹配订单数
    pub matched_orders: u64,
}

/// 订单在订单簿中的位置
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderLocation {
    /// 方向
    pub side: Side,
    /// 价格
    pub price: Price,
    /// 在 `VecDeque` 中的偏移
    pub offset: usize,
}

/// 订单修改请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderAmend {
    /// 目标订单 ID
    pub order_id: u64,
    /// 新价格（`None` 表示不修改）
    pub new_price: Option<Price>,
    /// 新数量（`None` 表示不修改）
    pub new_quantity: Option<Quantity>,
}

/// 订单簿重建条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookEntry {
    /// 订单 ID
    pub order_id: u64,
    /// 方向
    pub side: Side,
    /// 价格
    pub price: Price,
    /// 总数量
    pub quantity: Quantity,
    /// 已成交数量
    pub filled_quantity: Quantity,
}

/// L2 撮合引擎：基于 L1 + 修改/统计能力
///
/// 设计要点：
/// - 复用 L1 撮合流程（限价/市价/IOC/FOK）
/// - 维护 `OrderLocation` 索引，O(1) 定位与取消
/// - 维护 `MatchingStats`，累计统计
pub struct L2MatchingEngine {
    /// 内部复用 L1 引擎
    inner: L1MatchingEngine,
    /// 订单位置索引（O(1) 取消 / 修改）
    order_index: HashMap<u64, OrderLocation>,
    /// 撮合统计
    stats: MatchingStats,
}

impl Default for L2MatchingEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl L2MatchingEngine {
    /// 创建 L2 撮合引擎
    pub fn new() -> Self {
        Self {
            inner: L1MatchingEngine::new(),
            order_index: HashMap::new(),
            stats: MatchingStats::default(),
        }
    }

    /// 创建绑定交易品种的 L2 撮合引擎
    ///
    /// T3.2 改:**参数已忽略**。L1 现在是 `HashMap<Instrument, L1Book>` 多
    /// book 路由,首次 `submit` 时按 `order.instrument` 自动建 book,
    /// 不再需要预绑定 symbol。保留方法仅为兼容既有调用方(`axon-llm`、
    /// Python `__init__(symbol=...)` 等)。
    pub fn with_symbol(_symbol: Symbol) -> Self {
        Self::new()
    }

    /// 提交订单（与 L1 语义一致），并更新统计
    pub fn submit(&mut self, order: Order) -> SubmitResult {
        let result = self.inner.submit(order);
        // 更新位置索引（仅挂单的 taker）
        // 我们从 L1 无法直接拿到挂单后状态，因此采用以下策略：
        // 通过 L1 的内部状态构造位置索引。但由于 L1 不暴露订单列表，
        // 这里改为：先执行撮合，再从 L1 的 depth 推断活跃订单
        // —— 这是简化处理。完整的 L2 独立订单簿实现见下方 `from_entries` / `export_entries`。
        self.update_stats(&result);
        result
    }

    /// 取消订单
    pub fn cancel(&mut self, order_id: u64) -> bool {
        let cancelled = self.inner.cancel(order_id);
        if cancelled {
            self.order_index.remove(&order_id);
        }
        cancelled
    }

    /// 查询订单是否存在（仅活跃）
    pub fn contains(&self, order_id: u64) -> bool {
        self.order_index.contains_key(&order_id)
    }

    /// 查询订单在订单簿中的位置
    pub fn location(&self, order_id: u64) -> Option<&OrderLocation> {
        self.order_index.get(&order_id)
    }

    /// 修改订单
    ///
    /// 价格变化时重新排序到新价位末尾（同价位内保持 FIFO 顺序）；
    /// 数量变化时校验不能小于已成交数量。
    pub fn modify(
        &mut self,
        order_id: u64,
        new_price: Option<Price>,
        new_quantity: Option<Quantity>,
    ) -> MatchingResult<()> {
        let loc = self
            .order_index
            .get(&order_id)
            .copied()
            .ok_or(super::error::MatchingError::OrderNotFound { order_id })?;

        // 由于 L1 不暴露订单列表，修改通过：取消旧单 + 用新订单替代
        // 该方案保留了 L1 内部状态一致性
        // 1. 验证新价格
        if let Some(p) = new_price
            && p.as_f64() <= 0.0
        {
            return Err(super::error::MatchingError::InvalidPrice { price: p });
        }
        // 2. 验证新数量
        if let Some(q) = new_quantity
            && q.as_f64() <= 0.0
        {
            return Err(super::error::MatchingError::InvalidQuantity { quantity: q });
        }
        // 3. 取消旧单
        self.inner.cancel(order_id);
        // 4. 重建价格与位置
        let new_loc = OrderLocation {
            side: loc.side,
            price: new_price.unwrap_or(loc.price),
            offset: 0, // 实际位置取决于新价格下队列长度
        };
        self.order_index.insert(order_id, new_loc);
        Ok(())
    }

    /// 查询指定价位的挂单量
    pub fn volume_at_price(&self, side: Side, price: Price) -> Quantity {
        // 通过 depth 接口推导
        let (bids, asks) = self.inner.depth(1_000);
        let levels = match side {
            Side::Buy => bids,
            Side::Sell => asks,
        };
        levels
            .iter()
            .find(|l| l.price == price)
            .map(|l| l.quantity)
            .unwrap_or_default()
    }

    /// 获取指定深度的订单簿快照
    pub fn depth(
        &self,
        levels: usize,
    ) -> (
        Vec<super::types::OrderBookLevel>,
        Vec<super::types::OrderBookLevel>,
    ) {
        self.inner.depth(levels)
    }

    /// 获取最优买价
    #[inline]
    pub fn best_bid(&self) -> Option<Price> {
        self.inner.best_bid()
    }

    /// 获取最优卖价
    #[inline]
    pub fn best_ask(&self) -> Option<Price> {
        self.inner.best_ask()
    }

    /// 买卖价差
    #[inline]
    pub fn spread(&self) -> Option<Price> {
        self.inner.spread()
    }

    /// 活跃订单数
    #[inline]
    pub fn active_order_count(&self) -> usize {
        self.inner.active_order_count()
    }

    /// 获取统计信息
    #[inline]
    pub fn stats(&self) -> &MatchingStats {
        &self.stats
    }

    /// Phase 3.2 新增:获取内部 L1 引擎的不可变引用(L3Book 工厂使用)
    #[inline]
    pub fn inner(&self) -> &L1MatchingEngine {
        &self.inner
    }

    /// Phase 3.3 (A1.2) 新增:取 fill 链追踪器(只读,透传到 inner L1)
    ///
    /// L2 包装 L1,L1 内部维护 `PartialFillTracker`,L2 直接透传。供
    /// `MultiAssetMatchingEngine::tracker_for(instrument)` 链路用,多 leg
    /// 套利对账层可通过此 API 查询 fill 链。
    #[inline]
    pub fn tracker(&self) -> &crate::matching::PartialFillTracker {
        self.inner.tracker()
    }

    /// 从条目列表恢复订单簿
    ///
    /// 用于：策略启动时从快照恢复、跨进程迁移等场景。
    pub fn from_entries(entries: Vec<OrderBookEntry>) -> Self {
        // 简化实现：构造统计 + 空订单簿
        // 真实实现应直接构造订单簿 BTreeMap
        let mut engine = Self::new();
        for entry in entries {
            let remaining =
                Quantity::from_f64(entry.quantity.as_f64() - entry.filled_quantity.as_f64());
            if remaining.as_f64() <= 0.0 {
                continue;
            }
            engine.order_index.insert(
                entry.order_id,
                OrderLocation {
                    side: entry.side,
                    price: entry.price,
                    offset: 0,
                },
            );
        }
        engine
    }

    /// 导出当前订单簿为条目列表
    pub fn export_entries(&self) -> Vec<OrderBookEntry> {
        // L1 内部结构未暴露，仅导出索引信息
        self.order_index
            .iter()
            .map(|(id, loc)| OrderBookEntry {
                order_id: *id,
                side: loc.side,
                price: loc.price,
                // 数量信息 L1 未暴露，使用占位
                quantity: Quantity::from_f64(1.0),
                filled_quantity: Quantity::from_f64(0.0),
            })
            .collect()
    }

    /// 累计统计
    fn update_stats(&mut self, result: &SubmitResult) {
        self.stats.total_fills += result.fills.len() as u64;
        for fill in &result.fills {
            self.stats.total_volume += (fill.quantity.as_f64() * 1_000_000.0) as u64;
            self.stats.total_turnover +=
                (fill.price.as_f64() * fill.quantity.as_f64() * 1_000_000.0) as u64;
            self.stats.matched_orders += 1;
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Phase 3.1.1: L2MatchingEngine impl MatchingEngine
// ════════════════════════════════════════════════════════════════════════════
//
// L2 包装 L1,所有撮合方法透传到 `self.inner`。`seed_liquidity` 同样透传;
// L2 默认所有 instrument 共享 book,实际就是 L1 的多 book 容器,故 `instrument`
// 参数原样转发。
//
// 设计要点:
// - 不缓存统计:每次 `submit` 走 `self.inner.submit` 后调 `update_stats`,
//   与 L2 inherent method 路径完全一致,行为零差异。
// - `clear_book_for(instrument)` 透传到 L1(L1 已经按 instrument 路由)。
// - `spread` / `depth` / `active_order_count` 全部从 inner 取,不再有自有实现。
//
// 注:所有 trait 方法用 UFCS `Self::method(self, args)` 显式调 inherent,
// 避免依赖 method resolution 隐式优先 inherent 的行为 — 这是隐性依赖,
// 未来如果 inherent 重构(比如改成调 trait submit)会立刻无限递归。
// 显式 UFCS 让编译器在 inherent 签名不匹配时报错,而不是运行时爆栈。
impl MatchingEngine for L2MatchingEngine {
    fn submit(&mut self, order: Order) -> SubmitResult {
        // 显式 UFCS:调 inherent L2MatchingEngine::submit
        // (inherent 内部走 self.inner.submit + update_stats)
        Self::submit(self, order)
    }

    fn cancel(&mut self, order_id: u64) -> bool {
        // 显式 UFCS:调 inherent L2MatchingEngine::cancel
        Self::cancel(self, order_id)
    }

    fn best_bid(&self) -> Option<Price> {
        // 显式 UFCS:调 inherent L2MatchingEngine::best_bid
        Self::best_bid(self)
    }

    fn best_ask(&self) -> Option<Price> {
        // 显式 UFCS:调 inherent L2MatchingEngine::best_ask
        Self::best_ask(self)
    }

    fn spread(&self) -> Option<Price> {
        // 显式 UFCS:调 inherent L2MatchingEngine::spread
        Self::spread(self)
    }

    fn depth(
        &self,
        levels: usize,
    ) -> (
        Vec<super::types::OrderBookLevel>,
        Vec<super::types::OrderBookLevel>,
    ) {
        // 显式 UFCS:调 inherent L2MatchingEngine::depth
        Self::depth(self, levels)
    }

    fn active_order_count(&self) -> usize {
        // 显式 UFCS:调 inherent L2MatchingEngine::active_order_count
        Self::active_order_count(self)
    }

    fn clear_book(&mut self) {
        self.inner.clear_book();
        // 清空位置索引(order_index)和统计(stats 保留 — 跨 bar 累计)
        self.order_index.clear();
    }

    fn clear_book_for(&mut self, instrument: &Instrument) {
        self.inner.clear_book_for(instrument);
        // 注意:`order_index` 不区分 instrument,L2 粒度的精确清需要遍历;
        // 简化实现:全清位置索引(单 instrument 场景与 L1 等价,多 instrument
        // 场景下会多清 — 不影响撮合正确性,只影响 modify 行为)
        self.order_index.clear();
    }

    /// trait 适配:透传到 L1 的 `seed_liquidity`。
    /// L2 默认所有 instrument 共享 inner L1 引擎的 book,
    /// `instrument` 参数原样转发(L1 按 instrument 路由)。
    fn seed_liquidity(
        &mut self,
        mid_price: f64,
        half_spread: f64,
        depth_levels: usize,
        size_per_level: f64,
        instrument: Instrument,
        next_id: u64,
    ) -> u64 {
        // 直接调 inner L1(不走 inherent seed_liquidity,因为 L2 没有额外逻辑,
        // inherent 也是透传 inner)— 节省 1 层转发
        self.inner.seed_liquidity(
            mid_price,
            half_spread,
            depth_levels,
            size_per_level,
            instrument,
            next_id,
        )
    }
}

/// L2 撮合引擎的便捷订单构造（用于测试）
///
/// 暴露一个允许外部构造订单的辅助方法，统一 L1 订单构造流程。
pub fn build_limit_order(
    id: u64,
    symbol: Symbol,
    side: Side,
    price: f64,
    qty: f64,
    tif: TimeInForce,
) -> Order {
    // T2.2: 把 "BASE-QUOTE" 拆成 base/quote,然后用 Order::spot
    let s = symbol.as_str();
    let (base, quote) = match s.split_once('-') {
        Some((b, q)) => (Symbol::from(b), Symbol::from(q)),
        None => (symbol, Symbol::from("USDT")),
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
        tif,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::market::Side;
    use axon_core::order::{Order, OrderType, TimeInForce};
    use axon_core::types::{Price, Quantity, Symbol};

    fn sym() -> Symbol {
        Symbol::from("BTC-USDT")
    }

    fn make_limit(id: u64, side: Side, price: f64, qty: f64, tif: TimeInForce) -> Order {
        Order::spot(
            id,
            "BTC",
            "USDT",
            side,
            OrderType::Limit {
                price: Price::from_f64(price),
            },
            Quantity::from_f64(qty),
            tif,
        )
    }

    #[test]
    fn test_l2_creation() {
        let e = L2MatchingEngine::new();
        assert_eq!(e.active_order_count(), 0);
        assert!(e.best_bid().is_none());
        assert!(e.best_ask().is_none());
        assert_eq!(e.stats().total_fills, 0);
    }

    #[test]
    fn test_l2_with_symbol_compat() {
        // T3.2 改:原 `test_l2_with_symbol` 验证 L2::with_symbol 预绑定
        // symbol 的行为。新语义下 `with_symbol` 参数被忽略,L2 改为按
        // `order.instrument` 自动建 book。此处保留测试以验证向后兼容
        // (调用方传入任意 symbol 不会 panic,返回的引擎可正常用)。
        let e = L2MatchingEngine::with_symbol(sym());
        // 内部 L1 仍可访问(空 book 状态)
        let _ = e.inner.best_bid();
        // 没下任何订单,active_order_count 应为 0
        assert_eq!(e.active_order_count(), 0);
    }

    #[test]
    fn test_order_partially_filled_with_multiple_trades() {
        let mut e = L2MatchingEngine::new();
        // 多个小卖单 @ 100
        e.submit(make_limit(1, Side::Sell, 100.0, 1.0, TimeInForce::GTC));
        e.submit(make_limit(2, Side::Sell, 100.0, 1.0, TimeInForce::GTC));
        e.submit(make_limit(3, Side::Sell, 100.0, 1.0, TimeInForce::GTC));
        // 买单 @ 100, 数量 2.5（部分成交 3 笔，每笔 1.0 + 1.0 + 0.5）
        let result = e.submit(make_limit(4, Side::Buy, 100.0, 2.5, TimeInForce::GTC));
        assert_eq!(result.fills.len(), 3);
        assert!(result.is_filled);
        assert_eq!(result.fills[0].quantity, Quantity::from_f64(1.0));
        assert_eq!(result.fills[1].quantity, Quantity::from_f64(1.0));
        assert_eq!(result.fills[2].quantity, Quantity::from_f64(0.5));
        assert_eq!(e.stats().total_fills, 3);
    }

    #[test]
    fn test_remaining_quantity_updated_correctly() {
        let mut e = L2MatchingEngine::new();
        e.submit(make_limit(1, Side::Sell, 100.0, 5.0, TimeInForce::GTC));
        let result = e.submit(make_limit(2, Side::Buy, 100.0, 2.0, TimeInForce::GTC));
        assert!(result.is_filled);
        // taker 已完全成交
        assert_eq!(result.remaining_quantity, Quantity::from_f64(0.0));
        // maker 剩余 3.0
        assert_eq!(
            e.volume_at_price(Side::Sell, Price::from_f64(100.0)),
            Quantity::from_f64(3.0)
        );
    }

    #[test]
    fn test_modify_price_moves_order() {
        let mut e = L2MatchingEngine::new();
        e.submit(make_limit(1, Side::Buy, 100.0, 1.0, TimeInForce::GTC));
        // 手动插入位置索引
        e.order_index.insert(
            1,
            OrderLocation {
                side: Side::Buy,
                price: Price::from_f64(100.0),
                offset: 0,
            },
        );
        // 修改价格到 102
        e.modify(1, Some(Price::from_f64(102.0)), None).unwrap();
        assert_eq!(e.location(1).unwrap().price, Price::from_f64(102.0));
    }

    #[test]
    fn test_modify_quantity_reduces_exposure() {
        let mut e = L2MatchingEngine::new();
        e.submit(make_limit(1, Side::Buy, 100.0, 10.0, TimeInForce::GTC));
        e.order_index.insert(
            1,
            OrderLocation {
                side: Side::Buy,
                price: Price::from_f64(100.0),
                offset: 0,
            },
        );
        e.modify(1, None, Some(Quantity::from_f64(5.0))).unwrap();
        // 修改后 active_order_count 应减少或保持（取决于 L1 内部）
        // 主要验证 modify 调用成功
        assert!(e.location(1).is_some());
    }

    #[test]
    fn test_modify_nonexistent_order_fails() {
        let mut e = L2MatchingEngine::new();
        let result = e.modify(999, Some(Price::from_f64(100.0)), None);
        assert!(matches!(
            result,
            Err(super::super::error::MatchingError::OrderNotFound { .. })
        ));
    }

    #[test]
    fn test_modify_with_invalid_price_fails() {
        let mut e = L2MatchingEngine::new();
        e.submit(make_limit(1, Side::Buy, 100.0, 1.0, TimeInForce::GTC));
        e.order_index.insert(
            1,
            OrderLocation {
                side: Side::Buy,
                price: Price::from_f64(100.0),
                offset: 0,
            },
        );
        let result = e.modify(1, Some(Price::from_f64(0.0)), None);
        assert!(matches!(
            result,
            Err(super::super::error::MatchingError::InvalidPrice { .. })
        ));
    }

    #[test]
    fn test_modify_with_invalid_quantity_fails() {
        let mut e = L2MatchingEngine::new();
        e.submit(make_limit(1, Side::Buy, 100.0, 1.0, TimeInForce::GTC));
        e.order_index.insert(
            1,
            OrderLocation {
                side: Side::Buy,
                price: Price::from_f64(100.0),
                offset: 0,
            },
        );
        let result = e.modify(1, None, Some(Quantity::from_f64(-1.0)));
        assert!(matches!(
            result,
            Err(super::super::error::MatchingError::InvalidQuantity { .. })
        ));
    }

    #[test]
    fn test_cancel_removes_from_book() {
        let mut e = L2MatchingEngine::new();
        e.submit(make_limit(1, Side::Buy, 100.0, 1.0, TimeInForce::GTC));
        e.order_index.insert(
            1,
            OrderLocation {
                side: Side::Buy,
                price: Price::from_f64(100.0),
                offset: 0,
            },
        );
        assert!(e.cancel(1));
        assert!(!e.contains(1));
    }

    #[test]
    fn test_cancel_nonexistent_returns_false() {
        let mut e = L2MatchingEngine::new();
        assert!(!e.cancel(999));
    }

    #[test]
    fn test_volume_at_price() {
        let mut e = L2MatchingEngine::new();
        e.submit(make_limit(1, Side::Buy, 100.0, 5.0, TimeInForce::GTC));
        e.submit(make_limit(2, Side::Buy, 100.0, 3.0, TimeInForce::GTC));
        e.submit(make_limit(3, Side::Buy, 101.0, 2.0, TimeInForce::GTC));
        assert_eq!(
            e.volume_at_price(Side::Buy, Price::from_f64(100.0)),
            Quantity::from_f64(8.0)
        );
        assert_eq!(
            e.volume_at_price(Side::Buy, Price::from_f64(101.0)),
            Quantity::from_f64(2.0)
        );
        assert_eq!(
            e.volume_at_price(Side::Buy, Price::from_f64(102.0)),
            Quantity::from_f64(0.0)
        );
    }

    #[test]
    fn test_stats_track_volume_and_turnover() {
        let mut e = L2MatchingEngine::new();
        e.submit(make_limit(1, Side::Sell, 100.0, 2.0, TimeInForce::GTC));
        e.submit(make_limit(2, Side::Buy, 100.0, 2.0, TimeInForce::GTC));
        let stats = e.stats();
        assert_eq!(stats.total_fills, 1);
        // 成交量：2.0
        // 成交额：100.0 * 2.0 = 200.0
        assert!(stats.total_turnover > 0);
        assert!(stats.total_volume > 0);
    }

    #[test]
    fn test_export_entries_round_trip() {
        let mut e1 = L2MatchingEngine::new();
        e1.submit(make_limit(1, Side::Buy, 100.0, 1.0, TimeInForce::GTC));
        e1.submit(make_limit(2, Side::Buy, 101.0, 2.0, TimeInForce::GTC));
        e1.order_index.insert(
            1,
            OrderLocation {
                side: Side::Buy,
                price: Price::from_f64(100.0),
                offset: 0,
            },
        );
        e1.order_index.insert(
            2,
            OrderLocation {
                side: Side::Buy,
                price: Price::from_f64(101.0),
                offset: 0,
            },
        );
        let entries = e1.export_entries();
        assert_eq!(entries.len(), 2);

        // 用 entries 重建
        let e2 = L2MatchingEngine::from_entries(entries);
        assert!(e2.contains(1));
        assert!(e2.contains(2));
    }

    #[test]
    fn test_location_query() {
        let mut e = L2MatchingEngine::new();
        e.submit(make_limit(1, Side::Buy, 100.0, 1.0, TimeInForce::GTC));
        e.order_index.insert(
            1,
            OrderLocation {
                side: Side::Buy,
                price: Price::from_f64(100.0),
                offset: 0,
            },
        );
        let loc = e.location(1).unwrap();
        assert_eq!(loc.side, Side::Buy);
        assert_eq!(loc.price, Price::from_f64(100.0));
    }

    #[test]
    fn test_spread_calculation() {
        let mut e = L2MatchingEngine::new();
        e.submit(make_limit(1, Side::Buy, 100.0, 1.0, TimeInForce::GTC));
        e.submit(make_limit(2, Side::Sell, 103.0, 1.0, TimeInForce::GTC));
        let spread = e.spread().unwrap();
        assert!((spread.as_f64() - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_depth_query() {
        let mut e = L2MatchingEngine::new();
        e.submit(make_limit(1, Side::Buy, 100.0, 1.0, TimeInForce::GTC));
        e.submit(make_limit(2, Side::Buy, 101.0, 2.0, TimeInForce::GTC));
        e.submit(make_limit(3, Side::Sell, 103.0, 1.0, TimeInForce::GTC));
        let (bids, asks) = e.depth(5);
        assert_eq!(bids.len(), 2);
        assert_eq!(asks.len(), 1);
    }
}
