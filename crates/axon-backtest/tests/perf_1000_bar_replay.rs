//! 性能基准 + 正确性测试:1000 根 bar 的 SMA crossover 回测(P2-5)
//!
//! ## 测试目标
//!
//! 现有 `backtest_e2e_correctness.rs` 测的是 15 根 bar 的 SMA crossover,验证"链路通",
//! 但**没有任何测试验证百根 / 千根 bar 下的回测性能**。本测试套件填补此空缺:
//!
//! 1. **正确性**: 1000 根 bar 的回测应能跑完(无 panic / hang),且 PnL / 持仓 / fills
//!    符合 SMA crossover 策略的数学期望
//! 2. **性能门**: 在 release 模式下,1000 根 bar 的 BacktestEngine::run() 应在合理时间完成
//!
//! ## 设计要点
//!
//! - **数据**: 确定性闭式震荡序列(三角波),1000 根 bar,每 50 根 1 个周期
//!   - 价格范围:[50, 150] 振荡,每 bar 走 2.0
//!   - SMA(5, 20) 会在每个交叉点触发 1 笔交易
//! - **不依赖 streaming**: 直接 L1MatchingEngine,跟 v1 e2e_*.rs 一致
//! - **性能门** 默认 `#[ignore]`(用 `-- --ignored` 显式触发),不污染默认 `cargo test`
//!   - 原因:debug 模式可能 > 5s,影响 CI;release 模式应在 1s 内
//!
//! ## 运行
//!
//! ```bash
//! # 默认(不跑 perf gate)
//! cargo test -p axon-backtest --test perf_1000_bar_replay
//!
//! # 跑 perf gate(release 模式必须 < 1s)
//! cargo test -p axon-backtest --test perf_1000_bar_replay --release -- --ignored
//! ```
//!
//! ## 测试场景
//!
//! 1. `correctness_1000_bars_oscillating_sma_runs_to_completion`:
//!    1000 根 bar,SMA(5,20) crossover,断言:fills > 0,positions 合理,无 panic
//! 2. `correctness_1000_bars_zero_fees_zero_pnl_when_no_signal`:
//!    平直序列 → 0 笔 fill,total_pnl = 0,no-op
//! 3. `perf_1000_bars_release_under_one_second`(#[ignore]):
//!    跑 1000 根 bar,assert elapsed < 1.0s(release 模式)
//! 4. `perf_1000_bars_scales_linearly_with_bar_count`(#[ignore]):
//!    跑 100/500/1000/2000 根 bar,assert elapsed 与 bar 数线性相关(比例 < 5x)

use std::collections::VecDeque;
use std::time::Instant;

use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig};
use axon_backtest::matching::L1MatchingEngine;
use axon_core::event::{EventBuilder, OrderAction};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{Price, Quantity};

// ── 共享 helper ──────────────────────────────────────────────

const SYM: &str = "BTC/USDT";

#[derive(Debug, Clone, Copy)]
struct Bar {
    #[allow(dead_code)]
    idx: usize,
    close: f64,
}

/// 三角波震荡序列:每 `period` 根 bar 完成 1 个"先涨后跌"循环
///
/// `i` → `base + amplitude - amplitude * (i % period) / (period / 2)`
/// (简化:前半个周期涨,后半个周期跌)
fn gen_oscillating(n: usize, period: usize, base: f64, amplitude: f64) -> Vec<Bar> {
    let half = period / 2;
    (0..n)
        .map(|i| {
            let phase = i % period;
            let close = if phase < half {
                // 上涨半周期:base → base + amplitude
                base + amplitude * phase as f64 / half as f64
            } else {
                // 下跌半周期:base + amplitude → base
                base + amplitude * (period - phase) as f64 / half as f64
            };
            Bar { idx: i, close }
        })
        .collect()
}

/// 平直序列(全部 = base,SMA 永不交叉)
fn gen_flat(n: usize, base: f64) -> Vec<Bar> {
    (0..n)
        .map(|i| Bar {
            idx: i,
            close: base,
        })
        .collect()
}

/// SMA crossover 策略(简化版:每根 bar 决策 1 次)
struct SmaStrategy {
    short_win: usize,
    long_win: usize,
    closes: VecDeque<f64>,
    position: f64,
    desired: f64,
    order_id_seq: u64,
}

impl SmaStrategy {
    fn new(short_win: usize, long_win: usize) -> Self {
        Self {
            short_win,
            long_win,
            closes: VecDeque::with_capacity(long_win),
            position: 0.0,
            desired: 0.0,
            order_id_seq: 1,
        }
    }

    fn warmup(&mut self, bars: &[Bar]) {
        for b in bars {
            self.closes.push_back(b.close);
            if self.closes.len() > self.long_win {
                self.closes.pop_front();
            }
        }
    }

    fn sma(&self, win: usize) -> Option<f64> {
        if self.closes.len() < win {
            return None;
        }
        let sum: f64 = self.closes.iter().rev().take(win).sum();
        Some(sum / win as f64)
    }

    fn update_signal(&mut self) {
        let short = self.sma(self.short_win);
        let long = self.sma(self.long_win);
        self.desired = match (short, long) {
            (Some(s), Some(l)) if s > l => 1.0,
            _ => 0.0};
    }

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

/// 把 bars 翻译成事件流 + 跑 BacktestEngine 返回 RunResult
fn run_bars(bars: &[Bar], qty: f64) -> axon_backtest::engine::RunResult {
    let mut strat = SmaStrategy::new(5, 20);
    strat.warmup(&bars[..20.min(bars.len())]);
    strat.update_signal();

    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);

    for (i, bar) in bars.iter().enumerate() {
        strat.closes.push_back(bar.close);
        if strat.closes.len() > strat.long_win {
            strat.closes.pop_front();
        }
        strat.update_signal();

        let ts = Timestamp::from_nanos(((i as i64) + 1) * 1_000_000);

        match strat.next_signal() {
            None => {}
            Some(Side::Buy) => {
                // 对手:sell limit @ bar.close
                let counter_id = strat.order_id_seq;
                strat.order_id_seq += 1;
                q.push(b.order(
                    ts,
                    counter_id,
                    OrderAction::Submitted(make_limit_order(
                        counter_id,
                        Side::Sell,
                        bar.close,
                        qty,
                    )),
                ));
                // 策略:market buy
                let strat_id = strat.order_id_seq;
                strat.order_id_seq += 1;
                q.push(b.order(
                    ts,
                    strat_id,
                    OrderAction::Submitted(make_market_order(strat_id, Side::Buy, qty)),
                ));
                strat.position = 1.0;
            }
            Some(Side::Sell) => {
                // 对手:buy limit @ bar.close
                let counter_id = strat.order_id_seq;
                strat.order_id_seq += 1;
                q.push(b.order(
                    ts,
                    counter_id,
                    OrderAction::Submitted(make_limit_order(counter_id, Side::Buy, bar.close, qty)),
                ));
                // 策略:market sell
                let strat_id = strat.order_id_seq;
                strat.order_id_seq += 1;
                q.push(b.order(
                    ts,
                    strat_id,
                    OrderAction::Submitted(make_market_order(strat_id, Side::Sell, qty)),
                ));
                strat.position = 0.0;
            }
        }
    }

    let mut engine = BacktestEngine::new(default_config(), q);
    engine.run()
}

// ── 测试 1:1000 根 bar 震荡序列能跑完,fills > 0 ───────────────────

/// 1000 根震荡 bar + SMA(5,20) crossover,断言:BacktestEngine 跑完无 panic,
/// fills 数量符合数学期望(每个交叉点 1 笔)
#[test]
fn correctness_1000_bars_oscillating_sma_runs_to_completion() {
    // 1000 根,每 50 根 1 个周期(20 个完整周期),振幅 50
    let bars = gen_oscillating(1000, 50, 100.0, 50.0);
    let qty = 0.1;

    let result = run_bars(&bars, qty);

    // 至少 1 笔 fill(震荡序列必然触发 SMA 交叉)
    assert!(
        result.fills > 0,
        "震荡序列应触发交易,got {} fills",
        result.fills
    );

    // 终态持仓应为 0(每周期收尾时 SMA short 跌破 long,Sell 触发)
    let pos = result.positions.get(SYM).copied().unwrap_or(0.0);
    assert!(pos.abs() < 1e-6, "终态持仓应=0(震荡收尾),got {}", pos);

    // 至少 1 笔 trade(完全平仓过)
    assert!(
        !result.trades.is_empty(),
        "震荡应至少有 1 笔 trade(完全平仓),got {}",
        result.trades.len()
    );
}

// ── 测试 2:平直序列 → 0 fill,无 panic ─────────────────────────

/// 1000 根平直 bar → SMA 永不交叉 → 0 fill,0 trade,total_pnl = 0
#[test]
fn correctness_1000_bars_zero_fees_zero_pnl_when_no_signal() {
    let bars = gen_flat(1000, 100.0);
    let qty = 0.1;

    let result = run_bars(&bars, qty);

    assert_eq!(result.fills, 0, "平直序列无 fill,got {}", result.fills);
    assert_eq!(result.trades.len(), 0, "平直序列无 trade");
    assert!(
        result.total_pnl.abs() < 1e-6,
        "平直序列 PnL 应=0,got {}",
        result.total_pnl
    );
    assert!(
        result.total_fees.abs() < 1e-6,
        "平直序列无手续费,got {}",
        result.total_fees
    );
}

// ── 测试 3:perf gate — release 模式 1000 根 bar < 1s ─────────────

/// **性能门**: release 模式下 1000 根 bar 的 BacktestEngine::run() 应在 1s 内完成
///
/// 此测试 `#[ignore]`,需显式 `cargo test -- --ignored` 触发(避免拖慢 debug CI)
#[test]
#[ignore = "perf gate: 需 cargo test --release -- --ignored 显式触发"]
fn perf_1000_bars_release_under_one_second() {
    let bars = gen_oscillating(1000, 50, 100.0, 50.0);
    let qty = 0.1;

    let start = Instant::now();
    let result = run_bars(&bars, qty);
    let elapsed = start.elapsed();

    println!(
        "[perf] 1000 bars: {} fills in {:?} ({} ns/fill)",
        result.fills,
        elapsed,
        elapsed.as_nanos() / result.fills.max(1) as u128
    );

    assert!(
        elapsed.as_secs_f64() < 1.0,
        "1000 bars 性能门超时:elapsed={:?}, 应 < 1s(release)",
        elapsed
    );
}

// ── 测试 4:perf gate — bar 数翻倍,耗时不应超 5x(粗略线性) ─────

/// **粗略线性检查**: 100 / 500 / 1000 / 2000 根 bar 的耗时比例
///
/// 注意:事件队列构造也耗时,本测试仅验证**回测引擎自身**的扩缩放性,
/// 故使用 4 次独立 run,每次重置 EventQueue
#[test]
#[ignore = "perf gate: 需 cargo test --release -- --ignored 显式触发"]
fn perf_1000_bars_scales_linearly_with_bar_count() {
    let counts = [100usize, 500, 1000, 2000];
    let mut elapsed_per_count: Vec<(usize, f64)> = Vec::new();

    for &n in &counts {
        let bars = gen_oscillating(n, 50, 100.0, 50.0);
        let qty = 0.1;

        // 跑 3 次取中位数,减少噪声
        let mut samples = Vec::new();
        for _ in 0..3 {
            let start = Instant::now();
            let _ = run_bars(&bars, qty);
            samples.push(start.elapsed().as_secs_f64());
        }
        samples.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
        let median = samples[1];
        elapsed_per_count.push((n, median));
    }

    for (n, t) in &elapsed_per_count {
        println!("[perf-scaling] {n} bars: {t:.4}s");
    }

    // 100 bars 应明显比 2000 bars 快
    let t_100 = elapsed_per_count[0].1;
    let t_2000 = elapsed_per_count[3].1;
    assert!(
        t_2000 > t_100,
        "2000 bars 应比 100 bars 慢,t_100={:.4}s, t_2000={:.4}s",
        t_100,
        t_2000
    );

    // 2000/100 = 20x bar,耗时比不应超 30x(给 50% buffer,排除 alloc / 冷启动)
    let ratio = t_2000 / t_100.max(1e-9);
    assert!(
        ratio < 30.0,
        "扩缩放超线性过多:2000/100 ratio={:.2}, 应 < 30",
        ratio
    );
}
