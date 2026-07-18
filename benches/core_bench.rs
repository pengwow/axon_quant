//! axon-core 核心路径 Criterion 基准测试
//!
//! 运行：`cargo bench -p axon-core`
//!
//! 覆盖：
//! - 冲击模型：Linear / PowerLaw / Adaptive
//! - 波动率估计：EWMA / Rolling / Garman-Klass
//! - 延迟模型：Constant / Normal / Queue
//! - 订单簿构造与中间价计算
//! - 订单状态机
//! - 事件构建器与事件路由
//! - 费用模型计算

use std::hint::black_box;

use axon_core::event::builder::EventBuilder;
use axon_core::event::handler::EventHandler;
use axon_core::event::market::MarketDataPayload;
use axon_core::event::router::EventRouter;
use axon_core::event::types::{Event, EventType};
use axon_core::fee::model::{FeeModel, FeePosition, FeeTrade, TieredFeeModel};
use axon_core::fee::role::TradeRole;
use axon_core::fee::table::FeeTable;
use axon_core::fee::types::{ExchangeId, FeeType, VolumeTier};
use axon_core::impact::{AdaptiveImpactModel, ImpactModel, LinearImpactModel, PowerLawImpactModel};
use axon_core::latency::{ConstantLatencyModel, LatencyModel, PathType, QueueLatencyModel};
use axon_core::market::Side;
use axon_core::market::orderbook::{OrderBookLevel, OrderBookSnapshot};
use axon_core::market::{Bar, Tick};
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::time::Timestamp;
use axon_core::types::{Price, Quantity};
use axon_core::volatility::{EwmaVolatility, GarmanKlassVolatility, OhlcBar, VolatilityEstimator};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

// ─── 辅助函数 ─────────────────────────────────────────

/// 构造一个有 10 档买卖盘的订单簿快照
fn make_sample_orderbook(levels: usize) -> OrderBookSnapshot {
    let bids: Vec<OrderBookLevel> = (0..levels)
        .map(|i| {
            OrderBookLevel::new(
                Price::from_f64(100.0 - i as f64 * 0.5),
                Quantity::from_f64(10.0),
            )
        })
        .collect();
    let asks: Vec<OrderBookLevel> = (0..levels)
        .map(|i| {
            OrderBookLevel::new(
                Price::from_f64(101.0 + i as f64 * 0.5),
                Quantity::from_f64(10.0),
            )
        })
        .collect();
    OrderBookSnapshot {
        timestamp: Timestamp::from_nanos(0),
        bids,
        asks,
    }
}

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

// ─── 冲击模型基准 ───────────────────────────────────

fn bench_linear_impact(c: &mut Criterion) {
    let m = LinearImpactModel::new(0.05);
    let ob = make_sample_orderbook(20);

    c.bench_function("impact_linear", |b| {
        b.iter(|| {
            let impact = m.compute_impact(
                black_box(Quantity::from_f64(10.0)),
                black_box(Side::Buy),
                black_box(&ob),
            );
            black_box(impact);
        })
    });
}

fn bench_power_law_impact(c: &mut Criterion) {
    let m = PowerLawImpactModel::new(0.1, 0.5);
    let ob = make_sample_orderbook(20);

    c.bench_function("impact_power_law", |b| {
        b.iter(|| {
            let impact = m.compute_impact(
                black_box(Quantity::from_f64(10.0)),
                black_box(Side::Buy),
                black_box(&ob),
            );
            black_box(impact);
        })
    });
}

fn bench_adaptive_impact(c: &mut Criterion) {
    let m =
        AdaptiveImpactModel::new(Box::new(LinearImpactModel::new(0.05)), 2.0).with_volatility(0.5);
    let ob = make_sample_orderbook(20);

    c.bench_function("impact_adaptive", |b| {
        b.iter(|| {
            let impact = m.compute_impact(
                black_box(Quantity::from_f64(10.0)),
                black_box(Side::Buy),
                black_box(&ob),
            );
            black_box(impact);
        })
    });
}

fn bench_impact_depth_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("impact_depth_scaling");
    for &depth in &[1_usize, 5, 10, 20, 50, 100] {
        let m = LinearImpactModel::new(0.05);
        let ob = make_sample_orderbook(depth);
        group.bench_with_input(BenchmarkId::from_parameter(depth), &depth, |b, _| {
            b.iter(|| {
                let impact = m.compute_impact(
                    black_box(Quantity::from_f64(10.0)),
                    black_box(Side::Buy),
                    black_box(&ob),
                );
                black_box(impact);
            })
        });
    }
    group.finish();
}

fn bench_impact_quantity_scaling(c: &mut Criterion) {
    let m = LinearImpactModel::new(0.05);
    let ob = make_sample_orderbook(20);
    let mut group = c.benchmark_group("impact_quantity_scaling");
    for &qty in &[0.1_f64, 1.0, 10.0, 100.0, 1_000.0] {
        group.bench_with_input(BenchmarkId::from_parameter(qty), &qty, |b, _| {
            b.iter(|| {
                let impact = m.compute_impact(
                    black_box(Quantity::from_f64(qty)),
                    black_box(Side::Buy),
                    black_box(&ob),
                );
                black_box(impact);
            })
        });
    }
    group.finish();
}

// ─── 波动率估计器基准 ─────────────────────────────────

fn bench_ewma_update(c: &mut Criterion) {
    let mut e = EwmaVolatility::riskmetrics().unwrap();
    e.reset_with_variance(0.01);
    let mut group = c.benchmark_group("volatility_ewma_update");
    for &n in &[100_usize, 1_000, 10_000, 100_000] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter(|| {
                for i in 0..n {
                    let r = ((i as f64).sin()) * 0.01;
                    e.update(black_box(r)).unwrap();
                }
                black_box(e.variance());
            })
        });
    }
    group.finish();
}

fn bench_rolling_update(c: &mut Criterion) {
    use axon_core::volatility::RollingVolatility;
    let mut e = RollingVolatility::new(64).unwrap();
    let mut group = c.benchmark_group("volatility_rolling_update");
    for &n in &[100_usize, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter(|| {
                for i in 0..n {
                    let r = ((i as f64).sin()) * 0.01;
                    e.update(black_box(r)).unwrap();
                }
                let _ = black_box(e.variance());
            })
        });
    }
    group.finish();
}

fn bench_garman_klass(c: &mut Criterion) {
    let bar = OhlcBar {
        open: 100.0,
        high: 101.0,
        low: 99.5,
        close: 100.5,
    };
    c.bench_function("volatility_gk", |b| {
        b.iter(|| {
            let v = GarmanKlassVolatility::gk_variance(black_box(&bar));
            black_box(v);
        })
    });
}

// ─── 延迟模型基准 ─────────────────────────────────────

fn bench_constant_latency(c: &mut Criterion) {
    let m = ConstantLatencyModel::uniform(std::time::Duration::from_millis(10));
    c.bench_function("latency_constant", |b| {
        b.iter(|| {
            let d = m.sample_delay(black_box(PathType::OrderSubmit));
            black_box(d);
        })
    });
}

fn bench_normal_latency(c: &mut Criterion) {
    use axon_core::latency::LatencyModelFactory;
    let m = LatencyModelFactory::normal(5.0, 1.0);
    c.bench_function("latency_normal", |b| {
        b.iter(|| {
            let d = m.sample_delay(black_box(PathType::OrderSubmit));
            black_box(d);
        })
    });
}

fn bench_queue_latency(c: &mut Criterion) {
    let m = QueueLatencyModel::new(
        std::time::Duration::from_millis(10),
        std::time::Duration::from_millis(1),
    );
    c.bench_function("latency_queue", |b| {
        b.iter(|| {
            let d = m.sample_delay(black_box(PathType::OrderSubmit));
            black_box(d);
        })
    });
}

// ─── 订单簿基准 ───────────────────────────────────────

fn bench_orderbook_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook_construction");
    for &levels in &[10_usize, 50, 100, 500, 1_000] {
        group.bench_with_input(BenchmarkId::from_parameter(levels), &levels, |b, _| {
            b.iter(|| {
                let ob = make_sample_orderbook(black_box(levels));
                black_box(ob);
            })
        });
    }
    group.finish();
}

fn bench_orderbook_mid_price(c: &mut Criterion) {
    let ob = make_sample_orderbook(100);
    c.bench_function("orderbook_mid_price", |b| {
        b.iter(|| {
            let mid = ob.mid_price();
            black_box(mid);
        })
    });
}

fn bench_orderbook_spread(c: &mut Criterion) {
    let ob = make_sample_orderbook(100);
    c.bench_function("orderbook_spread", |b| {
        b.iter(|| {
            let s = ob.spread();
            black_box(s);
        })
    });
}

// ─── 订单创建基准 ─────────────────────────────────────

fn bench_order_creation(c: &mut Criterion) {
    c.bench_function("order_creation_limit", |b| {
        b.iter(|| {
            let o = make_limit(black_box(1), black_box(Side::Buy), 100.0, 1.0);
            black_box(o);
        })
    });
}

// ─── 事件系统基准 ─────────────────────────────────────

/// 构造一个 Tick 事件（市场数据）
fn make_tick_event(builder: &mut EventBuilder, ts: Timestamp, price: f64, qty: f64) -> Event {
    let tick = Tick::new(
        ts,
        Price::from_f64(price),
        Quantity::from_f64(qty),
        Side::Buy,
    );
    builder.market_data(ts, MarketDataPayload::Tick(tick))
}

/// 构造一个 K线 事件
fn make_bar_event(
    builder: &mut EventBuilder,
    ts: Timestamp,
    o: f64,
    h: f64,
    l: f64,
    c: f64,
) -> Event {
    let bar = Bar {
        timestamp: ts,
        open: Price::from_f64(o),
        high: Price::from_f64(h),
        low: Price::from_f64(l),
        close: Price::from_f64(c),
        volume: Quantity::from_f64(100.0),
    };
    builder.market_data(ts, MarketDataPayload::Bar(bar))
}

/// 轻量级事件处理器：仅计数，不累积 event
///
/// 用于 router benchmark 避免 `EventCollector` 反复 push event clone 撑爆内存。
struct CountingHandler {
    count: u64,
    interested: EventType,
}

impl CountingHandler {
    fn new(interested: EventType) -> Self {
        Self {
            count: 0,
            interested,
        }
    }
}

impl EventHandler for CountingHandler {
    fn on_event(&mut self, _event: &Event) {
        self.count = self.count.wrapping_add(1);
    }
    fn event_types(&self) -> EventType {
        self.interested
    }
}

fn bench_event_builder_tick(c: &mut Criterion) {
    let mut builder = EventBuilder::new(0);
    let ts = Timestamp::from_nanos(1_000);
    c.bench_function("event_builder_tick", |b| {
        b.iter(|| {
            let evt = make_tick_event(black_box(&mut builder), black_box(ts), 100.0, 10.0);
            black_box(evt);
        })
    });
}

fn bench_event_builder_bar(c: &mut Criterion) {
    let mut builder = EventBuilder::new(0);
    let ts = Timestamp::from_nanos(1_000);
    c.bench_function("event_builder_bar", |b| {
        b.iter(|| {
            let evt = make_bar_event(
                black_box(&mut builder),
                black_box(ts),
                100.0,
                101.0,
                99.0,
                100.5,
            );
            black_box(evt);
        })
    });
}

fn bench_event_builder_throughput(c: &mut Criterion) {
    // 衡量连续构造 N 个事件的总耗时
    // 注意：必须 black_box 每次迭代的价格，避免编译器将循环优化为常量折叠
    let mut group = c.benchmark_group("event_builder_throughput");
    for &n in &[100_usize, 1_000] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter(|| {
                let mut builder = EventBuilder::new(0);
                let ts = Timestamp::from_nanos(0);
                for i in 0..n {
                    // 用 black_box 包裹 i 防止优化
                    let _ = make_tick_event(
                        &mut builder,
                        ts,
                        black_box(100.0 + i as f64),
                        black_box(1.0),
                    );
                }
                black_box(builder.next_seq());
            })
        });
    }
    group.finish();
}

fn bench_event_router_dispatch(c: &mut Criterion) {
    // 单事件 dispatch 到 5 个订阅者（使用不累积的 CountingHandler）
    let mut router = EventRouter::new();
    for _ in 0..5 {
        router.register(Box::new(CountingHandler::new(EventType::ALL)));
    }
    let ts = Timestamp::from_nanos(1_000);
    let mut builder = EventBuilder::new(0);
    let evt = make_tick_event(&mut builder, ts, 100.0, 10.0);

    c.bench_function("event_router_dispatch_5", |b| {
        b.iter(|| {
            router.dispatch(black_box(&evt));
        })
    });
}

fn bench_event_router_dispatch_batch(c: &mut Criterion) {
    // 批量 dispatch：注意 batch 大小不宜过大，避免单 iter 大量分配
    // 改为 100 而非 1000 防止 router 内部 + on_event 累积放大
    let mut router = EventRouter::new();
    for _ in 0..3 {
        router.register(Box::new(CountingHandler::new(EventType::ALL)));
    }
    let mut builder = EventBuilder::new(0);
    let events: Vec<Event> = (0..100)
        .map(|i| {
            let ts = Timestamp::from_nanos(i as i64);
            make_tick_event(&mut builder, ts, 100.0 + i as f64, 1.0)
        })
        .collect();

    c.bench_function("event_router_dispatch_batch_100", |b| {
        b.iter(|| {
            router.dispatch_batch(black_box(&events));
        })
    });
}

fn bench_event_router_subscribers_scaling(c: &mut Criterion) {
    // 不同订阅者数量的 dispatch 开销（使用不累积的 CountingHandler）
    let mut group = c.benchmark_group("event_router_subscribers_scaling");
    let ts = Timestamp::from_nanos(1_000);
    let mut builder = EventBuilder::new(0);
    let evt = make_tick_event(&mut builder, ts, 100.0, 10.0);

    for &n in &[1_usize, 5, 10, 50] {
        let mut router = EventRouter::new();
        for _ in 0..n {
            router.register(Box::new(CountingHandler::new(EventType::ALL)));
        }
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                router.dispatch(black_box(&evt));
            })
        });
    }
    group.finish();
}

// ─── 费用模型基准 ─────────────────────────────────────

/// 注册一个带 5 档阶梯的 Binance 默认费率表（与生产等价）
fn build_fee_model() -> TieredFeeModel {
    use rust_decimal_macros::dec;

    let mut model = TieredFeeModel::new();
    // 简化的 5 档费率表
    let table = FeeTable::new(ExchangeId::Binance)
        .add_tier(VolumeTier {
            min_volume: dec!(0),
            label: "VIP 0".to_string(),
            maker_fee: FeeType::Percentage(dec!(0.0010)),
            taker_fee: FeeType::Percentage(dec!(0.0010)),
        })
        .add_tier(VolumeTier {
            min_volume: dec!(50_000),
            label: "VIP 1".to_string(),
            maker_fee: FeeType::Percentage(dec!(0.0009)),
            taker_fee: FeeType::Percentage(dec!(0.0010)),
        })
        .add_tier(VolumeTier {
            min_volume: dec!(250_000),
            label: "VIP 2".to_string(),
            maker_fee: FeeType::Percentage(dec!(0.0008)),
            taker_fee: FeeType::Percentage(dec!(0.0009)),
        })
        .add_tier(VolumeTier {
            min_volume: dec!(1_000_000),
            label: "VIP 3".to_string(),
            maker_fee: FeeType::Percentage(dec!(0.0007)),
            taker_fee: FeeType::Percentage(dec!(0.0008)),
        })
        .add_tier(VolumeTier {
            min_volume: dec!(10_000_000),
            label: "VIP 4".to_string(),
            maker_fee: FeeType::Percentage(dec!(0.0006)),
            taker_fee: FeeType::Percentage(dec!(0.0007)),
        });
    model.register_exchange(table);
    model.update_volume(ExchangeId::Binance, dec!(500_000));
    model
}

fn bench_fee_calculate_taker(c: &mut Criterion) {
    use rust_decimal_macros::dec;
    let model = build_fee_model();
    let trade = FeeTrade::new(1, dec!(50_000), dec!(0.5));

    c.bench_function("fee_calculate_taker", |b| {
        b.iter(|| {
            let _ = model
                .calculate_fee(
                    black_box(ExchangeId::Binance),
                    black_box(&trade),
                    black_box(TradeRole::Taker),
                )
                .unwrap();
        })
    });
}

fn bench_fee_calculate_maker(c: &mut Criterion) {
    use rust_decimal_macros::dec;
    let model = build_fee_model();
    let trade = FeeTrade::new(1, dec!(50_000), dec!(0.5));

    c.bench_function("fee_calculate_maker", |b| {
        b.iter(|| {
            let _ = model
                .calculate_fee(
                    black_box(ExchangeId::Binance),
                    black_box(&trade),
                    black_box(TradeRole::Maker),
                )
                .unwrap();
        })
    });
}

fn bench_fee_calculate_throughput(c: &mut Criterion) {
    use rust_decimal_macros::dec;
    let model = build_fee_model();
    let mut group = c.benchmark_group("fee_calculate_throughput");
    for &n in &[100_usize, 1_000, 10_000] {
        let trades: Vec<FeeTrade> = (0..n)
            .map(|i| FeeTrade::new(i as u64, dec!(50_000), dec!(0.5)))
            .collect();
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                for t in &trades {
                    let _ = model
                        .calculate_fee(
                            black_box(ExchangeId::Binance),
                            black_box(t),
                            black_box(TradeRole::Taker),
                        )
                        .unwrap();
                }
            })
        });
    }
    group.finish();
}

fn bench_fee_position_funding(c: &mut Criterion) {
    use rust_decimal_macros::dec;
    let model = build_fee_model();
    let position = FeePosition::new(dec!(1.5), dec!(50_000));

    c.bench_function("fee_calculate_funding", |b| {
        b.iter(|| {
            let _ = model.calculate_funding(black_box(&position), black_box(dec!(0.0001)));
        })
    });
}

// 引用 VolumeTier（build_fee_model 中已使用，确保导入存在）

criterion_group!(
    benches,
    // 冲击模型
    bench_linear_impact,
    bench_power_law_impact,
    bench_adaptive_impact,
    bench_impact_depth_scaling,
    bench_impact_quantity_scaling,
    // 波动率
    bench_ewma_update,
    bench_rolling_update,
    bench_garman_klass,
    // 延迟
    bench_constant_latency,
    bench_normal_latency,
    bench_queue_latency,
    // 订单簿
    bench_orderbook_construction,
    bench_orderbook_mid_price,
    bench_orderbook_spread,
    // 订单
    bench_order_creation,
    // 事件
    bench_event_builder_tick,
    bench_event_builder_bar,
    bench_event_builder_throughput,
    bench_event_router_dispatch,
    bench_event_router_dispatch_batch,
    bench_event_router_subscribers_scaling,
    // 费用
    bench_fee_calculate_taker,
    bench_fee_calculate_maker,
    bench_fee_calculate_throughput,
    bench_fee_position_funding,
);
criterion_main!(benches);
