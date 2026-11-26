//! Canonical golden 指纹负载（0.26.0 C3）
//!
//! 设计依据：`docs/superpowers/specs/2026-11-25-axon-quant-0.26.0-accuracy-hardening-design.md`
//! §3.3（C3/C7），实施计划同名 plan 的 C3 节。
//!
//! 目的：给撮合 / 聚合语义变更建立**数值指纹护栏**——五个 canonical 负载
//! 各自跑一遍统一内核（`Kernel` + `BacktestSink`），把 `RunResult` 序列化结果与
//! `crates/axon-integration-tests/tests/fixtures/canonical/<workload>.json` 逐字段
//! 比对；数值走 1e-9 相对容差，其余字段全等。
//!
//! # 单品种四负载与多品种负载的时序
//!
//! C3 的四个单品种负载天然确定性（不依赖多品种同帧 `HashMap` 迭代序），因此
//! 指纹建在 0.26.0 的行为修复（C4 sort-on-collect）**之前**，修复后被同一批
//! 指纹接住。C7 追加的 `MultiSymbolRebalance` 则相反：它**必须**建在 C4 之后，
//! 用来把"同帧多品种决策 / 成交按品种标签字典序执行"这一新语义锁存为基线。
//!
//! # 负载构造
//!
//! 蓝本：`crates/axon-kernel/src/kernel_loop.rs` 的 `frames_3bars` / `TargetHost`
//! / `PairTargetHost` 测试模板 + `lifecycle.rs` 九步编排契约。五个 Host 在本文件
//! 内自写，其中 EMA 交叉在 Host 的 `on_event(BarClosedEvent)` 里自算 EMA——刻意
//! 不依赖 `axon-strategy`，避免为测试引入跨 crate 依赖。
//!
//! # 合成数据
//!
//! 单品种：12 根 1m bar（`CLOSES` / `VOLUMES` 手写常量 + 固定公式派生 OHLC）。
//! 多品种：`MULTI_A_*` / `MULTI_B_*` 各 10 根 bar，同帧双品种
//! `SetTarget` → `engine.rs::rebalance_to_target` 的多 legs 收集路径。
//! `initial_cash = 10_000.0`，种子流动性 `half_spread = 0.5 / depth = 3 /
//! size_per_level = 10`。**不使用 `rand`**，没有任何随机源。
//!
//! # 指纹更新流程
//!
//! 有意变更语义后：
//!
//! ```bash
//! AXON_CANONICAL_UPDATE=all cargo test -p axon-integration-tests canonical
//! ```
//!
//! 然后**必须**把指纹文件的 `reasoning` 改成"为何更新"（改了什么语义 / 对应
//! issue），否则按 `docs/zh/about/contributing.md`「回归与修复约定」视为违规。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use axon_core::market::Side;
use axon_core::types::{Instrument, Price, Quantity, SpotInstrument, Symbol};
use axon_core::{Bar, Timestamp};
use axon_kernel::core::{KernelConfig, SeedLiquidity};
use axon_kernel::event::BarClosedEvent;
use axon_kernel::host::{HostCtx, OrderIntent, OrderSpec, StrategyHost};
use axon_kernel::kernel_loop::Kernel;
use axon_kernel::pump::GridFrame;
use axon_kernel::sink::BacktestSink;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// 首选基线 `reasoning`（首版指纹固定值）
pub const BASELINE_REASONING: &str =
    "0.26.0 initial baseline: single-symbol canonical workloads (spec 2026-11-25 C3)";

/// 多品种负载的基线 `reasoning`（C7 锁存 C4 的字典序执行语义）
pub const MULTI_SYMBOL_REASONING: &str = "lock deterministic lexicographic ordering introduced in C4 (same-frame multi-instrument decisions execute in instrument-label lexicographic order)";

/// 指纹更新开关的环境变量名
///
/// 值 = 负载名（`replay_only` 等）或 `all`：命中时就地重写对应指纹文件。
pub const UPDATE_ENV: &str = "AXON_CANONICAL_UPDATE";

/// 数值比对相对容差（镜像 `kernel_loop.rs` 的 `assert_same_run` 口径）
const REL_TOL: f64 = 1e-9;

/// canonical 品种（交易所写法；`Instrument` 内部归一化为 `BTC/USDT`）
const SYMBOL: &str = "BTCUSDT";
/// 固定初始资金
const INITIAL_CASH: f64 = 10_000.0;
/// 固定手续费率
const FEE_RATE: f64 = 0.001;
/// 一根 bar 的纳秒数（`1m`）
const ONE_MIN: i64 = 60_000_000_000;
/// 首根 bar 起始时间（固定常量，不取系统时钟）
const START_NS: i64 = 1_700_000_000_000_000_000;
/// 种子流动性：半价差（绝对价格单位）
const SEED_HALF_SPREAD: f64 = 0.5;
/// 种子流动性：每侧层数
const SEED_DEPTH_LEVELS: usize = 3;
/// 种子流动性：每层数量
const SEED_SIZE_PER_LEVEL: f64 = 10.0;

/// 手写收盘价序列（确定性；形态含趋势 + 回调，覆盖多空切换）
const CLOSES: [f64; 12] = [
    100.0, 101.5, 103.0, 101.0, 99.5, 98.0, 99.0, 101.0, 104.0, 106.0, 103.5, 105.0,
];
/// 手写成交量序列（确定性；只影响账务无关字段，保留审计价值）
const VOLUMES: [f64; 12] = [
    1_200.0, 1_350.0, 1_500.0, 900.0, 1_100.0, 1_600.0, 1_250.0, 1_400.0, 1_800.0, 2_100.0,
    1_550.0, 1_300.0,
];

/// 固定 bar 计划：市价单 `(bar 序号, 方向, 数量)`
///
/// 含一次完全平仓（bar 5 卖掉 bar 2 建的多头 4 手），保证指纹覆盖
/// `trades` / `win_rate` 等只在 round-trip 时才有值的字段。
const MARKET_SCHEDULE: [(u64, Side, f64); 4] = [
    (2, Side::Buy, 4.0),
    (5, Side::Sell, 4.0),
    (8, Side::Buy, 5.0),
    (11, Side::Sell, 2.0),
];

/// 固定 bar 计划：限价单 `(bar 序号, 方向, 数量, 相对 close 的价差)`
///
/// 种子卖盘在 `close + 0.5 * level`，买盘在 `close - 0.5 * level`，每层 10 手：
/// - `+0.75` 只够吃穿卖一（成交 10，余量挂单在下一 bar 开书时被清掉）；
/// - `-0.75` 只够吃穿买一（同上）；
/// - `+1.6` 吃穿卖一 + 卖二的一部分。
const LIMIT_SCHEDULE: [(u64, Side, f64, f64); 3] = [
    (2, Side::Buy, 15.0, 0.75),
    (6, Side::Sell, 12.0, -0.75),
    (9, Side::Buy, 8.0, 1.6),
];

/// EMA 快线周期
const EMA_FAST: usize = 3;
/// EMA 慢线周期
const EMA_SLOW: usize = 7;
/// 快线在上时的目标权益占比（否则空仓 0.0）
const EMA_LONG_RATIO: f64 = 0.5;

/// 多品种 canonical 的一号品种（交易所写法）
const MULTI_A_SYMBOL: &str = "BTCUSDT";
/// 多品种 canonical 的二号品种（交易所写法）
const MULTI_B_SYMBOL: &str = "ETHUSDT";
/// 多品种一号品种收盘价序列（确定性；上涨 → 回调 → 反弹）
const MULTI_A_CLOSES: [f64; 10] = [
    100.0, 102.0, 101.0, 104.0, 99.0, 97.0, 100.0, 101.0, 103.0, 105.0,
];
/// 多品种二号品种收盘价序列（确定性；形态与一号品种错位）
const MULTI_B_CLOSES: [f64; 10] = [50.0, 49.5, 51.0, 52.5, 51.0, 48.0, 49.0, 50.5, 49.5, 52.0];
/// 多品种一号品种成交量序列
const MULTI_A_VOLUMES: [f64; 10] = [
    1_200.0, 1_300.0, 1_100.0, 900.0, 1_400.0, 1_500.0, 1_200.0, 1_000.0, 1_600.0, 1_300.0,
];
/// 多品种二号品种成交量序列
const MULTI_B_VOLUMES: [f64; 10] = [
    8_000.0, 7_500.0, 9_000.0, 7_000.0, 8_500.0, 9_500.0, 8_000.0, 7_000.0, 9_000.0, 10_000.0,
];

/// 多品种目标权益占比计划（逐 bar；两个品种每帧同时出 `SetTarget`）
///
/// 形态刻意让两品种**永远同帧有决策**，且含有多次"同帧双向调仓"
/// （如 bar 2：一号品种减仓 + 二号品种加仓）与"同帧两个品种都新开仓"
/// （bar 0 / bar 8）——这正是 `engine.rs::rebalance_to_target` 的多 legs
/// 收集路径，也是 C4 字典序语义的着力点。
///
/// 占比都压得足够小（< 0.25），保证单笔市价数量不超过种子盘口
/// （`depth_levels × size_per_level = 30`），避免部分成交掩盖执行序。
const MULTI_A_RATIOS: [f64; 10] = [0.20, 0.20, 0.10, 0.10, 0.25, 0.25, 0.00, 0.00, 0.15, 0.15];
/// 多品种二号品种目标权益占比计划（字段说明见 [`MULTI_A_RATIOS`]）
const MULTI_B_RATIOS: [f64; 10] = [0.10, 0.10, 0.12, 0.12, 0.00, 0.00, 0.10, 0.10, 0.12, 0.12];

/// canonical 负载集（C3 四个单品种 + C7 一个多品种）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CanonicalWorkload {
    /// 纯行情回放：Host 永不下单（把撮合 / 账务空跑的基线锁死）
    ReplayOnly,
    /// 固定 bar 提交市价单
    ScheduledMarketOrders,
    /// 挂限价单（限价吃穿种子盘口，余量挂单被下一 bar 清书丢弃）
    PassiveLimitOrders,
    /// EMA 快慢线交叉 → `SetTarget` 权益占比切换
    EmaCross,
    /// 双品种（BTC/USDT + ETH/USDT）同帧 `SetTarget` rebalance
    ///
    /// 走 `engine.rs::rebalance_to_target` 的多 legs 收集路径；指纹锁存 C4 之后
    /// 的"品种标签字典序执行"语义。
    MultiSymbolRebalance,
}

impl CanonicalWorkload {
    /// 全部负载（指纹矩阵的遍历顺序，也是输出顺序）
    pub const ALL: [CanonicalWorkload; 5] = [
        CanonicalWorkload::ReplayOnly,
        CanonicalWorkload::ScheduledMarketOrders,
        CanonicalWorkload::PassiveLimitOrders,
        CanonicalWorkload::EmaCross,
        CanonicalWorkload::MultiSymbolRebalance,
    ];

    /// 指纹文件 / 环境变量使用的稳定名字（snake_case）
    pub fn name(self) -> &'static str {
        match self {
            CanonicalWorkload::ReplayOnly => "replay_only",
            CanonicalWorkload::ScheduledMarketOrders => "scheduled_market_orders",
            CanonicalWorkload::PassiveLimitOrders => "passive_limit_orders",
            CanonicalWorkload::EmaCross => "ema_cross",
            CanonicalWorkload::MultiSymbolRebalance => "multi_symbol_rebalance",
        }
    }

    /// 首次生成指纹时的默认 `reasoning`（已有指纹就地重写时保留原文）
    pub fn default_reasoning(self) -> &'static str {
        match self {
            CanonicalWorkload::MultiSymbolRebalance => MULTI_SYMBOL_REASONING,
            _ => BASELINE_REASONING,
        }
    }

    /// 是否多品种负载（决定配置与帧表）
    pub fn is_multi_symbol(self) -> bool {
        matches!(self, CanonicalWorkload::MultiSymbolRebalance)
    }

    /// 按名字解析（供 `AXON_CANONICAL_UPDATE` 使用）
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| w.name() == name)
    }
}

/// 一次 canonical 运行的报告（指纹文件的运行期形态）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonicalReport {
    /// 负载名（`CanonicalWorkload::name`）
    pub workload: String,
    /// 配置指纹：负载名 + 内核配置 + bar 计划的 sha256 十六进制
    pub config_hash: String,
    /// `RunResult` 的 serde 序列化结果（指标 + 结构 + 原始曲线都在里面）
    pub run_metrics: Value,
    /// 逐点曲线（`bar_nav` / `bar_balance`），便于 diff 时定位到具体帧
    pub curves: Value,
}

/// 指纹文件内容
///
/// 落盘 schema：
/// `{"reasoning": ..., "workload": ..., "config_hash": ..., "metrics": ..., "curves": ...}`
///
/// `config_hash` 比 spec 底稿多一列：配置漂移（初始资金 / 费率 / 种子流动性 /
/// bar 表）必须能被指纹接住，否则指标相同也只是巧合。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonicalFingerprint {
    /// 为何更新该指纹（首版为 [`BASELINE_REASONING`]）
    pub reasoning: String,
    /// 负载名
    pub workload: String,
    /// 配置指纹
    pub config_hash: String,
    /// 运行指标（`CanonicalReport::run_metrics`）
    pub metrics: Value,
    /// 逐点曲线（`CanonicalReport::curves`）
    pub curves: Value,
}

impl CanonicalFingerprint {
    /// 由报告构造首版指纹（`reasoning` 由调用方给）
    pub fn from_report(report: &CanonicalReport, reasoning: &str) -> Self {
        Self {
            reasoning: reasoning.to_string(),
            workload: report.workload.clone(),
            config_hash: report.config_hash.clone(),
            metrics: report.run_metrics.clone(),
            curves: report.curves.clone(),
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// 合成数据与配置
// ════════════════════════════════════════════════════════════════════════════

/// canonical 品种：`BTC/USDT` 现货
pub fn canonical_instrument() -> Instrument {
    Instrument::Spot(SpotInstrument {
        base: Symbol::from("BTC"),
        quote: Symbol::from("USDT"),
    })
}

/// 第 `idx` 根 bar（OHLC 由 `CLOSES` 常量 + 固定公式派生，无随机源）
///
/// 公式：`open = 上根 close`（首根 = 自身 close）、`high = max + 0.5`、
/// `low = min - 0.5`、`close = CLOSES[idx]`。
pub fn canonical_bar(idx: usize) -> Bar {
    let close = CLOSES[idx];
    let open = if idx == 0 { close } else { CLOSES[idx - 1] };
    let high = open.max(close) + 0.5;
    let low = open.min(close) - 0.5;
    Bar {
        timestamp: Timestamp::from_nanos(START_NS + idx as i64 * ONE_MIN),
        open: Price::from_f64(open),
        high: Price::from_f64(high),
        low: Price::from_f64(low),
        close: Price::from_f64(close),
        volume: Quantity::from_f64(VOLUMES[idx]),
    }
}

/// 12 帧网格（单品种，每帧一根真实 bar，`states` 为空）
pub fn canonical_frames() -> Vec<GridFrame> {
    let instrument = canonical_instrument();
    (0..CLOSES.len())
        .map(|idx| GridFrame {
            ts: Timestamp::from_nanos(START_NS + idx as i64 * ONE_MIN),
            bars: vec![(instrument.clone(), canonical_bar(idx))],
            states: HashMap::new(),
        })
        .collect()
}

/// 多品种二号品种：`ETH/USDT` 现货
pub fn multi_symbol_instrument_b() -> Instrument {
    Instrument::Spot(SpotInstrument {
        base: Symbol::from("ETH"),
        quote: Symbol::from("USDT"),
    })
}

/// `close` 指定、OHLC 合法的 bar（high/low 环绕 close，open = close）
fn bar_with_close(ts: i64, close: f64, volume: f64) -> Bar {
    Bar {
        timestamp: Timestamp::from_nanos(ts),
        open: Price::from_f64(close),
        high: Price::from_f64(close * 1.01),
        low: Price::from_f64(close * 0.99),
        close: Price::from_f64(close),
        volume: Quantity::from_f64(volume),
    }
}

/// 多品种 10 帧网格：每帧两个品种各一根 bar（同帧双 legs）
pub fn multi_symbol_frames() -> Vec<GridFrame> {
    let a = canonical_instrument();
    let b = multi_symbol_instrument_b();
    (0..MULTI_A_CLOSES.len())
        .map(|idx| {
            let ts = START_NS + idx as i64 * ONE_MIN;
            GridFrame {
                ts: Timestamp::from_nanos(ts),
                bars: vec![
                    (
                        a.clone(),
                        bar_with_close(ts, MULTI_A_CLOSES[idx], MULTI_A_VOLUMES[idx]),
                    ),
                    (
                        b.clone(),
                        bar_with_close(ts, MULTI_B_CLOSES[idx], MULTI_B_VOLUMES[idx]),
                    ),
                ],
                states: HashMap::new(),
            }
        })
        .collect()
}

/// 按负载给出内核配置（单品种四负载共用一份，多品种单独一份）
pub fn canonical_config_for(workload: CanonicalWorkload) -> KernelConfig {
    if workload.is_multi_symbol() {
        multi_symbol_config()
    } else {
        canonical_config()
    }
}

/// 多品种内核配置（不启用保证金 / 资金费 / EOD 强平，差异只在两个 leg）
pub fn multi_symbol_config() -> KernelConfig {
    KernelConfig {
        expected_instruments: HashSet::from([canonical_instrument(), multi_symbol_instrument_b()]),
        ..canonical_config()
    }
}

/// 按负载给出帧表
pub fn canonical_frames_for(workload: CanonicalWorkload) -> Vec<GridFrame> {
    if workload.is_multi_symbol() {
        multi_symbol_frames()
    } else {
        canonical_frames()
    }
}

/// canonical 内核配置（四个负载共用同一份，差异只在 Host）
pub fn canonical_config() -> KernelConfig {
    KernelConfig {
        initial_cash: INITIAL_CASH,
        fee_rate: FEE_RATE,
        force_liquidate: false,
        margin_specs: HashMap::new(),
        instrument_mode: axon_kernel::InstrumentMode::Spot,
        interval: "1m".to_string(),
        rebalance_threshold: 0.001,
        seed: SeedLiquidity {
            half_spread: SEED_HALF_SPREAD,
            depth_levels: SEED_DEPTH_LEVELS,
            size_per_level: SEED_SIZE_PER_LEVEL,
        },
        capital_budget: None,
        funding_by_symbol: BTreeMap::new(),
        expected_instruments: HashSet::from([canonical_instrument()]),
    }
}

/// 配置指纹：把"决定数值结果的全部输入"拼成稳定文本后取 sha256
///
/// 只含确定性量（配置 + bar 表 + 负载自己的下单计划），不含路径 / 时间 / 环境。
/// 下单计划按负载分别收录：改市价计划只让市价负载的指纹变红，不会牵连
/// `replay_only` / `ema_cross`。
fn config_hash(workload: CanonicalWorkload, config: &KernelConfig) -> String {
    // 多品种负载的输入集（两份 bar 表 + 两份占比计划）与单品种结构不同，
    // 走独立分支；单品种分支保持原格式，已入库的四份指纹 config_hash 不变。
    if workload.is_multi_symbol() {
        return multi_symbol_config_hash(config);
    }
    let closes: Vec<String> = CLOSES.iter().map(|c| format!("{c:.6}")).collect();
    let volumes: Vec<String> = VOLUMES.iter().map(|v| format!("{v:.6}")).collect();
    let plan = match workload {
        CanonicalWorkload::ReplayOnly => "plan=hold".to_string(),
        CanonicalWorkload::ScheduledMarketOrders => format!("plan={MARKET_SCHEDULE:?}"),
        CanonicalWorkload::PassiveLimitOrders => format!("plan={LIMIT_SCHEDULE:?}"),
        CanonicalWorkload::EmaCross => {
            format!("plan=ema(fast={EMA_FAST},slow={EMA_SLOW},long_ratio={EMA_LONG_RATIO:.6})")
        }
        // 多品种在上方提前返回，这里仅为 match 完备性
        CanonicalWorkload::MultiSymbolRebalance => "plan=multi-symbol".to_string(),
    };
    let description = format!(
        "workload={};symbol={};bars={};start_ns={};step_ns={};closes=[{}];volumes=[{}];\
         initial_cash={:.6};fee_rate={:.6};force_liquidate={};interval={};rebalance_threshold={:.6};\
         seed=(half_spread={:.6},depth_levels={},size_per_level={:.6});capital_budget={:?};{}",
        workload.name(),
        SYMBOL,
        CLOSES.len(),
        START_NS,
        ONE_MIN,
        closes.join(","),
        volumes.join(","),
        config.initial_cash,
        config.fee_rate,
        config.force_liquidate,
        config.interval,
        config.rebalance_threshold,
        config.seed.half_spread,
        config.seed.depth_levels,
        config.seed.size_per_level,
        config.capital_budget,
        plan,
    );
    let digest = Sha256::digest(description.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// 多品种负载的配置指纹：两份 bar 表 + 两份占比计划 + 共用内核参数
fn multi_symbol_config_hash(config: &KernelConfig) -> String {
    let closes_a: Vec<String> = MULTI_A_CLOSES.iter().map(|c| format!("{c:.6}")).collect();
    let closes_b: Vec<String> = MULTI_B_CLOSES.iter().map(|c| format!("{c:.6}")).collect();
    let volumes_a: Vec<String> = MULTI_A_VOLUMES.iter().map(|v| format!("{v:.6}")).collect();
    let volumes_b: Vec<String> = MULTI_B_VOLUMES.iter().map(|v| format!("{v:.6}")).collect();
    let ratios_a: Vec<String> = MULTI_A_RATIOS.iter().map(|r| format!("{r:.6}")).collect();
    let ratios_b: Vec<String> = MULTI_B_RATIOS.iter().map(|r| format!("{r:.6}")).collect();
    let description = format!(
        "workload={};symbol_a={};symbol_b={};bars={};start_ns={};step_ns={};\
         closes_a=[{}];volumes_a=[{}];ratios_a=[{}];\
         closes_b=[{}];volumes_b=[{}];ratios_b=[{}];\
         initial_cash={:.6};fee_rate={:.6};force_liquidate={};interval={};rebalance_threshold={:.6};\
         seed=(half_spread={:.6},depth_levels={},size_per_level={:.6});capital_budget={:?}",
        CanonicalWorkload::MultiSymbolRebalance.name(),
        MULTI_A_SYMBOL,
        MULTI_B_SYMBOL,
        MULTI_A_CLOSES.len(),
        START_NS,
        ONE_MIN,
        closes_a.join(","),
        volumes_a.join(","),
        ratios_a.join(","),
        closes_b.join(","),
        volumes_b.join(","),
        ratios_b.join(","),
        config.initial_cash,
        config.fee_rate,
        config.force_liquidate,
        config.interval,
        config.rebalance_threshold,
        config.seed.half_spread,
        config.seed.depth_levels,
        config.seed.size_per_level,
        config.capital_budget,
    );
    let digest = Sha256::digest(description.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

// ════════════════════════════════════════════════════════════════════════════
// 四个 canonical Host
// ════════════════════════════════════════════════════════════════════════════

/// 只回放不下单的 Host（`Hold` 语义 = 空 `Vec`）
pub struct ReplayOnlyHost {
    instrument: Instrument,
}

impl ReplayOnlyHost {
    /// 构造
    pub fn new(instrument: Instrument) -> Self {
        Self { instrument }
    }
}

impl StrategyHost for ReplayOnlyHost {
    fn name(&self) -> &str {
        "canonical_replay_only"
    }

    fn on_event(&mut self, ev: &BarClosedEvent<'_>, _ctx: &HostCtx) -> Vec<OrderIntent> {
        debug_assert_eq!(ev.instrument, &self.instrument, "canonical 单品种");
        // 空 vec = 维持上次生效目标，永不下单
        Vec::new()
    }
}

/// 固定 bar 提交市价单的 Host
pub struct ScheduledMarketHost {
    instrument: Instrument,
    bars_seen: u64,
}

impl ScheduledMarketHost {
    /// 构造
    pub fn new(instrument: Instrument) -> Self {
        Self {
            instrument,
            bars_seen: 0,
        }
    }
}

impl StrategyHost for ScheduledMarketHost {
    fn name(&self) -> &str {
        "canonical_scheduled_market"
    }

    fn on_event(&mut self, ev: &BarClosedEvent<'_>, _ctx: &HostCtx) -> Vec<OrderIntent> {
        debug_assert_eq!(ev.instrument, &self.instrument, "canonical 单品种");
        let idx = self.bars_seen;
        self.bars_seen += 1;
        MARKET_SCHEDULE
            .iter()
            .filter(|(bar_idx, _, _)| *bar_idx == idx)
            .map(|(_, side, qty)| {
                OrderIntent::Submit(OrderSpec {
                    instrument: self.instrument.clone(),
                    side: *side,
                    quantity: Quantity::from_f64(*qty),
                    price: None,
                })
            })
            .collect()
    }
}

/// 固定 bar 挂限价单的 Host
///
/// 限价吃穿种子盘口即成交；没成交的余量以 GTC 挂在簿上，下一 bar 的
/// `begin_bar_multi` 会清书重建种子流动性（瞬时对手盘语义），因此余量被丢弃
/// ——这正是回测撮合的真实语义，指纹把它锁住。
pub struct PassiveLimitHost {
    instrument: Instrument,
    bars_seen: u64,
}

impl PassiveLimitHost {
    /// 构造
    pub fn new(instrument: Instrument) -> Self {
        Self {
            instrument,
            bars_seen: 0,
        }
    }
}

impl StrategyHost for PassiveLimitHost {
    fn name(&self) -> &str {
        "canonical_passive_limit"
    }

    fn on_event(&mut self, ev: &BarClosedEvent<'_>, _ctx: &HostCtx) -> Vec<OrderIntent> {
        debug_assert_eq!(ev.instrument, &self.instrument, "canonical 单品种");
        let idx = self.bars_seen;
        self.bars_seen += 1;
        let close = ev.bar.close.as_f64();
        LIMIT_SCHEDULE
            .iter()
            .filter(|(bar_idx, _, _, _)| *bar_idx == idx)
            .map(|(_, side, qty, offset)| {
                OrderIntent::Submit(OrderSpec {
                    instrument: self.instrument.clone(),
                    side: *side,
                    quantity: Quantity::from_f64(*qty),
                    price: Some(Price::from_f64(close + offset)),
                })
            })
            .collect()
    }
}

/// 单条 EMA 状态（Host 内部自算，不依赖 `axon-strategy`）
#[derive(Debug, Clone, Copy)]
struct Ema {
    /// 平滑系数 `2 / (period + 1)`
    alpha: f64,
    /// 当前值；`None` = 尚未初始化（首根直接用收盘价播种）
    value: Option<f64>,
}

impl Ema {
    fn new(period: usize) -> Self {
        Self {
            alpha: 2.0 / (period as f64 + 1.0),
            value: None,
        }
    }

    fn update(&mut self, price: f64) -> f64 {
        let next = match self.value {
            Some(prev) => prev + self.alpha * (price - prev),
            None => price,
        };
        self.value = Some(next);
        next
    }
}

/// EMA 快慢线交叉 → `SetTarget`（快线在上做多 0.5 权益，否则空仓）
pub struct EmaCrossHost {
    instrument: Instrument,
    fast: Ema,
    slow: Ema,
}

impl EmaCrossHost {
    /// 构造（fast = `EMA_FAST`、slow = `EMA_SLOW`，12 根 bar 内会来回切换）
    pub fn new(instrument: Instrument) -> Self {
        Self {
            instrument,
            fast: Ema::new(EMA_FAST),
            slow: Ema::new(EMA_SLOW),
        }
    }
}

impl StrategyHost for EmaCrossHost {
    fn name(&self) -> &str {
        "canonical_ema_cross"
    }

    fn on_event(&mut self, ev: &BarClosedEvent<'_>, _ctx: &HostCtx) -> Vec<OrderIntent> {
        debug_assert_eq!(ev.instrument, &self.instrument, "canonical 单品种");
        let close = ev.bar.close.as_f64();
        let fast = self.fast.update(close);
        let slow = self.slow.update(close);
        let target_ratio = if fast > slow { EMA_LONG_RATIO } else { 0.0 };
        vec![OrderIntent::SetTarget {
            instrument: self.instrument.clone(),
            target_ratio,
        }]
    }
}

/// 双品种同帧 rebalance Host
///
/// 每帧对两个品种各吐一个 `SetTarget`（`lifecycle` 逐 bar 逐品种调用
/// `on_event`），目标由 `MULTI_A_RATIOS` / `MULTI_B_RATIOS` 按 bar 序号给出。
/// 两个 leg 的目标在同一 `finish_bar` 落地 → 单次
/// `engine.rs::rebalance_to_target` 同时处理两个品种。
pub struct MultiSymbolRebalanceHost {
    a: Instrument,
    b: Instrument,
    bars_seen: u64,
}

impl MultiSymbolRebalanceHost {
    /// 构造
    pub fn new(a: Instrument, b: Instrument) -> Self {
        Self { a, b, bars_seen: 0 }
    }
}

impl StrategyHost for MultiSymbolRebalanceHost {
    fn name(&self) -> &str {
        "canonical_multi_symbol_rebalance"
    }

    fn on_event(&mut self, ev: &BarClosedEvent<'_>, _ctx: &HostCtx) -> Vec<OrderIntent> {
        // `on_event` 每帧被两个品种各调用一次；bar 序号只在二号品种（每帧最后
        // 一次调用）推进。这样两个品种读到的是同一个 bar 序号。
        let (instrument, ratio) = if ev.instrument == &self.a {
            (self.a.clone(), MULTI_A_RATIOS[self.bars_seen as usize])
        } else {
            debug_assert_eq!(ev.instrument, &self.b, "canonical 多品种只有两个 leg");
            let ratio = MULTI_B_RATIOS[self.bars_seen as usize];
            self.bars_seen += 1;
            (self.b.clone(), ratio)
        };
        vec![OrderIntent::SetTarget {
            instrument,
            target_ratio: ratio,
        }]
    }
}

/// 按负载装配 Host
fn host_for(workload: CanonicalWorkload) -> Box<dyn StrategyHost> {
    let instrument = canonical_instrument();
    match workload {
        CanonicalWorkload::ReplayOnly => Box::new(ReplayOnlyHost::new(instrument)),
        CanonicalWorkload::ScheduledMarketOrders => Box::new(ScheduledMarketHost::new(instrument)),
        CanonicalWorkload::PassiveLimitOrders => Box::new(PassiveLimitHost::new(instrument)),
        CanonicalWorkload::EmaCross => Box::new(EmaCrossHost::new(instrument)),
        CanonicalWorkload::MultiSymbolRebalance => Box::new(MultiSymbolRebalanceHost::new(
            canonical_instrument(),
            multi_symbol_instrument_b(),
        )),
    }
}

// ════════════════════════════════════════════════════════════════════════════
// 运行与报告
// ════════════════════════════════════════════════════════════════════════════

/// 跑一个 canonical 负载并产出报告
///
/// 内核装配：`Kernel::new(CanonicalConfig, BacktestSink::new())` + 对应 Host，
/// 12 帧一次性 `run_history`。无需 journal / progress 回调。
pub fn run_report(workload: CanonicalWorkload) -> CanonicalReport {
    let config = canonical_config_for(workload);
    let hash = config_hash(workload, &config);
    let mut kernel = Kernel::new(config, Box::new(BacktestSink::new()));
    kernel.add_host(host_for(workload));
    let result = kernel
        .run_history(canonical_frames_for(workload))
        .unwrap_or_else(|err| panic!("canonical workload {} 必须跑通: {err}", workload.name()));

    let run_metrics = serde_json::to_value(&result.run)
        .unwrap_or_else(|err| panic!("RunResult 序列化失败（{}）: {err}", workload.name()));
    let curves = json!({
        "bar_nav": curve_points(&result.run.bar_nav_curve),
        "bar_balance": curve_points(&result.run.bar_balance_curve),
    });
    CanonicalReport {
        workload: workload.name().to_string(),
        config_hash: hash,
        run_metrics,
        curves,
    }
}

/// 曲线逐点值 → `[{"ts": 纳秒, "value": 值}, ...]`
fn curve_points(curve: &[(Timestamp, f64)]) -> Vec<Value> {
    curve
        .iter()
        .map(|(ts, value)| json!({ "ts": ts.nanos, "value": value }))
        .collect()
}

// ════════════════════════════════════════════════════════════════════════════
// 指纹读写与比对
// ════════════════════════════════════════════════════════════════════════════

/// 指纹文件路径（`crates/axon-integration-tests/tests/fixtures/canonical/`）
pub fn fingerprint_path(workload: CanonicalWorkload) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/canonical")
        .join(format!("{}.json", workload.name()))
}

/// 编译期内嵌的指纹文本（`include_str!`，与 `fingerprint_path` 同一份文件）
pub fn fingerprint_source(workload: CanonicalWorkload) -> &'static str {
    match workload {
        CanonicalWorkload::ReplayOnly => {
            include_str!("../tests/fixtures/canonical/replay_only.json")
        }
        CanonicalWorkload::ScheduledMarketOrders => {
            include_str!("../tests/fixtures/canonical/scheduled_market_orders.json")
        }
        CanonicalWorkload::PassiveLimitOrders => {
            include_str!("../tests/fixtures/canonical/passive_limit_orders.json")
        }
        CanonicalWorkload::EmaCross => include_str!("../tests/fixtures/canonical/ema_cross.json"),
        CanonicalWorkload::MultiSymbolRebalance => {
            include_str!("../tests/fixtures/canonical/multi_symbol_rebalance.json")
        }
    }
}

/// 就地重写指纹文件（`AXON_CANONICAL_UPDATE` 通道）
///
/// 保留原文件的 `reasoning`（若已有且非空），否则用
/// [`CanonicalWorkload::default_reasoning`]：重生成指标不等于"为何更新"，
/// 理由必须由人填写。
fn write_fingerprint(report: &CanonicalReport, workload: CanonicalWorkload) {
    let previous = std::fs::read_to_string(fingerprint_path(workload))
        .ok()
        .and_then(|text| serde_json::from_str::<CanonicalFingerprint>(&text).ok());
    let reasoning = previous
        .map(|fp| fp.reasoning)
        .filter(|reasoning| !reasoning.trim().is_empty())
        .unwrap_or_else(|| workload.default_reasoning().to_string());
    let fingerprint = CanonicalFingerprint::from_report(report, &reasoning);
    let text = serde_json::to_string_pretty(&fingerprint).expect("指纹必须可序列化");
    let path = fingerprint_path(workload);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("创建指纹目录");
    }
    std::fs::write(&path, format!("{text}\n"))
        .unwrap_or_else(|err| panic!("写入指纹 {} 失败: {err}", path.display()));
}

/// 指纹比对结果
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FingerprintDiff {
    /// 字段级差异（空 = 完全一致）
    pub fields: Vec<String>,
}

impl FingerprintDiff {
    /// 是否一致
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// 渲染成多行文本（失败信息 / 日志用）
    pub fn to_message(&self) -> String {
        self.fields.join("\n")
    }
}

/// 逐字段比对两份 JSON（数值 1e-9 相对容差，其余全等）
///
/// 递归下钻到标量，差异以 `路径: 期望 X, 实际 Y` 逐行收集：
/// - 数字：`a == b` 或 `|a - b| <= 1e-9 * max(|a|, |b|)`（镜像 `assert_same_run`）；
/// - 数组：先比长度，再按下标递归；
/// - 对象：先比键集合（缺键 / 多键各一行），再按 key 递归；
/// - 其余（字符串 / 布尔 / null）：全等。
pub fn diff_json(expected: &Value, actual: &Value) -> FingerprintDiff {
    let mut diff = FingerprintDiff::default();
    diff_value("$", expected, actual, &mut diff.fields);
    diff
}

/// 比对报告与指纹（`workload` / `config_hash` / `metrics` / `curves` 四路）
pub fn diff_fingerprint(
    expected: &CanonicalFingerprint,
    actual: &CanonicalReport,
) -> FingerprintDiff {
    let mut diff = FingerprintDiff::default();
    if expected.workload != actual.workload {
        diff.fields.push(format!(
            "$.workload: 期望 {}, 实际 {}",
            expected.workload, actual.workload
        ));
    }
    if expected.config_hash != actual.config_hash {
        diff.fields.push(format!(
            "$.config_hash: 期望 {}, 实际 {}（配置 / bar 表变了）",
            expected.config_hash, actual.config_hash
        ));
    }
    diff_value(
        "$.metrics",
        &expected.metrics,
        &actual.run_metrics,
        &mut diff.fields,
    );
    diff_value(
        "$.curves",
        &expected.curves,
        &actual.curves,
        &mut diff.fields,
    );
    if expected.reasoning.trim().is_empty() {
        diff.fields
            .push("$.reasoning: 指纹缺少更新理由（C2 约定要求填写）".to_string());
    }
    diff
}

fn diff_value(path: &str, expected: &Value, actual: &Value, out: &mut Vec<String>) {
    match (expected, actual) {
        (Value::Number(left), Value::Number(right)) => {
            // `ts` 存纳秒时间戳（~1.7e18）：1e-9 相对容差能吸收约 ±1.7e9 ns
            // （≈1.7s）的漂移，对时间轴字段绝不能容忍。按 Number 精确相等比对，
            // 其余数值字段仍走 float_close。
            if is_timestamp_path(path) {
                if left != right {
                    out.push(format!("{path}: 期望 {left}, 实际 {right}"));
                }
                return;
            }
            let (Some(left), Some(right)) = (left.as_f64(), right.as_f64()) else {
                if left != right {
                    out.push(format!("{path}: 期望 {left}, 实际 {right}"));
                }
                return;
            };
            if !float_close(left, right) {
                out.push(format!("{path}: 期望 {left}, 实际 {right}"));
            }
        }
        (Value::Array(left), Value::Array(right)) => {
            if left.len() != right.len() {
                out.push(format!(
                    "{path}: 数组长度 期望 {}, 实际 {}",
                    left.len(),
                    right.len()
                ));
            }
            for (idx, (left, right)) in left.iter().zip(right.iter()).enumerate() {
                diff_value(&format!("{path}[{idx}]"), left, right, out);
            }
        }
        (Value::Object(left), Value::Object(right)) => {
            for key in left.keys() {
                if !right.contains_key(key) {
                    out.push(format!("{path}.{key}: 指纹有该字段,实际结果缺失"));
                    continue;
                }
                diff_value(&format!("{path}.{key}"), &left[key], &right[key], out);
            }
            for key in right.keys() {
                if !left.contains_key(key) {
                    out.push(format!("{path}.{key}: 实际结果多出该字段（指纹未收录）"));
                }
            }
        }
        (left, right) if left == right => {}
        (left, right) => out.push(format!("{path}: 期望 {left}, 实际 {right}")),
    }
}

/// 1e-9 相对容差（与 `kernel_loop.rs::f64_close` 同口径）
fn float_close(left: f64, right: f64) -> bool {
    left == right || (left - right).abs() <= REL_TOL * left.abs().max(right.abs())
}

/// `path` 末段是否是纳秒时间戳字段 `ts`
///
/// 曲线点形如 `$.curves.bar_nav[3].ts`，数组下标先剥掉再比字段名。
fn is_timestamp_path(path: &str) -> bool {
    let last = path.rsplit('.').next().unwrap_or(path);
    last.split('[').next().unwrap_or(last) == "ts"
}

// ════════════════════════════════════════════════════════════════════════════
// 测试入口
// ════════════════════════════════════════════════════════════════════════════

/// `AXON_CANONICAL_UPDATE` 是否要求重写该负载的指纹
fn update_requested(workload: CanonicalWorkload) -> bool {
    match std::env::var(UPDATE_ENV) {
        Ok(value) => {
            let value = value.trim();
            value == "all" || value == workload.name()
        }
        Err(_) => false,
    }
}

/// canonical 指纹矩阵测试主体（`tests/integration_tests.rs` 挂一个 `#[test]`）
///
/// 逐负载：构造报告 → 读内嵌指纹 → 递归比对。`AXON_CANONICAL_UPDATE` 命中时
/// 改为就地重写指纹文件（首次生成 / 有意变更的通道），不参与比对。
pub fn run_matrix() {
    let mut failures: Vec<String> = Vec::new();
    for workload in CanonicalWorkload::ALL {
        let report = run_report(workload);
        if update_requested(workload) {
            write_fingerprint(&report, workload);
            eprintln!(
                "canonical: 已重写指纹 {}（{}）",
                fingerprint_path(workload).display(),
                workload.name()
            );
            continue;
        }
        let expected: CanonicalFingerprint = serde_json::from_str(fingerprint_source(workload))
            .unwrap_or_else(|err| {
                panic!(
                    "指纹 {} 解析失败（{err}）;用 {UPDATE_ENV}=all 重新生成",
                    workload.name()
                )
            });
        let diff = diff_fingerprint(&expected, &report);
        if !diff.is_empty() {
            failures.push(format!(
                "── canonical workload `{}` 指纹不一致（{} 处）──\n{}",
                workload.name(),
                diff.fields.len(),
                diff.to_message()
            ));
        }
    }
    if !failures.is_empty() {
        panic!(
            "{}\n\n提示: 若本次变更是有意的撮合 / 聚合语义变更（或 0.26.0 首次生成），\
             先按 docs/zh/about/contributing.md「回归与修复约定」确认，再用 \
             {UPDATE_ENV}=all 重生成指纹，并在指纹 reasoning 中写明为何更新。\
             无理由的指标变更 = 违规。",
            failures.join("\n\n")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 五个负载都要在窗口内真的动起来（否则指纹是"空转基线"）
    #[test]
    fn canonical_workloads_are_not_degenerate() {
        let replay = run_report(CanonicalWorkload::ReplayOnly);
        assert_eq!(replay.run_metrics["fills"], 0, "replay_only 不应成交");
        assert_eq!(replay.run_metrics["orders_accepted"], 0);
        assert_eq!(
            replay.run_metrics["final_nav"].as_f64(),
            Some(INITIAL_CASH),
            "无成交时终值应等于初始资金"
        );

        for workload in [
            CanonicalWorkload::ScheduledMarketOrders,
            CanonicalWorkload::PassiveLimitOrders,
            CanonicalWorkload::EmaCross,
            CanonicalWorkload::MultiSymbolRebalance,
        ] {
            let report = run_report(workload);
            assert!(
                report.run_metrics["fills"].as_u64().unwrap_or(0) > 0,
                "{} 必须有成交",
                workload.name()
            );
            assert!(
                report.run_metrics["final_nav"].as_f64() != Some(INITIAL_CASH),
                "{} 终值应偏离初始资金",
                workload.name()
            );
        }
    }

    /// 多品种负载必须真的走到 `engine.rs::rebalance_to_target` 的多 legs 路径：
    /// 两个品种在同一帧都有 `SetTarget`，成交也必须覆盖两个品种（单品种成交
    /// 说明配置退化成了单 leg，指纹就锁不住同帧执行序）。
    #[test]
    fn multi_symbol_workload_fills_both_instruments() {
        let report = run_report(CanonicalWorkload::MultiSymbolRebalance);
        let mut bases: BTreeMap<String, u64> = BTreeMap::new();
        for fill in report.run_metrics["fills_detail"]
            .as_array()
            .expect("fills_detail 必须是数组")
        {
            let base = fill["instrument"]["details"]["base"]
                .as_str()
                .unwrap_or_else(|| panic!("fill 缺少 instrument.base: {fill}"));
            *bases.entry(base.to_string()).or_default() += 1;
        }
        assert!(
            bases.len() >= 2,
            "多品种负载的成交必须覆盖 ≥2 个品种，实际 {bases:?}"
        );
        assert!(bases.contains_key("BTC"), "缺少 BTC 成交: {bases:?}");
        assert!(bases.contains_key("ETH"), "缺少 ETH 成交: {bases:?}");
        assert!(
            report.run_metrics["rebalances_triggered"]
                .as_u64()
                .unwrap_or(0)
                >= 2,
            "同帧多 legs rebalance 至少应发 2 单: {}",
            report.run_metrics["rebalances_triggered"]
        );
    }

    /// 同负载双跑必须逐字段一致（duration 已被 serde skip，天然排除墙钟抖动）
    #[test]
    fn canonical_report_is_deterministic() {
        for workload in CanonicalWorkload::ALL {
            let first = run_report(workload);
            let second = run_report(workload);
            let diff = diff_fingerprint(
                &CanonicalFingerprint::from_report(&first, BASELINE_REASONING),
                &second,
            );
            assert!(
                diff.is_empty(),
                "{} 双跑不一致:\n{}",
                workload.name(),
                diff.to_message()
            );
        }
    }

    /// 数值容差之外的差异必须被 diff 抓到（自检比对器本身）
    #[test]
    fn diff_detects_tampered_metric() {
        let report = run_report(CanonicalWorkload::ScheduledMarketOrders);
        let mut fingerprint = CanonicalFingerprint::from_report(&report, BASELINE_REASONING);
        // 1e-6 相对偏差 >> 1e-9 容差 ⇒ 必须被抓到
        let nav = fingerprint.metrics["final_nav"].as_f64().expect("有终值");
        fingerprint.metrics["final_nav"] = json!(nav * (1.0 + 1e-6));
        let diff = diff_fingerprint(&fingerprint, &report);
        assert!(!diff.is_empty(), "篡改 final_nav 必须让比对变红");
        assert!(
            diff.to_message().contains("final_nav"),
            "diff 必须定位到字段: {}",
            diff.to_message()
        );

        // 1e-12 相对偏差 < 1e-9 容差 ⇒ 容差内应放行
        let mut tolerated = CanonicalFingerprint::from_report(&report, BASELINE_REASONING);
        tolerated.metrics["final_nav"] = json!(nav * (1.0 + 1e-12));
        assert!(
            diff_fingerprint(&tolerated, &report).is_empty(),
            "1e-12 偏差应在 1e-9 容差内"
        );
    }

    /// `ts` 是纳秒时间戳：纳秒级漂移必须被抓到
    ///
    /// 1e-9 相对容差在 ~1.7e18 上等于 ±1.7e9 ns（≈1.7s），时间轴字段绝不能
    /// 走容差，否则「曲线整体后移 1 秒」这类语义漂移会被静默放行。
    #[test]
    fn diff_detects_nanosecond_timestamp_drift() {
        let expected = json!({
            "curves": {
                "bar_nav": [
                    { "ts": 1_700_000_000_000_000_000i64, "value": 100_000.0 },
                ],
            },
        });

        let mut drifted = expected.clone();
        drifted["curves"]["bar_nav"][0]["ts"] = json!(1_700_000_000_000_000_001i64);
        let diff = diff_json(&expected, &drifted);
        assert!(!diff.is_empty(), "1ns 的 ts 漂移必须让比对变红");
        assert!(
            diff.to_message().contains("ts"),
            "diff 必须定位到 ts 字段: {}",
            diff.to_message()
        );
        assert!(
            diff.to_message().contains("1700000000000000001"),
            "diff 必须给出实际值: {}",
            diff.to_message()
        );

        // 同一纳秒值（指纹文件按整数、报告按 i64 序列化）必须判等
        let mut same = expected.clone();
        same["curves"]["bar_nav"][0]["ts"] = json!(1_700_000_000_000_000_000u64);
        assert!(
            diff_json(&expected, &same).is_empty(),
            "同一纳秒值不应产生 diff"
        );
    }

    /// 非 `ts` 的浮点字段仍走 1e-9 相对容差（既有行为回归）
    #[test]
    fn diff_keeps_float_tolerance_for_non_timestamp_fields() {
        let expected = json!({ "metrics": { "final_nav": 100_000.0 } });
        let actual = json!({ "metrics": { "final_nav": 100_000.0 * (1.0 + 1e-12) } });
        assert!(
            diff_json(&expected, &actual).is_empty(),
            "1e-12 相对偏差应在 1e-9 容差内（非 ts 字段口径不变）"
        );
    }
}
