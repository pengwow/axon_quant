//! M3 Shadow oracle 的 10 组场景（验收门：全部 `report.matched == true`）
//!
//! 同一份网格输入分别喂给两条路径，差异按类别计数（见
//! [`axon_worker::shadow`]），每组把报告落盘到
//! `$CARGO_TARGET_TMPDIR/shadow-reports/<scenario>.json`（不污染仓库根）。
//!
//! # 场景适用性
//!
//! | # | 场景 | 结论 |
//! |---|------|------|
//! | 1 | 单品种 | 做：30 根之字形 bar，多次 Buy/Sell/Hold |
//! | 2 | 多品种 | 做：两品种 + `None` 缺口，**每帧最多一个品种调仓**（规避引擎 `HashMap` 迭代序导致的 order id / 成交顺序漂移） |
//! | 3 | 资金费 | 做：永续 + 两个结算点（一个 mark 缺省走 close 兜底） |
//! | 4 | 强平 | 做：20x 全仓 + 暴跌穿仓，验证强平事件与「强平后 Hold 不复活」 |
//! | 5 | 多策略 | 做：同品种两个策略，验证「后者覆盖前者」的顺序一致性 |
//! | 6 | 部分成交 | 做：目标数量远超 L1 种子深度（5 层 × 100），逐 bar 部分成交 |
//! | 7 | 冲击模型 | **不做**：旧 `BacktestRunner::run()` 硬编码 `impact_model: None`，新内核 `KernelCore::new` 同样硬编码 `None`，两条路径都没有冲击模型注入入口，无法构造可比场景。保留 `#[ignore]` 用例记录该缺口 |
//! | 8 | 跨零仓位 | 做：目标 +2 → -2 单次调仓穿越零轴 |
//! | 9 | 空 bar | 做：品种未上市的前导 `None` 缺口 + 零成交量 bar（价格前向填充） |
//! | 10 | 长周期 | 做：500 根 bar |
//!
//! # 为什么必须「每帧最多一个品种调仓」
//!
//! 引擎 `bt_state.legs` 是 `HashMap`，`rebalance_to_target` 按它的迭代序发单，
//! order id 来自全局自增计数器。两个 `HashMap` 实例的迭代序不保证一致
//! （std 默认随机哈希种子）。虽然报告不比较 order id，但同一帧两品种同时调仓
//! 会让**成交顺序**（`fills_detail` 的 Vec 顺序）漂移，`fills` 类别就会报差异。
//! 因此多品种用例都设计成每个网格点最多一个品种改变目标。
//!
//! 文件末尾另有一组**反向对照**（`negative_control_reports_mismatch`）：故意让
//! 两条路径的配置不一致，确认 oracle 会如实报差异、不是恒绿的摆设。

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use axon_backtest::{Bracket, MarginMode, MarginSpec};
use axon_core::Timestamp;
use axon_core::market::bar::Bar;
use axon_core::types::{Instrument, Price, Quantity, SwapSettle};
use axon_kernel::event::BarClosedEvent;
use axon_kernel::{
    BacktestSink, HostCtx, InstrumentMode, Kernel, KernelConfig, OrderIntent, StrategyHost,
};
use axon_llm::decision::Action;
use axon_strategy::trait_::{BarContext, Strategy, StrategyDecision, StrategyError};
use axon_worker::BacktestRunner;
use axon_worker::shadow::{ShadowInput, ShadowReport, run_shadow};

const START: i64 = 1_700_000_000_000_000_000;
const ONE_MIN: i64 = 60_000_000_000;

// ─────────────────────────────────────────────────────────────────────
// 两条路径共用的策略判据
// ─────────────────────────────────────────────────────────────────────

/// 绝对目标数量的判据：收盘 > 开盘 → +0.5；收盘 < 开盘 → -0.5；相等 → Hold
///
/// A 路策略与 B 路 Host 必须调用同一个函数，否则比较的是两份不同策略。
fn target_quantity(bar: &Bar) -> Option<f64> {
    let open = bar.open.as_f64();
    let close = bar.close.as_f64();
    if close > open {
        Some(0.5)
    } else if close < open {
        Some(-0.5)
    } else {
        None
    }
}

/// 多策略场景 ①：只要涨跌就给 ±0.5（每根非平盘 bar 都调仓）
fn wide_target(bar: &Bar) -> Option<f64> {
    let delta = bar.close.as_f64() - bar.open.as_f64();
    if delta > 0.0 {
        Some(0.5)
    } else if delta < 0.0 {
        Some(-0.5)
    } else {
        None
    }
}

/// 多策略场景 ②：只有大幅涨跌（≥2）才给 ±0.25，其余 Hold
///
/// 与 `wide_target` 同时注册在同一 symbol 上：后注册者胜出，验证两条路径的
/// 覆盖顺序一致。
fn strong_target(bar: &Bar) -> Option<f64> {
    let delta = bar.close.as_f64() - bar.open.as_f64();
    if delta >= 2.0 {
        Some(0.25)
    } else if delta <= -2.0 {
        Some(-0.25)
    } else {
        None
    }
}

/// 强平场景：首根上涨 bar 一次性开 50 单位，之后平盘 bar 全部 Hold
fn fifty_on_up(bar: &Bar) -> Option<f64> {
    if bar.close.as_f64() > bar.open.as_f64() {
        Some(50.0)
    } else {
        None
    }
}

/// 部分成交场景：恒定目标 2000 单位（远超单 bar 种子深度 500）
fn always_2000(_bar: &Bar) -> Option<f64> {
    Some(2_000.0)
}

/// 跨零场景：±2 单位单次反手（一次调仓同时平多 + 开空）
fn cross_zero_target(bar: &Bar) -> Option<f64> {
    let delta = bar.close.as_f64() - bar.open.as_f64();
    if delta > 0.0 {
        Some(2.0)
    } else if delta < 0.0 {
        Some(-2.0)
    } else {
        None
    }
}

type Selector = fn(&Bar) -> Option<f64>;

/// 一条策略的装配描述（两条路径共用同一份判据）
struct StrategySpec {
    name: String,
    symbol: String,
    select: Selector,
}

/// A 路：直接给出绝对目标数量的 `Strategy`
struct AbsoluteStrategy {
    name: String,
    symbol: String,
    select: Selector,
}

#[async_trait]
impl Strategy for AbsoluteStrategy {
    fn name(&self) -> &str {
        &self.name
    }

    fn symbol(&self) -> &str {
        &self.symbol
    }

    async fn decide(&self, ctx: &BarContext<'_>) -> Result<StrategyDecision, StrategyError> {
        match (self.select)(ctx.bar) {
            Some(quantity) => {
                // A 路语义：`target_position` 是绝对数量，Action 只决定方向
                let action = if quantity > 0.0 {
                    Action::Buy
                } else {
                    Action::Sell
                };
                Ok(StrategyDecision::from_rule(action).with_target_position(quantity))
            }
            None => Ok(StrategyDecision::from_rule(Action::Hold)),
        }
    }
}

/// B 路：把同一判据给出的绝对数量换算成权益占比的 `StrategyHost`
struct RatioHost {
    instrument: Instrument,
    select: Selector,
}

impl StrategyHost for RatioHost {
    fn name(&self) -> &str {
        "ratio_host"
    }

    fn on_event(&mut self, ev: &BarClosedEvent<'_>, ctx: &HostCtx) -> Vec<OrderIntent> {
        // 内核按帧广播所有品种的收市事件，每个 Host 只处理自己的品种
        if ev.instrument != &self.instrument {
            return Vec::new();
        }
        let Some(quantity) = (self.select)(ev.bar) else {
            // 空 vec = Hold（维持上次生效目标），不是平仓
            return Vec::new();
        };
        let price = ctx.last_price.get(&self.instrument).copied().unwrap_or(0.0);
        if price <= 0.0 || ctx.equity <= 0.0 {
            return Vec::new();
        }
        vec![OrderIntent::SetTarget {
            instrument: self.instrument.clone(),
            // `BacktestSink` 的换算基准是 `core.equity()`，决策前内核刚把
            // `ctx.equity` 回填成同一个值，因此这里逆运算能无损回推数量。
            // 不要用 initial_cash：权益漂移后 parity 立刻失败。
            target_ratio: quantity * price / ctx.equity,
        }]
    }
}

// ─────────────────────────────────────────────────────────────────────
// 用例装配
// ─────────────────────────────────────────────────────────────────────

/// 一次 shadow 用例的运行参数（除 bars / strategies 外的全部外部输入）
struct Case {
    initial_cash: f64,
    force_liquidate: bool,
    funding: BTreeMap<String, BTreeMap<i64, (f64, f64)>>,
    margin_specs: HashMap<String, MarginSpec>,
    /// `Some((settle, contract_size))` = 永续模式，`None` = 现货
    futures: Option<(SwapSettle, f64)>,
    capital_budget: Option<f64>,
}

impl Default for Case {
    fn default() -> Self {
        Self {
            initial_cash: 100_000.0,
            force_liquidate: false,
            funding: BTreeMap::new(),
            margin_specs: HashMap::new(),
            futures: None,
            capital_budget: None,
        }
    }
}

/// 每个 symbol 一条默认策略（`target_quantity` 判据）
fn one_per_symbol(bars: &BTreeMap<String, Vec<Option<Bar>>>) -> Vec<StrategySpec> {
    bars.keys()
        .map(|symbol| StrategySpec {
            name: format!("abs_{symbol}"),
            symbol: symbol.clone(),
            select: target_quantity,
        })
        .collect()
}

/// 同一份输入装配两条路径、跑 shadow、并把报告落盘到 target 目录
fn run_case(
    scenario: &str,
    bars: &BTreeMap<String, Vec<Option<Bar>>>,
    specs: &[StrategySpec],
    cfg: &Case,
) -> ShadowReport {
    // ── A 路：BacktestRunner（语义真源）──
    let mut runner = BacktestRunner::simple(cfg.initial_cash)
        .with_force_liquidate(cfg.force_liquidate)
        .with_margin(cfg.margin_specs.clone());
    if let Some((settle, contract_size)) = cfg.futures {
        runner = runner.with_futures(settle, contract_size);
    }
    for spec in specs {
        runner.add_strategy(Arc::new(AbsoluteStrategy {
            name: spec.name.clone(),
            symbol: spec.symbol.clone(),
            select: spec.select,
        }));
    }

    // ── B 路：Kernel + RatioHost ──
    let mode = match cfg.futures {
        Some((settle, contract_size)) => InstrumentMode::Swap {
            settle,
            contract_size,
        },
        None => InstrumentMode::Spot,
    };
    let kernel_config = KernelConfig {
        initial_cash: cfg.initial_cash,
        force_liquidate: cfg.force_liquidate,
        margin_specs: cfg.margin_specs.clone(),
        instrument_mode: mode,
        funding_by_symbol: cfg.funding.clone(),
        capital_budget: cfg.capital_budget,
        ..KernelConfig::default()
    };
    let mut kernel = Kernel::new(kernel_config, Box::new(BacktestSink::new()));
    for spec in specs {
        kernel.add_host(Box::new(RatioHost {
            instrument: mode.instrument(&spec.symbol),
            select: spec.select,
        }));
    }

    let input = ShadowInput {
        bars_by_symbol: bars.clone(),
        // 本文件的判据只看 bar 形状，state 一律为 Null（与缺省口径一致）
        states_by_symbol: BTreeMap::new(),
        funding_by_symbol: cfg.funding.clone(),
        capital_budget: cfg.capital_budget,
    };

    let report = run_shadow(scenario, &mut runner, &mut kernel, &input);

    // 报告归档：CARGO_TARGET_TMPDIR 由 cargo 为集成测试注入，产物留在 target 下
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("shadow-reports");
    let path = report.write_json(&dir).expect("shadow 报告应能落盘");
    assert!(path.exists(), "报告文件必须存在: {}", path.display());

    report
}

/// 断言差异为 0，并打印各类别计数便于定位
fn assert_matched(report: &ShadowReport) {
    assert!(
        report.matched,
        "{} 差异非零\n旧: {}\n新: {}\n{:#?}",
        report.scenario, report.old_summary, report.new_summary, report.diffs
    );
    for diff in &report.diffs {
        assert_eq!(
            diff.count, 0,
            "{} 的 {} 类别差异 {} 条: {:?}",
            report.scenario, diff.category, diff.count, diff.samples
        );
    }
}

/// 从摘要里取单个指标（摘要格式见 `axon_worker::shadow::ShadowReport`）
fn metric(summary: &str, key: &str) -> f64 {
    let prefix = format!("{key}=");
    summary
        .split_whitespace()
        .find_map(|token| token.strip_prefix(prefix.as_str()))
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("摘要里没有 {key}: {summary}"))
}

// ─────────────────────────────────────────────────────────────────────
// 数据构造
// ─────────────────────────────────────────────────────────────────────

fn bar_at(ts: i64, open: f64, close: f64, volume: f64) -> Bar {
    Bar {
        timestamp: Timestamp::from_nanos(ts),
        open: Price::from_f64(open),
        high: Price::from_f64(open.max(close) + 0.4),
        low: Price::from_f64(open.min(close) - 0.4),
        close: Price::from_f64(close),
        volume: Quantity::from_f64(volume),
    }
}

/// 有涨有跌的之字形序列：`(open 偏移, close 偏移)` 四态循环 + 缓慢漂移，
/// 覆盖 Buy / Sell / Hold（相等态）三种决策。
fn zigzag_bars(n: usize, start_price: f64) -> Vec<Option<Bar>> {
    const PATTERN: [(f64, f64); 4] = [(0.0, 1.0), (1.0, 0.0), (0.0, 0.0), (0.0, -1.0)];
    (0..n)
        .map(|i| {
            let (open_delta, close_delta) = PATTERN[i % PATTERN.len()];
            let base = start_price + i as f64 * 0.25;
            Some(bar_at(
                START + i as i64 * ONE_MIN,
                base + open_delta,
                base + close_delta,
                1_000.0,
            ))
        })
        .collect()
}

/// 每根 bar 都反手的序列：`open→close` 偏移在 +1 / -1 之间交替
fn alternating_bars(n: usize, start_price: f64) -> Vec<Option<Bar>> {
    (0..n)
        .map(|i| {
            let base = start_price;
            let (open_delta, close_delta) = if i % 2 == 0 { (0.0, 1.0) } else { (1.0, 0.0) };
            Some(bar_at(
                START + i as i64 * ONE_MIN,
                base + open_delta,
                base + close_delta,
                1_000.0,
            ))
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────
// 场景 1：单品种
// ─────────────────────────────────────────────────────────────────────

#[test]
fn case1_single_symbol_30_bars() {
    let bars = BTreeMap::from([("BTC/USDT".to_string(), zigzag_bars(30, 100.0))]);
    let report = run_case(
        "case1_single_symbol",
        &bars,
        &one_per_symbol(&bars),
        &Case::default(),
    );

    assert_matched(&report);
    // 前置条件：用例必须真的成交、反手，否则零差异没有力度
    assert!(
        metric(&report.old_summary, "fills") > 0.0,
        "case1 应产生成交: {}",
        report.old_summary
    );
    assert!(
        metric(&report.old_summary, "trades") >= 4.0,
        "case1 应产生多次反手/平仓的 round-trip: {}",
        report.old_summary
    );
    assert!(
        metric(&report.old_summary, "fees") > 0.0,
        "有成交就必然有手续费"
    );
}

// ─────────────────────────────────────────────────────────────────────
// 场景 2：多品种 + 缺口（每帧最多一个品种调仓）
// ─────────────────────────────────────────────────────────────────────

#[test]
fn case2_multi_symbol_with_gaps() {
    const N: usize = 12;

    let btc: Vec<Option<Bar>> = (0..N)
        .map(|i| {
            let ts = START + i as i64 * ONE_MIN;
            let base = 100.0 + i as f64 * 0.1;
            if i % 2 == 0 {
                // 偶数点：有方向 → 调仓
                let up = (i / 2) % 2 == 0;
                Some(if up {
                    bar_at(ts, base, base + 1.0, 1_000.0)
                } else {
                    bar_at(ts, base, base - 1.0, 1_000.0)
                })
            } else {
                // 奇数点：开收相等 → Hold（把本帧的调仓机会让给 ETH）
                Some(bar_at(ts, base, base, 1_000.0))
            }
        })
        .collect();

    let eth: Vec<Option<Bar>> = (0..N)
        .map(|i| {
            if i % 2 == 0 {
                return None; // 缺口：价格由内核前向填充
            }
            let ts = START + i as i64 * ONE_MIN;
            let base = 20.0 + i as f64 * 0.02;
            let up = ((i - 1) / 2) % 2 == 0;
            Some(if up {
                bar_at(ts, base, base + 0.6, 1_000.0)
            } else {
                bar_at(ts, base, base - 0.6, 1_000.0)
            })
        })
        .collect();

    let bars = BTreeMap::from([("BTC/USDT".to_string(), btc), ("ETH/USDT".to_string(), eth)]);
    let report = run_case(
        "case2_multi_symbol",
        &bars,
        &one_per_symbol(&bars),
        &Case::default(),
    );

    assert_matched(&report);
    // 两品种各 6 个方向点 → 至少 6 个 rebalance 单
    assert!(
        metric(&report.old_summary, "fills") >= 5.0,
        "两品种都应成交: {}",
        report.old_summary
    );
}

// ─────────────────────────────────────────────────────────────────────
// 场景 3：资金费结算（永续）
// ─────────────────────────────────────────────────────────────────────

#[test]
fn case3_funding_settlements() {
    let bars = BTreeMap::from([("BTC/USDT".to_string(), zigzag_bars(10, 100.0))]);

    let mut schedule: BTreeMap<i64, (f64, f64)> = BTreeMap::new();
    // 第 3 帧：mark = 0 → 两侧都必须用该品种最近 close 兜底
    schedule.insert(START + 3 * ONE_MIN, (0.0001, 0.0));
    // 第 7 帧：显式 mark，rate 为负（付款方向反向）
    schedule.insert(START + 7 * ONE_MIN, (-0.0002, 101.0));

    let cfg = Case {
        funding: BTreeMap::from([("BTC/USDT".to_string(), schedule)]),
        futures: Some((SwapSettle::UsdMargin, 1.0)),
        ..Case::default()
    };
    let report = run_case("case3_funding", &bars, &one_per_symbol(&bars), &cfg);

    assert_matched(&report);
    assert!(
        metric(&report.old_summary, "funding").abs() > 0.0,
        "非零 rate 必须产生资金费 PnL: {}",
        report.old_summary
    );
    assert_eq!(
        metric(&report.old_summary, "liq"),
        0.0,
        "资金费场景不应触发强平"
    );
}

// ─────────────────────────────────────────────────────────────────────
// 场景 4：强平（保证金 + 暴跌穿仓 + 强平后 Hold 不复活）
// ─────────────────────────────────────────────────────────────────────

#[test]
fn case4_liquidation() {
    // 首根 99→100 开 50 单位；随后暴跌到 1，3 根平盘 bar 全部 Hold
    let closes = [(99.0, 100.0), (1.0, 1.0), (1.0, 1.0), (1.0, 1.0)];
    let bars: Vec<Option<Bar>> = closes
        .iter()
        .enumerate()
        .map(|(i, (open, close))| Some(bar_at(START + i as i64 * ONE_MIN, *open, *close, 1_000.0)))
        .collect();
    let bars = BTreeMap::from([("LTC/USDT".to_string(), bars)]);

    let spec = MarginSpec {
        leverage: 20.0,
        mode: MarginMode::Cross,
        brackets: vec![Bracket {
            notional_floor: 0.0,
            notional_cap: None,
            mmr: 0.004,
            cum: 0.0,
            liquidation_fee_rate: 0.001,
            max_leverage: 125.0,
        }],
        bracket_source: "default".into(),
    };
    let cfg = Case {
        initial_cash: 1_000.0,
        margin_specs: HashMap::from([("LTC/USDT".to_string(), spec)]),
        futures: Some((SwapSettle::UsdMargin, 1.0)),
        ..Case::default()
    };
    let specs = vec![StrategySpec {
        name: "open_then_hold".to_string(),
        symbol: "LTC/USDT".to_string(),
        select: fifty_on_up,
    }];
    let report = run_case("case4_liquidation", &bars, &specs, &cfg);

    assert_matched(&report);
    assert!(
        metric(&report.old_summary, "liq") > 0.0,
        "暴跌必须产生强平事件: {}",
        report.old_summary
    );
    assert!(
        metric(&report.old_summary, "fills") >= 2.0,
        "应有开仓 + 强平成交: {}",
        report.old_summary
    );
}

// ─────────────────────────────────────────────────────────────────────
// 场景 5：多策略（同品种两条策略，后者覆盖前者）
// ─────────────────────────────────────────────────────────────────────

#[test]
fn case5_multi_strategy() {
    // 幅度 [+2, +0.5, -2, -0.5] 循环：`strong_target` 只在大幅 bar 上覆盖
    // `wide_target` 的目标，其余 bar 由 `wide_target` 决定
    const PATTERN: [(f64, f64); 4] = [(0.0, 2.0), (0.0, 0.5), (2.0, 0.0), (0.5, 0.0)];
    let bars: Vec<Option<Bar>> = (0..24)
        .map(|i| {
            let (open_delta, close_delta) = PATTERN[i % PATTERN.len()];
            let base = 100.0 + i as f64 * 0.1;
            Some(bar_at(
                START + i as i64 * ONE_MIN,
                base + open_delta,
                base + close_delta,
                1_000.0,
            ))
        })
        .collect();
    let bars = BTreeMap::from([("BTC/USDT".to_string(), bars)]);

    let specs = vec![
        StrategySpec {
            name: "wide".to_string(),
            symbol: "BTC/USDT".to_string(),
            select: wide_target,
        },
        StrategySpec {
            name: "strong".to_string(),
            symbol: "BTC/USDT".to_string(),
            select: strong_target,
        },
    ];
    let report = run_case("case5_multi_strategy", &bars, &specs, &Case::default());

    assert_matched(&report);
    assert!(
        metric(&report.old_summary, "fills") > 0.0,
        "两条策略至少要真的调仓: {}",
        report.old_summary
    );
}

// ─────────────────────────────────────────────────────────────────────
// 场景 6：部分成交（目标远超 L1 种子深度）
// ─────────────────────────────────────────────────────────────────────

#[test]
fn case6_partial_fill() {
    // 恒定价格 100：目标 2000 单位，单 bar 种子深度仅 5 层 × 100 = 500 单位，
    // 因此必须跨多根 bar 部分成交，且每 bar 的剩余差额都会被重新下市价单。
    let bars: Vec<Option<Bar>> = (0..12)
        .map(|i| Some(bar_at(START + i as i64 * ONE_MIN, 100.0, 100.0, 1_000.0)))
        .collect();
    let bars = BTreeMap::from([("BTC/USDT".to_string(), bars)]);

    let specs = vec![StrategySpec {
        name: "always_2000".to_string(),
        symbol: "BTC/USDT".to_string(),
        select: always_2000,
    }];
    let cfg = Case {
        initial_cash: 1_000_000.0,
        ..Case::default()
    };
    let report = run_case("case6_partial_fill", &bars, &specs, &cfg);

    assert_matched(&report);
    // 500 单位/bar → 至少 2 根 bar 才能完成，fills 必然 > 一笔
    assert!(
        metric(&report.old_summary, "fills") >= 5.0,
        "部分成交应产生多笔 fill: {}",
        report.old_summary
    );
}

// ─────────────────────────────────────────────────────────────────────
// 场景 7：冲击模型（不适用）
// ─────────────────────────────────────────────────────────────────────

/// 冲击模型场景**不适用**，原因见文件头表格。
///
/// 旧 `BacktestRunner::run()` 在构造 `BacktestEngineConfig` 时硬编码
/// `impact_model: None`；新内核 `KernelCore::new` 同样硬编码 `None`。两条路径
/// 都没有冲击模型注入入口，构造不出「有冲击 vs 无冲击」的可比场景 —— 强行加
/// 一个 `None` 对 `None` 的用例只会得到零差异的假绿灯，不能证明任何东西。
///
/// 升级路径：给 `BacktestEngineConfig` 暴露 `impact_model` 注入点（旧路径）与
/// `KernelConfig::impact_model`（新内核），两者都接 `ImpactedMatchingEngine`
/// 后再启用本用例。
#[test]
#[ignore = "两条路径都硬编码 impact_model=None，没有冲击模型注入入口，无法构造可比场景"]
fn case7_impact_model_not_applicable() {
    // 刻意留空：ignored 用例只作为缺口记录，不产生假绿灯。
}

// ─────────────────────────────────────────────────────────────────────
// 场景 8：跨零仓位（+2 → -2 单次反手）
// ─────────────────────────────────────────────────────────────────────

#[test]
fn case8_cross_zero() {
    let bars = BTreeMap::from([("BTC/USDT".to_string(), alternating_bars(20, 100.0))]);
    let specs = vec![StrategySpec {
        name: "cross_zero".to_string(),
        symbol: "BTC/USDT".to_string(),
        select: cross_zero_target,
    }];
    let report = run_case("case8_cross_zero", &bars, &specs, &Case::default());

    assert_matched(&report);
    assert!(
        metric(&report.old_summary, "fills") > 0.0,
        "跨零调仓必须真的成交: {}",
        report.old_summary
    );
}

// ─────────────────────────────────────────────────────────────────────
// 场景 9：空 bar（前导缺口 + 零成交量 bar）
// ─────────────────────────────────────────────────────────────────────

#[test]
fn case9_empty_bar_gap() {
    const N: usize = 12;
    const ETH_LISTED_AT: usize = 4;

    // BTC：偶数点有方向，奇数点平盘（把本帧调仓机会让给 ETH）
    let btc: Vec<Option<Bar>> = (0..N)
        .map(|i| {
            let ts = START + i as i64 * ONE_MIN;
            let base = 100.0 + i as f64 * 0.1;
            // 第 6 帧给零成交量 bar：成交量不参与撮合/决策，但必须被当成真实 bar
            let volume = if i == 6 { 0.0 } else { 1_000.0 };
            if i % 2 == 0 {
                let up = (i / 2) % 2 == 0;
                Some(if up {
                    bar_at(ts, base, base + 1.0, volume)
                } else {
                    bar_at(ts, base, base - 1.0, volume)
                })
            } else {
                Some(bar_at(ts, base, base, volume))
            }
        })
        .collect();

    // ETH：前 4 帧未上市（None → 价格前向填充不可用，legs 里不出现），
    // 上市后在奇数点有方向，偶数点平盘
    let eth: Vec<Option<Bar>> = (0..N)
        .map(|i| {
            if i < ETH_LISTED_AT {
                return None;
            }
            let ts = START + i as i64 * ONE_MIN;
            let base = 20.0 + i as f64 * 0.02;
            if i % 2 == 1 {
                let up = ((i - ETH_LISTED_AT) / 2).is_multiple_of(2);
                Some(if up {
                    bar_at(ts, base, base + 0.6, 1_000.0)
                } else {
                    bar_at(ts, base, base - 0.6, 1_000.0)
                })
            } else {
                Some(bar_at(ts, base, base, 1_000.0))
            }
        })
        .collect();

    let bars = BTreeMap::from([("BTC/USDT".to_string(), btc), ("ETH/USDT".to_string(), eth)]);
    let report = run_case(
        "case9_empty_bar_gap",
        &bars,
        &one_per_symbol(&bars),
        &Case::default(),
    );

    assert_matched(&report);
    assert!(
        metric(&report.old_summary, "fills") > 0.0,
        "上市后的 ETH 与 BTC 都必须成交: {}",
        report.old_summary
    );
}

// ─────────────────────────────────────────────────────────────────────
// 场景 10：长周期（500 根 bar）
// ─────────────────────────────────────────────────────────────────────

#[test]
fn case10_long_period() {
    const N: usize = 500;
    let bars = BTreeMap::from([("BTC/USDT".to_string(), zigzag_bars(N, 100.0))]);
    let report = run_case(
        "case10_long_period",
        &bars,
        &one_per_symbol(&bars),
        &Case::default(),
    );

    assert_matched(&report);
    assert!(
        metric(&report.old_summary, "fills") > 100.0,
        "500 根 bar 应有大量成交: {}",
        report.old_summary
    );
    assert!(
        metric(&report.old_summary, "rebalances") > 100.0,
        "长周期应产生大量 rebalance: {}",
        report.old_summary
    );
}

// ─────────────────────────────────────────────────────────────────────
// 反向对照：故意让两条路径的配置不一致
// ─────────────────────────────────────────────────────────────────────

/// 只给新内核启用资金预算（旧路径不缩放目标），差异必须被如实计入报告
///
/// 这组不是验收场景，而是 oracle 的**自检**：如果 `run_shadow` 的报告恒绿
/// （比如比较逻辑写空、类别没接上），本用例会失败。
#[test]
fn negative_control_reports_mismatch() {
    let bars = BTreeMap::from([("BTC/USDT".to_string(), zigzag_bars(6, 100.0))]);
    let specs = one_per_symbol(&bars);

    let mut runner = BacktestRunner::simple(100_000.0);
    for spec in &specs {
        runner.add_strategy(Arc::new(AbsoluteStrategy {
            name: spec.name.clone(),
            symbol: spec.symbol.clone(),
            select: spec.select,
        }));
    }

    // 新内核：budget = 1.0 → 0.5 单位（名义 50）被压到名义 1.0；
    // 旧路径：`ShadowInput.capital_budget = None` → 不缩放。两边必然不同。
    let mut kernel = Kernel::new(
        KernelConfig {
            initial_cash: 100_000.0,
            capital_budget: Some(1.0),
            ..KernelConfig::default()
        },
        Box::new(BacktestSink::new()),
    );
    for spec in &specs {
        kernel.add_host(Box::new(RatioHost {
            instrument: InstrumentMode::Spot.instrument(&spec.symbol),
            select: spec.select,
        }));
    }

    let input = ShadowInput {
        bars_by_symbol: bars,
        ..ShadowInput::default()
    };
    let report = run_shadow("negative_control", &mut runner, &mut kernel, &input);

    assert!(
        !report.matched,
        "配置不一致必须判为不匹配: {}",
        report.old_summary
    );
    assert!(
        report.diffs.iter().any(|diff| diff.count > 0),
        "至少一个类别必须计数非零: {:#?}",
        report.diffs
    );
}
