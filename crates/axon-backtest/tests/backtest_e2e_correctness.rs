//! 端到端回测结果正确性验证测试
//!
//! ## 测试目标
//!
//! 现有 `run_result_fields.rs` / `impact_breakdown.rs` 等测试只验证了回测引擎
//! 在「硬编码订单事件」下的机械行为。**没有任何测试跑过完整链路**:
//!
//! ```text
//! 真实行情数据 (OHLCV bar) → 策略信号 (SMA crossover) → 下单 → 撮合 → 成交 → PnL 结算
//! ```
//!
//! 本测试套件填补此空缺:用确定性合成数据 + 一个能跑出可预测行为的简单策略,
//! 把「策略层」纳入测试回路,验证 PnL / 持仓 / 手续费 / 成交数量是否符合数学期望。
//!
//! ## 测试场景
//!
//! 1. `sma_crossover_profitable_in_uptrend`:单边上涨行情,SMA 触发 buy,total_pnl > 0
//! 2. `sma_crossover_loses_in_downtrend`:单边下跌行情,总盈亏 ≤ 0
//! 3. `sma_crossover_oscillating_market_multiple_trades`:震荡行情,多次开/平,total_fees > 0
//! 4. `pnl_matches_hand_calculated_value`:手算期望 PnL,与 result 对账
//! 5. `total_fees_equals_sum_per_fill`:验证手续费累加 = Σ price·qty·taker_rate
//!
//! ## 设计要点
//!
//! - **数据**:确定性闭式价格序列(无随机/无外部 CSV),保证 CI 稳定
//! - **撮合对手盘**:策略发 market order 之前,先 push 1 个 limit @ bar_close 做对手方
//!   (L1 是订单簿撮合,market 单需要挂单对手)
//! - **可手算**:bar 数量 / 价位 / SMA 窗口 / qty 都选成数学上能直接推出 expected 值
//!
//! 运行:`cargo test -p axon-backtest --test backtest_e2e_correctness`

use std::collections::VecDeque;

use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig};
use axon_backtest::matching::L1MatchingEngine;
use axon_core::event::{EventBuilder, OrderAction};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{Price, Quantity};

// ── 共享 helper ──────────────────────────────────────────────────────

/// 1 根 OHLCV bar(简化为 close only,本测试不依赖 OHLC 全部字段)
#[derive(Debug, Clone, Copy)]
struct Bar {
    /// bar 序号
    idx: usize,
    /// 收盘价(撮合价)
    close: f64,
}

/// 用确定性闭式公式生成 1 个 bar 序列
///
/// `i` → `base + step * (i % period < half_period ? 1 : -1)`:
/// - 前 half_period 根 bar 单调上升
/// - 后 half_period 根 bar 单调下降
/// - 形成"先涨后跌"的尖峰
fn gen_peak_series(n: usize, base: f64, step: f64) -> Vec<Bar> {
    let half = n / 2;
    (0..n)
        .map(|i| Bar {
            idx: i,
            close: if i < half {
                base + step * (i + 1) as f64
            } else {
                base + step * (n - i) as f64
            },
        })
        .collect()
}

/// 用确定性闭式公式生成 1 个单边上涨序列
fn gen_uptrend(n: usize, base: f64, step: f64) -> Vec<Bar> {
    (0..n)
        .map(|i| Bar {
            idx: i,
            close: base + step * (i + 1) as f64,
        })
        .collect()
}

/// 用确定性闭式公式生成 1 个单边下跌序列
fn gen_downtrend(n: usize, base: f64, step: f64) -> Vec<Bar> {
    (0..n)
        .map(|i| Bar {
            idx: i,
            close: base - step * (i + 1) as f64,
        })
        .collect()
}

/// 简单 SMA crossover 策略状态
///
/// 状态机:
/// - 每根 bar 末尾计算 SMA(short) 与 SMA(long)
/// - `short > long` → desired = `Long`(持有多仓)
/// - `short < long` → desired = `Flat`(空仓)
/// - 当前仓位 ≠ desired 时,在下一根 bar 发市价单调整
struct SmaStrategy {
    short_win: usize,
    long_win: usize,
    /// 最近 (long_win) 根 bar 的收盘价
    closes: VecDeque<f64>,
    /// 当前真实仓位(> 0 long,< 0 short,0 flat)
    position: f64,
    /// 每根 bar 调一次,生成事件
    order_id_seq: u64,
    /// 期望仓位(策略信号)
    desired: f64,
}

impl SmaStrategy {
    fn new(short_win: usize, long_win: usize) -> Self {
        Self {
            short_win,
            long_win,
            closes: VecDeque::with_capacity(long_win),
            position: 0.0,
            order_id_seq: 1, // 1 留给首根 bar 之前可能的对手机单
            desired: 0.0,
        }
    }

    /// 用已有 bar 预热 SMA 窗口(不回看 order)
    fn warmup(&mut self, bars: &[Bar]) {
        for b in bars {
            self.closes.push_back(b.close);
            if self.closes.len() > self.long_win {
                self.closes.pop_front();
            }
        }
    }

    /// SMA(返回 None 当窗口未满)
    fn sma(&self, win: usize) -> Option<f64> {
        if self.closes.len() < win {
            return None;
        }
        let sum: f64 = self.closes.iter().rev().take(win).sum();
        Some(sum / win as f64)
    }

    /// 决定本 bar 末尾的 desired 仓位
    fn update_signal(&mut self) {
        let short = self.sma(self.short_win);
        let long = self.sma(self.long_win);
        self.desired = match (short, long) {
            (Some(s), Some(l)) if s > l => 1.0,
            _ => 0.0};
    }

    /// 根据 (desired, position) 决定是否需要下单;返回 Some(side) 表示要发单
    fn next_signal(&self) -> Option<Side> {
        if (self.desired - self.position).abs() < 1e-9 {
            None
        } else if self.desired > self.position {
            Some(Side::Buy)
        } else {
            Some(Side::Sell)
        }
    }
}

/// 构造限价单 helper
fn make_limit_order(id: u64, side: Side, price: f64, qty: f64) -> Order {
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

/// 构造市价单 helper
fn make_market_order(id: u64, side: Side, qty: f64) -> Order {
    Order::spot(
        id,
        "BTC",
        "USDT",
        side,
        OrderType::Market,
        Quantity::from_f64(qty),
        TimeInForce::IOC,
    )
}

/// 默认回测配置
fn default_config() -> BacktestEngineConfig {
    BacktestEngineConfig {
        clock: SimulatedClock::new(Timestamp::from_nanos(0)),
        matching_engine: Box::new(L1MatchingEngine::new()),
        impact_model: None,
        initial_cash: 100_000.0,
        fee_config: FeeConfig::default(),
        force_liquidate: false,
    }
}

/// 把 1 根 bar 翻译成事件流(对手盘 + 策略信号)
///
/// 每个 bar 在自己的 timestamp 推 2 个事件:
/// 1. 限价对手机单(卖 0.1 @ bar.close)— 让 market buy 能撮合
/// 2. 策略信号(可选)— 仅当 next_signal() != None 时发 market order
///
/// 注:market sell 需要对手机买单,但 buy 之后 position > 0,
/// 下一根 bar 平仓时为了撮合 sell,需要先推 1 个对手机买单。
/// 这里采用「bar 末尾先推策略单 → 下根 bar 开始时再推对手机买」的做法,
/// 但更简单:**让每根 bar 都推 1 买 + 1 卖对手机(数量足够)**,撮合会自动选最优价。
/// 实际我们只推所需方向的对手盘,避免污染 order book。
fn emit_bar(
    q: &mut EventQueue,
    b: &mut EventBuilder,
    bar: &Bar,
    strategy: &mut SmaStrategy,
    qty: f64,
) {
    // 时间戳:每根 bar 间隔 1ms(用 nanos 表达)
    let ts = Timestamp::from_nanos(((bar.idx as i64) + 1) * 1_000_000);
    // 推 1 个对手机(始终推,与策略方向相反)
    // 若策略期望 buy:推 1 个 sell limit @ close(对手)
    // 若策略期望 sell(平仓):推 1 个 buy limit @ close(对手)
    match strategy.next_signal() {
        None => {
            // 策略不发单 → 这根 bar 不需要事件
        }
        Some(Side::Buy) => {
            // 对手:1 个 sell limit @ bar.close
            let counter_id = strategy.order_id_seq;
            strategy.order_id_seq += 1;
            q.push(b.order(
                ts,
                counter_id,
                OrderAction::Submitted(make_limit_order(counter_id, Side::Sell, bar.close, qty)),
            ));
            // 策略:1 个 market buy
            let strat_id = strategy.order_id_seq;
            strategy.order_id_seq += 1;
            q.push(b.order(
                ts,
                strat_id,
                OrderAction::Submitted(make_market_order(strat_id, Side::Buy, qty)),
            ));
            strategy.position = qty;
        }
        Some(Side::Sell) => {
            // 对手:1 个 buy limit @ bar.close
            let counter_id = strategy.order_id_seq;
            strategy.order_id_seq += 1;
            q.push(b.order(
                ts,
                counter_id,
                OrderAction::Submitted(make_limit_order(counter_id, Side::Buy, bar.close, qty)),
            ));
            // 策略:1 个 market sell
            let strat_id = strategy.order_id_seq;
            strategy.order_id_seq += 1;
            q.push(b.order(
                ts,
                strat_id,
                OrderAction::Submitted(make_market_order(strat_id, Side::Sell, qty)),
            ));
            strategy.position = 0.0;
        }
    }
}

// ── 测试 1: 单边上涨 + SMA crossover → 应盈利 ────────────────────

/// 单边上涨行情,bar 100 → 130,SMA(2) > SMA(5) 在涨势中触发,策略开多。
///
/// 期望:
/// - 至少 1 笔 buy 被撮合(策略触发)
/// - total_pnl > 0(持多仓期间价格上涨)
/// - 终态 cash + 持仓 mark = final_nav
#[test]
fn sma_crossover_profitable_in_uptrend() {
    let bars = gen_uptrend(20, 100.0, 1.5); // 20 根 bar,100 → 130
    let qty = 0.1;

    // 跑策略在 bar 上的预热与信号
    let mut strat = SmaStrategy::new(2, 5);
    // 用前 5 根 bar 预热 SMA(5)
    strat.warmup(&bars[..5]);
    strat.update_signal();
    // 注:对于单边上涨数据,SMA(2) 在前 5 根末尾可能已超过 SMA(5),
    // 不强制要求首根不发单,只验证后续 bar 上的 fill 行为与 PnL 符号

    // 构造事件流
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    for bar in &bars[5..] {
        // 推进 closes
        strat.closes.push_back(bar.close);
        if strat.closes.len() > strat.long_win {
            strat.closes.pop_front();
        }
        strat.update_signal();
        emit_bar(&mut q, &mut b, bar, &mut strat, qty);
    }

    let mut engine = BacktestEngine::new(default_config(), q);
    let result = engine.run();

    // 至少 1 笔 buy 成交(策略在上涨中触发)
    assert!(
        result.fills > 0,
        "上涨趋势中 SMA crossover 应至少触发 1 笔 buy,fills={}",
        result.fills
    );
    // 策略在上涨中持续持多 → 终态 PnL 应当为正(忽略手续费)
    // 上涨 25 单位(100→125 中位价),qty=0.1,粗略毛利 ~ 2.5
    // 扣手续费(每笔 0.001 * 100 * 0.1 ≈ 0.01)后仍 > 0
    assert!(
        result.total_pnl > 0.0,
        "上涨趋势 + 持多仓应盈利,total_pnl={} (fills={}, total_fees={})",
        result.total_pnl,
        result.fills,
        result.total_fees
    );
    // final_nav = initial_cash + total_pnl
    assert!(
        (result.final_nav - (100_000.0 + result.total_pnl)).abs() < 1e-6,
        "final_nav = initial + total_pnl 不成立,final_nav={}, total_pnl={}",
        result.final_nav,
        result.total_pnl
    );
    // 至少 1 笔 trade 记录(开仓 → 中间可能有平仓)
    // 不强制 trades.len()>=1(可能持到最后 1 根,force_liquidate=false 留仓),只看 fills
}

// ── 测试 2: 单边下跌 + SMA crossover → 不应盈利 ────────────────────

/// 单边下跌行情,bar 130 → 95,SMA(2) < SMA(5) 全程,策略空仓。
///
/// 期望:
/// - 没有 buy 成交(策略不开仓)
/// - 没有 sell 成交(也无平仓)
/// - total_pnl = 0(无交易)
/// - final_nav = initial_cash(无变化)
#[test]
fn sma_crossover_loses_in_downtrend() {
    let bars = gen_downtrend(20, 130.0, 1.75); // 20 根 bar,130 → 95
    let qty = 0.1;

    let mut strat = SmaStrategy::new(2, 5);
    strat.warmup(&bars[..5]);
    strat.update_signal();
    // 注:不强制要求首根不发单(SMA 行为依赖数据形状),只验证后续 fill 与 PnL

    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    for bar in &bars[5..] {
        strat.closes.push_back(bar.close);
        if strat.closes.len() > strat.long_win {
            strat.closes.pop_front();
        }
        strat.update_signal();
        emit_bar(&mut q, &mut b, bar, &mut strat, qty);
    }

    let mut engine = BacktestEngine::new(default_config(), q);
    let result = engine.run();

    // 下跌趋势策略空仓 → 无成交
    assert_eq!(
        result.fills,
        0,
        "下跌趋势策略应空仓,fills={}, trades.len()={}",
        result.fills,
        result.trades.len()
    );
    assert_eq!(result.trades.len(), 0);
    assert_eq!(result.orders_accepted, 0);
    // 无交易 → PnL / fee / nav 不变
    assert!(
        (result.total_pnl - 0.0).abs() < 1e-9,
        "空仓无成交,total_pnl 应为 0,got {}",
        result.total_pnl
    );
    assert!(
        result.total_fees.abs() < 1e-9,
        "无成交 → total_fees=0,got {}",
        result.total_fees
    );
    assert!(
        (result.final_nav - 100_000.0).abs() < 1e-9,
        "无成交 → final_nav=initial,got {}",
        result.final_nav
    );
}

// ── 测试 3: 震荡行情 → 多次开/平,total_fees > 0 ──────────────────

/// 尖峰(先涨后跌)行情,bar 价格先涨后跌,SMA crossover 在顶点附近触发
/// buy 然后在下跌中触发 sell,产生完整 round-trip。
///
/// 期望:
/// - fills >= 2(至少 1 买 + 1 卖)
/// - trades.len() >= 1(完全平仓)
/// - total_fees > 0
#[test]
fn sma_crossover_oscillating_market_multiple_trades() {
    // 20 根 bar:100 → 130 → 100
    let bars = gen_peak_series(20, 100.0, 1.5);
    let qty = 0.1;

    let mut strat = SmaStrategy::new(2, 5);
    strat.warmup(&bars[..5]);
    strat.update_signal();

    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    for bar in &bars[5..] {
        strat.closes.push_back(bar.close);
        if strat.closes.len() > strat.long_win {
            strat.closes.pop_front();
        }
        strat.update_signal();
        emit_bar(&mut q, &mut b, bar, &mut strat, qty);
    }

    let mut engine = BacktestEngine::new(default_config(), q);
    let result = engine.run();

    // 震荡应有成交
    assert!(
        result.fills >= 2,
        "震荡行情应至少 1 买 + 1 卖,fills={}",
        result.fills
    );
    assert!(
        !result.trades.is_empty(),
        "至少 1 笔完全平仓 trade,got {}",
        result.trades.len()
    );
    assert!(
        result.total_fees > 0.0,
        "每笔 fill 都应扣手续费,total_fees={}",
        result.total_fees
    );
    // 尖峰行情策略至少产生交易,total_pnl 符号不强制
    // (尖峰 buy 在高点附近、sell 在低点附近 → 反而亏;但只要有 trade 即可)
    // 这里只断言手续费累加 > 0(已经上面断言)
}

// ── 测试 4: 完整 PnL 数学验证(确定性,手算) ──────────────────────

/// **手算可验证**:构造 1 个完全可控的事件流,数学上推出 expected total_pnl,
/// 与 `result.total_pnl` 对账。
///
/// 场景:
/// - bar 0: buy 0.1 @ 100,fill @ 100(手续费 0.01)
/// - bar 1: sell 0.1 @ 110,fill @ 110(手续费 0.011)
/// - 1 笔 trade:realized = (110-100) * 0.1 = 1.0
/// - total_fees = 100*0.1*0.001 + 110*0.1*0.001 = 0.021
/// - total_pnl = 1.0 - 0.021 = 0.979
/// - final_nav = 100_000 + 0.979 = 100_000.979
#[test]
fn pnl_matches_hand_calculated_value() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);

    // bar 0:先推 1 个 sell limit @ 100 做对手盘
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(make_limit_order(1, Side::Sell, 100.0, 0.1)),
    ));
    // bar 0:策略 buy market 0.1 → 撮合 @ 100
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        2,
        OrderAction::Submitted(make_market_order(2, Side::Buy, 0.1)),
    ));

    // bar 1:先推 1 个 buy limit @ 110 做对手盘
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        3,
        OrderAction::Submitted(make_limit_order(3, Side::Buy, 110.0, 0.1)),
    ));
    // bar 1:策略 sell market 0.1 → 撮合 @ 110
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        4,
        OrderAction::Submitted(make_market_order(4, Side::Sell, 0.1)),
    ));

    let mut engine = BacktestEngine::new(default_config(), q);
    let result = engine.run();

    // 手算期望
    let expected_realized = (110.0 - 100.0) * 0.1; // 1.0
    let expected_fee_open = 100.0 * 0.1 * 0.001; // 0.01
    let expected_fee_close = 110.0 * 0.1 * 0.001; // 0.011
    let expected_total_fees = expected_fee_open + expected_fee_close; // 0.021
    let expected_total_pnl = expected_realized - expected_total_fees; // 0.979
    let expected_final_nav = 100_000.0 + expected_total_pnl; // 100_000.979

    // trades
    assert_eq!(result.trades.len(), 1, "完全平仓 push 1 笔 TradeRecord");
    let tr = &result.trades[0];
    // realized_pnl 单位是 × 1e6 定点
    assert!(
        (tr.realized_pnl - (expected_realized * 1e6) as i64).abs() < 1,
        "expected realized_pnl={}, got {}",
        (expected_realized * 1e6) as i64,
        tr.realized_pnl
    );

    // total_fees
    assert!(
        (result.total_fees - expected_total_fees).abs() < 1e-9,
        "expected total_fees={}, got {}",
        expected_total_fees,
        result.total_fees
    );

    // total_pnl(账户视角)≈ 0.979
    assert!(
        (result.total_pnl - expected_total_pnl).abs() < 1e-6,
        "expected total_pnl={}, got {}",
        expected_total_pnl,
        result.total_pnl
    );

    // final_nav = initial + total_pnl
    assert!(
        (result.final_nav - expected_final_nav).abs() < 1e-6,
        "expected final_nav={}, got {}",
        expected_final_nav,
        result.final_nav
    );
}

// ── 测试 5: 手续费累加 = Σ price·qty·taker_rate ──────────────────

/// 验证 `total_fees` 等于「每笔 fill 的 notional × taker_rate」的精确累加。
///
/// 构造 3 笔 fill(单价、数量都不同),手算 expected 累加值。
#[test]
fn total_fees_equals_sum_per_fill() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);

    // ── fill 1: sell @ 100, qty 0.1 → 100, buy 吃 → fill @ 100 ──
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(make_limit_order(1, Side::Sell, 100.0, 0.1)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        2,
        OrderAction::Submitted(make_market_order(2, Side::Buy, 0.1)),
    ));

    // ── fill 2: sell @ 200, qty 0.05 → 200, buy 吃 → fill @ 200 ──
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        3,
        OrderAction::Submitted(make_limit_order(3, Side::Sell, 200.0, 0.05)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        4,
        OrderAction::Submitted(make_market_order(4, Side::Buy, 0.05)),
    ));

    // ── fill 3: sell @ 50, qty 0.2 → 50, buy 吃 → fill @ 50 ──
    q.push(b.order(
        Timestamp::from_nanos(3_000),
        5,
        OrderAction::Submitted(make_limit_order(5, Side::Sell, 50.0, 0.2)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(3_000),
        6,
        OrderAction::Submitted(make_market_order(6, Side::Buy, 0.2)),
    ));

    let mut engine = BacktestEngine::new(default_config(), q);
    let result = engine.run();

    // 3 笔 fill 全部成交
    assert_eq!(result.fills, 3, "应有 3 笔 fill");

    // 期望手续费(每笔 notional × 0.001):
    //   100*0.1*0.001 = 0.01
    //   200*0.05*0.001 = 0.01
    //   50*0.2*0.001 = 0.01
    // 累加 = 0.03
    let expected_total_fees = 100.0 * 0.1 * 0.001 + 200.0 * 0.05 * 0.001 + 50.0 * 0.2 * 0.001;
    assert!(
        (result.total_fees - expected_total_fees).abs() < 1e-9,
        "expected total_fees={}, got {}",
        expected_total_fees,
        result.total_fees
    );

    // 终态持仓:三笔都是 buy,long 累加 = 0.1 + 0.05 + 0.2 = 0.35(force_liquidate=false 留仓)
    assert_eq!(result.positions.len(), 1);
    assert!(
        (result.positions["BTC/USDT"] - 0.35).abs() < 1e-9,
        "expected long 0.35,got {}",
        result.positions["BTC/USDT"]
    );
}
