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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use axon_backtest::L1MatchingEngine;
use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, EffectiveMarginSpec, FeeConfig};
use axon_backtest::margin::{LiquidationEvent, MarginSpec};
use axon_core::market::bar::Bar;
use axon_core::portfolio::{FillRecord, TradeRecord};
use axon_core::types::{SwapInstrument, SwapSettle};
use axon_core::{EventQueue, Instrument, SimulatedClock, Symbol, Timestamp};
use axon_strategy::trait_::{BarContext, Strategy, StrategyDecision};
use serde::Serialize;

// 撮合种子流动性 + 自动 rebalance 参数。
//
// ponytail: 这几个值硬编码对齐 QuantCell `backtest_loop.py` 的 `_DEFAULT_*`
// 常量，使 Rust 编排路径与现有 Python 编排路径撮合口径一致（parity）。
// 上限：跨项目耦合，多消费者或需调参时改走 `BacktestRunner` 构造参数下发。
const REBALANCE_THRESHOLD: f64 = 0.001; // 对应 _DEFAULT_AUTO_REBALANCE_THRESHOLD
const SEED_HALF_SPREAD: f64 = 0.0005; // 对应 _DEFAULT_HALF_SPREAD_RATIO
const SEED_DEPTH_LEVELS: usize = 5; // 对应 _DEFAULT_DEPTH_LEVELS
const SEED_SIZE_PER_LEVEL: f64 = 100.0; // 对应 _DEFAULT_SIZE_PER_LEVEL

/// 回测使用的品种类型（现货 / 永续）
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InstrumentKind {
    /// 现货
    Spot,
    /// 永续合约（保证金结算方式 + 合约乘数）
    Swap {
        settle: SwapSettle,
        contract_size: f64,
    },
}

/// 回测运行器
///
/// 持有一个 BacktestEngine（L1 撮合 + SimulatedClock + EventQueue），
/// 逐 bar 编排 Strategy.decide() → set_target_position → begin_bar_multi → rebalance。
pub struct BacktestRunner {
    strategies: Vec<Arc<dyn Strategy>>,
    initial_cash: f64,
    force_liquidate: bool,
    kind: InstrumentKind,
    /// 每品种保证金规格（key = "BTC/USDT"）。空 map = 不启用保证金模拟
    margin_specs: HashMap<String, MarginSpec>,
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
            kind: InstrumentKind::Spot,
            margin_specs: HashMap::new(),
        }
    }

    /// 最简构造器（给测试/快速原型用）
    pub fn simple(initial_cash: f64) -> Self {
        Self {
            strategies: Vec::new(),
            initial_cash,
            force_liquidate: false,
            kind: InstrumentKind::Spot,
            margin_specs: HashMap::new(),
        }
    }

    /// 注入每品种保证金规格（空 map = 不启用保证金模拟）
    pub fn with_margin(mut self, specs: HashMap<String, MarginSpec>) -> Self {
        self.margin_specs = specs;
        self
    }

    /// 原地替换保证金规格（给 Python 绑定用）
    ///
    /// 用 setter 而不是 `mem::replace` 重建 runner：后者会把已注册策略、
    /// futures 品种类型等状态一起丢掉。
    pub fn set_margin_specs(&mut self, specs: HashMap<String, MarginSpec>) {
        self.margin_specs = specs;
    }

    /// 切换为永续合约品种（futures 模式，支持资金费结算）
    pub fn with_futures(mut self, settle: SwapSettle, contract_size: f64) -> Self {
        self.kind = InstrumentKind::Swap {
            settle,
            contract_size,
        };
        self
    }

    /// 回测结束 EOD 是否强制市价平仓（透传到 `BacktestEngineConfig.force_liquidate`）
    pub fn with_force_liquidate(mut self, on: bool) -> Self {
        self.force_liquidate = on;
        self
    }

    /// 按 `kind` 构造 Instrument：先复用 `from_symbol` 拆 base/quote，再按需改造成 Swap。
    /// symbol 无 `BASE/QUOTE` 分隔符时 `from_symbol` 退化为默认品种，此处原样返回。
    fn make_instrument(&self, sym: &str) -> Instrument {
        let parsed = Instrument::from_symbol(&Symbol::from(sym));
        match (self.kind, parsed) {
            (
                InstrumentKind::Swap {
                    settle,
                    contract_size,
                },
                Instrument::Spot(s),
            ) => Instrument::Swap(SwapInstrument {
                base: s.base,
                quote: s.quote,
                settle,
                contract_size,
            }),
            (_, other) => other,
        }
    }

    pub fn add_strategy(&mut self, s: Arc<dyn Strategy>) {
        self.strategies.push(s);
    }

    pub fn with_strategy(mut self, s: Arc<dyn Strategy>) -> Self {
        self.strategies.push(s);
        self
    }

    /// 回测入口（单品种与多品种共享账户统一走这一条路径）
    ///
    /// 接受**按网格对齐**的 per-symbol bar 序列，每个 instrument 用各自价格参与
    /// `begin_bar_multi`，所有策略共用同一个 `BacktestEngine`（同一笔 initial_cash）。
    /// 单品种回测只需传一个 symbol 的完整 bar 列表（无 `None` 缺口）。
    ///
    /// # 参数
    /// - `bars_by_symbol`：每个 symbol 的**等长**逐网格点 bar 列表。`None` 表示该
    ///   symbol 在该网格点无真实 bar：价格按前向填充参与盯市，但不触发策略决策。
    ///   所有向量必须等长，且每个下标至少有一个 `Some`（网格由调用方取并集保证）
    /// - `states_by_symbol`：逐 bar state（下标与 `bars_by_symbol` 对齐）
    /// - `funding_by_symbol`：资金费查找表（键为纳秒时间戳）
    /// - `capital_budget`：资金约束预算（调用方传 `initial_cash × leverage`）。
    ///   `Some(b)` 时，每个网格点汇总全部 instrument 的目标名义价值，超过 `b`
    ///   则对所有目标按同一比例缩减，保证「总名义敞口 ≤ b」。单品种传 `None`。
    ///
    ///   注意：启用保证金（`margin_specs` 非空）时预算会被强制停用。
    ///   原因是 shared 模式的预算缩放会把目标数量按比例压回 `initial_cash` 口径，
    ///   而杠杆放大正是通过目标数量放大实现的 —— 两者同时生效等于杠杆被抵消；
    ///   敞口约束改由每品种杠杆 + 维持保证金/强平判定接管。
    ///
    /// # Hold 语义
    /// `Hold` 决策沿用**上一次已生效（可能已被约束缩放）**的目标，而不是策略
    /// 自报值——否则约束缩放后会被下一次 hold 复原，产生反复调仓抖动。
    ///
    /// ponytail: 约束预算静态（不随净值浮动），升级路径是引擎暴露回测中途 NAV
    /// 后改用实时权益；当前引擎 `compute_nav` 私有，无中途查询接口。
    pub async fn run(
        &self,
        bars_by_symbol: &BTreeMap<String, Vec<Option<Bar>>>,
        states_by_symbol: &BTreeMap<String, Vec<serde_json::Value>>,
        funding_by_symbol: &BTreeMap<String, BTreeMap<i64, (f64, f64)>>,
        capital_budget: Option<f64>,
    ) -> BacktestResult {
        // 网格长度：调用方保证所有 symbol 等长
        let grid_len = bars_by_symbol.values().map(|v| v.len()).max().unwrap_or(0);
        assert!(grid_len > 0, "bars_by_symbol must not be empty");
        for (sym, v) in bars_by_symbol {
            assert_eq!(v.len(), grid_len, "bars_by_symbol[{sym}] 长度不一致");
        }

        let initial_cash = self.initial_cash;
        // 保证金与资金预算互斥：启用保证金时停用预算缩放，否则杠杆会被预算按比例抵消。
        let margin_enabled = !self.margin_specs.is_empty();
        let budget = if margin_enabled { None } else { capital_budget };

        // 1) 收集策略涉及的所有 symbol（去重）
        let unique_symbols: Vec<String> = {
            let mut set = BTreeMap::new();
            for s in &self.strategies {
                set.entry(s.symbol().to_string()).or_insert_with(|| ());
            }
            set.into_keys().collect()
        };

        // 2) 为每个 symbol 预构造 Instrument
        let instruments: BTreeMap<String, Instrument> = unique_symbols
            .iter()
            .map(|sym| (sym.clone(), self.make_instrument(sym)))
            .collect();

        // 3) 构造 BacktestEngine（单账户：所有 instrument 共享同一笔 initial_cash）
        let first_ts = unique_symbols
            .iter()
            .filter_map(|sym| {
                bars_by_symbol
                    .get(sym)
                    .and_then(|v| v.iter().flatten().next())
            })
            .map(|bar| bar.timestamp)
            .next()
            .expect("每个网格点至少有一个真实 bar");
        let mut engine = BacktestEngine::new(
            BacktestEngineConfig {
                clock: SimulatedClock::new(first_ts),
                matching_engine: Box::new(L1MatchingEngine::new()),
                impact_model: None,
                initial_cash,
                fee_config: FeeConfig::default(),
                force_liquidate: self.force_liquidate,
                // key 已是 "BASE/QUOTE" 字符串，引擎侧不再做 Instrument 转换
                margin_specs: self.margin_specs.clone(),
            },
            EventQueue::new(),
        );
        // 与 run() 完全一致的撮合配置（parity 常量见模块顶部）
        engine.with_auto_rebalance(REBALANCE_THRESHOLD);
        engine.with_seed_liquidity(SEED_HALF_SPREAD, SEED_DEPTH_LEVELS, SEED_SIZE_PER_LEVEL);

        // 4) 逐网格点编排
        let mut decisions_timeline: Vec<BarDecisionRecord> =
            Vec::with_capacity(grid_len * self.strategies.len().max(1));
        // 各 instrument 最近一次真实 bar 的 close（前向填充，用于盯市与名义价值折算）
        let mut last_price: BTreeMap<String, f64> = BTreeMap::new();
        // 各 instrument 当前生效的目标数量（Hold 沿用此值，避免约束缩放被复原）
        let mut effective_targets: BTreeMap<String, f64> = BTreeMap::new();

        for grid_idx in 0..grid_len {
            // 4a) 本网格点时间戳：取任一真实 bar（网格为并集，必有）
            let ts = unique_symbols
                .iter()
                .filter_map(|sym| bars_by_symbol.get(sym).and_then(|v| v[grid_idx].as_ref()))
                .map(|bar| bar.timestamp)
                .next()
                .expect("每个网格点至少有一个真实 bar");
            engine.set_clock(ts);

            // 4b) 先更新前向填充价格
            for sym in &unique_symbols {
                if let Some(bar) = bars_by_symbol.get(sym).and_then(|v| v[grid_idx].as_ref()) {
                    last_price.insert(sym.clone(), bar.close.as_f64());
                }
            }

            // 4b-1) 上一根 bar 被强平的品种快照（在 4c 的自动 rebalance 之前取）：
            // 引擎已把 leg 目标清零，这里再挡住 Hold 决策把旧目标原样写回去。
            let liquidated_before = if margin_enabled {
                engine.liquidated_symbols().clone()
            } else {
                HashSet::new()
            };

            // 4c) begin_bar_multi：只含已有价格的 instrument（尚未上市者不参与）
            let legs: Vec<(Instrument, f64)> = unique_symbols
                .iter()
                .filter_map(|sym| {
                    let px = *last_price.get(sym)?;
                    Some((instruments.get(sym)?.clone(), px))
                })
                .collect();
            engine.begin_bar_multi(legs);

            // 4d) 逐策略决策：仅当本网格点该 symbol 有真实 bar
            for strategy in &self.strategies {
                let sym = strategy.symbol();
                let Some(bar) = bars_by_symbol.get(sym).and_then(|v| v[grid_idx].as_ref()) else {
                    continue;
                };

                let state: serde_json::Value = states_by_symbol
                    .get(sym)
                    .and_then(|v| v.get(grid_idx))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);

                let ctx = BarContext {
                    symbol: sym,
                    bar_index: grid_idx as u64,
                    timestamp_ns: bar.timestamp.nanos as u64,
                    bar,
                    state: &state,
                };

                let decision = strategy.decide(&ctx).await;
                let decision_for_timeline = match &decision {
                    Ok(dec) => Ok(dec.clone()),
                    Err(e) => Err(e.to_string()),
                };
                decisions_timeline.push(BarDecisionRecord {
                    bar_index: grid_idx as u64,
                    strategy: strategy.name().into(),
                    symbol: sym.into(),
                    decision: decision_for_timeline,
                    bar_close: bar.close.as_f64(),
                });

                if let Ok(ref dec) = decision {
                    let target = if dec.action == axon_llm::decision::Action::Hold {
                        if liquidated_before.contains(&instrument_key_for(sym)) {
                            // 刚被强平的品种不能靠 Hold 沿用旧目标复活
                            0.0
                        } else {
                            // Hold：沿用上次生效目标（含约束缩放结果），而非策略自报值
                            *effective_targets.get(sym).unwrap_or(&0.0)
                        }
                    } else {
                        dec.target_position
                            .unwrap_or_else(|| action_to_target(dec.action))
                    };
                    effective_targets.insert(sym.to_string(), target);
                }
            }

            // 4e) 资金约束：总名义敞口 ≤ budget，超出则全部目标同比例缩减
            // `budget` 在启用保证金时已置 None（见函数开头互斥处理）
            if let Some(budget) = budget {
                let total_notional: f64 = effective_targets
                    .iter()
                    .filter_map(|(sym, qty)| last_price.get(sym).map(|px| qty.abs() * px))
                    .sum();
                if total_notional > budget && total_notional > 0.0 {
                    let scale = budget / total_notional;
                    for qty in effective_targets.values_mut() {
                        *qty *= scale;
                    }
                }
            }

            // 4f) 写入目标位（引擎下一次 rebalance 用它撮合）
            for sym in &unique_symbols {
                let Some(inst) = instruments.get(sym) else {
                    continue;
                };
                let target = *effective_targets.get(sym).unwrap_or(&0.0);
                engine.set_target_position(inst.clone(), target);
            }

            // 4g) 显式 rebalance：语义与 run() 完全一致（guarded by bar_id）
            engine.rebalance_to_target(None);

            // 4h) 资金费结算：仅本网格点有真实 bar 的 symbol
            let bar_ts_ns = ts.nanos;
            let mut funding_pushed = false;
            for sym in &unique_symbols {
                if bars_by_symbol
                    .get(sym)
                    .and_then(|v| v[grid_idx].as_ref())
                    .is_none()
                {
                    continue;
                }
                let Some(sched) = funding_by_symbol.get(sym) else {
                    continue;
                };
                let Some(&(rate, mark)) = sched.get(&bar_ts_ns) else {
                    continue;
                };
                if let Some(inst) = instruments.get(sym) {
                    let fallback = last_price.get(sym).copied().unwrap_or(0.0);
                    // mark 缺数据时用该品种最近 close 兜底（对齐 run 的 bar.close 兜底）
                    let mark_price = if mark > 0.0 { mark } else { fallback };
                    engine.push_funding(inst.clone(), rate, mark_price, ts);
                    funding_pushed = true;
                }
            }
            if funding_pushed {
                while engine.pending_events() > 0 {
                    engine.step();
                }
            }

            // 4i) 保证金检查与强平：放在资金费结算之后，确保本 bar 的权益口径完整
            if margin_enabled {
                engine.check_margin();
            }
        }

        // 5) run() 消费 EventQueue + 聚合 RunResult
        let run_result = engine.run();

        let final_positions: Vec<(Instrument, f64)> = run_result
            .positions
            .iter()
            .map(|(inst, qty)| (inst.clone(), *qty))
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
            bar_balance_curve: run_result.bar_balance_curve,
            equity_curve: run_result.equity_curve,
            trades: run_result.trades,
            fills_detail: run_result.fills_detail,
            rebalances_triggered: run_result.rebalances_triggered,
            final_positions,
            n_bars: grid_len as u64,
            n_strategies: self.strategies.len() as u32,
            liquidation_events: run_result.liquidation_events.clone(),
            insurance_fund_loss: run_result.insurance_fund_loss,
            price_basis: run_result.price_basis.clone(),
            margin_applied: run_result.margin_applied,
            margin_specs_effective: run_result.margin_specs_effective,
            liquidation_count: run_result.liquidation_count,
            margin_shortfall_count: run_result.margin_shortfall_count,
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

/// 把 "BTCUSDT" / "BTC-USDT" 归一化为引擎/保证金模块使用的 "BTC/USDT"
///
/// 与 QuantCell `_normalize_symbol` 同规则：已有分隔符原样保留，否则去掉
/// "-"/"_" 后按已知计价币后缀切分（长的优先，避免 USDC 被 USD 抢先截断）。
pub(crate) fn instrument_key_for(sym: &str) -> String {
    let s = sym.to_uppercase();
    if s.contains('/') {
        return s;
    }
    let s = s.replace(['-', '_'], "");
    for quote in ["USDT", "USDC", "USD", "BTC", "ETH"] {
        if s.ends_with(quote) && s.len() > quote.len() {
            return format!("{}/{}", &s[..s.len() - quote.len()], quote);
        }
    }
    s
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
    /// 每 bar 末 balance 存款曲线（与 `bar_nav_curve` 逐帧对齐，`(timestamp_ns, balance)`）
    ///
    /// `balance = cash + Σ(qty × avg_cost)`；`nav - balance` 即浮盈。
    pub bar_balance_curve: Vec<(Timestamp, f64)>,
    /// 稀疏 NAV 曲线（fill/funding 事件触发时采样）
    pub equity_curve: Vec<(Timestamp, f64)>,
    /// round-trip 配对交易记录
    pub trades: Vec<TradeRecord>,
    /// 每笔 fill 明细
    pub fills_detail: Vec<FillRecord>,
    /// 自动 rebalance 触发的下单次数
    pub rebalances_triggered: u64,
    /// 终态持仓快照（`Instrument` → qty），只保留非零持仓
    pub final_positions: Vec<(Instrument, f64)>,
    /// 回测 bar 数
    pub n_bars: u64,
    /// 策略数
    pub n_strategies: u32,
    /// 强平事件时间线（未启用保证金时为空）
    pub liquidation_events: Vec<LiquidationEvent>,
    /// 穿仓兜底累计额（保险基金语义，未启用保证金时恒为 0）
    pub insurance_fund_loss: f64,
    /// 判定价口径（v1 固定 `"close"`）
    pub price_basis: String,
    /// 是否启用保证金账务（`margin_specs` 非空即启用）
    pub margin_applied: bool,
    /// 实际生效的每品种规格（含档位来源），用于前端核对
    pub margin_specs_effective: Vec<EffectiveMarginSpec>,
    /// 强平事件条数（= `liquidation_events.len()`）
    pub liquidation_count: u32,
    /// 因可用保证金不足而被缩量的开仓次数
    pub margin_shortfall_count: u32,
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
        // 逐 bar state：每根 bar 都是同一 features（策略只看当 bar state）
        let mut state_by_symbol = BTreeMap::new();
        state_by_symbol.insert("BTC/USDT".into(), vec![make_state(); bars.len()]);
        let strategy = Arc::new(RuleStrategyImpl::new("dual_ma", "BTC/USDT"));

        // 2) 跑 BacktestRunner（单品种：包装成单 symbol 的网格，无缺口）
        let mut runner = BacktestRunner::simple(100_000.0);
        runner.add_strategy(strategy);
        let bars_by_symbol = BTreeMap::from([(
            "BTC/USDT".to_string(),
            bars.iter().cloned().map(Some).collect::<Vec<_>>(),
        )]);
        let result = runner
            .run(&bars_by_symbol, &state_by_symbol, &BTreeMap::new(), None)
            .await;

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
            vec![
                serde_json::json!({"features": {"sma5_vs_sma20": 0.0, "ret_1": 0.0, "vol_20": 0.01, "volume_z": 0.5}});
                bars.len()
            ],
        );
        let strategy = Arc::new(RuleStrategyImpl::new("dual_ma", "BTC/USDT"));

        let mut runner = BacktestRunner::simple(100_000.0);
        runner.add_strategy(strategy);
        let bars_by_symbol = BTreeMap::from([(
            "BTC/USDT".to_string(),
            bars.iter().cloned().map(Some).collect::<Vec<_>>(),
        )]);
        let result = runner
            .run(&bars_by_symbol, &state_by_symbol, &BTreeMap::new(), None)
            .await;

        // 全 Hold → target 始终 0 → rebalance 无订单 → fills=0, orders_accepted=0
        assert_eq!(result.fills, 0);
        assert_eq!(result.orders_accepted, 0);
        assert_eq!(result.orders_rejected, 0);
        assert_eq!(result.final_nav, 100_000.0);
    }

    /// 构造指定起价与斜率的 bar 序列（1min 间隔）
    fn make_bars_from(start_price: f64, step: f64, n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let close_f = start_price + i as f64 * step;
                Bar {
                    timestamp: Timestamp::from_secs(i as i64 * 60),
                    open: Price::from_f64(close_f - 0.5),
                    high: Price::from_f64(close_f + 0.3),
                    low: Price::from_f64(close_f - 0.8),
                    close: Price::from_f64(close_f),
                    volume: Quantity::from_f64(1000.0),
                }
            })
            .collect()
    }

    /// 不启用资金约束：两个品种按各自价格成交，每笔成交的 instrument 归属正确
    #[tokio::test]
    async fn run_multi_two_symbols_uses_per_symbol_prices() {
        const N: usize = 5;
        let btc = make_bars_from(100.0, 1.0, N);
        let eth = make_bars_from(20.0, 0.5, N);

        let mut bars_by_symbol: BTreeMap<String, Vec<Option<Bar>>> = BTreeMap::new();
        bars_by_symbol.insert("BTC/USDT".into(), btc.into_iter().map(Some).collect());
        bars_by_symbol.insert("ETH/USDT".into(), eth.into_iter().map(Some).collect());

        let mut states: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
        states.insert("BTC/USDT".into(), vec![make_state(); N]);
        states.insert("ETH/USDT".into(), vec![make_state(); N]);

        let mut runner = BacktestRunner::simple(100_000.0);
        runner.add_strategy(Arc::new(RuleStrategyImpl::new("dual_ma", "BTC/USDT")));
        runner.add_strategy(Arc::new(RuleStrategyImpl::new("dual_ma", "ETH/USDT")));

        let result = runner
            .run(&bars_by_symbol, &states, &BTreeMap::new(), None)
            .await;

        assert_eq!(result.n_bars, N as u64);
        assert_eq!(result.n_strategies, 2);
        // 两个品种各自成交 → 每笔 fill 明细至少覆盖两个 instrument
        //
        // 注：`RuleStrategyImpl` 全程 Buy，不会产生平仓配对，故 `trades`（round-trip）
        // 为空；改用 `fills_detail`（每笔 fill 均带 instrument）验证跨品种成交归属。
        let instruments_seen: std::collections::BTreeSet<String> = result
            .fills_detail
            .iter()
            .map(|f| {
                format!(
                    "{}/{}",
                    f.instrument.base().as_str(),
                    f.instrument.quote().as_str()
                )
            })
            .collect();
        assert!(
            instruments_seen.len() >= 2,
            "expected fills from both instruments, got {instruments_seen:?}"
        );
        // 每 bar 一帧
        assert_eq!(result.bar_nav_curve.len(), N);
    }

    /// 启用资金约束：两个品种各要 1 单位，价格恒定 → 总名义敞口被缩放到 budget
    ///
    /// 注：`RuleStrategyImpl` 在 Buy 时不带 target_position，`run` 语义回退到
    /// `action_to_target(Buy) = 1.0`（绝对 1 单位）。故两品种名义价值 = 100 + 20 = 120，
    /// 恒为常量，便于精确断言缩放后总敞口 == budget。
    #[tokio::test]
    async fn run_multi_capital_guard_scales_targets() {
        const N: usize = 3;
        const BTC_PX: f64 = 100.0;
        const ETH_PX: f64 = 20.0;
        // 价格恒定（step = 0），使名义价值与网格点无关，断言可精确到常量
        let btc = make_bars_from(BTC_PX, 0.0, N);
        let eth = make_bars_from(ETH_PX, 0.0, N);

        let mut bars_by_symbol: BTreeMap<String, Vec<Option<Bar>>> = BTreeMap::new();
        bars_by_symbol.insert("BTC/USDT".into(), btc.into_iter().map(Some).collect());
        bars_by_symbol.insert("ETH/USDT".into(), eth.into_iter().map(Some).collect());

        let mut states: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
        states.insert("BTC/USDT".into(), vec![make_state(); N]);
        states.insert("ETH/USDT".into(), vec![make_state(); N]);

        let mut runner = BacktestRunner::simple(1_000.0);
        runner.add_strategy(Arc::new(RuleStrategyImpl::new("dual_ma", "BTC/USDT")));
        runner.add_strategy(Arc::new(RuleStrategyImpl::new("dual_ma", "ETH/USDT")));

        // budget = 1.0，远小于两品种原始名义 120 → 必须缩放到总敞口 ≤ 1.0
        let result = runner
            .run(&bars_by_symbol, &states, &BTreeMap::new(), Some(1.0))
            .await;

        let final_notional: f64 = result
            .final_positions
            .iter()
            .map(|(inst, qty)| {
                let px = if inst.base().as_str() == "BTC" {
                    BTC_PX
                } else {
                    ETH_PX
                };
                qty.abs() * px
            })
            .sum();
        assert!(
            final_notional <= 1.0 * 1.01,
            "final gross notional {final_notional} should be capped by budget 1.0"
        );
        assert!(
            final_notional > 0.5,
            "guard should scale down, not wipe out positions (got {final_notional})"
        );
        // 未启用保证金 → 预算未被接管，仍按原口径生效
        assert!(!result.margin_applied);
    }

    /// 单品种最小保证金规格（币安 BTC 量级，1 档）
    fn btc_margin_spec() -> MarginSpec {
        MarginSpec {
            leverage: 10.0,
            mode: axon_backtest::MarginMode::Cross,
            brackets: vec![axon_backtest::Bracket {
                notional_floor: 0.0,
                notional_cap: None,
                mmr: 0.005,
                cum: 0.0,
                liquidation_fee_rate: 0.001,
                max_leverage: 125.0,
            }],
            bracket_source: "default".into(),
        }
    }

    /// 启用保证金 → 预算被停用，结果透出生效规格与强平统计
    #[tokio::test]
    async fn margin_specs_disable_capital_budget() {
        let bars = make_bars();
        let state_by_symbol =
            BTreeMap::from([("BTC/USDT".to_string(), vec![make_state(); bars.len()])]);
        let bars_by_symbol = BTreeMap::from([(
            "BTC/USDT".to_string(),
            bars.iter().cloned().map(Some).collect::<Vec<_>>(),
        )]);

        let mut runner = BacktestRunner::simple(100_000.0)
            .with_margin(HashMap::from([("BTC/USDT".to_string(), btc_margin_spec())]));
        runner.add_strategy(Arc::new(RuleStrategyImpl::new("dual_ma", "BTC/USDT")));

        // 同时传预算：启用保证金时应被忽略，并在结果里标记
        let result = runner
            .run(
                &bars_by_symbol,
                &state_by_symbol,
                &BTreeMap::new(),
                Some(1.0),
            )
            .await;

        assert!(result.margin_applied);
        assert_eq!(result.margin_specs_effective.len(), 1);
        assert_eq!(result.margin_specs_effective[0].symbol, "BTC/USDT");
        assert_eq!(result.margin_specs_effective[0].bracket_source, "default");
        assert_eq!(
            result.liquidation_count,
            result.liquidation_events.len() as u32
        );
        // 未触发强平时结果字段保持默认口径
        assert_eq!(result.price_basis, "close");
        assert_eq!(result.insurance_fund_loss, 0.0);
    }

    /// 未启用保证金 → 结果里的保证金字段全为默认值
    #[tokio::test]
    async fn no_margin_specs_reports_margin_defaults() {
        let bars = make_bars();
        let state_by_symbol =
            BTreeMap::from([("BTC/USDT".to_string(), vec![make_state(); bars.len()])]);
        let bars_by_symbol = BTreeMap::from([(
            "BTC/USDT".to_string(),
            bars.iter().cloned().map(Some).collect::<Vec<_>>(),
        )]);

        let mut runner = BacktestRunner::simple(100_000.0);
        runner.add_strategy(Arc::new(RuleStrategyImpl::new("dual_ma", "BTC/USDT")));

        let result = runner
            .run(
                &bars_by_symbol,
                &state_by_symbol,
                &BTreeMap::new(),
                Some(1.0),
            )
            .await;

        assert!(!result.margin_applied);
        assert!(result.margin_specs_effective.is_empty());
        assert_eq!(result.liquidation_count, 0);
        assert_eq!(result.margin_shortfall_count, 0);
        assert!(result.liquidation_events.is_empty());
    }

    /// None 占位：某品种前两根网格点无 bar → 该品种不参与搓合，也不影响另一品种
    #[tokio::test]
    async fn run_multi_none_slots_are_forward_filled_not_decided() {
        const N: usize = 4;
        let btc = make_bars_from(100.0, 1.0, N);
        // ETH 只有后两根 bar，前两根用 None 占位（网格 = BTC 的并集）
        let eth_full = make_bars_from(20.0, 0.5, N);

        let mut bars_by_symbol: BTreeMap<String, Vec<Option<Bar>>> = BTreeMap::new();
        bars_by_symbol.insert("BTC/USDT".into(), btc.into_iter().map(Some).collect());
        bars_by_symbol.insert(
            "ETH/USDT".into(),
            vec![None, None, Some(eth_full[2]), Some(eth_full[3])],
        );

        let mut states: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
        states.insert("BTC/USDT".into(), vec![make_state(); N]);
        states.insert("ETH/USDT".into(), vec![make_state(); N]);

        let mut runner = BacktestRunner::simple(100_000.0);
        runner.add_strategy(Arc::new(RuleStrategyImpl::new("dual_ma", "BTC/USDT")));
        runner.add_strategy(Arc::new(RuleStrategyImpl::new("dual_ma", "ETH/USDT")));

        let result = runner
            .run(&bars_by_symbol, &states, &BTreeMap::new(), None)
            .await;

        // 决策轨迹仍每 bar 一条（BTC 每 bar 都有真实 bar），ETH 只在后两根出现
        let eth_decisions = result
            .decisions_timeline
            .iter()
            .filter(|d| d.symbol == "ETH/USDT")
            .count();
        assert_eq!(eth_decisions, 2, "ETH 只应在有真实 bar 的网格点决策");
        assert_eq!(result.bar_nav_curve.len(), N);
    }

    /// PnL parity（关键 invariant）：共享账户无资金约束时，多品种共享 `run` 的
    /// `total_pnl` 必须等于各品种**独立跑** `run` 的 `total_pnl` 之和。
    ///
    /// 为什么必须断言这条：`total_pnl = final_nav - initial_cash`，而 `final_nav` 取自
    /// `bar_nav_curve` 末帧（engine `build_result`）。一旦盯市估值把某品种的持仓用
    /// 另一品种的价格折算，PnL 会整体失真，但 **fills / fees / 终态持仓数量仍然正确**
    /// —— 只看撮合结果查不出这个 bug（0.19.0 修的就是 `compute_nav` 的标量 fallback）。
    ///
    /// 用恒定价格（step = 0）使两品种名义价值为常量，误差不随网格点漂移。
    #[tokio::test]
    async fn run_multi_pnl_matches_sum_of_independent_runs() {
        const N: usize = 3;
        const BTC_PX: f64 = 100.0;
        const ETH_PX: f64 = 20.0;
        let btc = make_bars_from(BTC_PX, 0.0, N);
        let eth = make_bars_from(ETH_PX, 0.0, N);

        // -- 独立：两个单品种 run（各自包装成单 symbol 网格）--
        let mut r_btc = BacktestRunner::simple(100_000.0);
        r_btc.add_strategy(Arc::new(RuleStrategyImpl::new("dual_ma", "BTC/USDT")));
        let btc_states = BTreeMap::from([("BTC/USDT".to_string(), vec![make_state(); N])]);
        let btc_solo_bars = BTreeMap::from([(
            "BTC/USDT".to_string(),
            btc.iter().cloned().map(Some).collect::<Vec<_>>(),
        )]);
        let solo_btc = r_btc
            .run(&btc_solo_bars, &btc_states, &BTreeMap::new(), None)
            .await;

        let mut r_eth = BacktestRunner::simple(100_000.0);
        r_eth.add_strategy(Arc::new(RuleStrategyImpl::new("dual_ma", "ETH/USDT")));
        let eth_states = BTreeMap::from([("ETH/USDT".to_string(), vec![make_state(); N])]);
        let eth_solo_bars = BTreeMap::from([(
            "ETH/USDT".to_string(),
            eth.iter().cloned().map(Some).collect::<Vec<_>>(),
        )]);
        let solo_eth = r_eth
            .run(&eth_solo_bars, &eth_states, &BTreeMap::new(), None)
            .await;

        // -- 共享：一个多品种 run --
        let mut bars_by_symbol: BTreeMap<String, Vec<Option<Bar>>> = BTreeMap::new();
        bars_by_symbol.insert("BTC/USDT".into(), btc.into_iter().map(Some).collect());
        bars_by_symbol.insert("ETH/USDT".into(), eth.into_iter().map(Some).collect());
        let states: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::from([
            ("BTC/USDT".to_string(), vec![make_state(); N]),
            ("ETH/USDT".to_string(), vec![make_state(); N]),
        ]);

        let mut r_multi = BacktestRunner::simple(100_000.0);
        r_multi.add_strategy(Arc::new(RuleStrategyImpl::new("dual_ma", "BTC/USDT")));
        r_multi.add_strategy(Arc::new(RuleStrategyImpl::new("dual_ma", "ETH/USDT")));
        let shared = r_multi
            .run(&bars_by_symbol, &states, &BTreeMap::new(), None)
            .await;

        let expected = solo_btc.total_pnl + solo_eth.total_pnl;
        assert!(
            (shared.total_pnl - expected).abs() < 1e-6,
            "run_multi total_pnl {} 与独立之和 {} 不一致（差 {}）；\
             差值若约等于 某品种持仓量×(末 leg 价格−自身价格)，说明 compute_nav \
             的 fallback 又把某品种持仓用别的品种价格估值了",
            shared.total_pnl,
            expected,
            shared.total_pnl - expected
        );
    }

    /// 第一根 bar 开仓、随后暴跌被强平、之后一直 Hold：仓位不得被 Hold 复活
    #[tokio::test]
    async fn hold_after_liquidation_does_not_resurrect_position() {
        struct OpenThenHold {
            symbol: String,
        }

        #[async_trait::async_trait]
        impl Strategy for OpenThenHold {
            fn name(&self) -> &str {
                "open_then_hold"
            }

            fn symbol(&self) -> &str {
                &self.symbol
            }

            async fn decide(
                &self,
                ctx: &BarContext<'_>,
            ) -> Result<StrategyDecision, axon_strategy::trait_::StrategyError> {
                use axon_llm::decision::Action;
                if ctx.bar_index == 0 {
                    Ok(StrategyDecision::from_rule(Action::Buy).with_target_position(50.0))
                } else {
                    Ok(StrategyDecision::from_rule(Action::Hold))
                }
            }
        }

        // 首根 100 开仓，第二根直接跌到 1 → 20x 全仓深度穿仓，之后维持 1
        let closes = [100.0, 1.0, 1.0, 1.0];
        let bars: Vec<Option<Bar>> = closes
            .iter()
            .enumerate()
            .map(|(i, &close)| {
                Some(Bar {
                    timestamp: Timestamp::from_secs(i as i64 * 60),
                    open: Price::from_f64(close),
                    high: Price::from_f64(close),
                    low: Price::from_f64(close),
                    close: Price::from_f64(close),
                    volume: Quantity::from_f64(1000.0),
                })
            })
            .collect();
        let bars_by_symbol = BTreeMap::from([("LTC/USDT".to_string(), bars)]);
        let states = BTreeMap::from([(
            "LTC/USDT".to_string(),
            vec![serde_json::Value::Null; closes.len()],
        )]);

        let spec = MarginSpec {
            leverage: 20.0,
            mode: axon_backtest::MarginMode::Cross,
            brackets: vec![axon_backtest::Bracket {
                notional_floor: 0.0,
                notional_cap: None,
                mmr: 0.004,
                cum: 0.0,
                liquidation_fee_rate: 0.001,
                max_leverage: 125.0,
            }],
            bracket_source: "default".into(),
        };
        let mut runner = BacktestRunner::simple(1_000.0)
            .with_futures(SwapSettle::UsdMargin, 1.0)
            .with_margin(HashMap::from([("LTC/USDT".to_string(), spec)]));
        runner.add_strategy(Arc::new(OpenThenHold {
            symbol: "LTC/USDT".into(),
        }));

        let result = runner
            .run(&bars_by_symbol, &states, &BTreeMap::new(), None)
            .await;

        assert!(
            !result.liquidation_events.is_empty(),
            "暴跌必须产生强平事件"
        );
        assert!(
            result.final_positions.is_empty(),
            "强平后 Hold 不得把旧目标复活，终态持仓应为空：{:?}",
            result.final_positions
        );
    }
}
