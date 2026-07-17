//! Phase 4 核心路径 Criterion 基准测试
//!
//! 运行：`cargo bench --bench phase4_bench`
//!
//! 覆盖：
//! - axon-risk：风控检查延迟（check_order, circuit_breaker）
//! - axon-oms：订单提交与状态更新延迟
//! - axon-monitor：指标采集延迟（counter, gauge, histogram）

use std::hint::black_box;

use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::portfolio::{Currency, Portfolio};
use axon_core::types::{Price, Quantity};

use axon_monitor::{LatencyHistogram, MetricsRegistry};
use axon_oms::{OrderManager, OrderStatus};
use axon_risk::{DefaultRiskEngine, RiskConfig, RiskEngine};

use criterion::{Criterion, criterion_group, criterion_main};

fn make_limit_order(price: f64, qty: f64) -> Order {
    Order::spot(
        1,
        "BTC",
        "USDT",
        Side::Buy,
        OrderType::Limit {
            price: Price::from_f64(price),
        },
        Quantity::from_f64(qty),
        TimeInForce::GTC,
    )
}

fn funded_portfolio(cash: f64) -> Portfolio {
    let mut p = Portfolio::new(Currency::USD, 0.001);
    p.deposit(Currency::USD, cash);
    p
}

// ── axon-risk 基准测试 ──

fn bench_risk_check_order(c: &mut Criterion) {
    let engine = DefaultRiskEngine::new(RiskConfig::default());
    let portfolio = funded_portfolio(1_000_000.0);
    let order = make_limit_order(50_000.0, 0.01);

    c.bench_function("risk_check_order", |b| {
        b.iter(|| black_box(engine.check_order(black_box(&order), black_box(&portfolio))))
    });
}

fn bench_risk_check_order_with_circuit_breaker(c: &mut Criterion) {
    let engine = DefaultRiskEngine::new(RiskConfig::default());
    let portfolio = funded_portfolio(1_000_000.0);
    let order = make_limit_order(50_000.0, 0.01);

    c.bench_function("risk_check_order_circuit_breaker_active", |b| {
        b.iter(|| {
            engine.update_daily_pnl(-100_000.0); // 触发熔断器
            black_box(engine.check_order(black_box(&order), black_box(&portfolio)))
        })
    });
}

fn bench_risk_update_pnl(c: &mut Criterion) {
    let engine = DefaultRiskEngine::new(RiskConfig::default());

    c.bench_function("risk_update_daily_pnl", |b| {
        b.iter(|| {
            engine.update_daily_pnl(black_box(-100.0));
            black_box(())
        })
    });
}

fn bench_risk_get_metrics(c: &mut Criterion) {
    let engine = DefaultRiskEngine::new(RiskConfig::default());
    let portfolio = funded_portfolio(1_000_000.0);

    c.bench_function("risk_get_metrics", |b| {
        b.iter(|| black_box(engine.get_metrics(black_box(&portfolio))))
    });
}

// ── axon-oms 基准测试 ──

fn bench_oms_submit(c: &mut Criterion) {
    let oms = OrderManager::new();

    c.bench_function("oms_submit_order", |b| {
        let mut counter = 0u64;
        b.iter(|| {
            counter += 1;
            let order = axon_oms::Order::new(
                "BTC-USDT".into(),
                axon_oms::Side::Buy,
                axon_oms::OrderType::Limit,
                rust_decimal::Decimal::new(1, 3),
                rust_decimal::Decimal::from(50000),
            );
            black_box(oms.submit(black_box(order)))
        })
    });
}

fn bench_oms_submit_with_idempotency(c: &mut Criterion) {
    let oms = OrderManager::new();

    c.bench_function("oms_submit_idempotent", |b| {
        let mut counter = 0u64;
        b.iter(|| {
            counter += 1;
            let order = axon_oms::Order::new(
                "BTC-USDT".into(),
                axon_oms::Side::Buy,
                axon_oms::OrderType::Limit,
                rust_decimal::Decimal::new(1, 3),
                rust_decimal::Decimal::from(50000),
            )
            .with_idempotency_key(format!("key-{}", counter));
            black_box(oms.submit(black_box(order)))
        })
    });
}

fn bench_oms_update_status(c: &mut Criterion) {
    let oms = OrderManager::new();

    c.bench_function("oms_update_status", |b| {
        b.iter_batched(
            || {
                let order = axon_oms::Order::new(
                    "BTC-USDT".into(),
                    axon_oms::Side::Buy,
                    axon_oms::OrderType::Limit,
                    rust_decimal::Decimal::new(1, 3),
                    rust_decimal::Decimal::from(50000),
                );
                oms.submit(order).unwrap()
            },
            |id| black_box(oms.update_status(id, OrderStatus::Acknowledged)),
            criterion::BatchSize::SmallInput,
        )
    });
}

fn bench_oms_snapshot(c: &mut Criterion) {
    let oms = OrderManager::new();
    for i in 0..100 {
        let order = axon_oms::Order::new(
            "BTC-USDT".into(),
            axon_oms::Side::Buy,
            axon_oms::OrderType::Limit,
            rust_decimal::Decimal::new(1, 3),
            rust_decimal::Decimal::from(50000 + i),
        );
        oms.submit(order).unwrap();
    }

    c.bench_function("oms_snapshot_100_orders", |b| {
        b.iter(|| black_box(oms.snapshot()))
    });
}

// ── axon-monitor 基准测试 ──

fn bench_monitor_counter_inc(c: &mut Criterion) {
    let mut registry = MetricsRegistry::new();
    let counter = registry.register_counter("orders_total");

    c.bench_function("monitor_counter_inc", |b| {
        b.iter(|| {
            counter.inc();
            black_box(())
        })
    });
}

fn bench_monitor_counter_inc_by(c: &mut Criterion) {
    let mut registry = MetricsRegistry::new();
    let counter = registry.register_counter("orders_total");

    c.bench_function("monitor_counter_inc_by", |b| {
        b.iter(|| {
            counter.inc_by(black_box(5));
            black_box(())
        })
    });
}

fn bench_monitor_gauge_set(c: &mut Criterion) {
    let mut registry = MetricsRegistry::new();
    let gauge = registry.register_gauge("daily_pnl");

    c.bench_function("monitor_gauge_set", |b| {
        b.iter(|| {
            gauge.set(black_box(-1000.0));
            black_box(())
        })
    });
}

fn bench_monitor_gauge_add(c: &mut Criterion) {
    let mut registry = MetricsRegistry::new();
    let gauge = registry.register_gauge("daily_pnl");

    c.bench_function("monitor_gauge_add", |b| {
        b.iter(|| {
            gauge.add(black_box(-100.0));
            black_box(())
        })
    });
}

fn bench_monitor_histogram_observe(c: &mut Criterion) {
    let hist = LatencyHistogram::default_latency();

    c.bench_function("monitor_histogram_observe", |b| {
        b.iter(|| {
            hist.observe(black_box(150_000.0));
            black_box(())
        })
    });
}

fn bench_monitor_histogram_quantile(c: &mut Criterion) {
    let hist = LatencyHistogram::default_latency();
    for i in 0..1000 {
        hist.observe(i as f64 * 10_000.0);
    }

    c.bench_function("monitor_histogram_quantile_p99", |b| {
        b.iter(|| black_box(hist.quantile(black_box(0.99))))
    });
}

fn bench_monitor_registry_check_alerts(c: &mut Criterion) {
    let mut registry = MetricsRegistry::new();
    registry.add_alert_rule(axon_monitor::AlertRule::Threshold {
        metric_name: "latency_ns".into(),
        condition: axon_monitor::ThresholdCondition::GreaterThan(10_000_000.0),
        severity: axon_monitor::AlertSeverity::Warning,
        message: "latency exceeds 10ms".into(),
    });

    c.bench_function("monitor_check_alerts", |b| {
        b.iter(|| {
            registry.check_alerts(black_box("latency_ns"), black_box(5_000_000.0));
            black_box(())
        })
    });
}

// ── 基准测试组 ──

criterion_group!(
    benches,
    bench_risk_check_order,
    bench_risk_check_order_with_circuit_breaker,
    bench_risk_update_pnl,
    bench_risk_get_metrics,
    bench_oms_submit,
    bench_oms_submit_with_idempotency,
    bench_oms_update_status,
    bench_oms_snapshot,
    bench_monitor_counter_inc,
    bench_monitor_counter_inc_by,
    bench_monitor_gauge_set,
    bench_monitor_gauge_add,
    bench_monitor_histogram_observe,
    bench_monitor_histogram_quantile,
    bench_monitor_registry_check_alerts,
);

criterion_main!(benches);
