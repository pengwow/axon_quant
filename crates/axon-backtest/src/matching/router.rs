//! 多撮合引擎路由(0.8.0 Phase 3.1 A2.1)
//!
//! # 动机
//!
//! 0.8.0 之前 `BacktestEngineConfig.matching_engine: Box<dyn MatchingEngine>`
//! 一次只能装一个引擎(L1 / L2 / L3 / Impacted 之一)。多 leg 套利场景下:
//! - spot leg 走 L1(简单价格-时间优先)
//! - perp leg 走 L3(支持暗池/拍卖)
//! - 另一 instrument 又想用 Impacted(带冲击模型)
//!
//! 都要在外层手写多 `BacktestEngine` 协同,语义割裂。`EngineRouter` 把
//! 多 engine 装到一个 `HashMap<Instrument, RoutedEngine>` + primary fallback
//! 的统一抽象里,对外仍是 `MatchingEngine` trait,可直接放进现有
//! `BacktestEngineConfig.matching_engine` 字段(`Box::new(EngineRouter::...)`),
//! 兼容所有现有调用方。
//!
//! # 设计要点
//!
//! - **enum dispatch,not `Box<dyn>`**:`RoutedEngine` 是 `enum`,每个 arm
//!   `match` 编译为跳转表(LLVM 通常能 inline),零虚表开销。L1/L2/L3/Impacted
//!   都是 Send + Sync,enum 自然满足。
//! - **per-instrument + primary fallback**:优先按 `order.instrument` 查
//!   `routes`,没注册时按 `RoutingStrategy` 走 primary 或直接 empty。
//! - **`MatchingEngine` trait 全实现**:`submit` / `cancel` / `best_bid` /
//!   `best_ask` / `spread` / `depth` / `active_order_count` / `clear_book` /
//!   `clear_book_for` / `seed_liquidity` 全部 dispatch,`BacktestEngine` 集成零摩擦。
//! - **聚合语义**:无 instrument 参数的 trait 方法(`best_bid` / `depth` /
//!   `active_order_count` / `clear_book`)跨所有 engine 聚合;有 instrument
//!   参数的(`clear_book_for` / `seed_liquidity`)按 instrument 路由。
//!
//! # 示例
//!
//! ```ignore
//! use axon_backtest::matching::{
//!     EngineRouter, L1MatchingEngine, L2MatchingEngine, MultiAssetMatchingEngine,
//!     RoutedEngine, RoutingStrategy,
//! };
//! use axon_core::types::{Instrument, SpotInstrument};
//!
//! // 场景:spot 走 L1,perp 走 L3 暗池,fallback 走 L2
//! let btc = Instrument::Spot(SpotInstrument { base: "BTC".into(), quote: "USDT".into() });
//! let btc_perp = /* ... */;
//!
//! let mut router = EngineRouter::new()
//!     .with_strategy(RoutingStrategy::PerInstrumentWithFallback)
//!     .with_primary(RoutedEngine::L2(L2MatchingEngine::new()));
//! router.register(btc, RoutedEngine::L1(L1MatchingEngine::new()));
//! router.register(btc_perp, RoutedEngine::L3(MultiAssetMatchingEngine::new()));
//!
//! // 用法同普通 engine
//! let result = router.submit(order);
//! ```
//!
//! # 限制 / 已知问题
//!
//! - **`Box<dyn>` 用户自定义 engine 不可用**:enum dispatch 只能路由到
//!   内置 4 种 engine。Python 端通过 `PyMatchingEngine` 自定义的 engine
//!   仍需走 `Box<dyn MatchingEngine>` 路径,直接放进
//!   `BacktestEngineConfig.matching_engine` 即可(无需 Router)。0.8.0 不
//!   解决,0.9.0 评估加 `RoutedEngine::User(Box<dyn MatchingEngine>)` arm。
//! - **`seed_liquidity` 不区分 primary/route**:只看 instrument,没注册
//!   时退到 primary 兜底;primary 也无则 no-op。同 `submit` 行为。
//! - **`depth` 聚合简化**:把多 engine 的 bids/asks 合并后排序取 top N,
//!   不维护跨 engine 价位冲突检测(回测场景下通常 single engine per
//!   instrument,价位不重叠)。

use std::collections::HashMap;

use axon_core::order::Order;
use axon_core::types::{Instrument, Price, Quantity};

use super::engine::MatchingEngine;
use super::l2::L2MatchingEngine;
use super::l3::engine_l3::MultiAssetMatchingEngine;
use super::types::{OrderBookLevel, SubmitResult};
use crate::impact::ImpactedMatchingEngine;
use crate::matching::L1MatchingEngine;

/// 路由后的具体撮合引擎种类
///
/// 用 enum 而不是 `Box<dyn MatchingEngine>`:
/// - 零 vtable 开销(LLVM 通常 inline)
/// - 无堆分配(直接 inline 存放在 `EngineRouter` 里)
/// - 编译期穷尽检查(新增 engine 变体时所有 match 都会被强制更新)
pub enum RoutedEngine {
    /// 基础价格-时间优先
    L1(L1MatchingEngine),
    /// L1 + O(1) 取消 + 统计
    L2(L2MatchingEngine),
    /// 多资产 + 暗池 + 拍卖 + 套利
    L3(MultiAssetMatchingEngine),
    /// L1 + 冲击模型(Linear / PowerLaw / 自适应)
    Impacted(ImpactedMatchingEngine),
}

/// 路由策略
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum RoutingStrategy {
    /// 严格按 `routes` 路由,未注册的 instrument → `SubmitResult::empty`
    ///(不报错,仅不撮合)。适合测试场景,要求显式声明所有 instrument。
    StrictByInstrument,
    /// per-instrument 路由 + primary 兜底(默认)。
    /// 未注册 instrument 走 `primary`;若 `primary` 也没设,empty。
    #[default]
    PerInstrumentWithFallback,
}

/// 多 engine 路由器
///
/// # 字段
///
/// - `routes`:per-instrument 引擎注册表,优先匹配
/// - `primary`:fallback 引擎,无 instrument 路由时使用
/// - `strategy`:路由决策(严格 / 兜底)
pub struct EngineRouter {
    /// per-instrument 路由表(同 instrument 重复注册时后写覆盖前写,与 `HashMap::insert` 一致)
    routes: HashMap<Instrument, RoutedEngine>,
    /// primary 兜底
    primary: Option<RoutedEngine>,
    /// 路由策略
    strategy: RoutingStrategy,
}

impl Default for EngineRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineRouter {
    /// 创建空 Router(无 primary,默认 `PerInstrumentWithFallback` 策略)
    pub fn new() -> Self {
        Self {
            routes: HashMap::new(),
            primary: None,
            strategy: RoutingStrategy::default(),
        }
    }

    /// 设置 primary(链式)
    pub fn with_primary(mut self, primary: RoutedEngine) -> Self {
        self.primary = Some(primary);
        self
    }

    /// 设置路由策略(链式)
    pub fn with_strategy(mut self, strategy: RoutingStrategy) -> Self {
        self.strategy = strategy;
        self
    }

    /// 注册 instrument → engine 路由(同 instrument 后写覆盖前写)
    pub fn register(&mut self, instrument: Instrument, engine: RoutedEngine) {
        self.routes.insert(instrument, engine);
    }

    /// 移除 instrument 路由(不影响 primary)
    pub fn unregister(&mut self, instrument: &Instrument) -> Option<RoutedEngine> {
        self.routes.remove(instrument)
    }

    /// 当前注册路由数
    pub fn route_count(&self) -> usize {
        self.routes.len()
    }

    /// 是否设置了 primary
    pub fn has_primary(&self) -> bool {
        self.primary.is_some()
    }

    /// 查找 instrument 对应的 engine(优先 routes,fallback primary)
    fn route_for(&mut self, instrument: &Instrument) -> Option<&mut RoutedEngine> {
        if let Some(engine) = self.routes.get_mut(instrument) {
            return Some(engine);
        }
        if self.strategy == RoutingStrategy::PerInstrumentWithFallback {
            return self.primary.as_mut();
        }
        None
    }
}

// ─── 内部 dispatch helpers ─────────────────────────────

/// `submit` dispatch(每个 arm 直接调 inherent 或 trait method)
fn dispatch_submit(engine: &mut RoutedEngine, order: Order) -> SubmitResult {
    match engine {
        RoutedEngine::L1(e) => e.submit(order),
        RoutedEngine::L2(e) => e.submit(order),
        RoutedEngine::L3(e) => {
            // L3 inherent 返回 `MatchingL3Result<Vec<MatchFill>>`,用 trait 转 `SubmitResult`
            MatchingEngine::submit(e, order)
        }
        RoutedEngine::Impacted(e) => e.submit(order),
    }
}

/// `cancel` dispatch(L1 / L2 / L3 / Impacted 都有 `cancel` inherent)
fn dispatch_cancel(engine: &mut RoutedEngine, order_id: u64) -> bool {
    match engine {
        RoutedEngine::L1(e) => e.cancel(order_id),
        RoutedEngine::L2(e) => e.cancel(order_id),
        RoutedEngine::L3(e) => {
            // L3 没 inherent cancel,走 trait
            MatchingEngine::cancel(e, order_id)
        }
        RoutedEngine::Impacted(e) => e.cancel(order_id),
    }
}

/// `best_bid` dispatch
fn dispatch_best_bid(engine: &RoutedEngine) -> Option<Price> {
    match engine {
        RoutedEngine::L1(e) => e.best_bid(),
        RoutedEngine::L2(e) => e.best_bid(),
        RoutedEngine::L3(e) => e.best_bid(),
        RoutedEngine::Impacted(e) => e.best_bid(),
    }
}

/// `best_ask` dispatch
fn dispatch_best_ask(engine: &RoutedEngine) -> Option<Price> {
    match engine {
        RoutedEngine::L1(e) => e.best_ask(),
        RoutedEngine::L2(e) => e.best_ask(),
        RoutedEngine::L3(e) => e.best_ask(),
        RoutedEngine::Impacted(e) => e.best_ask(),
    }
}

/// `depth` dispatch
fn dispatch_depth(
    engine: &RoutedEngine,
    levels: usize,
) -> (Vec<OrderBookLevel>, Vec<OrderBookLevel>) {
    match engine {
        RoutedEngine::L1(e) => e.depth(levels),
        RoutedEngine::L2(e) => e.depth(levels),
        RoutedEngine::L3(e) => e.depth(levels),
        RoutedEngine::Impacted(e) => e.depth(levels),
    }
}

/// `active_order_count` dispatch
fn dispatch_active_count(engine: &RoutedEngine) -> usize {
    match engine {
        RoutedEngine::L1(e) => e.active_order_count(),
        RoutedEngine::L2(e) => e.active_order_count(),
        RoutedEngine::L3(e) => e.active_order_count(),
        RoutedEngine::Impacted(e) => e.active_order_count(),
    }
}

/// `clear_book` dispatch
fn dispatch_clear_book(engine: &mut RoutedEngine) {
    match engine {
        RoutedEngine::L1(e) => e.clear_book(),
        RoutedEngine::L2(e) => e.clear_book(),
        RoutedEngine::L3(e) => e.clear_book(),
        RoutedEngine::Impacted(e) => e.clear_book(),
    }
}

/// `clear_book_for` dispatch(per-instrument)
fn dispatch_clear_book_for(engine: &mut RoutedEngine, instrument: &Instrument) {
    match engine {
        RoutedEngine::L1(e) => e.clear_book_for(instrument),
        RoutedEngine::L2(e) => e.clear_book_for(instrument),
        RoutedEngine::L3(e) => e.clear_book_for(instrument),
        RoutedEngine::Impacted(e) => e.clear_book_for(instrument),
    }
}

/// `seed_liquidity` dispatch
fn dispatch_seed_liquidity(
    engine: &mut RoutedEngine,
    mid_price: f64,
    half_spread: f64,
    depth_levels: usize,
    size_per_level: f64,
    instrument: Instrument,
    next_id: u64,
) -> u64 {
    match engine {
        RoutedEngine::L1(e) => e.seed_liquidity(
            mid_price,
            half_spread,
            depth_levels,
            size_per_level,
            instrument,
            next_id,
        ),
        RoutedEngine::L2(e) => e.seed_liquidity(
            mid_price,
            half_spread,
            depth_levels,
            size_per_level,
            instrument,
            next_id,
        ),
        RoutedEngine::L3(e) => e.seed_liquidity(
            mid_price,
            half_spread,
            depth_levels,
            size_per_level,
            instrument,
            next_id,
        ),
        RoutedEngine::Impacted(e) => e.seed_liquidity(
            mid_price,
            half_spread,
            depth_levels,
            size_per_level,
            instrument,
            next_id,
        ),
    }
}

// ─── MatchingEngine trait 实现 ─────────────────────────

impl MatchingEngine for EngineRouter {
    fn submit(&mut self, order: Order) -> SubmitResult {
        let instrument = order.instrument.clone();
        if let Some(engine) = self.route_for(&instrument) {
            return dispatch_submit(engine, order);
        }
        // 无路由 + 无 primary(或 strict 模式)→ empty
        SubmitResult::empty(Quantity::from_f64(0.0))
    }

    fn cancel(&mut self, order_id: u64) -> bool {
        // 扫 routes
        for engine in self.routes.values_mut() {
            if dispatch_cancel(engine, order_id) {
                return true;
            }
        }
        // 再扫 primary
        if let Some(primary) = self.primary.as_mut()
            && dispatch_cancel(primary, order_id)
        {
            return true;
        }
        false
    }

    fn best_bid(&self) -> Option<Price> {
        let mut best: Option<Price> = None;
        for engine in self.routes.values() {
            if let Some(bid) = dispatch_best_bid(engine) {
                best = Some(match best {
                    None => bid,
                    Some(prev) if bid.as_f64() > prev.as_f64() => bid,
                    Some(prev) => prev,
                });
            }
        }
        if let Some(primary) = &self.primary
            && let Some(bid) = dispatch_best_bid(primary)
        {
            best = Some(match best {
                None => bid,
                Some(prev) if bid.as_f64() > prev.as_f64() => bid,
                Some(prev) => prev,
            });
        }
        best
    }

    fn best_ask(&self) -> Option<Price> {
        let mut best: Option<Price> = None;
        for engine in self.routes.values() {
            if let Some(ask) = dispatch_best_ask(engine) {
                best = Some(match best {
                    None => ask,
                    Some(prev) if ask.as_f64() < prev.as_f64() => ask,
                    Some(prev) => prev,
                });
            }
        }
        if let Some(primary) = &self.primary
            && let Some(ask) = dispatch_best_ask(primary)
        {
            best = Some(match best {
                None => ask,
                Some(prev) if ask.as_f64() < prev.as_f64() => ask,
                Some(prev) => prev,
            });
        }
        best
    }

    fn spread(&self) -> Option<Price> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) if ask.as_f64() >= bid.as_f64() => {
                Some(Price::from_f64(ask.as_f64() - bid.as_f64()))
            }
            _ => None,
        }
    }

    fn depth(&self, levels: usize) -> (Vec<OrderBookLevel>, Vec<OrderBookLevel>) {
        let mut all_bids = Vec::new();
        let mut all_asks = Vec::new();
        for engine in self.routes.values() {
            let (b, a) = dispatch_depth(engine, levels);
            all_bids.extend(b);
            all_asks.extend(a);
        }
        if let Some(primary) = &self.primary {
            let (b, a) = dispatch_depth(primary, levels);
            all_bids.extend(b);
            all_asks.extend(a);
        }
        // 合并排序: bids 降序(最优买价最大), asks 升序(最优卖价最小)
        all_bids.sort_by_key(|x| std::cmp::Reverse(x.price));
        all_bids.truncate(levels);
        all_asks.sort_by_key(|x| x.price);
        all_asks.truncate(levels);
        (all_bids, all_asks)
    }

    fn active_order_count(&self) -> usize {
        self.routes
            .values()
            .map(dispatch_active_count)
            .sum::<usize>()
            + self
                .primary
                .as_ref()
                .map(dispatch_active_count)
                .unwrap_or(0)
    }

    fn clear_book(&mut self) {
        for engine in self.routes.values_mut() {
            dispatch_clear_book(engine);
        }
        if let Some(primary) = self.primary.as_mut() {
            dispatch_clear_book(primary);
        }
    }

    fn clear_book_for(&mut self, instrument: &Instrument) {
        if let Some(engine) = self.route_for(instrument) {
            dispatch_clear_book_for(engine, instrument);
        }
    }

    fn seed_liquidity(
        &mut self,
        mid_price: f64,
        half_spread: f64,
        depth_levels: usize,
        size_per_level: f64,
        instrument: Instrument,
        next_id: u64,
    ) -> u64 {
        if let Some(engine) = self.route_for(&instrument) {
            return dispatch_seed_liquidity(
                engine,
                mid_price,
                half_spread,
                depth_levels,
                size_per_level,
                instrument,
                next_id,
            );
        }
        // 严格模式 + 未注册 → no-op(返回原 next_id)
        next_id
    }
}

// ─── 静态 Send + Sync 断言 ─────────────────────────────

/// 编译期保证 `EngineRouter` 是 `Send + Sync`(BacktestEngine / Python 绑定需要)
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<EngineRouter>();
    assert_send_sync::<RoutedEngine>();
};

// ─── 单元测试 ──────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::market::Side;
    use axon_core::order::{OrderType, TimeInForce};
    use axon_core::types::{Price, SpotInstrument};

    fn btc() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: "BTC".into(),
            quote: "USDT".into(),
        })
    }

    fn eth() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: "ETH".into(),
            quote: "USDT".into(),
        })
    }

    fn make_limit(id: u64, inst: &Instrument, side: Side, price: f64, qty: f64) -> Order {
        // 简化:只支持 spot(测试够用)
        let (base, quote) = match inst {
            Instrument::Spot(s) => (s.base.as_str().to_string(), s.quote.as_str().to_string()),
            _ => panic!("test only supports spot"),
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
    }

    #[test]
    fn router_per_instrument_routes_to_correct_engine() {
        let mut router = EngineRouter::new();
        router.register(btc(), RoutedEngine::L1(L1MatchingEngine::new()));
        router.register(eth(), RoutedEngine::L2(L2MatchingEngine::new()));

        // BTC 走 L1
        let r1 = router.submit(make_limit(1, &btc(), Side::Sell, 100.0, 1.0));
        // ETH 走 L2
        let r2 = router.submit(make_limit(2, &eth(), Side::Sell, 200.0, 1.0));

        // 不验证具体 SubmitResult(各 engine 内部逻辑),只验证不 panic + active_count 累计
        assert_eq!(router.active_order_count(), 2);
        let _ = (r1, r2); // suppress unused
    }

    #[test]
    fn router_primary_fallback_for_unregistered() {
        // 没注册 SOL,但 primary 是 L1,应该走 primary
        let sol = Instrument::Spot(SpotInstrument {
            base: "SOL".into(),
            quote: "USDT".into(),
        });
        let mut router = EngineRouter::new()
            .with_strategy(RoutingStrategy::PerInstrumentWithFallback)
            .with_primary(RoutedEngine::L1(L1MatchingEngine::new()));

        let r = router.submit(make_limit(1, &sol, Side::Sell, 50.0, 1.0));
        let _ = r;
        // primary 的活跃订单应该是 1
        assert_eq!(router.active_order_count(), 1);
    }

    #[test]
    fn router_strict_mode_returns_empty_for_unregistered() {
        let sol = Instrument::Spot(SpotInstrument {
            base: "SOL".into(),
            quote: "USDT".into(),
        });
        // 严格模式 + 无 primary + 未注册 → empty
        let mut router = EngineRouter::new().with_strategy(RoutingStrategy::StrictByInstrument);

        let r = router.submit(make_limit(1, &sol, Side::Sell, 50.0, 1.0));
        // SubmitResult::empty → 0 fills, 0 active
        assert_eq!(router.active_order_count(), 0);
        let _ = r;
    }

    #[test]
    fn router_strict_mode_with_primary_still_empty() {
        // 严格模式 + 有 primary + 未注册 → empty(strict 优先于 fallback)
        let sol = Instrument::Spot(SpotInstrument {
            base: "SOL".into(),
            quote: "USDT".into(),
        });
        let mut router = EngineRouter::new()
            .with_strategy(RoutingStrategy::StrictByInstrument)
            .with_primary(RoutedEngine::L1(L1MatchingEngine::new()));

        let r = router.submit(make_limit(1, &sol, Side::Sell, 50.0, 1.0));
        assert_eq!(router.active_order_count(), 0); // primary 没动
        let _ = r;
    }

    #[test]
    fn router_register_overwrites_previous() {
        let mut router = EngineRouter::new();
        router.register(btc(), RoutedEngine::L1(L1MatchingEngine::new()));
        assert_eq!(router.route_count(), 1);

        // 重新注册 BTC → L2
        router.register(btc(), RoutedEngine::L2(L2MatchingEngine::new()));
        assert_eq!(router.route_count(), 1, "重复注册不增计数");

        // 卸载后应回到无路由
        let removed = router.unregister(&btc());
        assert!(removed.is_some());
        assert_eq!(router.route_count(), 0);
    }

    #[test]
    fn router_clear_book_clears_all_engines() {
        let mut router =
            EngineRouter::new().with_primary(RoutedEngine::L1(L1MatchingEngine::new()));
        router.register(eth(), RoutedEngine::L1(L1MatchingEngine::new()));

        // 注入 3 单: BTC(primary) + ETH(routes) + SOL(primary fallback)
        let sol = Instrument::Spot(SpotInstrument {
            base: "SOL".into(),
            quote: "USDT".into(),
        });
        let _ = router.submit(make_limit(1, &btc(), Side::Sell, 100.0, 1.0));
        let _ = router.submit(make_limit(2, &eth(), Side::Sell, 200.0, 1.0));
        let _ = router.submit(make_limit(3, &sol, Side::Sell, 50.0, 1.0));

        assert_eq!(router.active_order_count(), 3);

        router.clear_book();
        assert_eq!(
            router.active_order_count(),
            0,
            "clear_book 应清空所有 engine"
        );
    }

    #[test]
    fn router_clear_book_for_routes_to_specific() {
        let mut router = EngineRouter::new();
        router.register(btc(), RoutedEngine::L1(L1MatchingEngine::new()));
        router.register(eth(), RoutedEngine::L1(L1MatchingEngine::new()));

        let _ = router.submit(make_limit(1, &btc(), Side::Sell, 100.0, 1.0));
        let _ = router.submit(make_limit(2, &eth(), Side::Sell, 200.0, 1.0));
        assert_eq!(router.active_order_count(), 2);

        // 只清 BTC
        router.clear_book_for(&btc());
        assert_eq!(router.active_order_count(), 1, "ETH 的单应保留");
    }

    #[test]
    fn router_best_bid_ask_aggregates_across_engines() {
        // BTC L1 卖 100, ETH L1 卖 200, SOL L1 卖 50
        // bids 簿空,asks 取最低
        let sol = Instrument::Spot(SpotInstrument {
            base: "SOL".into(),
            quote: "USDT".into(),
        });
        let mut router = EngineRouter::new();
        router.register(btc(), RoutedEngine::L1(L1MatchingEngine::new()));
        router.register(eth(), RoutedEngine::L1(L1MatchingEngine::new()));
        router.register(sol.clone(), RoutedEngine::L1(L1MatchingEngine::new()));

        let _ = router.submit(make_limit(1, &btc(), Side::Sell, 100.0, 1.0));
        let _ = router.submit(make_limit(2, &eth(), Side::Sell, 200.0, 1.0));
        let _ = router.submit(make_limit(3, &sol, Side::Sell, 50.0, 1.0));

        // 跨 engine 聚合 best_ask = min(100, 200, 50) = 50
        let best_ask = router.best_ask().expect("should have best_ask");
        assert!(
            (best_ask.as_f64() - 50.0).abs() < 1e-9,
            "best_ask 应 = 50(SOL),got {}",
            best_ask.as_f64()
        );

        // bids 空
        assert!(router.best_bid().is_none());
    }

    #[test]
    fn router_depth_aggregates_and_sorts() {
        let mut router = EngineRouter::new();
        router.register(btc(), RoutedEngine::L1(L1MatchingEngine::new()));
        router.register(eth(), RoutedEngine::L1(L1MatchingEngine::new()));

        // BTC asks @ 100, 105
        let _ = router.submit(make_limit(1, &btc(), Side::Sell, 100.0, 1.0));
        let _ = router.submit(make_limit(2, &btc(), Side::Sell, 105.0, 1.0));
        // ETH asks @ 90, 110
        let _ = router.submit(make_limit(3, &eth(), Side::Sell, 90.0, 1.0));
        let _ = router.submit(make_limit(4, &eth(), Side::Sell, 110.0, 1.0));

        let (bids, asks) = router.depth(2);
        // asks 升序:top 2 = 90 (ETH) + 100 (BTC)
        assert_eq!(asks.len(), 2);
        assert!((asks[0].price.as_f64() - 90.0).abs() < 1e-9);
        assert!((asks[1].price.as_f64() - 100.0).abs() < 1e-9);
        // bids 空
        assert!(bids.is_empty());
    }

    #[test]
    fn router_active_order_count_sums_all_engines() {
        let sol = Instrument::Spot(SpotInstrument {
            base: "SOL".into(),
            quote: "USDT".into(),
        });
        let mut router =
            EngineRouter::new().with_primary(RoutedEngine::L1(L1MatchingEngine::new()));
        router.register(btc(), RoutedEngine::L1(L1MatchingEngine::new()));
        router.register(eth(), RoutedEngine::L2(L2MatchingEngine::new()));

        let _ = router.submit(make_limit(1, &btc(), Side::Sell, 100.0, 1.0));
        let _ = router.submit(make_limit(2, &eth(), Side::Sell, 200.0, 1.0));
        // SOL 走 primary
        let _ = router.submit(make_limit(3, &sol, Side::Sell, 50.0, 1.0));

        // BTC L1: 1, ETH L2: 1, SOL L1 (primary): 1 → 总 3
        assert_eq!(router.active_order_count(), 3);
    }

    #[test]
    fn router_cancel_scans_all_engines() {
        let sol = Instrument::Spot(SpotInstrument {
            base: "SOL".into(),
            quote: "USDT".into(),
        });
        let mut router =
            EngineRouter::new().with_primary(RoutedEngine::L1(L1MatchingEngine::new()));
        router.register(btc(), RoutedEngine::L1(L1MatchingEngine::new()));

        let _ = router.submit(make_limit(42, &btc(), Side::Sell, 100.0, 1.0));
        let _ = router.submit(make_limit(43, &sol, Side::Sell, 50.0, 1.0));
        assert_eq!(router.active_order_count(), 2);

        // 取消 BTC 上的 42 → 命中 routes
        assert!(router.cancel(42));
        assert_eq!(router.active_order_count(), 1);

        // 取消 SOL 上的 43 → 命中 primary
        assert!(router.cancel(43));
        assert_eq!(router.active_order_count(), 0);

        // 不存在的 id → false
        assert!(!router.cancel(999));
    }

    #[test]
    fn router_l3_dispatch_works() {
        // L3 engine 在 router 中可正常 submit
        let mut router = EngineRouter::new();
        let mut l3 = MultiAssetMatchingEngine::new();
        l3.register_instrument(btc());
        router.register(btc(), RoutedEngine::L3(l3));

        let r = router.submit(make_limit(1, &btc(), Side::Sell, 100.0, 1.0));
        // L3::submit 返回 Result,我们通过 trait 转 SubmitResult,不 panic
        let _ = r;
        assert_eq!(router.active_order_count(), 1);
    }

    #[test]
    fn router_send_sync_compile_time() {
        // 静态断言:EngineRouter 必须 Send + Sync
        // (const _ 在文件顶部已 assert,这里只验证编译通过)
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<EngineRouter>();
        assert_send_sync::<RoutedEngine>();
    }

    #[test]
    fn router_impacted_dispatch_works() {
        // Impacted engine 在 router 中可正常 submit
        use axon_core::impact::linear_impact;
        let mut router = EngineRouter::new();
        router.register(
            btc(),
            RoutedEngine::Impacted(ImpactedMatchingEngine::new(Box::new(linear_impact()))),
        );

        // 卖一档
        let r = router.submit(make_limit(1, &btc(), Side::Sell, 100.0, 1.0));
        let _ = r;
        assert_eq!(router.active_order_count(), 1);
        // Impacted 的 best_bid 在无单时返回 None
        assert!(router.best_bid().is_none());
    }

    #[test]
    fn router_seed_liquidity_dispatches_to_route() {
        // seed_liquidity 走 per-instrument 路由
        let mut router = EngineRouter::new();
        router.register(btc(), RoutedEngine::L1(L1MatchingEngine::new()));
        router.register(eth(), RoutedEngine::L1(L1MatchingEngine::new()));

        // 给 BTC seed:10 档 / qty=2 / mid=100 / half=0.5
        // → 注入 20 单(10 bid + 10 ask)
        let next = router.seed_liquidity(100.0, 0.5, 10, 2.0, btc(), 100_000);
        assert!(next > 100_000, "应消费 id 计数器");
        assert_eq!(router.active_order_count(), 20);

        // ETH 不动
        // 给 ETH seed
        let next2 = router.seed_liquidity(200.0, 1.0, 5, 1.0, eth(), 200_000);
        assert!(next2 > 200_000);
        assert_eq!(router.active_order_count(), 30);
    }

    #[test]
    fn router_seed_liquidity_falls_back_to_primary() {
        // 未注册 instrument 的 seed 走 primary
        let sol = Instrument::Spot(SpotInstrument {
            base: "SOL".into(),
            quote: "USDT".into(),
        });
        let mut router =
            EngineRouter::new().with_primary(RoutedEngine::L1(L1MatchingEngine::new()));
        let next = router.seed_liquidity(50.0, 0.25, 4, 1.0, sol, 50_000);
        assert!(next > 50_000, "primary 应消费 id");
        assert_eq!(router.active_order_count(), 8); // 4 bid + 4 ask
    }

    #[test]
    fn router_seed_liquidity_no_op_in_strict_mode() {
        // 严格模式 + 未注册 → no-op(返回原 next_id)
        let sol = Instrument::Spot(SpotInstrument {
            base: "SOL".into(),
            quote: "USDT".into(),
        });
        let mut router = EngineRouter::new()
            .with_strategy(RoutingStrategy::StrictByInstrument)
            .with_primary(RoutedEngine::L1(L1MatchingEngine::new()));
        let next = router.seed_liquidity(50.0, 0.25, 4, 1.0, sol, 50_000);
        assert_eq!(next, 50_000, "严格模式无路由→next_id 不变");
        assert_eq!(router.active_order_count(), 0);
    }
}
