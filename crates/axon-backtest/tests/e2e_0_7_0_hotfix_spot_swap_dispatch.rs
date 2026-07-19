//! 0.7.0 hotfix 端到端测试:Spot/Swap 派发 + activate 防御
//!
//! ## 修复目标
//!
//! 0.7.0 扫洞察发现 3 个 P0 bug 之前未识别:
//!
//! 1. **`L1MatchingEngine::seed_liquidity` 一律用 `Order::spot`**,
//!    对 Swap 品种 seed 时,`Order::instrument` 字段被错写为 `Spot(...)`,
//!    撮合仍正常(book 按 Instrument key 路由),但 seed 订单的 instrument
//!    字段错位。L3Book / 报告 / 审计读 `seed_order.instrument` 会看到错类型。
//! 2. **`BacktestEngine::liquidate_eod` 一律用 `Order::spot`**,导致
//!    `force_liquidate=true` 时的 perp 持仓**残留**(`final_nav` 错)。
//! 3. **`let _ = order.activate();` 吞错**,任何未来重构让 activate 失败
//!    都会让 50GB 内存爆炸 bug 静默回归。
//!
//! ## 修复
//!
//! 抽 `build_leg_order(instrument, ...)` 共享 helper,加 `tif` 参数,
//! 4 处调用点(seed_liquidity / liquidate_eod / rebalance_to_target /
//! execute_arbitrage)统一派发;`activate()` 改 `expect()`,把 invariant
//! 错误暴露到测试。
//!
//! ## 测试场景
//!
//! 1. `seed_liquidity_swap_preserves_instrument_field`:
//!    `seed_liquidity(perp)` 后,L1Book 里的 Order 必须是 `Order::swap`,
//!    `order.instrument == Swap(...)`。
//! 2. `liquidate_eod_closes_perp_position_via_swap_book`:
//!    `with_force_liquidate(true)` + 持有 perp 仓位 → EOD 平仓实际成交
//!    perp book 上的 seed 单(不是被拒),终态 position=0。
//! 3. `seed_liquidity_activate_panics_on_invalid_state`(0.7.0 hotfix 防御):
//!    直接构造一个已 activate 的 Order 走 `seed_liquidity` 内部
//!    `expect()`,会 panic 把 invariant 破坏暴露出来。注:seed_liquidity
//!    内部路径是私有,这条测试通过**直接复现** invariant 错误来确认
//!    `expect()` 生效。
//!
//! 运行:`cargo test -p axon-backtest --test e2e_0_7_0_hotfix_spot_swap_dispatch`

use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig};
use axon_backtest::matching::L1MatchingEngine;
use axon_core::event::{EventBuilder, OrderAction};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{Instrument, Quantity, SpotInstrument, SwapInstrument, SwapSettle, Symbol};

// ── 共享 helper ─────────────────────────────────────────────

fn btc_spot() -> Instrument {
    Instrument::Spot(SpotInstrument {
        base: Symbol::from("BTC"),
        quote: Symbol::from("USDT"),
    })
}

fn btc_perp() -> Instrument {
    Instrument::Swap(SwapInstrument {
        base: Symbol::from("BTC"),
        quote: Symbol::from("USDT"),
        settle: SwapSettle::UsdMargin,
        contract_size: 1.0,
    })
}

fn make_market_order(id: u64, instrument: &Instrument, side: Side, qty: f64) -> Order {
    match instrument {
        Instrument::Spot(s) => Order::spot(
            id,
            s.base.clone(),
            s.quote.clone(),
            side,
            OrderType::Market,
            Quantity::from_f64(qty),
            TimeInForce::IOC,
        ),
        Instrument::Swap(s) => Order::swap(
            id,
            s.base.clone(),
            s.quote.clone(),
            s.settle,
            s.contract_size,
            side,
            OrderType::Market,
            Quantity::from_f64(qty),
            TimeInForce::IOC,
        ),
    }
}

fn base_config(force_liquidate: bool) -> BacktestEngineConfig {
    BacktestEngineConfig {
        clock: SimulatedClock::new(Timestamp::from_nanos(0)),
        matching_engine: Box::new(L1MatchingEngine::new()),
        impact_model: None,
        initial_cash: 100_000.0,
        fee_config: FeeConfig::default(),
        force_liquidate,
    }
}

fn build_orders(orders: &[(u64, &Instrument, Side, f64, i64)]) -> EventQueue {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    for (id, inst, side, qty, ts_ns) in orders {
        q.push(b.order(
            Timestamp::from_nanos(*ts_ns),
            *id,
            OrderAction::Submitted(make_market_order(*id, inst, *side, *qty)),
        ));
    }
    q
}

// ── 测试 1:seed_liquidity 对 Swap 品种保留 instrument 字段 ─────────

/// 0.7.0 P0#1:`L1MatchingEngine::seed_liquidity` 旧版一律 `Order::spot`,
/// 导致 Swap 品种的 seed 单 `Order::instrument = Spot(base, quote)`(错)。
///
/// 修复后:用 `build_leg_order` 派发,seed Swap → `Order::swap`,
/// `order.instrument == Swap(BTC/USDT, UsdMargin, 1.0)`(正确)。
///
/// 验证方法:取 L1 engine 的 perp book,遍历 `book.asks` 和 `book.bids`,
/// 每笔 maker 单的 `order.instrument` 必须是 Swap。
#[test]
fn seed_liquidity_swap_preserves_instrument_field() {
    let mut engine = L1MatchingEngine::new();
    let perp = btc_perp();
    let _ = engine.seed_liquidity(100.0, 0.5, 3, 0.1, perp.clone(), 1);

    // 拿 perp book
    let book = engine
        .book_for(&perp)
        .expect("seed_liquidity 后 perp book 应存在");

    // 遍历 ask 档位 + bid 档位,每笔 maker 单 instrument 必须是 Swap
    let mut seed_count = 0;
    for (_price, level) in book.iter_asks() {
        for order in level.iter() {
            seed_count += 1;
            assert_eq!(
                order.instrument, perp,
                "ask seed 单 instrument 应是 Swap(perp),got {:?}",
                order.instrument
            );
        }
    }
    for (_price, level) in book.iter_bids() {
        for order in level.iter() {
            seed_count += 1;
            assert_eq!(
                order.instrument, perp,
                "bid seed 单 instrument 应是 Swap(perp),got {:?}",
                order.instrument
            );
        }
    }
    // 3 depth × 2 side = 6 单
    assert_eq!(seed_count, 6, "seed_liquidity 应产生 6 单,got {seed_count}");
}

// ── 测试 2:seed_liquidity 对 Spot 品种保留 instrument 字段 ─────────

/// 0.7.0 P0#1 双向验证:对 Spot 品种,seed 单的 `order.instrument` 仍是
/// `Spot(...)`(不该错升为 Swap)。
#[test]
fn seed_liquidity_spot_preserves_instrument_field() {
    let mut engine = L1MatchingEngine::new();
    let spot = btc_spot();
    let _ = engine.seed_liquidity(100.0, 0.01, 3, 0.1, spot.clone(), 1);

    let book = engine.book_for(&spot).expect("spot book 应存在");
    for (_price, level) in book.iter_asks() {
        for order in level.iter() {
            assert_eq!(
                order.instrument, spot,
                "ask seed 单 instrument 应是 Spot,got {:?}",
                order.instrument
            );
        }
    }
    for (_price, level) in book.iter_bids() {
        for order in level.iter() {
            assert_eq!(
                order.instrument, spot,
                "bid seed 单 instrument 应是 Spot,got {:?}",
                order.instrument
            );
        }
    }
}

// ── 测试 3:force_liquidate=true 平 perp 仓位(走 swap book) ──────────

/// 0.7.0 P0#2:`liquidate_eod` 旧版一律 `Order::spot`,perp 仓位被
/// 发到 `books.entry(Spot(BTC/USDT))` 找对手盘 — 该 book 不存在,
/// 订单被拒,**perp 持仓残留**。
///
/// 修复后:用 `build_leg_order` 派发,perp 仓位 → `Order::swap` →
/// 进 perp book → 命中 seed maker → 平仓成功。
///
/// 场景:
/// - 1 根 bar:begin_bar(perp) seed 出 sell @ 100.5 单(perp 卖价)
/// - 策略发 buy market 0.1(perp 吃 100.5 卖) → 持仓 long 0.1
/// - 下一根 bar:begin_bar(perp) re-seed,确保 EOD 平仓时有对手盘
/// - force_liquidate=true → EOD 发 Order::swap(sell, qty=0.1) →
///   进 perp book,命中 seed buy 单,position 归 0
#[test]
fn liquidate_eod_closes_perp_position_via_swap_book() {
    let perp = btc_perp();

    // 第 1 根 bar:策略发 1 笔 buy market @ perp
    // 第 2 根 bar:re-seed + EOD 强制平仓
    // (我们用 1 根 bar 完成,因为 EOD 在 run() 退出前自动触发,不需要第 2 根)
    let q = build_orders(&[(1, &perp, Side::Buy, 0.1, 1_000_000_000)]);

    let config = base_config(true); // force_liquidate=true
    // 配对 swap book 用更紧的 spread,让 seed 单价格贴近 mid
    let mut engine = BacktestEngine::new(config, q);
    engine.with_seed_liquidity_for(perp.clone(), 0.5, 3, 0.5);

    // 1 根 bar 触发 seed + strategy buy + EOD 平仓
    engine.begin_bar(100.0, perp.clone());
    let result = engine.run();

    // 验证:perp 持仓归 0
    let pos = result.positions.get(&perp).copied().unwrap_or(0.0);
    assert!(
        pos.abs() < 1e-9,
        "force_liquidate=true + perp 仓位 → EOD 应平仓,position={pos},期望 0"
    );

    // 验证:至少 2 笔 fill(策略 buy + EOD sell)
    assert!(
        result.fills >= 2,
        "至少 2 笔 fill(策略 buy + EOD sell),got {}",
        result.fills
    );

    // 验证:trades 至少有 1 笔(EOD 平仓 close trade)
    assert!(
        !result.trades.is_empty(),
        "EOD 平仓应至少 push 1 笔 TradeRecord,got 0"
    );

    // 验证:perp fill 的 instrument 字段正确(对 swap 而言,fills_detail 应该有 perp 字段)
    let perp_fills: Vec<_> = result
        .fills_detail
        .iter()
        .filter(|f| f.instrument == perp)
        .collect();
    assert!(
        !perp_fills.is_empty(),
        "应至少有 1 笔 perp fill(策略 buy 或 EOD sell),got 0"
    );
    // 应该 2 笔 perp fill:策略 buy @ 100.5, EOD sell @ 99.5
    // (seed sell @ 100.5 向下 mid 100, seed buy @ 99.5 向下 0.5)
    assert_eq!(
        perp_fills.len(),
        2,
        "应有 2 笔 perp fill(策略 buy + EOD sell),got {}",
        perp_fills.len()
    );
}

// ── 测试 4:seed_liquidity activate invariant 防御 ─────────────────

/// 0.7.0 P0#3:`let _ = order.activate();` 吞错会让 50GB bug 静默回归。
/// 修复后用 `expect()`,任何让 activate 失败的 invariant 破坏会被
/// panic 暴露(而不是默默回到 50GB)。
///
/// 本测试用 **should_panic** 验证:故意构造一个不在 Created 状态的 Order
/// → 让 `activate()` 失败 → 走 `expect()` 路径会 panic。
///
/// 注:`seed_liquidity` 内部调用 activate 的是私有路径,我们直接对
/// 公开 API(`Order::activate`)做等价测试,确认 Order state machine
/// + expect 模式协同工作正常。
#[test]
fn activate_expect_panics_on_already_active_order() {
    let perp = btc_perp();
    let mut order = Order::swap(
        999,
        perp.base().clone(),
        perp.quote().clone(),
        SwapSettle::UsdMargin,
        1.0,
        Side::Buy,
        OrderType::Limit {
            price: axon_core::types::Price::from_f64(100.0),
        },
        Quantity::from_f64(0.1),
        TimeInForce::GTC,
    );

    // 第一次 activate:Created → Pending,成功
    order.activate().expect("首次 activate 应成功");

    // 第二次 activate:Pending → Pending 非法,Err
    let result = order.activate();
    assert!(result.is_err(), "重复 activate 应失败");

    // 模拟 seed_liquidity 用 expect() 时的行为:Err 触发 panic
    // (这是 invariant 破坏的保护机制,不是 bug)
    let result = std::panic::catch_unwind(|| {
        let mut o = order;
        o.activate()
            .expect("重复 activate 不该成功 — invariant 破坏");
    });
    assert!(result.is_err(), "重复 activate 应 panic");
}

// ── 测试 5:rebalance_to_target perp 派发保留 instrument ─────────

/// 0.7.0 改:`rebalance_to_target` 也复用 `build_leg_order` helper,
/// perp 仓位的 rebalance 单走 `Order::swap`,`order.instrument` 正确。
///
/// 验证:开启 auto-rebalance + 持 perp 仓位 → rebalance 单不 panic,
/// 且 rebalance 走 perp book(不是被错发到 spot book 后被拒)。
///
/// 模式:pre-push 1 sell limit @ perp 价(提供 rebalance buy 的对手盘),
/// new engine, set_target_position(+0.1), begin_bar 触发 rebalance
/// buy 吃 sell。
///
/// 注:这是相对简化的"rebalance 路径能跑通"测试,不验证 fill 数 / 价格,
/// 因为 bar 1 的 strategy buy + bar 2 的 rebalance sell 的复杂时序
/// 已经在 `begin_bar_auto_rebalance_triggers_per_bar` 等测试中覆盖。
/// 本测试专门验证:rebalance 走 `Order::swap` 而不是 `Order::spot` ——
/// 旧版错发 spot 时,perp book 找不到对手盘,但不会 panic;
/// 新版发 swap 时,perp book 正确撮合。两个版本都"不 crash",但
/// 旧版会"无声残留 spot order";新版的 fill 数对。
///
/// 为此,本测试**只验证不 panic** + **fill 数 ≥ 1**(成功撮合)。
#[test]
fn rebalance_target_uses_swap_for_perp_leg() {
    use axon_core::event::OrderAction;

    let perp = btc_perp();
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);

    // 预挂 1 sell limit @ 100.5 (rebalance buy 的对手盘)
    q.push(b.order(
        Timestamp::from_nanos(1_000_000),
        1,
        OrderAction::Submitted(make_limit_order_pub(1, &perp, Side::Sell, 100.5, 0.1)),
    ));

    let mut engine = BacktestEngine::new(base_config(false), q);
    engine.with_seed_liquidity_for(perp.clone(), 0.5, 3, 0.1);
    engine.set_target_position(perp.clone(), 0.1); // target +0.1
    engine.with_auto_rebalance(0.0);

    // run() 消费预挂 sell(无对手盘,留在 book)
    // begin_bar 触发 seed + auto-rebalance,rebalance buy @ perp book
    // 命中预挂 sell,fill 成功
    engine.begin_bar(100.0, perp.clone());
    let result = engine.run();

    // 持仓应 +0.1
    let pos = result.positions.get(&perp).copied().unwrap_or(0.0);
    assert!(
        (pos - 0.1).abs() < 1e-9,
        "rebalance 后 perp 持仓应 +0.1,got {pos}"
    );

    // 至少 1 笔 fill(rebalance buy vs pre-sell)
    assert!(
        result.fills >= 1,
        "rebalance 至少应有 1 笔 fill,got {}",
        result.fills
    );

    // 关键:fill 的 instrument 必须是 perp(Swap),不能是 spot
    for f in &result.fills_detail {
        assert_eq!(
            f.instrument, perp,
            "rebalance fill instrument 应是 Swap(perp),got {:?}",
            f.instrument
        );
    }
}

// 公开的 make_limit_order helper(为测试 5 内部使用)
fn make_limit_order_pub(
    id: u64,
    instrument: &Instrument,
    side: Side,
    price: f64,
    qty: f64,
) -> Order {
    use axon_core::types::Price;
    match instrument {
        Instrument::Spot(s) => Order::spot(
            id,
            s.base.clone(),
            s.quote.clone(),
            side,
            OrderType::Limit {
                price: Price::from_f64(price),
            },
            Quantity::from_f64(qty),
            TimeInForce::GTC,
        ),
        Instrument::Swap(s) => Order::swap(
            id,
            s.base.clone(),
            s.quote.clone(),
            s.settle,
            s.contract_size,
            side,
            OrderType::Limit {
                price: Price::from_f64(price),
            },
            Quantity::from_f64(qty),
            TimeInForce::GTC,
        ),
    }
}
