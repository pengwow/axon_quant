//! [`ImpactedMatchingEngine`]：在 `L1MatchingEngine` 之上叠加市场冲击
//!
//! 完整流程：
//! 1. 接收订单 → 内部 `L1MatchingEngine.submit()` 产生**裸成交**
//! 2. 从内部订单簿（含永久冲击偏移）生成 [`OrderBookSnapshot`]
//! 3. 调用 [`ImpactModel::compute_impact`] 计算冲击
//! 4. 即时冲击叠加到每笔 `MatchFill.price`
//! 5. 永久冲击累加到 `permanent_offset`（影响后续订单簿中间价）
//!
//! # 性能
//!
//! - 每次 `submit` 增加 1 次 `compute_impact` 调用 + `O(N_fills)` 价格调整
//! - `compute_impact` 是 O(depth_levels) — 通常 10~20 层
//! - 价格调整是 O(1) per fill
//!
//! # 线程安全
//!
//! 与 `L1MatchingEngine` 一致：非线程安全（持有 `&mut self`）

use std::collections::HashMap;

use axon_core::impact::{ImpactModel, ImpactModelConfig};
use axon_core::market::{OrderBookLevel, OrderBookSnapshot, Side};
use axon_core::order::Order;
use axon_core::time::Timestamp;
use axon_core::types::{Instrument, Price, Quantity};

use crate::matching::engine::{L1MatchingEngine, MatchingEngine};
use crate::matching::types::SubmitResult;

/// 冲击统计
///
/// 跟踪累计的瞬时冲击、永久冲击、订单数等指标，便于回测后分析。
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ImpactStats {
    /// 累计瞬时冲击（绝对值，price 单位）
    pub cumulative_instantaneous: f64,
    /// 累计永久冲击（绝对值，price 单位）
    pub cumulative_permanent: f64,
    /// 已撮合订单数
    pub submitted_orders: u64,
    /// 已发生成交的订单数
    pub filled_orders: u64,
    /// 累计成交笔数
    pub total_fills: u64,
}

impl ImpactStats {
    /// 累计总冲击
    #[inline]
    pub fn cumulative_total(&self) -> f64 {
        self.cumulative_instantaneous + self.cumulative_permanent
    }
}

/// 冲击感知撮合引擎
///
/// 包装 `L1MatchingEngine` 并在撮合时应用市场冲击：
/// - **即时冲击**叠加到成交价
/// - **永久冲击**累加到内部状态，影响后续订单簿
pub struct ImpactedMatchingEngine {
    /// 内部撮合引擎
    inner: L1MatchingEngine,
    /// 市场冲击模型
    model: Box<dyn ImpactModel>,
    /// 累计永久冲击偏移（绝对价，叠加到中间价）
    permanent_offset: f64,
    /// 每次撮合后永久冲击的衰减率（0.0~1.0）
    /// - 0.0 = 不衰减（永久）
    /// - 0.1 = 每笔衰减 10%
    /// - None = 不衰减
    permanent_decay: Option<f64>,
    /// 统计信息
    stats: ImpactStats,
}

impl std::fmt::Debug for ImpactedMatchingEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImpactedMatchingEngine")
            .field("model", &self.model.name())
            .field("permanent_offset", &self.permanent_offset)
            .field("permanent_decay", &self.permanent_decay)
            .field("stats", &self.stats)
            .finish()
    }
}

impl ImpactedMatchingEngine {
    /// 创建冲击感知撮合引擎
    ///
    /// # 参数
    ///
    /// - `model`：市场冲击模型
    pub fn new(model: Box<dyn ImpactModel>) -> Self {
        Self {
            inner: L1MatchingEngine::new(),
            model,
            permanent_offset: 0.0,
            permanent_decay: None,
            stats: ImpactStats::default(),
        }
    }

    /// 从配置创建模型并构造引擎（便捷方法）
    pub fn from_config(config: ImpactModelConfig) -> Self {
        Self::new(axon_core::impact::create_model(config))
    }

    /// 设置永久冲击衰减率（每次撮合后）
    ///
    /// 取值范围 `[0.0, 1.0]`：
    /// - `0.0` ⇒ 不衰减（默认）
    /// - `0.1` ⇒ 每笔衰减 10%
    pub fn with_permanent_decay(mut self, decay: f64) -> Self {
        assert!(
            (0.0..=1.0).contains(&decay),
            "permanent_decay 必须在 [0, 1] 范围"
        );
        self.permanent_decay = Some(decay);
        self
    }

    /// 替换冲击模型
    pub fn set_model(&mut self, model: Box<dyn ImpactModel>) {
        self.model = model;
    }

    /// 获取当前冲击模型名称
    #[inline]
    pub fn model_name(&self) -> &str {
        self.model.name()
    }

    /// 获取当前累计永久冲击偏移
    #[inline]
    pub fn permanent_offset(&self) -> f64 {
        self.permanent_offset
    }

    /// 获取永久冲击衰减率
    #[inline]
    pub fn permanent_decay(&self) -> Option<f64> {
        self.permanent_decay
    }

    /// 获取统计信息
    #[inline]
    pub fn stats(&self) -> &ImpactStats {
        &self.stats
    }

    /// 重置永久冲击偏移与统计（订单簿保留）
    pub fn reset_impact_state(&mut self) {
        self.permanent_offset = 0.0;
        self.stats = ImpactStats::default();
    }

    /// 获取内部 L1 引擎的不可变引用
    #[inline]
    pub fn inner(&self) -> &L1MatchingEngine {
        &self.inner
    }

    /// 获取内部 L1 引擎的可变引用
    ///
    /// 警告：直接操作内部引擎可能破坏冲击状态一致性。
    #[inline]
    pub fn inner_mut(&mut self) -> &mut L1MatchingEngine {
        &mut self.inner
    }

    /// 在内部订单簿两侧播种虚拟流动性
    ///
    /// 这是回测辅助接口：让策略单在没有外部对手盘时仍能成交。
    /// 详见 [`L1MatchingEngine::seed_liquidity`] 的语义说明。
    ///
    /// # 参数
    ///
    /// - `mid_price`：中间价（通常为当前 bar close）
    /// - `half_spread`：每层价差（绝对价格单位）
    /// - `depth_levels`：每侧挂单层数
    /// - `size_per_level`：每层挂单数量
    /// - `instrument`：交易品种 (T2.3 改: 原 `symbol`),
    ///   用于从 `Instrument::base()` / `quote()` 派生 Order 的 base/quote
    /// - `next_id`：下一个可用订单 id（避免与外部订单 id 冲突）
    ///
    /// # 返回
    ///
    /// 更新后的 id 计数器（传给下一次 seed 调用）
    pub fn seed_liquidity(
        &mut self,
        mid_price: f64,
        half_spread: f64,
        depth_levels: usize,
        size_per_level: f64,
        instrument: Instrument, // 改: 原 symbol: Symbol (T2.3)
        next_id: u64,
    ) -> u64 {
        self.inner.seed_liquidity(
            mid_price,
            half_spread,
            depth_levels,
            size_per_level,
            instrument, // 改: 原 symbol (T2.3)
            next_id,
        )
    }

    /// 清空内部订单簿两侧（透传到 `L1MatchingEngine::clear_book`）
    ///
    /// 回测场景下，每根 bar 由应用层先 `clear_book()` 再 `seed_liquidity()`，
    /// 避免种子单跨 bar 累积撑爆 BTreeMap。**不**清空 `permanent_offset` 和
    /// `stats` —— 永久冲击跨 bar 持续累计。
    ///
    /// ponytail: 关键内存语义。
    /// - 透传到 `L1MatchingEngine::clear_book`,该函数已修复 `order_index`
    ///   替换为新 `HashMap` 实例以强制 deallocate,避免单调 `next_id`
    ///   扩容后 `HashMap::clear()` 不缩容导致的 raw table 累积。
    /// - 叠加 PyO3 端 `PyImpactModelAdapter` 持 `Arc<Py<PyAny>>` + 多次回测
    ///   引擎实例创建/丢弃,GB 级内存泄漏的真正根因在底层,这里只是确保
    ///   `L1MatchingEngine` 状态真正释放。
    pub fn clear_book(&mut self) {
        self.inner.clear_book();
    }

    /// 提交订单，应用市场冲击
    pub fn submit(&mut self, order: Order) -> SubmitResult {
        // 1. 在内部撮合前获取订单簿快照（用于冲击计算）
        //    撮合后对手方深度已被吃掉，再取快照会得到空簿，
        //    进而导致 `compute_impact` 返回零冲击。
        let pre_snapshot = self.snapshot_with_offset(Timestamp::from_nanos(0));

        // 2. 内部撮合（裸成交）
        let mut result = self.inner.submit(order);

        // 3. 仅当订单有成交时才计算并应用冲击
        if !result.fills.is_empty() {
            // 3a. 累计本次所有 fill 的成交量，按 taker 方向计算
            let filled_qty: f64 = result.fills.iter().map(|f| f.quantity.as_f64()).sum();
            if filled_qty > 0.0 {
                // 用 taker 方向（取首笔 fill 的 taker_side，所有 fill 同向）
                let side = result.fills[0].taker_side;
                let impact =
                    self.model
                        .compute_impact(Quantity::from_f64(filled_qty), side, &pre_snapshot);

                // 3b. 调整每笔成交价（叠加即时冲击）
                if impact.instantaneous != 0.0 {
                    for fill in &mut result.fills {
                        let adjusted = price_with_impact(
                            fill.price.as_f64(),
                            fill.taker_side,
                            impact.instantaneous,
                        );
                        fill.price = Price::from_f64(adjusted);
                    }
                }

                // 3c. 累计永久冲击
                if impact.permanent != 0.0 {
                    self.permanent_offset = decay_permanent_offset(
                        self.permanent_offset,
                        impact.permanent,
                        self.permanent_decay.unwrap_or(0.0),
                    );
                }

                // 3d. 更新统计
                self.stats.cumulative_instantaneous += impact.instantaneous.abs() * filled_qty;
                self.stats.cumulative_permanent += impact.permanent.abs() * filled_qty;
            }
        }

        // 4. 更新订单级别统计
        self.stats.submitted_orders += 1;
        if !result.fills.is_empty() {
            self.stats.filled_orders += 1;
            self.stats.total_fills += result.fills.len() as u64;
        }

        result
    }

    /// 从内部订单簿生成 [`OrderBookSnapshot`]，并叠加永久冲击偏移
    ///
    /// 永久冲击以**整体平移**的形式叠加到所有价格上：
    /// - bid 价格下移 `offset`（卖压）
    /// - ask 价格下移 `offset`（整体价格水平下移）
    ///
    /// # 实现说明
    ///
    /// 由于 `L1MatchingEngine` 不直接暴露订单簿结构，我们通过 `depth()`
    /// 提取价格级别，然后：
    /// - 买价：减去 `permanent_offset`（卖方永久冲击下移）
    /// - 卖价：减去 `permanent_offset`（永久冲击导致价格中枢下移）
    pub fn snapshot_with_offset(&self, timestamp: Timestamp) -> OrderBookSnapshot {
        let (bids, asks) = self.inner.depth(20);
        let mut snapshot_bids: Vec<OrderBookLevel> = bids
            .iter()
            .map(|l| {
                OrderBookLevel::new(
                    Price::from_f64(l.price.as_f64() - self.permanent_offset),
                    l.quantity,
                )
            })
            .collect();
        let mut snapshot_asks: Vec<OrderBookLevel> = asks
            .iter()
            .map(|l| {
                OrderBookLevel::new(
                    Price::from_f64(l.price.as_f64() - self.permanent_offset),
                    l.quantity,
                )
            })
            .collect();

        // 重新排序 + 过滤（与 OrderBookSnapshot::from_l2 一致）
        snapshot_bids.sort_by(|a, b| {
            b.price
                .as_f64()
                .partial_cmp(&a.price.as_f64())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        snapshot_asks.sort_by(|a, b| {
            a.price
                .as_f64()
                .partial_cmp(&b.price.as_f64())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        snapshot_bids.retain(|l| l.quantity.as_f64() > 0.0);
        snapshot_asks.retain(|l| l.quantity.as_f64() > 0.0);

        OrderBookSnapshot {
            timestamp,
            bids: snapshot_bids,
            asks: snapshot_asks,
        }
    }

    /// 取消订单
    pub fn cancel(&mut self, order_id: u64) -> bool {
        self.inner.cancel(order_id)
    }

    /// 最优买价（已应用永久冲击偏移）
    #[inline]
    pub fn best_bid(&self) -> Option<Price> {
        self.inner
            .best_bid()
            .map(|p| Price::from_f64(p.as_f64() - self.permanent_offset))
    }

    /// 最优卖价（已应用永久冲击偏移）
    #[inline]
    pub fn best_ask(&self) -> Option<Price> {
        self.inner
            .best_ask()
            .map(|p| Price::from_f64(p.as_f64() - self.permanent_offset))
    }

    /// 中间价（已应用永久冲击偏移）
    pub fn mid_price(&self) -> Option<Price> {
        match (self.best_bid(), self.best_ask()) {
            (Some(b), Some(a)) => Some(Price::from_f64((b.as_f64() + a.as_f64()) / 2.0)),
            _ => None,
        }
    }

    /// 活跃订单数
    #[inline]
    pub fn active_order_count(&self) -> usize {
        self.inner.active_order_count()
    }
}

/// 应用即时冲击调整价格
///
/// # 公式
///
/// - Buy 方向：`adjusted = base + instantaneous`
/// - Sell 方向：`adjusted = base - instantaneous`
///
/// 这与 `Impact::adjusted_price()` 一致：买方冲击抬高价格，卖方冲击压低价格。
#[inline]
pub fn price_with_impact(base_price: f64, side: Side, instantaneous: f64) -> f64 {
    match side {
        Side::Buy => base_price + instantaneous,
        Side::Sell => base_price - instantaneous,
    }
}

/// 应用永久冲击衰减
///
/// # 公式
///
/// ```text
/// new_offset = old_offset × (1 - decay) + permanent
/// ```
///
/// - `decay = 0.0` ⇒ 永久冲击不衰减
/// - `decay = 1.0` ⇒ 完全衰减（仅保留本次的 permanent）
/// - `decay` 超出 `[0, 1]` ⇒ 截断到边界
#[inline]
pub fn decay_permanent_offset(old_offset: f64, permanent: f64, decay: f64) -> f64 {
    let d = decay.clamp(0.0, 1.0);
    old_offset * (1.0 - d) + permanent
}

/// 从撮合引擎的 `(Vec<OrderBookLevel>, Vec<OrderBookLevel>)` 构造 `OrderBookSnapshot`
///
/// 这是一个通用辅助函数：给定 bids / asks（已排序），生成符合
/// `ImpactModel::compute_impact` 输入的快照。
///
/// # 参数
///
/// - `bids`：买盘（应按价格降序）
/// - `asks`：卖盘（应按价格升序）
/// - `timestamp`：快照时间戳
pub fn build_snapshot_from_levels(
    bids: &[crate::matching::types::OrderBookLevel],
    asks: &[crate::matching::types::OrderBookLevel],
    timestamp: Timestamp,
) -> OrderBookSnapshot {
    let snapshot_bids: Vec<OrderBookLevel> = bids
        .iter()
        .map(|l| OrderBookLevel::new(l.price, l.quantity))
        .collect();
    let snapshot_asks: Vec<OrderBookLevel> = asks
        .iter()
        .map(|l| OrderBookLevel::new(l.price, l.quantity))
        .collect();
    OrderBookSnapshot {
        timestamp,
        bids: snapshot_bids,
        asks: snapshot_asks,
    }
}

// `L1MatchingEngine` 暴露的 `depth()` 返回 `Vec<OrderBookLevel>`，
// 这里我们重新导出以方便模块外使用。
pub use crate::matching::types::OrderBookLevel as MatchingLevel;

/// 为 `L1MatchingEngine` 准备一个 `OrderBookSnapshot` 的便捷方法
///
/// 这是一个 trait 扩展，便于其它模块在 `L1MatchingEngine` 上调用。
pub trait ToOrderBookSnapshot {
    /// 生成订单簿快照
    fn to_snapshot(&self, timestamp: Timestamp) -> OrderBookSnapshot;
}

impl ToOrderBookSnapshot for L1MatchingEngine {
    fn to_snapshot(&self, timestamp: Timestamp) -> OrderBookSnapshot {
        let (bids, asks) = self.depth(20);
        let snapshot_bids: Vec<OrderBookLevel> = bids
            .iter()
            .map(|l| OrderBookLevel::new(l.price, l.quantity))
            .collect();
        let snapshot_asks: Vec<OrderBookLevel> = asks
            .iter()
            .map(|l| OrderBookLevel::new(l.price, l.quantity))
            .collect();
        OrderBookSnapshot {
            timestamp,
            bids: snapshot_bids,
            asks: snapshot_asks,
        }
    }
}

// 抑制 HashMap 未使用导入的警告（保留用于未来扩展，例如 per-symbol 永久冲击）
#[allow(dead_code)]
fn _assert_send_sync<T: Send + Sync>() {}
#[allow(dead_code)]
fn _hashmap_anchor() -> HashMap<String, f64> {
    HashMap::new()
}

// ════════════════════════════════════════════════════════════════════════════
// Phase 3.1.2: ImpactedMatchingEngine impl MatchingEngine
// ════════════════════════════════════════════════════════════════════════════
//
// ImpactedMatchingEngine 包装 L1 + 冲击模型,所有 trait 方法透传到 inherent
// 实现。注意 `best_bid` / `best_ask` 已叠加永久冲击偏移 — 与 L1 引擎不一致
// (L1 报裸价,Impacted 报偏移后价格)。这是"按 instrument 路由 + 冲击感知
// 视图"的撮合引擎,适合做策略回测。
//
// `seed_liquidity` 透传到 inherent 方法(inherent 走 inner.seed_liquidity)。
// `clear_book_for(instrument)` 透传到 inner L1(inherent 没有,这里直接调
// inner)。
//
// 注:所有 trait 方法用 UFCS `Self::method(self, args)` 显式调 inherent,
// 避免依赖 method resolution 隐式优先 inherent 的行为 — 这是隐性依赖,
// 未来如果 inherent 重构(比如改成调 trait submit)会立刻无限递归。
// 显式 UFCS 让编译器在 inherent 签名不匹配时报错,而不是运行时爆栈。
impl MatchingEngine for ImpactedMatchingEngine {
    fn submit(&mut self, order: Order) -> SubmitResult {
        // 显式 UFCS:调 inherent ImpactedMatchingEngine::submit
        // (内部会计算 impact + 转发 self.inner.submit)
        Self::submit(self, order)
    }

    fn cancel(&mut self, order_id: u64) -> bool {
        // 显式 UFCS:调 inherent ImpactedMatchingEngine::cancel
        Self::cancel(self, order_id)
    }

    fn best_bid(&self) -> Option<Price> {
        // 注意:Impacted 版本已叠加永久冲击偏移,不是裸 L1 价
        // 显式 UFCS:调 inherent ImpactedMatchingEngine::best_bid
        Self::best_bid(self)
    }

    fn best_ask(&self) -> Option<Price> {
        // 显式 UFCS:调 inherent ImpactedMatchingEngine::best_ask
        Self::best_ask(self)
    }

    /// spread 用 inherent 实现(也用偏移后价)
    fn spread(&self) -> Option<Price> {
        match (Self::best_bid(self), Self::best_ask(self)) {
            (Some(b), Some(a)) => Some(Price::from_f64(a.as_f64() - b.as_f64())),
            _ => None,
        }
    }

    fn depth(
        &self,
        levels: usize,
    ) -> (
        Vec<crate::matching::types::OrderBookLevel>,
        Vec<crate::matching::types::OrderBookLevel>,
    ) {
        // 从 inner L1 拿裸 depth,再叠加 permanent_offset(与 snapshot_with_offset 语义一致)
        let (bids, asks) = self.inner.depth(levels);
        let offset = self.permanent_offset;
        let bids: Vec<crate::matching::types::OrderBookLevel> = bids
            .iter()
            .map(|l| {
                crate::matching::types::OrderBookLevel::new(
                    Price::from_f64(l.price.as_f64() - offset),
                    l.quantity,
                    l.order_count,
                )
            })
            .collect();
        let asks: Vec<crate::matching::types::OrderBookLevel> = asks
            .iter()
            .map(|l| {
                crate::matching::types::OrderBookLevel::new(
                    Price::from_f64(l.price.as_f64() - offset),
                    l.quantity,
                    l.order_count,
                )
            })
            .collect();
        (bids, asks)
    }

    fn active_order_count(&self) -> usize {
        // 显式 UFCS:调 inherent ImpactedMatchingEngine::active_order_count
        Self::active_order_count(self)
    }

    fn clear_book(&mut self) {
        // 不清 permanent_offset / stats — 跨 bar 累计
        // 直接调 inner L1(inherent clear_book 也走这里,但 trait 这里更直接,
        // 避免 1 层 inherent 转发)
        self.inner.clear_book();
    }

    fn clear_book_for(&mut self, instrument: &Instrument) {
        // Impacted 自己的 inherent 没有 clear_book_for,直接调 inner
        self.inner.clear_book_for(instrument);
    }

    /// 透传到 inherent `seed_liquidity`(inherent 内部走 inner L1)
    fn seed_liquidity(
        &mut self,
        mid_price: f64,
        half_spread: f64,
        depth_levels: usize,
        size_per_level: f64,
        instrument: Instrument,
        next_id: u64,
    ) -> u64 {
        // 显式 UFCS:调 inherent ImpactedMatchingEngine::seed_liquidity
        Self::seed_liquidity(
            self,
            mid_price,
            half_spread,
            depth_levels,
            size_per_level,
            instrument,
            next_id,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::impact::{LinearImpactModel, PowerLawImpactModel};
    use axon_core::market::Side;
    use axon_core::order::{Order, OrderType, TimeInForce};
    use axon_core::types::Quantity;
    use pretty_assertions::assert_eq;

    fn make_limit(id: u64, side: Side, price: f64, qty: f64) -> Order {
        // T2.2: 用 Order::spot 替代 Order::new
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

    // ─── 构造与基础属性 ─────────────────────────────────

    #[test]
    fn test_new_constructs_with_default_state() {
        let m: Box<dyn ImpactModel> = Box::new(LinearImpactModel::default());
        let engine = ImpactedMatchingEngine::new(m);
        assert_eq!(engine.permanent_offset(), 0.0);
        assert_eq!(engine.stats().submitted_orders, 0);
        assert_eq!(engine.stats().cumulative_instantaneous, 0.0);
        assert_eq!(engine.stats().cumulative_permanent, 0.0);
    }

    #[test]
    fn test_from_config_uses_factory() {
        let engine = ImpactedMatchingEngine::from_config(ImpactModelConfig::Linear {
            coefficient: 0.05,
            depth_levels: 5,
            instantaneous_ratio: 0.7,
        });
        assert_eq!(engine.model_name(), "LinearImpact");
    }

    #[test]
    fn test_with_permanent_decay_stores_value() {
        let m: Box<dyn ImpactModel> = Box::new(LinearImpactModel::default());
        let engine = ImpactedMatchingEngine::new(m).with_permanent_decay(0.1);
        assert_eq!(engine.permanent_decay(), Some(0.1));
    }

    #[test]
    #[should_panic(expected = "permanent_decay 必须在 [0, 1] 范围")]
    fn test_with_permanent_decay_rejects_out_of_range() {
        let m: Box<dyn ImpactModel> = Box::new(LinearImpactModel::default());
        let _ = ImpactedMatchingEngine::new(m).with_permanent_decay(1.5);
    }

    // ─── 撮合无冲击场景（零 coefficient）──────────────

    #[test]
    fn test_zero_coefficient_preserves_fill_prices() {
        let m: Box<dyn ImpactModel> = Box::new(LinearImpactModel::new(0.0));
        let mut engine = ImpactedMatchingEngine::new(m);
        engine.submit(make_limit(1, Side::Sell, 100.0, 1.0));
        let result = engine.submit(make_limit(2, Side::Buy, 100.0, 1.0));
        assert_eq!(result.fills.len(), 1);
        // 零冲击 ⇒ 价格不变
        assert_eq!(result.fills[0].price, Price::from_f64(100.0));
        assert_eq!(engine.permanent_offset(), 0.0);
    }

    // ─── 撮合带冲击场景 ───────────────────────────────

    #[test]
    fn test_buy_order_instantaneous_impact_raises_price() {
        // 线性冲击: coefficient=0.05, depth=1, qty=1 ⇒ 0.05 total
        // 70% instantaneous ⇒ 0.035
        let m: Box<dyn ImpactModel> = Box::new(LinearImpactModel::new(0.05));
        let mut engine = ImpactedMatchingEngine::new(m);
        // 单个卖单 100，深度 1
        engine.submit(make_limit(1, Side::Sell, 100.0, 1.0));

        // 大买单 5.0 吃 1.0 @ 100
        let buy = make_limit(2, Side::Buy, 100.0, 5.0);
        let result = engine.submit(buy);
        // 只有 1 个 fill，因为卖单只有 1.0
        assert_eq!(result.fills.len(), 1);
        let expected = 100.0 + 0.035; // 0.05 × (1/1) × 0.7
        assert!((result.fills[0].price.as_f64() - expected).abs() < 1e-9);
    }

    #[test]
    fn test_sell_order_instantaneous_impact_lowers_price() {
        let m: Box<dyn ImpactModel> = Box::new(LinearImpactModel::new(0.05));
        let mut engine = ImpactedMatchingEngine::new(m);
        engine.submit(make_limit(1, Side::Buy, 100.0, 1.0));

        // 大卖单 5.0 吃 1.0 @ 100
        let sell = make_limit(2, Side::Sell, 100.0, 5.0);
        let result = engine.submit(sell);
        assert_eq!(result.fills.len(), 1);
        let expected = 100.0 - 0.035;
        assert!((result.fills[0].price.as_f64() - expected).abs() < 1e-9);
    }

    #[test]
    fn test_permanent_impact_accumulates() {
        // coefficient=0.05, ratio=0.0 ⇒ 全部永久冲击
        // qty=1, depth=1 ⇒ 0.05 permanent
        let m: Box<dyn ImpactModel> =
            Box::new(LinearImpactModel::new(0.05).with_instantaneous_ratio(0.0));
        let mut engine = ImpactedMatchingEngine::new(m);
        engine.submit(make_limit(1, Side::Sell, 100.0, 1.0));

        let buy = make_limit(2, Side::Buy, 100.0, 1.0);
        engine.submit(buy);
        // 0.05 × 1.0/1.0 = 0.05
        assert!((engine.permanent_offset() - 0.05).abs() < 1e-9);
    }

    #[test]
    fn test_cumulative_stats_track_impact() {
        let m: Box<dyn ImpactModel> = Box::new(LinearImpactModel::default());
        let mut engine = ImpactedMatchingEngine::new(m);
        engine.submit(make_limit(1, Side::Sell, 100.0, 1.0));
        let buy = make_limit(2, Side::Buy, 100.0, 1.0);
        engine.submit(buy);
        let stats = engine.stats();
        assert_eq!(stats.submitted_orders, 2);
        assert_eq!(stats.filled_orders, 1);
        assert_eq!(stats.total_fills, 1);
        assert!(stats.cumulative_instantaneous > 0.0);
    }

    // ─── 永久冲击影响后续订单簿 ─────────────────────

    #[test]
    fn test_permanent_offset_shifts_best_bid_ask() {
        let m: Box<dyn ImpactModel> =
            Box::new(LinearImpactModel::new(0.1).with_instantaneous_ratio(0.0));
        let mut engine = ImpactedMatchingEngine::new(m);
        engine.submit(make_limit(1, Side::Sell, 100.0, 10.0));
        // 初始 best_ask = 100
        assert_eq!(engine.best_ask(), Some(Price::from_f64(100.0)));

        // 大买单 1.0 吃 100 价 ⇒ permanent = 0.1 × 1/10 = 0.01
        let buy = make_limit(2, Side::Buy, 100.0, 1.0);
        engine.submit(buy);
        // 永久冲击下移 best_ask：100 - 0.01 = 99.99
        let ask_after = engine.best_ask().unwrap();
        assert!((ask_after.as_f64() - 99.99).abs() < 1e-6);
    }

    #[test]
    fn test_snapshot_with_offset_includes_permanent_impact() {
        let m: Box<dyn ImpactModel> =
            Box::new(LinearImpactModel::new(0.1).with_instantaneous_ratio(0.0));
        let mut engine = ImpactedMatchingEngine::new(m);
        engine.submit(make_limit(1, Side::Sell, 100.0, 10.0));
        let buy = make_limit(2, Side::Buy, 100.0, 1.0);
        engine.submit(buy);

        let snap = engine.snapshot_with_offset(Timestamp::from_nanos(0));
        // 永久冲击下移
        assert!(!snap.asks.is_empty());
        let top_ask = snap.asks[0].price.as_f64();
        assert!(top_ask < 100.0);
    }

    // ─── 永久冲击衰减 ───────────────────────────────

    #[test]
    fn test_decay_reduces_offset_over_time() {
        let m: Box<dyn ImpactModel> =
            Box::new(LinearImpactModel::new(0.1).with_instantaneous_ratio(0.0));
        let mut engine = ImpactedMatchingEngine::new(m).with_permanent_decay(0.5);
        // 两个卖单以支持两次撮合
        engine.submit(make_limit(1, Side::Sell, 100.0, 10.0));
        engine.submit(make_limit(2, Side::Sell, 100.0, 10.0));

        // 第 1 笔：offset = 0 * 0.5 + 0.01 = 0.01
        let buy1 = make_limit(10, Side::Buy, 100.0, 1.0);
        engine.submit(buy1);
        let offset1 = engine.permanent_offset();

        // 第 2 笔：offset = 0.01 * 0.5 + 0.01 = 0.015
        let buy2 = make_limit(11, Side::Buy, 100.0, 1.0);
        engine.submit(buy2);
        let offset2 = engine.permanent_offset();

        // offset2 = 0.015 < offset1 + 0.01 = 0.02（因衰减）
        assert!(offset2 < offset1 + 0.01);
    }

    #[test]
    fn test_full_decay_keeps_only_current_permanent() {
        let m: Box<dyn ImpactModel> =
            Box::new(LinearImpactModel::new(0.1).with_instantaneous_ratio(0.0));
        let mut engine = ImpactedMatchingEngine::new(m).with_permanent_decay(1.0);

        // 第 1 笔撮合：准备 1 个 sell qty=1.0，buy 1.0 全部吃光
        //   permanent = 0.1 × 1.0/1.0 = 0.1
        //   offset = 0 * 0 + 0.1 = 0.1
        engine.submit(make_limit(1, Side::Sell, 100.0, 1.0));
        let buy1 = make_limit(10, Side::Buy, 100.0, 1.0);
        engine.submit(buy1);
        let offset1 = engine.permanent_offset();

        // 第 2 笔撮合：重新准备 1 个 sell qty=1.0
        //   此时 `permanent_offset=0.1` 已使最优卖价偏移为 99.9，
        //   buy 仍按 100 下单，因此买不到（不在 ask 价），先取消原残留
        //   ⇒ 改用 market 风格的 buy（价格高于 ask）以确保成交
        engine.submit(make_limit(2, Side::Sell, 100.0, 1.0));
        let buy2 = make_limit(11, Side::Buy, 100.0, 1.0);
        engine.submit(buy2);
        let offset2 = engine.permanent_offset();
        // 完全衰减 ⇒ 旧 offset 归零，只保留本次 permanent ⇒ offset2 ≈ offset1
        assert!((offset2 - offset1).abs() < 1e-9);
    }

    // ─── 辅助函数测试 ──────────────────────────────

    #[test]
    fn test_price_with_impact_buy_raises() {
        let p = price_with_impact(100.0, Side::Buy, 0.5);
        assert!((p - 100.5).abs() < 1e-9);
    }

    #[test]
    fn test_price_with_impact_sell_lowers() {
        let p = price_with_impact(100.0, Side::Sell, 0.5);
        assert!((p - 99.5).abs() < 1e-9);
    }

    #[test]
    fn test_price_with_impact_zero_instantaneous() {
        let p = price_with_impact(100.0, Side::Buy, 0.0);
        assert!((p - 100.0).abs() < 1e-9);
    }

    #[test]
    fn test_decay_permanent_offset_no_decay() {
        let new = decay_permanent_offset(1.0, 0.5, 0.0);
        assert!((new - 1.5).abs() < 1e-9);
    }

    #[test]
    fn test_decay_permanent_offset_full_decay() {
        let new = decay_permanent_offset(1.0, 0.5, 1.0);
        assert!((new - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_decay_permanent_offset_half_decay() {
        let new = decay_permanent_offset(1.0, 0.5, 0.5);
        assert!((new - 1.0).abs() < 1e-9); // 1.0 * 0.5 + 0.5
    }

    #[test]
    fn test_decay_permanent_offset_clamps_decay() {
        // 衰减率超出 [0, 1] 时应截断
        let new = decay_permanent_offset(1.0, 0.5, 2.0);
        // decay clamp to 1.0 ⇒ new = 0.5
        assert!((new - 0.5).abs() < 1e-9);
    }

    // ─── PowerLaw 冲击模型集成 ─────────────────────

    #[test]
    fn test_power_law_impact_integration() {
        let m: Box<dyn ImpactModel> = Box::new(PowerLawImpactModel::new(0.1, 0.5));
        let mut engine = ImpactedMatchingEngine::new(m);
        engine.submit(make_limit(1, Side::Sell, 100.0, 1.0));
        let result = engine.submit(make_limit(2, Side::Buy, 100.0, 1.0));
        assert_eq!(result.fills.len(), 1);
        // 即时冲击 > 0 ⇒ 价格 > 100
        assert!(result.fills[0].price.as_f64() > 100.0);
    }

    // ─── 状态重置 ──────────────────────────────────

    #[test]
    fn test_reset_impact_state_clears_offset_and_stats() {
        let m: Box<dyn ImpactModel> = Box::new(LinearImpactModel::default());
        let mut engine = ImpactedMatchingEngine::new(m);
        engine.submit(make_limit(1, Side::Sell, 100.0, 1.0));
        let buy = make_limit(2, Side::Buy, 100.0, 1.0);
        engine.submit(buy);
        assert!(engine.permanent_offset() > 0.0);
        assert!(engine.stats().submitted_orders > 0);

        engine.reset_impact_state();
        assert_eq!(engine.permanent_offset(), 0.0);
        assert_eq!(engine.stats().submitted_orders, 0);
    }

    // ─── 设置模型 ────────────────────────────────

    #[test]
    fn test_set_model_replaces_impact_model() {
        let m1: Box<dyn ImpactModel> = Box::new(LinearImpactModel::new(0.05));
        let mut engine = ImpactedMatchingEngine::new(m1);
        assert_eq!(engine.model_name(), "LinearImpact");

        let m2: Box<dyn ImpactModel> = Box::new(PowerLawImpactModel::new(0.1, 0.5));
        engine.set_model(m2);
        assert_eq!(engine.model_name(), "PowerLawImpact");
    }

    // ─── ToOrderBookSnapshot trait ────────────────

    #[test]
    fn test_l1_engine_to_snapshot() {
        let mut engine = L1MatchingEngine::new();
        engine.submit(make_limit(1, Side::Buy, 100.0, 1.0));
        engine.submit(make_limit(2, Side::Sell, 101.0, 1.0));
        let snap = engine.to_snapshot(Timestamp::from_nanos(0));
        assert_eq!(snap.bids.len(), 1);
        assert_eq!(snap.asks.len(), 1);
        assert_eq!(snap.bids[0].price, Price::from_f64(100.0));
        assert_eq!(snap.asks[0].price, Price::from_f64(101.0));
    }

    #[test]
    fn test_l1_engine_to_snapshot_empty() {
        let engine = L1MatchingEngine::new();
        let snap = engine.to_snapshot(Timestamp::from_nanos(0));
        assert!(snap.bids.is_empty());
        assert!(snap.asks.is_empty());
    }

    // ─── 集成测试：多次撮合累计冲击 ──────────────

    #[test]
    fn test_multiple_submissions_accumulate_impact() {
        let m: Box<dyn ImpactModel> = Box::new(LinearImpactModel::new(0.1));
        let mut engine = ImpactedMatchingEngine::new(m);
        // 准备 5 个卖单（每次撮合消耗一个）
        for i in 0..5 {
            engine.submit(make_limit(1 + i, Side::Sell, 100.0, 1.0));
        }

        for i in 0..5 {
            let buy = make_limit(100 + i, Side::Buy, 100.0, 1.0);
            engine.submit(buy);
        }
        let stats = engine.stats();
        assert_eq!(stats.submitted_orders, 10); // 5 sell + 5 buy
        assert_eq!(stats.filled_orders, 5);
        assert_eq!(stats.total_fills, 5);
        assert!(stats.cumulative_instantaneous > 0.0);
        assert!(stats.cumulative_permanent > 0.0);
    }

    // ─── 边界：空订单簿下单 ─────────────────────

    #[test]
    fn test_submit_on_empty_book_returns_no_fills() {
        let m: Box<dyn ImpactModel> = Box::new(LinearImpactModel::default());
        let mut engine = ImpactedMatchingEngine::new(m);
        let result = engine.submit(make_limit(1, Side::Buy, 100.0, 1.0));
        assert!(result.fills.is_empty());
        // 零深度 ⇒ 零冲击
        assert_eq!(engine.permanent_offset(), 0.0);
        assert_eq!(engine.stats().cumulative_instantaneous, 0.0);
    }

    // ─── 边界：FOK 全部成交或取消 ──────────────

    #[test]
    fn test_fok_full_fill_applies_impact() {
        let m: Box<dyn ImpactModel> = Box::new(LinearImpactModel::new(0.05));
        let mut engine = ImpactedMatchingEngine::new(m);
        engine.submit(make_limit(1, Side::Sell, 100.0, 1.0));

        // FOK 买单
        let fok = Order::spot(
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
        let result = engine.submit(fok);
        assert_eq!(result.fills.len(), 1);
        // 成交价应 > 100（即时冲击）
        assert!(result.fills[0].price.as_f64() > 100.0);
    }

    // ─── 取消订单 ────────────────────────────────

    #[test]
    fn test_cancel_delegates_to_inner() {
        let m: Box<dyn ImpactModel> = Box::new(LinearImpactModel::default());
        let mut engine = ImpactedMatchingEngine::new(m);
        engine.submit(make_limit(1, Side::Sell, 100.0, 1.0));
        assert!(engine.cancel(1));
        assert_eq!(engine.active_order_count(), 0);
    }

    #[test]
    fn test_cancel_nonexistent_returns_false() {
        let m: Box<dyn ImpactModel> = Box::new(LinearImpactModel::default());
        let mut engine = ImpactedMatchingEngine::new(m);
        assert!(!engine.cancel(999));
    }

    // ─── Mid price ──────────────────────────────

    #[test]
    fn test_mid_price_with_both_sides() {
        let m: Box<dyn ImpactModel> = Box::new(LinearImpactModel::default());
        let mut engine = ImpactedMatchingEngine::new(m);
        engine.submit(make_limit(1, Side::Buy, 100.0, 1.0));
        engine.submit(make_limit(2, Side::Sell, 102.0, 1.0));
        // 永久冲击为 0 ⇒ mid = 101
        assert_eq!(engine.mid_price(), Some(Price::from_f64(101.0)));
    }

    #[test]
    fn test_mid_price_empty_book_returns_none() {
        let m: Box<dyn ImpactModel> = Box::new(LinearImpactModel::default());
        let engine = ImpactedMatchingEngine::new(m);
        assert!(engine.mid_price().is_none());
    }

    // ─── Debug 输出 ────────────────────────────

    #[test]
    fn test_debug_output_contains_key_fields() {
        let m: Box<dyn ImpactModel> = Box::new(LinearImpactModel::new(0.05));
        let engine = ImpactedMatchingEngine::new(m);
        let s = format!("{engine:?}");
        assert!(s.contains("ImpactedMatchingEngine"));
        assert!(s.contains("LinearImpact"));
        assert!(s.contains("permanent_offset"));
    }

    // ─── 并发测试 ───────────────────────────────────

    /// ImpactedMatchingEngine 与 L1MatchingEngine 一致：非线程安全（持有 &mut self）
    /// 每个线程构造独立实例并独立运行
    #[test]
    fn test_concurrent_independent_engines() {
        use std::sync::Arc;
        use std::thread;

        const N_THREADS: usize = 20;
        const ORDERS_PER_THREAD: usize = 100;

        let collected: Arc<std::sync::Mutex<Vec<ImpactStats>>> =
            Arc::new(std::sync::Mutex::new(Vec::with_capacity(N_THREADS)));

        let mut handles = Vec::with_capacity(N_THREADS);
        for thread_id in 0..N_THREADS {
            let c = Arc::clone(&collected);
            handles.push(thread::spawn(move || {
                // 每个线程独立构造引擎
                let m: Box<dyn ImpactModel> =
                    Box::new(LinearImpactModel::new(0.01 * (thread_id + 1) as f64));
                let mut engine = ImpactedMatchingEngine::new(m);
                // 构造订单簿：每个线程买卖单交错
                for i in 0..ORDERS_PER_THREAD {
                    let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };
                    let order = make_limit(
                        (thread_id * ORDERS_PER_THREAD + i) as u64,
                        side,
                        100.0 + (i as f64) * 0.1,
                        1.0,
                    );
                    engine.submit(order);
                }
                let thread_stats = engine.stats().clone();
                c.lock().unwrap().push(thread_stats);
            }));
        }
        for h in handles {
            h.join().expect("thread panicked");
        }

        let all_stats = collected.lock().unwrap();
        assert_eq!(all_stats.len(), N_THREADS);
        for (i, s) in all_stats.iter().enumerate() {
            assert_eq!(
                s.submitted_orders, ORDERS_PER_THREAD as u64,
                "thread {i} 应处理 {ORDERS_PER_THREAD} 笔订单"
            );
        }
    }

    /// 多线程独立 engine + 复杂订单簿 + 永久冲击累积
    /// 验证每个线程的 engine 状态独立维护，不会相互影响
    #[test]
    fn test_concurrent_permanent_offset_independence() {
        use std::thread;

        const N_THREADS: usize = 10;
        const ORDERS_PER_THREAD: usize = 50;

        let mut handles = Vec::with_capacity(N_THREADS);
        for thread_id in 0..N_THREADS {
            handles.push(thread::spawn(move || {
                // 不同线程使用不同 impact coefficient
                let coeff = 0.001 * (thread_id + 1) as f64;
                let m: Box<dyn ImpactModel> = Box::new(LinearImpactModel::new(coeff));
                let mut engine = ImpactedMatchingEngine::new(m);

                // 先在订单簿中放置卖单（对手方）
                for i in 0..ORDERS_PER_THREAD {
                    let sell_id = (thread_id * ORDERS_PER_THREAD * 2 + i) as u64;
                    engine.submit(make_limit(sell_id, Side::Sell, 100.0, 1.0));
                }
                // 然后用大买单吃卖单 ⇒ 产生成交 + 永久冲击
                for i in 0..ORDERS_PER_THREAD {
                    let buy_id = (thread_id * ORDERS_PER_THREAD * 2 + ORDERS_PER_THREAD + i) as u64;
                    let order = make_limit(buy_id, Side::Buy, 100.0, 1.0);
                    engine.submit(order);
                }
                // 至少有成交 ⇒ 永久冲击 > 0
                assert!(
                    engine.permanent_offset().abs() > 0.0,
                    "thread {thread_id}: coeff={coeff} 下应有累积永久冲击"
                );
            }));
        }
        for h in handles {
            h.join().expect("thread panicked");
        }
    }

    /// 大量线程独立构造 ImpactedMatchingEngine + 立即丢弃
    /// （验证 Drop 实现的线程安全）
    #[test]
    fn test_concurrent_engine_lifecycle() {
        use std::thread;

        const N_THREADS: usize = 100;

        let mut handles = Vec::with_capacity(N_THREADS);
        for _ in 0..N_THREADS {
            handles.push(thread::spawn(|| {
                // 多次创建并立即丢弃引擎
                for _ in 0..10 {
                    let m: Box<dyn ImpactModel> = Box::new(LinearImpactModel::default());
                    let engine = ImpactedMatchingEngine::new(m);
                    drop(engine);
                }
            }));
        }
        for h in handles {
            h.join().expect("thread panicked");
        }
    }

    /// 静态断言：ImpactStats 是 Send + Sync（用于跨线程传递统计结果）
    #[test]
    fn test_impact_stats_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ImpactStats>();
    }
}
