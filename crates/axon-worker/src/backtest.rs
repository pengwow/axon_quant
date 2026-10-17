//! BacktestRunner：策略组编排入口（Phase 2 真撮合版）
//!
//! 全 Rust 原生回测编排：逐 bar 跑所有策略的 decide()，
//! 然后直接把 target_position 交给 BacktestEngine 撮合引擎撮合。
//!
//! 流程：
//! ```text
//! bars (时间升序)
//!   │
//!   ▼ 逐 bar 循环
//! ┌─────────────────────────────────────────┐
//! │ 1. engine.clock.set(bar.timestamp)       │
//! │ 2. 所有策略 → decide() → action→target   │
//! │    engine.set_target_position(inst, tgt) │
//! │ 3. engine.begin_bar_multi(legs)          │
//! │    → seed_liquidity + rebalance_to_target│
//! └─────────────────────────────────────────┘
//!   │
//!   ▼ 所有 bar 处理完
//! engine.run() → RunResult (聚合所有撮合统计)
//!   │
//!   ▼ 合并决策轨迹 + RunResult → BacktestResult
//! ```
//!
//! 设计约束：
//! - BacktestEngine 内部用 bar_id guard 保证同 bar rebalance 只触发一次
//! - auto_rebalance_threshold 默认 1e-6，避免目标无意义抖动也触发撮合
//! - RunResult 聚合所有 fill/order 统计 + NAV 曲线（bar_nav_curve 每 bar 一帧）

use std::collections::BTreeMap;
use std::sync::Arc;

use axon_backtest::L1MatchingEngine;
use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig};
use axon_core::market::bar::Bar;
use axon_core::portfolio::{FillRecord, TradeRecord};
use axon_core::{EventQueue, Instrument, SimulatedClock, Symbol, Timestamp};
use axon_strategy::trait_::{BarContext, Strategy, StrategyDecision};
use serde::Serialize;

// 撮合种子流动性 + 自动 rebalance 参数。
//
// ponytail: 这几个值硬编码对齐 QuantCell `backtest_loop.py` 的 `_DEFAULT_*`
// 常量，使 Rust 编排路径与现有 Python 编排路径撮合口径一致（parity）。
// 上限：跨项目耦合，多消费者或需调参时改走 `BacktestRunnerConfig` 下发。
const REBALANCE_THRESHOLD: f64 = 0.001; // 对应 _DEFAULT_AUTO_REBALANCE_THRESHOLD
const SEED_HALF_SPREAD: f64 = 0.0005; // 对应 _DEFAULT_HALF_SPREAD_RATIO
const SEED_DEPTH_LEVELS: usize = 5; // 对应 _DEFAULT_DEPTH_LEVELS
const SEED_SIZE_PER_LEVEL: f64 = 100.0; // 对应 _DEFAULT_SIZE_PER_LEVEL

/// 回测运行器
///
/// 持有一个 BacktestEngine（L1 撮合 + SimulatedClock + EventQueue），
/// 逐 bar 编排 Strategy.decide() → set_target_position → begin_bar_multi → rebalance。
pub struct BacktestRunner {
    strategies: Vec<Arc<dyn Strategy>>,
    initial_cash: f64,
    force_liquidate: bool,
}

impl BacktestRunner {
    /// 构造 Runner
    ///
    /// `_engine_config` 字段保留（Phase 1 签名遗留），实际撮合配置硬编码：
    /// L1MatchingEngine + SimulatedClock + EventQueue + FeeConfig::default()。
    /// 未来需要可插拔撮合引擎时再暴露 Builder。
    pub fn new(_engine_config: BacktestEngineConfig) -> Self {
        Self {
            strategies: Vec::new(),
            initial_cash: _engine_config.initial_cash,
            force_liquidate: _engine_config.force_liquidate,
        }
    }

    /// 最简构造器（给测试/快速原型用）
    pub fn simple(initial_cash: f64) -> Self {
        Self {
            strategies: Vec::new(),
            initial_cash,
            force_liquidate: false,
        }
    }

    pub fn add_strategy(&mut self, s: Arc<dyn Strategy>) {
        self.strategies.push(s);
    }

    pub fn with_strategy(mut self, s: Arc<dyn Strategy>) -> Self {
        self.strategies.push(s);
        self
    }

    /// 全 Rust 原生回测入口
    ///
    /// # 参数
    /// - `bars`: 时间窗口内所有 K 线（必须按时间升序）
    /// - `state_by_symbol`: 每个 symbol 预计算好的 state JSON（features + window）
    ///
    /// # 返回
    /// BacktestResult：决策轨迹 + RunResult 聚合的撮合统计
    pub async fn run(
        &self,
        bars: &[Bar],
        state_by_symbol: &BTreeMap<String, serde_json::Value>,
        _initial_cash: f64,
    ) -> BacktestResult {
        assert!(!bars.is_empty(), "bars must not be empty");

        let initial_cash = self.initial_cash;

        // 1) 收集策略涉及的所有 symbol（去重）
        let unique_symbols: Vec<String> = {
            let mut set = BTreeMap::new();
            for s in &self.strategies {
                set.entry(s.symbol().to_string()).or_insert_with(|| ());
            }
            set.into_keys().collect()
        };

        // 2) 为每个 symbol 预构造 Instrument（复用，避免每 bar 重复 parse）
        let instruments: BTreeMap<String, Instrument> = unique_symbols
            .iter()
            .map(|sym| {
                let inst = Instrument::from_symbol(&Symbol::from(sym.as_str()));
                (sym.clone(), inst)
            })
            .collect();

        // 3) 构造 BacktestEngine
        let first_bar_ts = bars[0].timestamp;
        let mut engine = BacktestEngine::new(
            BacktestEngineConfig {
                clock: SimulatedClock::new(first_bar_ts),
                matching_engine: Box::new(L1MatchingEngine::new()),
                impact_model: None,
                initial_cash,
                fee_config: FeeConfig::default(),
                force_liquidate: self.force_liquidate,
            },
            EventQueue::new(),
        );
        // 启用自动 rebalance（begin_bar_multi 末尾会触发）
        engine.with_auto_rebalance(REBALANCE_THRESHOLD);
        // 启用虚拟对手盘（L1 撮合引擎默认空 book，没有对手盘 rebalance 发出去没人接）
        // seed 参数：half_spread 是**绝对价格**（不是 bps），depth_levels 层，每层 size 单位
        engine.with_seed_liquidity(SEED_HALF_SPREAD, SEED_DEPTH_LEVELS, SEED_SIZE_PER_LEVEL);

        // 4) 逐 bar 编排
        let mut decisions_timeline: Vec<BarDecisionRecord> =
            Vec::with_capacity(bars.len() * self.strategies.len().max(1));

        for (bar_idx, bar) in bars.iter().enumerate() {
            // 4a) 时钟推进到当前 bar（BacktestEngine.config 私有，走公开 setter）
            engine.set_clock(bar.timestamp);

            // 4b) 先推进 bar：begin_bar_multi → seed_liquidity + 自动 rebalance_to_target。
            //
            // 顺序对齐 QuantCell 的 BacktestLoop：每 bar 先 `begin_bar` 再让策略决策，
            // 因此本 bar 的 rebalance 执行的是**上一 bar**设定的 target
            // （信号在 bar 末产生、次 bar 成交，避免前视偏差）。若把决策放在 begin_bar
            // 之前，target 会同 bar 生效，结果与 QuantCell 现有路径不一致。
            // bar.close 作为 mid_price（L1 撮合引擎的 mark）。
            let legs: Vec<(Instrument, f64)> = instruments
                .values()
                .map(|inst| (inst.clone(), bar.close.as_f64()))
                .collect();
            engine.begin_bar_multi(legs);

            // 4c) 所有策略 decide + set_target_position（写入的 target 于下一 bar 成交）
            for strategy in &self.strategies {
                let state = state_by_symbol
                    .get(strategy.symbol())
                    .cloned()
                    .unwrap_or_default();

                let ctx = BarContext {
                    symbol: strategy.symbol(),
                    bar_index: bar_idx as u64,
                    // Phase 2 修复：用真实纳秒戳，之前是 RFC3339 字符串长度占位
                    timestamp_ns: bar.timestamp.nanos as u64,
                    bar,
                    state: &state,
                };

                let decision = strategy.decide(&ctx).await;

                // 先把 Result 转成可序列化的 timeline record（strategy.decide 返回值可能非 Clone）
                let decision_for_timeline = match &decision {
                    Ok(dec) => Ok(dec.clone()),
                    Err(e) => Err(e.to_string()),
                };

                // 无论决策结果如何都记录轨迹
                decisions_timeline.push(BarDecisionRecord {
                    bar_index: bar_idx as u64,
                    strategy: strategy.name().into(),
                    symbol: strategy.symbol().into(),
                    decision: decision_for_timeline,
                    bar_close: bar.close.as_f64(),
                });

                // 仅 Ok 时 set_target_position（引擎下一次 rebalance 用它撮合）
                if let Ok(ref dec) = decision {
                    // 优先用策略指定的 target_position，否则按 action 推导
                    let target = dec
                        .target_position
                        .unwrap_or_else(|| action_to_target(dec.action));
                    if let Some(inst) = instruments.get(strategy.symbol()) {
                        engine.set_target_position(inst.clone(), target);
                    }
                }
            }

            // 4d) 显式 rebalance：对齐 QuantCell BacktestLoop 在 set_target_position
            //     后的 `engine.rebalance_to_target()` 调用。
            //
            // 关键：`rebalance_to_target` 的 bar_id guard **仅在有成交时**才置位
            // （见 engine.rs `if triggered > 0 { last_rebalance_bar_id = bar_id }`）。
            // 故 4b 的自动 rebalance 若无成交，本步不会被打断，会用**本 bar 刚写入的
            // target** 立即调仓（同 bar 生效）；若有成交，本步被 guard 跳过（次 bar 生效）。
            // 这个"有成交则次 bar、无成交则同 bar"的语义是 QuantCell 现有行为，
            // 必须复刻才能保证结果一致。
            engine.rebalance_to_target(None);
        }

        // 5) run() 消费 EventQueue + 聚合 RunResult（队列空时也会 build_result 汇总撮合统计）
        let run_result = engine.run();

        // 终态持仓快照：Instrument → Vec<String>（spot: [kind,base,quote]；swap 再加 settle/size）
        let final_positions: Vec<(Vec<String>, f64)> = run_result
            .positions
            .iter()
            .map(|(inst, qty)| (instrument_to_vec(inst), *qty))
            .collect();

        BacktestResult {
            events_processed: run_result.events_processed,
            orders_accepted: run_result.orders_accepted,
            orders_rejected: run_result.orders_rejected,
            orders_cancelled: run_result.orders_cancelled,
            orders_modified: run_result.orders_modified,
            fills: run_result.fills,
            total_pnl: run_result.total_pnl,
            max_drawdown: run_result.max_drawdown,
            max_drawdown_pct: run_result.max_drawdown_pct,
            nav_peak: run_result.nav_peak,
            final_nav: run_result.final_nav,
            total_fees: run_result.total_fees,
            total_funding_pnl: run_result.total_funding_pnl,
            win_rate: run_result.win_rate,
            sharpe_ratio: run_result.sharpe_ratio,
            duration_secs: run_result.duration.as_secs_f64(),
            final_time_ns: run_result.final_time.nanos,
            decisions_timeline,
            bar_nav_curve: run_result.bar_nav_curve,
            equity_curve: run_result.equity_curve,
            trades: run_result.trades,
            fills_detail: run_result.fills_detail,
            rebalances_triggered: run_result.rebalances_triggered,
            final_positions,
            n_bars: bars.len() as u64,
            n_strategies: self.strategies.len() as u32,
        }
    }
}

/// Instrument → 字符串元组表示（与 Python 端 `instrument_to_tuple` 对齐）
///
/// spot: `["spot", base, quote]`；swap: `["swap", base, quote, settle, contract_size]`
fn instrument_to_vec(inst: &Instrument) -> Vec<String> {
    match inst {
        Instrument::Spot(s) => vec![
            "spot".to_string(),
            s.base.as_str().to_string(),
            s.quote.as_str().to_string(),
        ],
        Instrument::Swap(s) => {
            let settle = match s.settle {
                axon_core::types::SwapSettle::UsdMargin => "usd_margin",
                axon_core::types::SwapSettle::CoinMargin => "coin_margin",
            };
            vec![
                "swap".to_string(),
                s.base.as_str().to_string(),
                s.quote.as_str().to_string(),
                settle.to_string(),
                s.contract_size.to_string(),
            ]
        }
    }
}

// ── 辅助：Action → target_position 映射 ── //

/// SDM/Cascade Action → BacktestEngine target_position（f64，+1.0 = 全多仓，-1.0 = 全空仓）
///
/// ponytail: Phase 2 只用 Buy/Sell/Hold 三态映射 +-1/0；
/// 未来接入 SDM position_size 答案后会做分档（0pct/25pct/50pct/100pct）。
fn action_to_target(act: axon_llm::decision::Action) -> f64 {
    use axon_llm::decision::Action::*;
    match act {
        Buy => 1.0,
        Sell => -1.0,
        Hold => 0.0,
    }
}

// ── 结果类型 ── //

/// BacktestRunner 最终输出（扩展自 Phase 1，加入 RunResult 撮合统计）
///
/// 字段与 `axon_backtest::RunResult` 对齐（QuantCell 结果契约依赖这些字段），
/// 平铺存储以避免 `HashMap<Instrument, _>` 的 serde map-key 问题。
#[derive(Debug, Clone, Serialize)]
pub struct BacktestResult {
    /// 已处理事件总数（RunResult 聚合）
    pub events_processed: u64,
    /// 接受的订单数
    pub orders_accepted: u64,
    /// 拒绝的订单数
    pub orders_rejected: u64,
    /// 取消的订单数
    pub orders_cancelled: u64,
    /// 修改的订单数
    pub orders_modified: u64,
    /// 成交（fill）总数
    pub fills: u64,
    /// 最终 PnL（final_nav - initial_cash）
    pub total_pnl: f64,
    /// 最大回撤（USD）
    pub max_drawdown: f64,
    /// 最大回撤比例（0~1 小数形式）
    pub max_drawdown_pct: f64,
    /// NAV 历史峰值
    pub nav_peak: f64,
    /// 最终净资产
    pub final_nav: f64,
    /// 累计手续费
    pub total_fees: f64,
    /// 累计资金费结算 PnL
    pub total_funding_pnl: f64,
    /// 胜率（0~1 小数形式）
    pub win_rate: f64,
    /// 夏普比率
    pub sharpe_ratio: f64,
    /// 运行耗时（秒）
    pub duration_secs: f64,
    /// 引擎最终时间（纳秒）
    pub final_time_ns: i64,
    /// 每 bar 所有策略的决策轨迹
    pub decisions_timeline: Vec<BarDecisionRecord>,
    /// 每 bar 末 mark-to-market NAV 曲线（L1/L2 指标用这个）
    pub bar_nav_curve: Vec<(Timestamp, f64)>,
    /// 稀疏 NAV 曲线（fill/funding 事件触发时采样）
    pub equity_curve: Vec<(Timestamp, f64)>,
    /// round-trip 配对交易记录
    pub trades: Vec<TradeRecord>,
    /// 每笔 fill 明细
    pub fills_detail: Vec<FillRecord>,
    /// 自动 rebalance 触发的下单次数
    pub rebalances_triggered: u64,
    /// 终态持仓快照（`["spot","BTC","USDT"]` → qty），只保留非零持仓
    pub final_positions: Vec<(Vec<String>, f64)>,
    /// 回测 bar 数
    pub n_bars: u64,
    /// 策略数
    pub n_strategies: u32,
}

/// 单 bar × 单策略的决策记录
#[derive(Debug, Clone, Serialize)]
pub struct BarDecisionRecord {
    pub bar_index: u64,
    pub strategy: String,
    pub symbol: String,
    #[serde(serialize_with = "ser_decision")]
    pub decision: Result<StrategyDecision, String>,
    pub bar_close: f64,
}

fn ser_decision<S>(d: &Result<StrategyDecision, String>, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match d {
        Ok(dec) => serde::Serialize::serialize(dec, s),
        Err(e) => s.serialize_str(&format!("error: {e}")),
    }
}

// ── Symbol 构造小工具 ── //

#[allow(dead_code)]
fn parse_base_symbol(market: &str) -> String {
    const QUOTES: &[&str] = &["USDT", "USDC", "USD", "USDB", "KRW", "BTC"];
    let upper = market.to_uppercase();
    for q in QUOTES {
        if upper.ends_with(q) && upper.len() > q.len() {
            return upper[..upper.len() - q.len()].to_string();
        }
    }
    upper
}

// ─────────────────────────────────────────────────────────────────────
// 集成测试：BacktestRunner → BacktestEngine 全链路
// ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::market::bar::Bar;
    use axon_core::time::Timestamp;
    use axon_core::types::{Price, Quantity};
    use axon_strategy::impl_::RuleStrategyImpl;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    /// 构造 5 根上涨趋势 Bar（BTC/USDT，1min 间隔，price 100→105）
    fn make_bars() -> Vec<Bar> {
        (0..5)
            .map(|i| {
                let ts = Timestamp::from_secs(i * 60);
                let close_f = 100.0 + i as f64;
                Bar {
                    timestamp: ts,
                    open: Price::from_f64(close_f - 0.5),
                    high: Price::from_f64(close_f + 0.3),
                    low: Price::from_f64(close_f - 0.8),
                    close: Price::from_f64(close_f),
                    volume: Quantity::from_f64(1000.0),
                }
            })
            .collect()
    }

    /// 构造触发 RuleStrategyImpl → Buy 的 state
    fn make_state() -> serde_json::Value {
        serde_json::json!({
            "features": {
                "sma5_vs_sma20": 0.02,   // 均线向上分叉
                "ret_1": 0.01,           // 近一根正收益
                "vol_20": 0.01,
                "volume_z": 0.5,
            }
        })
    }

    #[tokio::test]
    async fn backtest_runner_produces_fills_with_rule_strategy() {
        // 1) 构造：bar + state + 策略（RuleStrategyImpl 用 sma5_vs_sma20/ret_1 决策）
        let bars = make_bars();
        let mut state_by_symbol = BTreeMap::new();
        state_by_symbol.insert("BTC/USDT".into(), make_state());
        let strategy = Arc::new(RuleStrategyImpl::new("dual_ma", "BTC/USDT"));

        // 2) 跑 BacktestRunner
        let mut runner = BacktestRunner::simple(100_000.0);
        runner.add_strategy(strategy);
        let result = runner.run(&bars, &state_by_symbol, 100_000.0).await;

        // 3) 断言：决策轨迹非空 + 撮合引擎有动作（fills/orders_accepted > 0）
        assert_eq!(result.n_bars, 5);
        assert_eq!(result.n_strategies, 1);
        assert_eq!(result.decisions_timeline.len(), 5);
        // 每根 bar 都产生一个 Buy target → 自动 rebalance → 至少一次 fill
        assert!(
            result.fills > 0 || result.orders_accepted > 0,
            "expected fills or orders_accepted, got fills={} accepted={} rejected={}",
            result.fills,
            result.orders_accepted,
            result.orders_rejected
        );
        // final_nav 至少等于 initial_cash（5 根 bar 全 Buy，price 上涨应该赚）
        assert!(
            result.final_nav >= 100_000.0 - 0.01, // 0.01 容忍浮点
            "final_nav {} < initial_cash 100000",
            result.final_nav
        );
        // bar_nav_curve 有 5 帧（begin_bar_multi 每 bar 末采样一帧）
        assert_eq!(result.bar_nav_curve.len(), 5);
    }

    #[tokio::test]
    async fn backtest_runner_hold_produces_no_fill() {
        // state 设 sma5_vs_sma20=0, ret_1=0 → RuleStrategyImpl 返回 Hold → target=0.0 → rebalance no-op
        let bars = make_bars();
        let mut state_by_symbol = BTreeMap::new();
        state_by_symbol.insert(
            "BTC/USDT".into(),
            serde_json::json!({"features": {"sma5_vs_sma20": 0.0, "ret_1": 0.0, "vol_20": 0.01, "volume_z": 0.5}}),
        );
        let strategy = Arc::new(RuleStrategyImpl::new("dual_ma", "BTC/USDT"));

        let mut runner = BacktestRunner::simple(100_000.0);
        runner.add_strategy(strategy);
        let result = runner.run(&bars, &state_by_symbol, 100_000.0).await;

        // 全 Hold → target 始终 0 → rebalance 无订单 → fills=0, orders_accepted=0
        assert_eq!(result.fills, 0);
        assert_eq!(result.orders_accepted, 0);
        assert_eq!(result.orders_rejected, 0);
        assert_eq!(result.final_nav, 100_000.0);
    }
}
