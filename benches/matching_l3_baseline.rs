//! 0.8.0 Phase 3 A3.0:L1 / L2 / L3 `submit` 延迟基线
//!
//! 运行:`cargo bench -p axon-backtest --bench matching_l3_baseline`
//!
//! ## 目的
//!
//! 在 0.8.0 Phase 3 重写 `MultiAssetMatchingEngine`(A1)和性能优化(A3)
//! 之前,先采集 L1 / L2 / L3 三层 `submit` 延迟的 0.8.0 起点数据。
//! 这是 A1.3 性能 gate(`L3 latency ≤ 2x L2`)和 A3 hot-path 优化目标
//! (`inner.submit` ≤ 50µs)的对比基准。
//!
//! ## 场景覆盖
//!
//! | bench | 引擎 | instrument 数 | ask 深度 | 含义 |
//! |-------|------|-------------|---------|------|
//! | `l1_submit` | L1 | 1 | 100 档 | 最底层撮合 L1 |
//! | `l2_submit` | L2 (L1+index) | 1 | 100 档 | L2 wrapping overhead |
//! | `l3_single_asset_submit` | L3 | 1 | 100 档 | L3 单 asset 路由 + HashMap 查 |
//! | `l3_multi_asset_submit` | L3 | 5 | 100 档 | L3 多 asset 路由 |
//! | `l3_depth_scaling` | L3 | 1 | 10/50/100/500 档 | 深度敏感性 |
//!
//! ## 复现性
//!
//! - 使用 `submit + refill` 模式(同 `impact_bench.rs`),保持订单簿深度稳定
//! - `black_box()` 防止编译器优化掉 Order 构造
//! - 每次 iter 推进 `next_id`,避免 id collision 导致 submit 路径分叉
//!
//! ## 验收
//!
//! - L1 < L2 < L3(单 asset)延迟单调(否则 L2 wrapping 出 bug)
//! - L3 单 asset ≈ L3 multi asset(HashMap 查是 O(1),差异应在 5% 内)
//! - L3 深度 100 档 vs 500 档:差异 < 2x(否则价簿线性扫描 O(n))

use std::hint::black_box;

use axon_backtest::matching::engine::MatchingEngine;
use axon_backtest::matching::l2::L2MatchingEngine;
use axon_backtest::matching::l3::engine_l3::MultiAssetMatchingEngine;
use axon_backtest::matching::L1MatchingEngine;
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::types::{Instrument, Price, Quantity, SpotInstrument};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

// ─── 辅助函数 ─────────────────────────────────────────

/// 构造一个 spot 限价单(instrument 是 `BTC/USDT`)
fn make_spot_limit(id: u64, side: Side, price: f64, qty: f64) -> Order {
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

/// 构造 BTC/USDT spot Instrument
fn btc_spot() -> Instrument {
    Instrument::Spot(SpotInstrument {
        base: "BTC".into(),
        quote: "USDT".into(),
    })
}

/// 填充卖单簿(创建 N 档同价深度,避免首档吃掉后无单可撮)
fn fill_ask_book_same_price<E: MatchingEngine>(engine: &mut E, start_id: u64, count: usize, price: f64, qty: f64) {
    for i in 0..count {
        let order = make_spot_limit(start_id + i as u64, Side::Sell, price, qty);
        engine.submit(order);
    }
}

/// 填充 N 档分层卖单簿(价位步进 0.5)
fn fill_ask_book_layered<E: MatchingEngine>(engine: &mut E, start_id: u64, levels: usize, qty_per_level: f64) {
    for i in 0..levels {
        let price = 100.0 + i as f64 * 0.5;
        let order = make_spot_limit(start_id + i as u64, Side::Sell, price, qty_per_level);
        engine.submit(order);
    }
}

// ─── 基准 ─────────────────────────────────────────────

/// L1:最底层 price-time priority
fn bench_l1_submit(c: &mut Criterion) {
    let mut engine = L1MatchingEngine::new();
    fill_ask_book_same_price(&mut engine, 1, 100, 100.0, 10.0);
    let mut next_id: u64 = 1000;

    c.bench_function("l1_submit", |b| {
        b.iter(|| {
            let buy = make_spot_limit(black_box(next_id), black_box(Side::Buy), 100.0, 1.0);
            let r = engine.submit(buy);
            // 补一档卖,保持簿深度
            let refill = make_spot_limit(next_id + 1_000_000, Side::Sell, 100.0, 1.0);
            engine.submit(refill);
            next_id = next_id.wrapping_add(1);
            black_box(r);
        })
    });
}

/// L2:L1 + order_index + stats 的 wrapping overhead
fn bench_l2_submit(c: &mut Criterion) {
    let mut engine = L2MatchingEngine::new();
    fill_ask_book_same_price(&mut engine, 1, 100, 100.0, 10.0);
    let mut next_id: u64 = 1000;

    c.bench_function("l2_submit", |b| {
        b.iter(|| {
            let buy = make_spot_limit(black_box(next_id), black_box(Side::Buy), 100.0, 1.0);
            let r = engine.submit(buy);
            let refill = make_spot_limit(next_id + 1_000_000, Side::Sell, 100.0, 1.0);
            engine.submit(refill);
            next_id = next_id.wrapping_add(1);
            black_box(r);
        })
    });
}

/// L3:单 instrument(HashMap 路由 + L2 wrapping)
///
/// 注:这里用 `engine.submit(order)`(inherent method)而非
/// `MatchingEngine::submit` trait method,因为 trait 路径在 L3 上是
/// 透传到 inherent + 加了一层 Result→SubmitResult 转译。多 asset bench
/// 为了对比一致性也用 inherent。L1/L2 bench 用 trait method 是因为
/// L1/L2 没有 inherent method overload(签名相同)。
fn bench_l3_single_asset_submit(c: &mut Criterion) {
    let mut engine = MultiAssetMatchingEngine::new();
    engine.register_instrument(btc_spot());
    fill_ask_book_same_price(&mut engine, 1, 100, 100.0, 10.0);
    let mut next_id: u64 = 1000;

    c.bench_function("l3_single_asset_submit", |b| {
        b.iter(|| {
            let buy = make_spot_limit(black_box(next_id), black_box(Side::Buy), 100.0, 1.0);
            let r = engine.submit(buy);
            // 注:L3::submit 失败会返回 Err,这里用 unwrap 防止 panic 污染 bench
            let refill = make_spot_limit(next_id + 1_000_000, Side::Sell, 100.0, 1.0);
            let _ = engine.submit(refill);
            next_id = next_id.wrapping_add(1);
            let _ = black_box(r);
        })
    });
}

/// 构造一个给定 base/quote 的 spot 限价单(用于 multi-asset bench)
fn make_spot_limit_for(
    id: u64,
    base: &str,
    quote: &str,
    side: Side,
    price: f64,
    qty: f64,
) -> Order {
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

/// L3:5 个 instrument(主路由压力,HashMap 查 O(1) 摊销)
fn bench_l3_multi_asset_submit(c: &mut Criterion) {
    let instruments: Vec<(&str, &str)> = vec![
        ("BTC", "USDT"),
        ("ETH", "USDT"),
        ("SOL", "USDT"),
        ("AVAX", "USDT"),
        ("MATIC", "USDT"),
    ];

    let mut engine = MultiAssetMatchingEngine::new();
    for (base, quote) in &instruments {
        let inst = Instrument::Spot(SpotInstrument {
            base: (*base).into(),
            quote: (*quote).into(),
        });
        engine.register_instrument(inst);
        // 用 make_spot_limit_for 预填 100 档卖单
        for i in 0..100_usize {
            let order = make_spot_limit_for(
                (i + 1) as u64,
                base,
                quote,
                Side::Sell,
                100.0,
                10.0,
            );
            let _ = engine.submit(order);
        }
    }
    // 锁定 BTC 为 primary(无 instrument 参数的 trait 方法走 primary)
    let btc_inst = Instrument::Spot(SpotInstrument {
        base: "BTC".into(),
        quote: "USDT".into(),
    });
    let mut engine = engine.with_primary(btc_inst);
    let mut next_id: u64 = 1000;
    // 在 5 个 instrument 间轮转,模拟真实多 leg 路由负载
    let mut tick: usize = 0;

    c.bench_function("l3_multi_asset_submit", |b| {
        b.iter(|| {
            let (base, quote) = instruments[tick % instruments.len()];
            let buy = make_spot_limit_for(
                black_box(next_id),
                base,
                quote,
                black_box(Side::Buy),
                100.0,
                1.0,
            );
            let r = engine.submit(buy);
            let refill = make_spot_limit_for(
                next_id + 1_000_000,
                base,
                quote,
                Side::Sell,
                100.0,
                1.0,
            );
            let _ = engine.submit(refill);
            tick = tick.wrapping_add(1);
            next_id = next_id.wrapping_add(1);
            let _ = black_box(r);
        })
    });
}

/// L3 深度敏感性:10 / 50 / 100 / 500 档
///
/// 目的:暴露价簿线性扫描(若 BTreeMap 匹配时档位遍历 O(n) 而非 O(log n))
fn bench_l3_depth_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("l3_depth_scaling");

    for &depth in &[10_usize, 50, 100, 500] {
        let mut engine = MultiAssetMatchingEngine::new();
        engine.register_instrument(btc_spot());
        // 用分层簿(各档不同价)测撮合遍历深度
        fill_ask_book_layered(&mut engine, 1, depth, 10.0);
        let mut next_id: u64 = 1_000_000;

        group.bench_with_input(BenchmarkId::from_parameter(depth), &depth, |b, _| {
            b.iter(|| {
                // 买单在最低卖价 100.0 → 吃首档
                let buy = make_spot_limit(black_box(next_id), black_box(Side::Buy), 100.0, 1.0);
                let r = engine.submit(buy);
                // 补一档 100.0 卖单,保持簿深度
                let refill = make_spot_limit(next_id + 10_000_000, Side::Sell, 100.0, 1.0);
                let _ = engine.submit(refill);
                next_id = next_id.wrapping_add(1);
                let _ = black_box(r);
            })
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_l1_submit,
    bench_l2_submit,
    bench_l3_single_asset_submit,
    bench_l3_multi_asset_submit,
    bench_l3_depth_scaling,
);
criterion_main!(benches);
