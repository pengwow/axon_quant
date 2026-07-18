//! 冲击感知撮合引擎的 Criterion 基准测试
//!
//! 运行：`cargo bench -p axon-backtest`
//!
//! 覆盖：
//! - 单笔订单撮合（带/不带冲击）
//! - 不同模型（Linear / PowerLaw）
//! - 多笔订单场景（订单簿深度构建 + 撮合）
//! - TOML 配置加载

use axon_backtest::impact::ImpactedMatchingEngine;
use axon_core::impact::{LinearImpactModel, PowerLawImpactModel};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::types::{Price, Quantity};
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

/// 构造一个限价单
fn make_limit(id: u64, side: Side, price: f64, qty: f64) -> Order {
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

/// 填充卖单簿（创建深度）
fn fill_ask_book(engine: &mut ImpactedMatchingEngine, levels: usize, qty_per_level: f64) {
    for i in 0..levels {
        let price = 100.0 + i as f64 * 0.5;
        let order = make_limit(i as u64 + 1, Side::Sell, price, qty_per_level);
        engine.submit(order);
    }
}

/// 准备 N 档同价卖单（用于持续撮合场景，避免价格分层导致首档吃掉后无法成交）
fn fill_ask_book_same_price(
    engine: &mut ImpactedMatchingEngine,
    start_id: u64,
    levels: usize,
    qty_per_level: f64,
) {
    for i in 0..levels {
        let order = make_limit(start_id + i as u64, Side::Sell, 100.0, qty_per_level);
        engine.submit(order);
    }
}

/// 基准：单笔买单撮合（无冲击）
fn bench_submit_no_impact(c: &mut Criterion) {
    let m: Box<dyn axon_core::impact::ImpactModel> = Box::new(LinearImpactModel::new(0.0));
    let mut engine = ImpactedMatchingEngine::new(m);
    // 准备 100 档同价卖单 + 每次补一档，确保每 iter 都有成交
    fill_ask_book_same_price(&mut engine, 1, 100, 10.0);
    let mut next_id: u64 = 1000;

    c.bench_function("submit_no_impact", |b| {
        b.iter(|| {
            // 每 iter 递增 id，避免重复 id 被去重导致空撮合
            let buy = make_limit(
                black_box(next_id),
                black_box(Side::Buy),
                100.0,
                black_box(1.0),
            );
            let r = engine.submit(buy);
            // 补一档卖单维持订单簿深度 + permanent offset
            let refill = make_limit(next_id + 1_000_000, Side::Sell, 100.0, 1.0);
            engine.submit(refill);
            next_id = next_id.wrapping_add(1);
            black_box(r);
        })
    });
}

/// 基准：单笔买单撮合（线性冲击）
fn bench_submit_linear_impact(c: &mut Criterion) {
    let m: Box<dyn axon_core::impact::ImpactModel> = Box::new(LinearImpactModel::new(0.05));
    let mut engine = ImpactedMatchingEngine::new(m);
    fill_ask_book_same_price(&mut engine, 1, 100, 10.0);
    let mut next_id: u64 = 1000;

    c.bench_function("submit_linear_impact", |b| {
        b.iter(|| {
            let buy = make_limit(
                black_box(next_id),
                black_box(Side::Buy),
                100.0,
                black_box(1.0),
            );
            let r = engine.submit(buy);
            let refill = make_limit(next_id + 1_000_000, Side::Sell, 100.0, 1.0);
            engine.submit(refill);
            next_id = next_id.wrapping_add(1);
            black_box(r);
        })
    });
}

/// 基准：单笔买单撮合（幂律冲击）
fn bench_submit_power_law_impact(c: &mut Criterion) {
    let m: Box<dyn axon_core::impact::ImpactModel> = Box::new(PowerLawImpactModel::new(0.1, 0.5));
    let mut engine = ImpactedMatchingEngine::new(m);
    fill_ask_book_same_price(&mut engine, 1, 100, 10.0);
    let mut next_id: u64 = 1000;

    c.bench_function("submit_power_law_impact", |b| {
        b.iter(|| {
            let buy = make_limit(
                black_box(next_id),
                black_box(Side::Buy),
                100.0,
                black_box(1.0),
            );
            let r = engine.submit(buy);
            let refill = make_limit(next_id + 1_000_000, Side::Sell, 100.0, 1.0);
            engine.submit(refill);
            next_id = next_id.wrapping_add(1);
            black_box(r);
        })
    });
}

/// 基准：不同深度层级数对冲击计算的影响
fn bench_submit_depth_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("submit_depth_scaling");

    for &depth in &[1_usize, 5, 10, 20, 50] {
        let m: Box<dyn axon_core::impact::ImpactModel> =
            Box::new(LinearImpactModel::new(0.05).with_depth(depth));
        let mut engine = ImpactedMatchingEngine::new(m);
        // 准备 100 档同价卖单 + 每 iter 补 1 档
        fill_ask_book_same_price(&mut engine, 1, 100, 10.0);
        let mut next_id: u64 = 1000;

        group.bench_with_input(BenchmarkId::from_parameter(depth), &depth, |b, _| {
            b.iter(|| {
                let buy = make_limit(
                    black_box(next_id),
                    black_box(Side::Buy),
                    100.0,
                    black_box(1.0),
                );
                let r = engine.submit(buy);
                let refill = make_limit(next_id + 1_000_000, Side::Sell, 100.0, 1.0);
                engine.submit(refill);
                next_id = next_id.wrapping_add(1);
                black_box(r);
            })
        });
    }
    group.finish();
}

/// 基准：永久冲击衰减的影响
fn bench_submit_with_decay(c: &mut Criterion) {
    let mut group = c.benchmark_group("submit_with_decay");

    for &decay in &[0.0_f64, 0.1, 0.5, 1.0] {
        let m: Box<dyn axon_core::impact::ImpactModel> = Box::new(LinearImpactModel::new(0.05));
        let mut engine = ImpactedMatchingEngine::new(m).with_permanent_decay(decay);
        fill_ask_book_same_price(&mut engine, 1, 100, 10.0);
        let mut next_id: u64 = 1000;

        group.bench_with_input(BenchmarkId::from_parameter(decay), &decay, |b, _| {
            b.iter(|| {
                let buy = make_limit(
                    black_box(next_id),
                    black_box(Side::Buy),
                    100.0,
                    black_box(1.0),
                );
                let r = engine.submit(buy);
                let refill = make_limit(next_id + 1_000_000, Side::Sell, 100.0, 1.0);
                engine.submit(refill);
                next_id = next_id.wrapping_add(1);
                black_box(r);
            })
        });
    }
    group.finish();
}

/// 基准：多笔订单顺序撮合
fn bench_multi_order_throughput(c: &mut Criterion) {
    let m: Box<dyn axon_core::impact::ImpactModel> = Box::new(LinearImpactModel::new(0.05));
    let mut engine = ImpactedMatchingEngine::new(m);
    fill_ask_book(&mut engine, 10, 10.0);

    c.bench_function("multi_order_throughput_100", |b| {
        b.iter(|| {
            for i in 0..100 {
                let buy = make_limit(10_000 + i, Side::Buy, 100.0, 0.5);
                engine.submit(buy);
            }
        })
    });
}

/// 基准：TOML 配置加载
fn bench_toml_config_load(c: &mut Criterion) {
    let toml_str = r#"
[model]
type = "linear"
coefficient = 0.05
depth_levels = 10
instantaneous_ratio = 0.7

[permanent]
decay = 0.1
"#;

    c.bench_function("toml_config_load", |b| {
        b.iter(|| {
            let _cfg = axon_backtest::impact::ImpactedEngineConfig::from_toml(toml_str).unwrap();
        })
    });
}

/// 基准：construct engine from config
fn bench_engine_construct(c: &mut Criterion) {
    let toml_str = r#"
[model]
type = "linear"
coefficient = 0.05
depth_levels = 10
instantaneous_ratio = 0.7
"#;

    c.bench_function("engine_construct_from_toml", |b| {
        b.iter(|| {
            let cfg = axon_backtest::impact::ImpactedEngineConfig::from_toml(toml_str).unwrap();
            let _engine = cfg.build_engine();
        })
    });
}

criterion_group!(
    benches,
    bench_submit_no_impact,
    bench_submit_linear_impact,
    bench_submit_power_law_impact,
    bench_submit_depth_scaling,
    bench_submit_with_decay,
    bench_multi_order_throughput,
    bench_toml_config_load,
    bench_engine_construct,
);
criterion_main!(benches);
