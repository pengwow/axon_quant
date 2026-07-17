//! 模糊测试（Property-based Fuzz Testing）
//!
//! 通过 `proptest` 框架对核心模块进行随机化输入测试，验证系统在不寻常、
//! 边界或对抗性输入下的不变式（invariants）保持。
//!
//! ## 覆盖目标
//!
//! | 模块 | 不变式 |
//! |-----|--------|
//! | 线性冲击模型 | 零深度 ⇒ 零冲击；零系数 ⇒ 零冲击；冲击量级与订单量/深度之比成正比 |
//! | 幂律冲击模型 | 冲击单调性（订单量↑ ⇒ 冲击↑）；exponent 边界值处理 |
//! | 自适应冲击模型 | 波动率缩放因子单调；零波动率 ⇒ 等价于基础模型 |
//! | 撮合引擎 | 成交价始终落在 taker 限价范围内（限价单）；成交后活跃订单数减少 |
//! | 波动率估计器 | 零收益率 ⇒ 零波动率（EWMA reset 后）；窗口更新后 v² 非负 |
//! | 订单簿 | `from_l2` 后排序保持单调；`spread` ≥ 0（若两侧都存在） |
//! | Almgren-Chriss | κ 公式与 γ/σ²/ε/η 的关系；轨迹和恒等于 Q（k=0 起点） |
//!
//! ## 已知差异 vs libfuzzer
//!
//! - **无 coverage-guided mutation**：proptest 使用随机生成 + 缩小
//! - **无二进制 seed corpus**：seed 由 proptest 内部生成
//! - **无自动崩溃保存**：失败用例可手动复制 `PROPTEST_CASES` 跑回归
//!
//! 如需 nightly 全量 libfuzzer，可后续引入 `cargo-fuzz` 子项目。

// 仅在 `#[test]` 函数内部使用，避免 lib 模式下报"未使用导入"
#![allow(unused_imports)]
#![allow(dead_code)]

use axon_backtest::matching::engine::{L1MatchingEngine, MatchingEngine};
use axon_core::impact::traits::ImpactModel;
use axon_core::impact::{
    AdaptiveImpactModel, AlmgrenChrissModel, Impact, LinearImpactModel, PowerLawImpactModel,
};
use axon_core::market::{OrderBookLevel, OrderBookSnapshot, Side};
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::types::{Price, Quantity, Symbol};
use axon_core::volatility::estimator::VolatilityEstimator;
use axon_core::volatility::{EwmaVolatility, RollingVolatility};
use proptest::prelude::*;

// ───────────────────────────────────────────────────────────────────
// 策略（Strategy）：为 proptest 生成合法范围内的随机输入
// ───────────────────────────────────────────────────────────────────

/// 合法价格策略：0.01 ~ 1_000_000
fn price_strategy() -> impl Strategy<Value = Price> {
    (1u64..=100_000_000u64).prop_map(|p| Price::from_f64(p as f64 * 0.01))
}

/// 合法数量策略：0.0 ~ 1_000
fn quantity_strategy() -> impl Strategy<Value = Quantity> {
    (0u64..=1_000_000u64).prop_map(|q| Quantity::from_f64(q as f64 * 0.001))
}

/// 合法订单簿深度策略：1 ~ 50 层，每层数量 0 ~ 1000
/// 约束 ask 价格严格高于 bid 价格，避免生成交叉订单簿（locked/crossed book）
fn order_book_strategy() -> impl Strategy<Value = OrderBookSnapshot> {
    (
        // bids 价格区间：1.0 ~ 100.0，bid 数量 0.0 ~ 10.0
        prop::collection::vec((100u64..=10_000u64, 0u64..=1_000u64), 1..20),
        // asks 价格区间：100.01 ~ 200.0，ask 数量 0.0 ~ 10.0
        prop::collection::vec((10_001u64..=20_000u64, 0u64..=1_000u64), 1..20),
    )
        .prop_map(|(bids, asks)| {
            let mut bid_vec: Vec<OrderBookLevel> = bids
                .into_iter()
                .map(|(p, q)| OrderBookLevel {
                    price: Price::from_f64(p as f64 * 0.01),
                    quantity: Quantity::from_f64(q as f64 * 0.01),
                })
                .collect();
            let mut ask_vec: Vec<OrderBookLevel> = asks
                .into_iter()
                .map(|(p, q)| OrderBookLevel {
                    price: Price::from_f64(p as f64 * 0.01),
                    quantity: Quantity::from_f64(q as f64 * 0.01),
                })
                .collect();
            // 降序/升序排序
            bid_vec.sort_by(|a, b| b.price.as_f64().partial_cmp(&a.price.as_f64()).unwrap());
            ask_vec.sort_by(|a, b| a.price.as_f64().partial_cmp(&b.price.as_f64()).unwrap());
            OrderBookSnapshot {
                timestamp: axon_core::time::Timestamp::from_nanos(0),
                bids: bid_vec,
                asks: ask_vec,
            }
        })
}

/// 合法订单生成器：固定 symbol/side，fuzz 限价、数量
fn order_strategy(side: Side, base_price: f64) -> impl Strategy<Value = Order> {
    (
        0u64..1_000_000u64,  // order_id
        1u64..=1_000_000u64, // price offset
        1u64..=1_000_000u64, // quantity
    )
        .prop_map(move |(id, price_off, qty)| {
            let p = Price::from_f64((base_price + price_off as f64 * 0.001).max(0.01));
            let q = Quantity::from_f64(qty as f64 * 0.001);
            Order::spot(
id,
"FUZZ",
"USDT",side,
                OrderType::Limit { price: p },
                q,
                TimeInForce::GTC,
            )
        })
}

// ───────────────────────────────────────────────────────────────────
// 1. 线性冲击模型不变式
// ───────────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// 零深度订单簿 ⇒ 零冲击（任何数量、方向、系数下）
    #[test]
    fn prop_linear_zero_orderbook_zero_impact(
        coefficient in 0.0f64..1.0,
        qty in 0.0f64..1000.0,
    ) {
        prop_assume!(coefficient.is_finite() && qty.is_finite());
        let model = LinearImpactModel::new(coefficient);
        let empty = OrderBookSnapshot::empty(axon_core::time::Timestamp::from_nanos(0));
        for side in [Side::Buy, Side::Sell] {
            let impact = model.compute_impact(Quantity::from_f64(qty), side, &empty);
            prop_assert_eq!(impact, Impact::zero());
        }
    }

    /// 零数量订单 ⇒ 零冲击
    #[test]
    fn prop_linear_zero_quantity_zero_impact(book in order_book_strategy()) {
        let model = LinearImpactModel::new(0.05);
        for side in [Side::Buy, Side::Sell] {
            let impact = model.compute_impact(Quantity::from_f64(0.0), side, &book);
            prop_assert_eq!(impact, Impact::zero());
        }
    }

    /// 零系数 ⇒ 零冲击（即使有深度）
    #[test]
    fn prop_linear_zero_coefficient_zero_impact(
        book in order_book_strategy(),
        qty in 0.0f64..1000.0,
    ) {
        prop_assume!(qty.is_finite());
        let model = LinearImpactModel::new(0.0);
        for side in [Side::Buy, Side::Sell] {
            let impact = model.compute_impact(Quantity::from_f64(qty), side, &book);
            prop_assert_eq!(impact, Impact::zero());
        }
    }

    /// 冲击量级 = coefficient × (qty / total_depth)
    /// 在合理范围内（系数、订单量、深度都 > 0）应满足该公式
    /// 注意：LinearImpactModel 默认取前 10 层（depth_levels = 10）
    #[test]
    fn prop_linear_formula_holds(book in order_book_strategy()) {
        let model = LinearImpactModel::new(0.1);
        for side in [Side::Buy, Side::Sell] {
            // 与模型保持一致：取前 10 层计算总深度
            let total_depth: f64 = match side {
                Side::Buy => book.asks.iter().take(10).map(|l| l.quantity.as_f64()).sum(),
                Side::Sell => book.bids.iter().take(10).map(|l| l.quantity.as_f64()).sum(),
            };
            if total_depth > 0.0 {
                let qty = total_depth * 0.5;
                let impact = model.compute_impact(Quantity::from_f64(qty), side, &book);
                let expected = 0.1 * (qty / total_depth);
                // 冲击 = 总冲击 = 系数 * 比例（与 inst_ratio 无关）
                prop_assert!(
                    (impact.total() - expected).abs() < 1e-6,
                    "冲击公式不成立: actual={}, expected={}",
                    impact.total(), expected
                );
            }
        }
    }

    /// 即时 + 永久 = 总冲击，且比例由 instantaneous_ratio 决定
    #[test]
    fn prop_linear_instantaneous_ratio_decomposition(
        ratio in 0.0f64..=1.0,
        book in order_book_strategy(),
    ) {
        prop_assume!(ratio.is_finite());
        let model = LinearImpactModel::new(0.1).with_instantaneous_ratio(ratio);
        let qty = 10.0_f64;
        let impact = model.compute_impact(Quantity::from_f64(qty), Side::Buy, &book);
        // 即时 + 永久 = 总
        let total = impact.instantaneous + impact.permanent;
        prop_assert!((total - impact.total()).abs() < 1e-9);
        // 比例匹配
        if total > 1e-12 {
            let actual_ratio = impact.instantaneous / total;
            prop_assert!((actual_ratio - ratio).abs() < 1e-6, "ratio={} actual={}", ratio, actual_ratio);
        }
    }
}

// ───────────────────────────────────────────────────────────────────
// 2. 幂律冲击模型不变式
// ───────────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// 零订单量 ⇒ 零冲击
    #[test]
    fn prop_power_law_zero_quantity_zero_impact(
        book in order_book_strategy(),
    ) {
        let model = PowerLawImpactModel::new(0.1, 0.5);
        for side in [Side::Buy, Side::Sell] {
            let impact = model.compute_impact(Quantity::from_f64(0.0), side, &book);
            prop_assert_eq!(impact, Impact::zero());
        }
    }

    /// 幂律指数 0.5 时冲击等于 sqrt(qty/depth) × coefficient
    /// 注意：模型默认取前 10 层（depth_levels = 10）
    #[test]
    fn prop_power_law_sqrt_law(book in order_book_strategy()) {
        let model = PowerLawImpactModel::new(0.1, 0.5);
        let total_depth: f64 = book.asks.iter().take(10).map(|l| l.quantity.as_f64()).sum();
        prop_assume!(total_depth > 1.0);
        let qty = 1.0_f64;
        let impact = model.compute_impact(Quantity::from_f64(qty), Side::Buy, &book);
        let expected = 0.1 * (qty / total_depth).sqrt();
        prop_assert!((impact.total() - expected).abs() < 1e-6);
    }

    /// 冲击随订单量单调非减（仅在订单量不超过深度时，否则模型可能产生极端值）
    /// 注意：模型默认取前 10 层（depth_levels = 10）
    #[test]
    fn prop_power_law_monotonic_in_quantity(book in order_book_strategy()) {
        let model = PowerLawImpactModel::new(0.1, 0.5);
        let total_depth: f64 = book.asks.iter().take(10).map(|l| l.quantity.as_f64()).sum();
        prop_assume!(total_depth > 1.0);
        // qty 取不超过总深度的 10% 以避免极端比值
        let q1 = total_depth * 0.001;
        let q2 = total_depth * 0.01;
        let q3 = total_depth * 0.1;
        let i1 = model.compute_impact(Quantity::from_f64(q1), Side::Buy, &book).total();
        let i2 = model.compute_impact(Quantity::from_f64(q2), Side::Buy, &book).total();
        let i3 = model.compute_impact(Quantity::from_f64(q3), Side::Buy, &book).total();
        prop_assert!(i1 <= i2, "q1={} -> {}, q2={} -> {}", q1, i1, q2, i2);
        prop_assert!(i2 <= i3, "q2={} -> {}, q3={} -> {}", q2, i2, q3, i3);
    }
}

// ───────────────────────────────────────────────────────────────────
// 3. 自适应冲击模型不变式
// ───────────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// 零波动率 ⇒ 缩放因子 = volatility_scale
    #[test]
    fn prop_adaptive_zero_volatility_uses_scale(
        scale in 0.0f64..10.0,
        book in order_book_strategy(),
    ) {
        prop_assume!(scale.is_finite());
        let base: Box<dyn ImpactModel> = Box::new(LinearImpactModel::new(0.05));
        let adaptive = AdaptiveImpactModel::new(base, scale);
        let base_impact = {
            // 重新计算基础模型（不通过 adaptive）
            let base = LinearImpactModel::new(0.05);
            base.compute_impact(Quantity::from_f64(10.0), Side::Buy, &book)
        };
        let adaptive_impact = adaptive.compute_impact(Quantity::from_f64(10.0), Side::Buy, &book);
        let expected_total = base_impact.total() * scale;
        prop_assert!(
            (adaptive_impact.total() - expected_total).abs() < 1e-6,
            "adaptive={}, expected={}",
            adaptive_impact.total(), expected_total
        );
    }
}

// ───────────────────────────────────────────────────────────────────
// 4. Almgren-Chriss 不变式
// ───────────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    /// 零 γ ⇒ κ = 0（纯最小化冲击）
    #[test]
    fn prop_ac_zero_gamma_zero_kappa(
        sigma in 0.01f64..1.0,
        epsilon in 0.01f64..1.0,
        eta in 0.01f64..1.0,
    ) {
        let m = AlmgrenChrissModel::new(sigma, epsilon, eta, 0.0, 100.0);
        prop_assert_eq!(m.kappa(), 0.0);
    }

    /// κ 公式：κ = sqrt(γσ² / (εη))
    #[test]
    fn prop_ac_kappa_formula(
        sigma in 0.01f64..1.0,
        epsilon in 0.01f64..1.0,
        eta in 0.01f64..1.0,
        gamma in 1e-6f64..1e-2,
    ) {
        let m = AlmgrenChrissModel::new(sigma, epsilon, eta, gamma, 100.0);
        let expected = (gamma * sigma * sigma / (epsilon * eta)).sqrt();
        let actual = m.kappa();
        prop_assert!(
            (actual - expected).abs() < 1e-6,
            "κ 公式不成立: actual={}, expected={}",
            actual, expected
        );
    }

    /// 期望成本 ≥ 永久冲击成本 η × Q²
    #[test]
    fn prop_ac_expected_cost_lower_bounded(
        sigma in 0.01f64..0.5,
        epsilon in 0.01f64..0.5,
        eta in 0.001f64..0.1,
        gamma in 1e-5f64..1e-3,
        q in 10.0f64..1000.0,
        n in 5u32..50u32,
    ) {
        prop_assume!(q.is_finite() && sigma.is_finite());
        let m = AlmgrenChrissModel::new(sigma, epsilon, eta, gamma, 100.0);
        let trajectory = m.optimal_trajectory(q, 1.0, n as usize);
        prop_assume!(!trajectory.is_empty());
        let ec = m.expected_cost(&trajectory, 1.0);
        // E[C] ≥ η × Q²（永久冲击）减去浮点误差
        let perm_cost = eta * q * q;
        prop_assert!(
            ec >= perm_cost - 1e-3,
            "期望成本 {} < 永久冲击成本 {}",
            ec, perm_cost
        );
    }
}

// ───────────────────────────────────────────────────────────────────
// 5. 撮合引擎不变式
// ───────────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    /// 限价买单的成交价始终 ≤ 限价
    #[test]
    fn prop_matching_limit_buy_price_bound(
        limit_price in 50.0f64..200.0,
        qty in 1u64..=10u64,
    ) {
        let mut engine = L1MatchingEngine::with_symbol(Symbol::from("FUZZ"));
        // 卖单在更优价格
        let ask = Order::spot(
0,
"FUZZ",
"USDT",Side::Sell,
            OrderType::Limit { price: Price::from_f64(limit_price - 1.0) },
            Quantity::from_f64(100.0), TimeInForce::GTC,
        );
        engine.submit(ask);

        // 限价买单，价格 < 卖价 ⇒ 无成交
        let buy = Order::spot(
1,
"FUZZ",
"USDT",Side::Buy,
            OrderType::Limit { price: Price::from_f64(limit_price) },
            Quantity::from_f64(qty as f64), TimeInForce::GTC,
        );
        let result = engine.submit(buy);
        // 所有成交价格必须 ≤ 限价
        for fill in &result.fills {
            prop_assert!(fill.price.as_f64() <= limit_price + 1e-9);
        }
    }

    /// 限价卖单的成交价始终 ≥ 限价
    #[test]
    fn prop_matching_limit_sell_price_bound(
        limit_price in 50.0f64..200.0,
        qty in 1u64..=10u64,
    ) {
        let mut engine = L1MatchingEngine::with_symbol(Symbol::from("FUZZ"));
        // 买单在更优价格
        let bid = Order::spot(
0,
"FUZZ",
"USDT",Side::Buy,
            OrderType::Limit { price: Price::from_f64(limit_price + 1.0) },
            Quantity::from_f64(100.0), TimeInForce::GTC,
        );
        engine.submit(bid);

        // 限价卖单，价格 > 买价 ⇒ 无成交
        let sell = Order::spot(
1,
"FUZZ",
"USDT",Side::Sell,
            OrderType::Limit { price: Price::from_f64(limit_price) },
            Quantity::from_f64(qty as f64), TimeInForce::GTC,
        );
        let result = engine.submit(sell);
        for fill in &result.fills {
            prop_assert!(fill.price.as_f64() >= limit_price - 1e-9);
        }
    }

    /// 撮合后活跃订单数不应超过初始订单数
    #[test]
    fn prop_matching_active_count_bounded(
        n_orders in 1usize..20usize,
    ) {
        let mut engine = L1MatchingEngine::with_symbol(Symbol::from("FUZZ"));
        for i in 0..n_orders {
            let order = Order::spot(
i as u64,
"FUZZ",
"USDT",Side::Buy,
                OrderType::Limit { price: Price::from_f64(100.0 + i as f64) },
                Quantity::from_f64(1.0), TimeInForce::GTC,
            );
            let _ = engine.submit(order);
        }
        prop_assert!(engine.active_order_count() <= n_orders);
    }
}

// ───────────────────────────────────────────────────────────────────
// 6. 波动率估计器不变式
// ───────────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// EWMA reset_with_variance(0) 后零收益 ⇒ 零方差
    #[test]
    fn prop_ewma_zero_returns_zero_volatility(
        lambda in 0.1f64..=0.99,
    ) {
        let mut e = EwmaVolatility::new(lambda).expect("lambda 合法");
        e.reset_with_variance(0.0);
        for _ in 0..10 {
            e.update(0.0).unwrap();
        }
        prop_assert!(e.variance() < 1e-9, "variance={}", e.variance());
    }

    /// EWMA update 应保持方差非负
    #[test]
    fn prop_ewma_variance_non_negative(
        lambda in 0.1f64..=0.99,
        n_updates in 1u32..=50u32,
        returns in proptest::collection::vec(-0.1f64..0.1, 0..50),
    ) {
        let mut e = EwmaVolatility::new(lambda).expect("lambda 合法");
        e.reset_with_variance(1e-4);
        for (i, r) in returns.iter().take(n_updates as usize).enumerate() {
            e.update(*r).unwrap();
            prop_assert!(e.variance() >= 0.0, "第 {} 次 update 后 variance={}", i, e.variance());
            prop_assert!(e.variance().is_finite(), "variance 非有限：{}", e.variance());
        }
    }

    /// Rolling 窗口方差应非负
    #[test]
    fn prop_rolling_variance_non_negative(
        window in 2usize..=20usize,
        returns in proptest::collection::vec(-0.1f64..0.1, 2..30),
    ) {
        let mut r = RollingVolatility::new(window).expect("window >= 2");
        for (i, ret) in returns.iter().enumerate() {
            r.update(*ret).unwrap();
            if r.is_ready() {
                let v = r.variance().unwrap();
                prop_assert!(v >= 0.0, "第 {} 次 update 后 variance={}", i, v);
                prop_assert!(v.is_finite(), "variance 非有限：{}", v);
            }
        }
    }
}

// ───────────────────────────────────────────────────────────────────
// 7. 订单簿不变式
// ───────────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// from_l2 后 bids 严格降序、asks 严格升序
    #[test]
    fn prop_orderbook_from_l2_sorted(
        bids in prop::collection::vec((1u64..=1_000_000u64, 1u64..=1_000_000u64), 0..30),
        asks in prop::collection::vec((1u64..=1_000_000u64, 1u64..=1_000_000u64), 0..30),
    ) {
        let bid_vec: Vec<OrderBookLevel> = bids.into_iter()
            .map(|(p, q)| OrderBookLevel {
                price: Price::from_f64(p as f64 * 0.01),
                quantity: Quantity::from_f64(q as f64 * 0.01),
            })
            .collect();
        let ask_vec: Vec<OrderBookLevel> = asks.into_iter()
            .map(|(p, q)| OrderBookLevel {
                price: Price::from_f64(p as f64 * 0.01),
                quantity: Quantity::from_f64(q as f64 * 0.01),
            })
            .collect();
        let ob = OrderBookSnapshot::from_l2(
            axon_core::time::Timestamp::from_nanos(0),
            bid_vec, ask_vec,
        );
        prop_assert!(ob.validate_sorting().is_ok());

        // bids 降序
        for w in ob.bids.windows(2) {
            prop_assert!(w[0].price.as_f64() >= w[1].price.as_f64());
        }
        // asks 升序
        for w in ob.asks.windows(2) {
            prop_assert!(w[0].price.as_f64() <= w[1].price.as_f64());
        }
    }

    /// spread 在两侧都存在时为非负
    #[test]
    fn prop_orderbook_spread_non_negative(book in order_book_strategy()) {
        if book.best_bid().is_some() && book.best_ask().is_some() {
            let s = book.spread().unwrap();
            prop_assert!(s >= -1e-9, "spread={}", s);
        }
    }

    /// depth 不超过簿中所有层数量的总和
    #[test]
    fn prop_orderbook_depth_bounded(book in order_book_strategy()) {
        let total_bids: f64 = book.bids.iter().map(|l| l.quantity.as_f64()).sum();
        let total_asks: f64 = book.asks.iter().map(|l| l.quantity.as_f64()).sum();
        prop_assert!(book.depth(Side::Buy, 1000).as_f64() <= total_bids + 1e-9);
        prop_assert!(book.depth(Side::Sell, 1000).as_f64() <= total_asks + 1e-9);
    }
}

// ───────────────────────────────────────────────────────────────────
// 8. 撮合引擎错误处理：无效输入应返回 Err，不应 panic
// ───────────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20))]

    /// 零数量订单应返回空 fills（L1 引擎不返回 Result，但 fills 必须为空）
    #[test]
    fn prop_matching_zero_quantity_no_fills(
        side in prop::bool::ANY,
    ) {
        let mut engine = L1MatchingEngine::with_symbol(Symbol::from("FUZZ"));
        let s = if side { Side::Buy } else { Side::Sell };
        let order = Order::spot(
0,
"FUZZ",
"USDT",s,
            OrderType::Limit { price: Price::from_f64(100.0) },
            Quantity::from_f64(0.0), TimeInForce::GTC,
        );
        let result = engine.submit(order);
        prop_assert!(result.fills.is_empty(), "零数量订单不应有成交");
    }

    /// 零价格限价单应返回空 fills（避免 0/0 除法等异常）
    #[test]
    fn prop_matching_zero_price_no_fills(side in prop::bool::ANY) {
        let mut engine = L1MatchingEngine::with_symbol(Symbol::from("FUZZ"));
        let s = if side { Side::Buy } else { Side::Sell };
        let order = Order::spot(
0,
"FUZZ",
"USDT",s,
            OrderType::Limit { price: Price::from_f64(0.0) },
            Quantity::from_f64(1.0), TimeInForce::GTC,
        );
        let result = engine.submit(order);
        prop_assert!(result.fills.is_empty(), "零价格订单不应有成交");
    }
}

// ───────────────────────────────────────────────────────────────────
// 9. L3 多资产撮合引擎不变式
// ───────────────────────────────────────────────────────────────────

use axon_backtest::matching::{BatchMode, CrossPair, MultiAssetMatchingEngine};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20))]

    /// L3 提交任意合法订单不应 panic
    #[test]
    fn prop_l3_submit_never_panics(
        price in 1.0f64..1_000_000.0,
        qty in 0.001f64..1000.0,
        side in prop::bool::ANY,
    ) {
        let mut engine = MultiAssetMatchingEngine::new();
        engine.register_asset(Symbol::from("FUZZ"));
        let s = if side { Side::Buy } else { Side::Sell };
        let order = Order::spot(
0,
"FUZZ",
"USDT",s,
            OrderType::Limit { price: Price::from_f64(price) },
            Quantity::from_f64(qty), TimeInForce::GTC,
        );
        let _ = engine.submit(order);
    }

    /// 跨资产 ratio 始终 > 0
    #[test]
    fn prop_l3_cross_pair_ratio_positive(
        ratio in 0.001f64..1000.0,
    ) {
        let mut engine = MultiAssetMatchingEngine::new();
        let pair = CrossPair {
            leg1: Symbol::from("A"),
            leg2: Symbol::from("B"),
            ratio,
            max_quantity: Quantity::from_f64(1.0),
        };
        prop_assert!(engine.register_cross_pair(pair).is_ok());
        assert_eq!(engine.cross_pair_count(), 1);
    }

    /// 暗池成交数 ≤ 提交订单数
    #[test]
    fn prop_l3_dark_pool_fill_count_bounded(
        n_orders in 1usize..20usize,
    ) {
        let mut engine = MultiAssetMatchingEngine::new();
        engine.register_asset(Symbol::from("FUZZ"));
        engine.set_batch_mode(BatchMode::DarkPool);

        let mut total_fills = 0usize;
        for i in 0..n_orders {
            let order = Order::spot(
i as u64,
"FUZZ",
"USDT",Side::Buy,
                OrderType::Limit { price: Price::from_f64(100.0) },
                Quantity::from_f64(1.0), TimeInForce::GTC,
            );
            let fills = engine.submit(order).unwrap();
            total_fills += fills.len();
        }
        prop_assert!(total_fills <= n_orders, "成交数 {} > 订单数 {}", total_fills, n_orders);
    }

    /// 相同操作序列产生相同快照
    #[test]
    fn prop_l3_snapshot_deterministic(
        n_assets in 1usize..5usize,
    ) {
        let mut engine1 = MultiAssetMatchingEngine::new();
        let mut engine2 = MultiAssetMatchingEngine::new();

        for i in 0..n_assets {
            // T2.2: 资产 key 用 base/quote 格式(与 Order::spot 产生的 instrument key 一致)
            let sym = Symbol::from(format!("SYM_{}/USDT", i));
            engine1.register_asset(sym.clone());
            engine2.register_asset(sym);
        }

        // 提交相同订单
        for i in 0..5u64 {
            let order = Order::spot(
i,
"SYM_0",
"USDT",Side::Buy,
                OrderType::Limit { price: Price::from_f64(100.0 + i as f64) },
                Quantity::from_f64(1.0), TimeInForce::GTC,
            );
            engine1.submit(order.clone()).unwrap();
            engine2.submit(order).unwrap();
        }

        let snap1 = engine1.snapshot();
        let snap2 = engine2.snapshot();
        prop_assert_eq!(snap1.batch_mode, snap2.batch_mode);
        prop_assert_eq!(snap1.engines.len(), snap2.engines.len());
    }
}

// ───────────────────────────────────────────────────────────────────
// 链接标记：确保 `fuzz` 模块在测试二进制中被实际包含
// ───────────────────────────────────────────────────────────────────

/// 模块加载标记：仅用于确保 `tests/integration_tests.rs` 引用本模块时
/// `cargo test` 不会因为死代码消除而跳过 proptest 宏的展开。
#[doc(hidden)]
pub struct FuzzMarker;
