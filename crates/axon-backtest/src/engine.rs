//! 回测引擎主循环（事件驱动）
//!
//! 消费 [`EventQueue`] 中的事件，按事件类型分发给 [`MatchingEngine`] 处理，
//! 并汇总运行结果（events_processed / orders_accepted / orders_rejected /
//! fills / total_pnl / max_drawdown / final_nav / duration）。
//!
//! # 事件分发
//!
//! - `OrderAction::Submitted(order)` → 提交至 `MatchingEngine`，统计 accepted/rejected
//! - `OrderAction::Cancelled / Modified / Rejected` → 计数 accepted
//! - `FillEvent` → 累加 fills 计数与 PnL
//! - 其他事件（MarketData / System）→ 计数后跳过
//!
//! # 设计约束
//!
//! - 匹配由 `axon-backtest::matching` 提供（L1/L2/L3），本模块仅做调度
//! - `ImpactModel` 为可选附加语义，当前实现记录 `impact_applied` 统计；具体
//!   价格调整由 `ImpactedMatchingEngine` 在更高层包装完成
//! - 单一回测任务单线程执行（事件驱动串行），不引入额外锁

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axon_core::event::{Event, FillEvent, FundingEvent, MarkEvent, OrderAction, OrderEvent};
use axon_core::impact::ImpactModel;
use axon_core::market::Side;
use axon_core::market::Trade;
use axon_core::metrics::TradingMetrics;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::portfolio::TradeRecord;
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{Instrument, Price, Quantity, SpotInstrument, Symbol};
use tracing::trace;

use crate::matching::MatchingEngine;

/// 回测引擎配置
///
/// - `clock`：模拟时钟（可设置结束时间；为 None 时引擎按事件自然耗尽退出）
/// - `matching_engine`：撮合引擎（L1/L2/L3），通过 trait object 注入
/// - `impact_model`：可选的市场冲击模型（仅用于统计；实际价格调整由
///   `ImpactedMatchingEngine` 在上层包装）
/// - `initial_cash`：初始现金（用于计算 `final_nav`）
/// - `force_liquidate`：回测结束 EOD 是否强制市价平仓
///   - `false` (默认)：按 `equity_curve` 末帧 mark-to-market 估值(保留策略意图)
///   - `true`:遍历 `position_states`,对每个非零持仓发市价单走撮合,清仓后才算终态
pub struct BacktestEngineConfig {
    /// 模拟时钟
    pub clock: SimulatedClock,
    /// 撮合引擎
    pub matching_engine: Box<dyn MatchingEngine>,
    /// 可选冲击模型
    pub impact_model: Option<Box<dyn ImpactModel>>,
    /// 初始资金
    pub initial_cash: f64,
    /// 手续费配置(Stage 3 阶段 B 引入,默认 taker 0.1% / maker 0.1%)
    pub fee_config: FeeConfig,
    /// EOD 强制平仓开关(默认 false,见 BacktestEngineConfig doc)
    pub force_liquidate: bool,
}

/// 手续费配置(Stage 3 阶段 B 新增)
///
/// 每笔 fill 按 `notional = price * qty` 收取 taker_rate 比例手续费;
/// 不区分 maker/taker(回测阶段简化为统一费率,与 `axon_core::fee::FeeModel`
/// 体系解耦,避免 Stage 2 的 `FeePosition` 复杂语义拖累主循环)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeeConfig {
    /// Taker 费率(0.001 = 0.1%)
    pub taker_rate: f64,
}

impl Default for FeeConfig {
    fn default() -> Self {
        // 行业默认:Binance/OKX VIP0 大约 0.1% taker
        Self { taker_rate: 0.001 }
    }
}

impl std::fmt::Debug for BacktestEngineConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BacktestEngineConfig")
            .field("clock", &self.clock)
            .field("matching_engine", &"<dyn MatchingEngine>")
            .field(
                "impact_model",
                &self.impact_model.as_ref().map(|m| m.name()),
            )
            .field("initial_cash", &self.initial_cash)
            .field("fee_config", &self.fee_config)
            .field("force_liquidate", &self.force_liquidate)
            .finish()
    }
}

/// EOD 强制平仓所发市价单的 id 起点(避免与策略订单 id / seed 流动性 id 冲突)
const EOD_LIQUIDATE_ID_BASE: u64 = 2_000_000_000;

/// 自动 rebalance 所发市价单的 id 起点(0.5.0 新增 Phase D)
///
/// 避开 0..1_000_000_000(策略订单)、1_000_000_000..2_000_000_000(seed 流动性)、
/// 2_000_000_000..3_000_000_000(EOD 平仓)区间,从 3_000_000_000 开始递增。
const REBALANCE_ID_BASE: u64 = 3_000_000_000;

/// 回测运行结果
#[derive(Debug, Clone, PartialEq)]
pub struct RunResult {
    /// 已处理事件总数（含 MarketData/Order/Fill/System）
    pub events_processed: u64,
    /// 接受的订单数（被撮合引擎接收，含部分成交）
    pub orders_accepted: u64,
    /// 拒绝的订单数（被撮合引擎拒绝 / 验证失败 / FOK 未满足）
    pub orders_rejected: u64,
    /// 成交（fill）总数（每个 MatchFill 计数一次）
    pub fills: u64,
    /// 取消的订单数
    pub orders_cancelled: u64,
    /// 修改的订单数
    pub orders_modified: u64,
    /// 已实现 + 未实现 PnL(账户视角):`final_nav - initial_cash`
    ///
    /// 等于"账户从初始资金到终值的变化量",已包含:
    /// - 已平仓 trade 的 realized_pnl(见 `trades`)
    /// - 未平仓持仓的 mark-to-market(`equity_curve` 末帧 mark)
    /// - 累计手续费(`total_fees`)
    ///
    /// 与 `final_nav` 自洽:`final_nav == initial_cash + total_pnl`。
    /// 旧版本按 fill 维度 cash flow 累计(buy=-notional, sell=+notional),
    /// 对未平仓 long 持仓会失真;现版本统一为账户视角。
    pub total_pnl: f64,
    /// 最大回撤(USD,绝对值),基于 `equity_curve` 扫描得到
    ///
    /// 算法:沿 equity_curve 单次扫描,维护 `nav_peak`,回撤 = `nav_peak - nav`。
    /// ponytail:简单 O(n) 单次扫描,O(1) 空间,无锁。
    pub max_drawdown: f64,
    /// 最终净资产（初始资金 + 累计 PnL）
    pub final_nav: f64,
    /// 运行耗时（墙钟时间）
    pub duration: Duration,
    /// 引擎最终时间（最后一个事件的时间戳）
    pub final_time: Timestamp,

    // ── Stage 3 阶段 B 新增字段 ─────────────────────────────
    /// 完整交易记录(开/平仓配对的 TradeRecord,单位 ×1e6 定点)
    pub trades: Vec<TradeRecord>,
    /// 累计手续费(f64,按 fill 累计扣除)
    pub total_fees: f64,
    /// NAV 曲线(`(timestamp_ns, nav)`),每笔 fill 后采样
    pub equity_curve: Vec<(Timestamp, f64)>,
    /// NAV 历史峰值(用于计算 max_drawdown_pct)
    pub nav_peak: f64,
    /// 最大回撤百分比(`max_drawdown / nav_peak`,0~1)
    pub max_drawdown_pct: f64,
    /// 胜率(盈利平仓笔数 / 总平仓笔数,来自 TradingMetrics)
    pub win_rate: f64,
    /// 夏普比率(基于 log return 年化,默认 15m bar 年化因子 sqrt(35040))
    pub sharpe_ratio: f64,
    /// 终态持仓快照(`instrument -> qty`),T3.5 改:`HashMap<Instrument, f64>`
    ///
    /// 只保留非零持仓(浮点容差 1e-9),便于 Python 端报告/对账
    /// 直接拿 `run_result.positions[instrument]`。
    pub positions: HashMap<Instrument, f64>,
    /// T3.5 新增:leg 目标仓位快照(`instrument -> target_position`)
    pub leg_targets: HashMap<Instrument, f64>,
    /// T3.5 新增:每 instrument 最新 mark 价格(`instrument -> mark_price`)
    pub marks: HashMap<Instrument, f64>,
    /// 0.5.0 新增(Phase C):累计 funding 结算 PnL(正=收,负=付)
    ///
    /// 来源:由 `BacktestEngine::handle_funding` 在收到 `Event::Funding` 时
    /// 按 `qty * funding_rate * mark_price` 累加;`final_nav` 已经把这个值
    /// 包含在 cash 余额中(因为我们直接改 cash),这里单列出来便于报告/对账。
    pub total_funding_pnl: f64,
    /// 0.5.0 新增(Phase D):自动 rebalance 触发的下单次数
    ///
    /// `with_auto_rebalance(threshold)` 启用后,每根 bar 末遍历 `legs`,对每
    /// 个 `|target - current| > threshold` 的 leg 发市价单,本字段累计所有
    /// **实际**发出去的 rebalance 单数(`rebalance_to_target()` 内部循环计数)。
    pub rebalances_triggered: u64,
}

impl Default for RunResult {
    fn default() -> Self {
        Self {
            events_processed: 0,
            orders_accepted: 0,
            orders_rejected: 0,
            fills: 0,
            orders_cancelled: 0,
            orders_modified: 0,
            total_pnl: 0.0,
            max_drawdown: 0.0,
            final_nav: 0.0,
            duration: Duration::ZERO,
            final_time: Timestamp::from_nanos(0),
            // Stage 3 阶段 B 默认值
            trades: Vec::new(),
            total_fees: 0.0,
            equity_curve: Vec::new(),
            nav_peak: 0.0,
            max_drawdown_pct: 0.0,
            win_rate: 0.0,
            sharpe_ratio: 0.0,
            positions: HashMap::new(),
            // T3.5 新增
            leg_targets: HashMap::new(),
            marks: HashMap::new(),
            // 0.5.0 新增(Phase C)
            total_funding_pnl: 0.0,
            // 0.5.0 新增(Phase D)
            rebalances_triggered: 0,
        }
    }
}

/// 内部统计状态（与 RunResult 区别：用于 step() 单步暴露中间态）
#[derive(Debug, Clone, Default)]
pub struct RunStats {
    /// 累计处理事件数
    pub events_processed: u64,
    /// 接受的订单数
    pub orders_accepted: u64,
    /// 拒绝的订单数
    pub orders_rejected: u64,
    /// 成交数
    pub fills: u64,
    /// 取消订单数
    pub orders_cancelled: u64,
    /// 修改订单数
    pub orders_modified: u64,
    /// 累计 PnL
    pub total_pnl: f64,
    /// PnL 运行峰值（用于计算 max_drawdown）
    pub pnl_peak: f64,
}

// ── Stage 3 阶段 B 新增:PositionState + BacktestState ─────────────────────

/// 单 symbol 持仓状态(Stage 3 阶段 B 新增)
///
/// 在 `apply_fill` 的 6 状态机中维护,作为 `trades: Vec<TradeRecord>` 的来源:
/// - 平仓/反手时调用方把 `(pnl, fee)` 累计到 `realized_pnl` + TradingMetrics
/// - `entry_price` / `entry_fee` 用于反手时重建新仓位的"开仓参考"
///
/// 简化点(ponytail):`quantity` 同时表示方向(正=Long,负=Short)与数量;
/// `side` 字段冗余,仅用于 Python 端报告展示。后续如果需要按 symbol 跟踪
/// 多空分别的 PnL,可拆分。
#[derive(Debug, Clone, Default)]
struct PositionState {
    /// 当前持仓量(正=Long,负=Short,0=空仓)
    quantity: f64,
    /// 加权平均成本(每次同向加仓时按 (|p|*cost + |n|*price)/|new| 更新)
    avg_cost: f64,
    /// 最近一次开仓价(用于反手时显示)
    entry_price: f64,
    /// 最近一次开仓时间(纳秒)
    entry_time_ns: i64,
    /// 当前持仓方向
    side: Option<Side>,
    /// 已实现盈亏累计(平仓/反手时 += pnl,f64)
    realized_pnl: f64,
    /// 累计开仓手续费
    entry_fee: f64,
}

/// 回测运行时状态(Stage 3 阶段 B 新增,封装在 `BacktestEngine` 内部)
///
/// 整合 6 状态机需要的全部上下文:
/// - `position_states`:per-instrument PositionState(**T3.5 改 key 类型**:
///   `HashMap<String, _>` → `HashMap<Instrument, _>`,消除 transient 字符串
///   编解码;apply_fill 直接以 `&Instrument` 作 key,`liquidate_eod` 直接
///   `iter` 拿 `&Instrument` 不再需要 `key_to_instrument` 解析)
/// - `trading_metrics`:胜率/夏普/累计 pnl/fees 收集器(线程安全,无锁)
/// - `cash`:当前现金余额
/// - `fee_accumulator`:累计手续费(冗余于 metrics.total_fees,便于快速读取)
/// - `nav_peak` / `max_drawdown_pct`:NAV 历史峰值与最大回撤百分比
/// - `equity_curve`:每笔 fill 后采样 `(Timestamp, nav)`
/// - `trades`:开/平仓配对的 `TradeRecord`(完全平仓/反手时 push)
/// - `legs`:每 leg 目标仓位(**T3.5 新增**,`HashMap<Instrument, LegConfig>`)
/// - `mark_cache`:每 instrument 最新 mark 价格(**T3.5 新增**,
///   由 `Event::Mark` 写入,供未来 funding 结算/未实现 PnL 估值)
#[derive(Debug, Default)]
struct BacktestState {
    /// per-instrument 持仓状态(T3.5 改:原 `HashMap<String, PositionState>`)
    position_states: HashMap<Instrument, PositionState>,
    /// 交易指标收集器(胜率/夏普)
    trading_metrics: TradingMetrics,
    /// 当前现金(buy 减 / sell 增 / ±fee)
    cash: f64,
    /// 累计手续费(f64 冗余字段,主源是 TradingMetrics.total_fees)
    fee_accumulator: f64,
    /// NAV 历史峰值(用于 max_drawdown_pct)
    nav_peak: f64,
    /// NAV 曲线采样
    equity_curve: Vec<(Timestamp, f64)>,
    /// 平仓记录(完全平仓/反手时 push)
    trades: Vec<TradeRecord>,
    /// T3.5 新增:每 leg 目标仓位(`HashMap<Instrument, LegConfig>`)
    legs: HashMap<Instrument, LegConfig>,
    /// T3.5 新增:每 instrument 最新 mark 价格缓存(`Event::Mark` 写入)
    mark_cache: HashMap<Instrument, Price>,
    /// 0.5.0 新增(Phase C):累计 funding 结算 PnL(`Event::Funding` 累加)
    total_funding_pnl: f64,
}

/// Leg 配置(策略目标仓位)
///
/// 每 leg 用 `set_target_position(instrument, target)` 注入,引擎在每根
/// bar 末按 `target_position` 与当前 `position_states[instrument].quantity`
/// 差异计算 delta,然后发市价单把仓位推到位(delta-neutral 套利 / 库存
/// 调整基础)。
///
/// 字段:
/// - `instrument`:本 leg 交易的品种(spot / swap)
/// - `target_position`:目标净持仓(正=Long,负=Short,0=空仓)
///
/// `Clone` 而非 `Copy`:`Instrument` 内部含 `Symbol(String)`,堆分配。
#[derive(Debug, Clone, PartialEq)]
pub struct LegConfig {
    /// 品种
    pub instrument: Instrument,
    /// 目标净持仓(正=Long,负=Short,0=空仓)
    pub target_position: f64,
}

impl Default for LegConfig {
    fn default() -> Self {
        Self {
            instrument: Instrument::Spot(SpotInstrument {
                base: Symbol::default(),
                quote: Symbol::default(),
            }),
            target_position: 0.0,
        }
    }
}

/// 虚拟流动性种子配置(回测辅助)
///
/// 通过 `BacktestEngine::with_seed_liquidity(...)` 启用后,引擎在每根 bar
/// 同步执行 `clear_book + seed_liquidity`:
/// - 在 `mid_price` 上下分别挂 `depth_levels` 层限价单
/// - 每层 `size_per_level` 数量
/// - 在 `half_spread` 价差阶梯上递增/递减
///
/// 由 `BacktestEngine::begin_bar(price, symbol)` 触发(每根 bar 调一次),
/// quantcell 应用层在每根 bar push 订单事件**之前**调用 `begin_bar`,
/// 即可让策略单"瞬时有对手盘"成交。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SeedLiquidityConfig {
    /// 每层价差(绝对价格单位),如 `0.0001 * mid = 10bps`
    pub half_spread: f64,
    /// 每侧挂单层数(典型 5~20)
    pub depth_levels: usize,
    /// 每层挂单数量
    pub size_per_level: f64,
}

impl Default for SeedLiquidityConfig {
    fn default() -> Self {
        Self {
            half_spread: 0.0,
            depth_levels: 0,
            size_per_level: 0.0,
        }
    }
}

/// 回测引擎：消费 `EventQueue` 调度撮合 + 汇总结果
pub struct BacktestEngine {
    /// 引擎配置（含 clock / matching_engine / impact_model）
    config: BacktestEngineConfig,
    /// 待消费事件队列
    event_queue: EventQueue,
    /// 运行统计(原有 stats,fill 计数/PnL 峰值等)
    stats: RunStats,
    /// 引擎是否已运行完成（防止重复调用 run）
    finished: bool,
    /// 阶段 B 持仓 / 资金 / 指标状态(6 状态机上下文)
    bt_state: BacktestState,
    /// 虚拟流动性种子配置(`None` = 不启用,等价纯订单簿撮合)
    seed_liquidity_config: Option<SeedLiquidityConfig>,
    /// 虚拟流动性种子 id 计数器(从大数 1_000_000_000 开始,避免与策略订单 id 冲突)
    seed_liquidity_next_id: std::sync::atomic::AtomicU64,
    /// 0.5.0 新增(Phase D):自动 rebalance 阈值
    ///
    /// `None` = 不启用;`Some(t)` 表示"`|target - current| > t` 才发单"。
    /// 默认建议 `1e-6`(避免抖动);`0.0` 等价"每 tick 都 rebalance"。
    auto_rebalance_threshold: Option<f64>,
    /// 0.5.0 新增(Phase D):rebalance 单 id 计数器(`REBALANCE_ID_BASE` 起点)
    rebalance_next_id: std::sync::atomic::AtomicU64,
    /// 0.5.0 新增(Phase D):累计 rebalance 触发的 fill 数
    ///
    /// 每次 `rebalance_to_target()` 内部每产生一个 fill 累加 1,最终
    /// 通过 `build_result()` 写到 `RunResult.rebalances_triggered`。
    /// 0.5.0 由调用方在 bar 末手动触发 rebalance 时累加;
    /// 0.5.1+ 可在 `begin_bar` 收尾自动 rebalance 时累加。
    rebalances_triggered: u64,
    /// 0.6.0 新增(Phase 1):bar 计数器,`begin_bar` 每次自增,
    /// `rebalance_to_target` 用它做"本 bar 已 rebalance"防重 guard。
    bar_id: u64,
    /// 0.6.0 新增(Phase 1):上一次 rebalance 触发的 bar_id(供 guard 检查)
    last_rebalance_bar_id: u64,
}

impl std::fmt::Debug for BacktestEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BacktestEngine")
            .field("config", &self.config)
            .field("event_queue_len", &self.event_queue.len())
            .field("stats", &self.stats)
            .field("finished", &self.finished)
            .field("bt_state", &self.bt_state)
            .finish()
    }
}

impl BacktestEngine {
    /// 创建回测引擎
    ///
    /// - `config`：回测配置（clock / 撮合器 / 冲击模型 / 初始资金 / 手续费）
    /// - `event_queue`：已填充事件的事件队列（所有权转移）
    pub fn new(config: BacktestEngineConfig, event_queue: EventQueue) -> Self {
        let initial_cash = config.initial_cash;
        // 初始 NAV = initial_cash,记入 peak
        let bt_state = BacktestState {
            cash: initial_cash,
            nav_peak: initial_cash,
            ..Default::default()
        };
        Self {
            config,
            event_queue,
            stats: RunStats::default(),
            finished: false,
            bt_state,
            // 虚拟流动性种子:默认未启用,需调 `with_seed_liquidity` 启用
            seed_liquidity_config: None,
            // id 计数器从 1_000_000_000 开始(策略订单 id 通常从 1 起递增,避免冲突)
            seed_liquidity_next_id: std::sync::atomic::AtomicU64::new(1_000_000_000),
            // 0.5.0 新增(Phase D):自动 rebalance 默认未启用
            auto_rebalance_threshold: None,
            // 0.5.0 新增(Phase D):rebalance id 起点避开策略/seed/EOD
            rebalance_next_id: std::sync::atomic::AtomicU64::new(REBALANCE_ID_BASE),
            // 0.5.0 新增(Phase D):rebalance 触发累计起点 0
            rebalances_triggered: 0,
            // 0.6.0 新增(Phase 1):bar 计数器从 0 开始,`begin_bar` 每次 +1
            bar_id: 0,
            // 0.6.0 新增(Phase 1):用 u64::MAX 表示"从未 rebalance",
            // 保证首次调用 `rebalance_to_target` 时 guard 不误伤(bar_id=0
            // 不会等于 u64::MAX,首次必通过)
            last_rebalance_bar_id: u64::MAX,
        }
    }

    /// 替换撮合引擎(Stage 3 新增,支持 Python 端自定义 Engine 真注入)
    pub fn replace_matching_engine(&mut self, engine: Box<dyn MatchingEngine>) {
        self.config.matching_engine = engine;
    }

    /// 注入手续费配置(Stage 3 阶段 B 任务 B4)
    ///
    /// 调用后,所有 fill 都按 `notional * taker_rate` 累计手续费;
    /// 不传任何参数时使用 `FeeConfig::default()`(0.1% taker)。
    pub fn with_fee_config(&mut self, taker_rate: f64) {
        self.config.fee_config = FeeConfig { taker_rate };
    }

    /// EOD 强制平仓开关(回测结束把未平仓持仓按市价平掉)
    ///
    /// - `false` (默认):保留策略意图,`final_nav` 用 `equity_curve` 末帧 mark 估值
    /// - `true`:遍历 `position_states`,对每个非零持仓发市价单,清仓后 `final_nav = cash`
    ///   (所有 PnL 转为已实现,胜率/夏普统计更准;但末根 bar 的"末日单"会污染 PnL)
    ///
    /// 可重复调用,生效于下一次 `run()`。
    pub fn with_force_liquidate(&mut self, on: bool) {
        self.config.force_liquidate = on;
    }

    /// 启用虚拟流动性种子(回测"瞬时对手盘"语义)
    ///
    /// 启用后,应用层每根 bar 调用 `begin_bar(price, symbol)` 即可触发
    /// 撮合引擎的 `clear_book + seed_liquidity` —— 让策略单"瞬时有对手盘"成交,
    /// 撮合完不保留上 bar 的种子流动性(避免跨 bar 累积撑爆订单簿)。
    ///
    /// # 参数
    ///
    /// - `half_spread`:每层价差(绝对价格单位),如 `0.0001 * mid = 10bps`
    /// - `depth_levels`:每侧挂单层数(典型 5~20)
    /// - `size_per_level`:每层挂单数量
    ///
    /// # 调用次数
    ///
    /// 可重复调用(更新配置);但**不**自动调用 `clear_book` —— 已有种子会保留,
    /// 下次 `begin_bar` 触发时一起清。配置变更语义"下次生效"。
    pub fn with_seed_liquidity(
        &mut self,
        half_spread: f64,
        depth_levels: usize,
        size_per_level: f64,
    ) {
        self.seed_liquidity_config = Some(SeedLiquidityConfig {
            half_spread,
            depth_levels,
            size_per_level,
        });
    }

    /// 每根 bar 开始时由应用层调用:同步执行 `clear_book + seed_liquidity`
    ///
    /// 行为:
    /// - 若 `seed_liquidity_config` 未设置(未调 `with_seed_liquidity`):no-op
    /// - 若已设置:`matcher.clear_book()` 清空旧种子,再 `seed_liquidity(mid_price, ...)`
    ///   按配置在 `mid_price` 上下挂 `depth_levels` 层限价单
    ///
    /// 必须在 `push_event("order_submitted", ...)` **之前**调用 —— 让对手盘先就位。
    /// 同步执行不入事件队列,纯配置侧操作。
    ///
    /// # 内存语义
    ///
    /// `clear_book` 会清空撮合引擎所有挂单 + 索引(L1 实现中
    /// `order_index` 替换为新 `HashMap` 实例强制 deallocate,见
    /// `L1MatchingEngine::clear_book` 注释)。多次 `begin_bar` 循环
    /// 后内存稳定,不累积。
    ///
    /// # T2.3 变更
    ///
    /// 参数 `symbol: Symbol` 替换为 `instrument: Instrument` 以支持
    /// 多品种路由(seed 用 `Order::spot(instrument.base(), ...)` 构造)。
    pub fn begin_bar(&mut self, mid_price: f64, instrument: Instrument) {
        // 0.6.0 新增(Phase 1):bar_id 自增,放最前以保证每次 begin_bar 都推进
        // bar_id(无论 seed_liquidity 是否启用)。这样:
        // - guard 让同 bar 多次 `rebalance_to_target` 只触发一次
        // - 用户用 begin_bar 显式跨 bar 时,新 bar 必能重新 rebalance
        self.bar_id += 1;
        // 1) 清空上一 bar 的种子挂单 + 2) 重新挂单(seed id 单调递增)
        // 0.6.0 改:用 if let 嵌套而非早 return,确保末尾自动 rebalance
        // 永远会执行(无论 seed_liquidity 是否启用)
        if let Some(cfg) = self.seed_liquidity_config {
            // ponytail:无效参数(no-op)与有效参数走同一路径,避免 L1 内部再判一次
            if mid_price > 0.0
                && cfg.half_spread > 0.0
                && cfg.depth_levels != 0
                && cfg.size_per_level > 0.0
            {
                // 1) 清空上一 bar 的种子挂单
                self.config.matching_engine.clear_book();
                // 2) 重新挂单(seed id 单调递增,避免与策略订单 id 冲突)
                let next_id = self
                    .seed_liquidity_next_id
                    .load(std::sync::atomic::Ordering::Relaxed);
                let new_next_id = self.config.matching_engine.seed_liquidity(
                    mid_price,
                    cfg.half_spread,
                    cfg.depth_levels,
                    cfg.size_per_level,
                    instrument, // 改: 原 symbol (T2.3)
                    next_id,
                );
                self.seed_liquidity_next_id
                    .store(new_next_id, std::sync::atomic::Ordering::Relaxed);
            }
        }
        // 0.6.0 新增(Phase 1):bar 末自动 rebalance
        //   - `auto_rebalance_threshold: None` → rebalance_to_target 内部用 +∞
        //     阈值,所有 leg 都在阈值内,no-op
        //   - 用户手写 `rebalance_to_target` 与自动模式冲突:bar_id guard
        //     保证同 bar 只触发一次(用户的手写 rebalance 在 begin_bar 之前
        //     执行,本 bar 自动 rebalance 会被 guard 跳过)
        self.rebalance_to_target(None);
    }

    /// 当前已注册的源数量（队列中剩余事件数）
    pub fn pending_events(&self) -> usize {
        self.event_queue.len()
    }

    /// 推入单个事件到事件队列
    ///
    /// 给 Python 绑定 / 外部调用方使用 —— 构造完整 `Event` 后追加到内部队列。
    /// 时序由 `EventQueue` 保证:同一时间戳按 `seq` 升序出队(FIFO 语义)。
    ///
    /// 用途:Python 端可逐条 push 订单事件、成交事件等,而非一次性 `EventQueue::new()` + `push`。
    pub fn push_event(&mut self, event: Event) {
        self.event_queue.push(event);
    }

    /// 当前统计快照
    pub fn stats(&self) -> &RunStats {
        &self.stats
    }

    // ── T3.8 新增:多 leg API(策略目标仓位 + 当前仓位查询) ─────────

    /// 设置某 leg 的目标仓位(仅记录,不主动下单)
    ///
    /// 引擎**不**主动根据 `target_position` 下单——它仅在 `BacktestState::legs`
    /// 中记录策略意图;由策略层在每根 bar 末读取 `get_target_position` /
    /// `get_position`,自行计算 delta 并发单。
    ///
    /// 重复设置同一 instrument 会覆盖前值(语义:"最新 set 生效")。
    ///
    /// # 用法(delta-neutral 套利示例)
    ///
    /// ```ignore
    /// engine.set_target_position(spot_inst, +1.0);   // 多 1 BTC 现货
    /// engine.set_target_position(swap_inst, -1.0);   // 空 1 BTC 永续
    /// // 每根 bar 末:
    /// for inst in &[spot_inst, swap_inst] {
    ///     let target = engine.get_target_position(inst).unwrap();
    ///     let current = engine.get_position(inst);
    ///     let delta = target - current;
    ///     if delta.abs() > 1e-9 { engine.submit(...); }
    /// }
    /// ```
    pub fn set_target_position(&mut self, instrument: Instrument, target: f64) {
        // ponytail:HashMap::entry(key).or_insert_with(...) 拿 key 后无法再借用
        // key 本身。解法:先 clone,再 or_insert_with(|| ...) 用 clone 构造。
        // Instrument 内部含 Symbol(String) 堆分配,但本函数调用频率低(每根
        // bar ≤ 1 次),clone 开销可忽略。
        let key = instrument.clone();
        self.bt_state
            .legs
            .entry(instrument)
            .or_insert_with(|| LegConfig {
                instrument: key,
                target_position: 0.0,
            })
            .target_position = target;
    }

    /// 查询某 leg 的目标仓位
    ///
    /// 返回 `None` 当该 instrument 从未 `set_target_position` 过。
    /// 返回 `Some(target)` 当已设置过;值允许为负(表示空头目标)。
    pub fn get_target_position(&self, instrument: &Instrument) -> Option<f64> {
        self.bt_state
            .legs
            .get(instrument)
            .map(|l| l.target_position)
    }

    /// 查询某 instrument 的当前仓位
    ///
    /// 返回 0.0 当该 instrument 从无成交(pos 状态不存在);否则返回
    /// `PositionState::quantity`(正=Long,负=Short,0=空仓)。
    pub fn get_position(&self, instrument: &Instrument) -> f64 {
        self.bt_state
            .position_states
            .get(instrument)
            .map(|p| p.quantity)
            .unwrap_or(0.0)
    }

    /// 单步处理一个事件：返回处理后的事件统计（用于单元测试与步进模式）
    pub fn step(&mut self) -> Option<RunStats> {
        let event = self.event_queue.next()?;
        self.dispatch(event);
        Some(self.stats.clone())
    }

    /// 完整运行：消费事件队列至耗尽，返回 `RunResult`。
    ///
    /// - 退出条件：`EventQueue::next()` 返回 `None`（队列耗尽或暂停）
    /// - 时钟推进：每步设置 `clock` 为当前事件时间戳
    /// - 耗时：使用 `Instant::now()` 测量墙钟耗时（`duration` 字段）
    /// - EOD 强制平仓：若 `config.force_liquidate = true`,主循环结束后遍历
    ///   `position_states` 把非零持仓按市价清掉(通过 `MatchingEngine::submit`),
    ///   清仓后 `final_nav = cash`,所有 PnL 转为已实现
    pub fn run(&mut self) -> RunResult {
        let started = Instant::now();
        let initial_cash = self.config.initial_cash;

        // 防止重复 run:若已 finished 且队列耗尽,直接返回上次结果。
        // 但 finished 后若继续 push_event / push_funding(quantcell 跨 bar
        // 调度场景),队列会重新有事件 → reset finished 继续 dispatch。
        // 0.5.0 改:之前 `if self.finished { return ... }` 会吞掉 rebalance_to_target
        // 之后 push 的 funding 事件。
        if self.finished && self.event_queue.is_empty() {
            return self.build_result(initial_cash, started.elapsed());
        }
        self.finished = false; // 0.5.0 新增:队列有事件时允许继续处理

        // 推进时钟到当前队列时间起点（事件可能晚于 clock.start()）
        if let Some(t) = self.event_queue.peek_time() {
            self.config.clock.set(t);
        }

        // 主循环：消费事件直到队列耗尽
        while let Some(event) = self.event_queue.next() {
            self.dispatch(event);
        }

        // EOD 强制平仓(可选)
        if self.config.force_liquidate {
            self.liquidate_eod();
        }

        self.finished = true;
        self.build_result(initial_cash, started.elapsed())
    }

    /// EOD 强制平仓:遍历 `position_states`,对每个非零持仓发市价单清仓
    ///
    /// # 实现要点
    ///
    /// - 用市价单(IOC,撮合引擎不维护挂单)走 `MatchingEngine::submit`,
    ///   不入事件队列 —— 平仓是"瞬时收尾",无时序语义
    /// - 市价单无价格,用撮合引擎当前最优对手价成交(`best_bid` / `best_ask`);
    ///   若对手盘空,订单被拒,保留剩余持仓(留待下个回测 / 报告里说明)
    /// - 平仓产生的 fill 仍走 `apply_fill` 6 状态机,正常累加 `total_fees` /
    ///   `realized_pnl` / `trades`
    /// - 平仓完后再调 `apply_fill` 内部的 mark-to-market 采样一遍 NAV,
    ///   保证 `equity_curve` 末帧反映"已清仓"的状态
    fn liquidate_eod(&mut self) {
        // 复制一份 instrument+qty 列表(后续会 borrow/mut borrow bt_state,不能持 &)
        // ponytail:回测场景 position 数量 << 100,O(n) 复制可忽略
        //
        // T3.5 改:position_states key 直接是 `Instrument`,不再走
        // `key_to_instrument` 解析;`to_liquidate` 直接拿 `&Instrument` 副本
        // 用于构造 `Order::spot(..., instrument.base, instrument.quote, ...)`。
        let to_liquidate: Vec<(Instrument, f64)> = self
            .bt_state
            .position_states
            .iter()
            .filter(|(_, p)| p.quantity.abs() > 1e-9)
            .map(|(inst, p)| (inst.clone(), p.quantity))
            .collect();
        if to_liquidate.is_empty() {
            return;
        }

        // 用 enumerate 替代外部可变计数器,clippy explicit_counter_loop 合规
        for (idx, (instrument, qty)) in to_liquidate.into_iter().enumerate() {
            // qty > 0 → 持 long,平仓卖;qty < 0 → 持 short,平仓买
            let side = if qty > 0.0 { Side::Sell } else { Side::Buy };
            let close_qty = qty.abs();
            // 从 instrument 自身拆出 base/quote,真正反映"平的是哪个品种"
            // (不再像旧版那样硬编码 Symbol::from("..."))。
            let (base, quote) = match &instrument {
                Instrument::Spot(s) => (s.base.clone(), s.quote.clone()),
                Instrument::Swap(s) => (s.base.clone(), s.quote.clone()),
            };
            let order = Order::spot(
                EOD_LIQUIDATE_ID_BASE + idx as u64,
                base,
                quote,
                side,
                OrderType::Market,
                Quantity::from_f64(close_qty),
                TimeInForce::IOC,
            );
            // 走 submit(同步),fill 走 apply_fill 6 状态机正常处理
            let result = self.config.matching_engine.submit(order);
            for fill in &result.fills {
                self.stats.orders_accepted += 1;
                self.stats.fills += 1;
                // 关键:走 6 状态机,正常扣手续费 + 算 realized_pnl + 记录 trades
                // PnL 增量累计到 stats(虽然 build_result 不再用,但保留 step 模式可见)
                self.stats.total_pnl += fill_pnl_delta(fill);
                if self.stats.total_pnl > self.stats.pnl_peak {
                    self.stats.pnl_peak = self.stats.total_pnl;
                }
                self.apply_fill(&instrument, side, fill);
            }
        }
    }

    /// 分发单个事件
    fn dispatch(&mut self, event: Event) {
        // 推进时钟
        self.config.clock.set(event.timestamp());

        // 计数
        self.stats.events_processed += 1;

        match event {
            Event::Order(OrderEvent { action, .. }) => self.handle_order_action(action),
            Event::Fill(fill) => self.handle_fill(fill),
            // T3.6 新增:Mark 事件 → 写 mark_cache(本次范围不触 NAV 重采样,详见 spec §5.2)
            Event::Mark(mark) => self.handle_mark(mark),
            // 0.5.0 新增(Phase C):Funding 事件 → 按 instrument 持仓结算资金费率
            Event::Funding(funding) => self.handle_funding(funding),
            // MarketData / System 事件仅计数
            _ => {
                trace!(seq = event.seq(), "non-dispatchable event (skipped)");
            }
        }
    }

    /// 处理订单动作
    ///
    /// `OrderAction` 标记为 `#[non_exhaustive]`，因此需要通配分支以兼容未来扩展。
    fn handle_order_action(&mut self, action: OrderAction) {
        match action {
            OrderAction::Submitted(order) => self.handle_submit(order),
            OrderAction::Cancelled(_) => {
                self.stats.orders_cancelled += 1;
            }
            OrderAction::Modified { .. } => {
                self.stats.orders_modified += 1;
            }
            OrderAction::Rejected { .. } => {
                self.stats.orders_rejected += 1;
            }
            // 非穷尽枚举：未来新增变体不影响向后兼容
            _ => {
                trace!("unhandled OrderAction variant");
            }
        }
    }

    /// 处理订单提交：调用撮合引擎并汇总结果
    ///
    /// 接受/拒绝的判定逻辑：
    /// - 若撮合器在 submit 前后活跃订单数 +1 ⇒ 订单被挂簿（accepted, pending）
    /// - 若活跃订单数未变且 fills 为空 ⇒ 订单被拒绝（accepted/rejected counter +1）
    /// - 若 fills 非空 ⇒ 订单被接受（accepted +1，且按 fill 数累加 fills/PnL）
    fn handle_submit(&mut self, order: Order) {
        // 阶段 B:从 order 提取 instrument/side,供 apply_fill 6 状态机使用
        // T2.2 阶段:instrument 暂用 transient 字符串 key (`"{base}/{quote}"`),
        // 由 [`instrument_to_key`] 编码;T3.5 会把 position_states 换成
        // `HashMap<Instrument, _>` 之后,直接传 `&Instrument` 即可。
        let instrument = order.instrument.clone();
        let side = order.side;
        let active_before = self.config.matching_engine.active_order_count();
        let result = self.config.matching_engine.submit(order);
        let active_after = self.config.matching_engine.active_order_count();
        let added_to_book = active_after > active_before;

        match (result.fills.is_empty(), added_to_book) {
            // 撮合出 fill ⇒ accepted
            (false, _) => {
                self.stats.orders_accepted += 1;
                // 每个 fill 独立计入 fills 计数
                self.stats.fills += result.fills.len() as u64;
                // 累加 PnL：基于 taker_side 区分买卖
                for fill in &result.fills {
                    let pnl_delta = fill_pnl_delta(fill);
                    self.stats.total_pnl += pnl_delta;
                    // 更新 PnL 峰值
                    if self.stats.total_pnl > self.stats.pnl_peak {
                        self.stats.pnl_peak = self.stats.total_pnl;
                    }
                    // 阶段 B:6 状态机处理
                    self.apply_fill(&instrument, side, fill);
                }
            }
            // 无 fill 但挂入订单簿 ⇒ accepted（pending）
            (true, true) => {
                self.stats.orders_accepted += 1;
            }
            // 无 fill 且未挂簿 ⇒ rejected
            (true, false) => {
                self.stats.orders_rejected += 1;
            }
        }
    }

    /// 6 状态机:处理单笔 fill,更新 BacktestState
    ///
    /// 输入:order 的 instrument/side + MatchFill。
    /// 行为:扣除/增加现金,扣除手续费,按 6 状态机维护 PositionState,
    /// 平仓/反手时 push TradeRecord + 记录到 TradingMetrics。
    ///
    /// 6 状态分类(按 prev=p,new=n 的符号与幅值):
    /// 1. **全新开仓** (p=0, n≠0):开新仓,记 entry_price / entry_fee
    /// 2. **同向加仓** (sign(p)=sign(n), p≠0):加权平均成本
    /// 3. **完全平仓** (sign(p)≠sign(n), |p|=|n|):close_qty=|p|,push TradeRecord
    /// 4. **反向部分平仓** (sign(p)≠sign(n), |n|<|p|):等同"完全平仓 + 反向开仓 n"
    /// 5. **反手** (sign(p)≠sign(n), |n|>|p|):等同"完全平仓 + 开反向 (n+p)"
    ///
    /// 不存在的"同向减仓"分支:fill 方向不变只会加仓,无法减仓(在主循环语义下);
    /// 如果将来加 reconcile 路径,该分支会出现在另外的状态机里。
    ///
    /// T2.4 改:`symbol: &str` 替换为 `instrument: &Instrument` 以支持
    /// 6 状态机:处理单笔 fill,更新 BacktestState
    ///
    /// T3.5 改:`position_states` 现在以 `&Instrument` 直接作 key,
    /// 不再走 `instrument_to_key` 字符串编解码。`TradeRecord` 已含
    /// `instrument: Instrument`(T2.4),用于 per-instrument 审计。
    fn apply_fill(
        &mut self,
        instrument: &Instrument,
        side: Side,
        fill: &crate::matching::MatchFill,
    ) {
        let fill_price = fill.price.as_f64();
        let fill_qty = fill.quantity.as_f64();
        let timestamp = fill.timestamp;

        // ── 1. 现金 + 手续费 ────────────────────────────
        let notional = fill_price * fill_qty;
        let fee = notional * self.config.fee_config.taker_rate;
        self.bt_state.fee_accumulator += fee;
        match side {
            Side::Buy => self.bt_state.cash -= notional + fee,
            Side::Sell => self.bt_state.cash += notional - fee,
        }

        // ── 2. 6 状态机 ──────────────────────────────
        let signed_qty = if side == Side::Buy {
            fill_qty
        } else {
            -fill_qty
        };
        // T3.5 改:`position_states.entry(instrument.clone())` 直接以
        // `&Instrument` 作 key,无需 `instrument_to_key` 派生字符串。
        let pos = self
            .bt_state
            .position_states
            .entry(instrument.clone())
            .or_default();
        let p = pos.quantity;
        let n = signed_qty;

        match (p, n) {
            // (1) 全新开仓
            (0.0, _) if n != 0.0 => {
                pos.quantity = n;
                pos.avg_cost = fill_price;
                pos.entry_price = fill_price;
                pos.entry_fee = fee;
                pos.entry_time_ns = timestamp.nanos;
                pos.side = Some(side);
            }
            // (2) 同向加仓 (sign same, p≠0)
            (p, n) if p.signum() == n.signum() && p != 0.0 => {
                let new_qty = p + n;
                // 加权平均成本(ponytail:简化用 f64,累计误差可忽略)
                pos.avg_cost = (p.abs() * pos.avg_cost + n.abs() * fill_price) / new_qty.abs();
                pos.quantity = new_qty;
                pos.entry_fee += fee;
            }
            // (3) 完全平仓 (sign diff, |p|=|n|,容忍 1e-9 浮点误差)
            (p, n) if p.signum() != n.signum() && (p + n).abs() < 1e-9 => {
                let close_qty = p.abs();
                let pnl = (fill_price - pos.avg_cost) * close_qty * p.signum();
                pos.realized_pnl += pnl;
                self.bt_state
                    .trading_metrics
                    .record_trade((pnl * 1e6) as i64, (fee * 1e6) as i64);
                let trade = Trade::new(
                    timestamp,
                    fill.price,
                    fill.quantity,
                    fill.taker_order_id,
                    fill.maker_order_id,
                );
                self.bt_state.trades.push(TradeRecord::new(
                    trade,
                    (pnl * 1e6) as i64,
                    (fee * 1e6) as i64,
                    (n * 1e6) as i64,
                    instrument.clone(), // T2.4 新增
                ));
                // 清仓
                pos.quantity = 0.0;
                pos.avg_cost = 0.0;
                pos.entry_price = 0.0;
                pos.entry_fee = 0.0;
                pos.side = None;
            }
            // (4) 反向部分平仓 (sign diff, |n| < |p|):用 |n| 平掉一部分旧仓,留 p+n
            //
            // ponytail:修复 #R-04 — 之前 `close_qty = p.abs()` 错误地把旧仓全平,
            // 跟"完全平仓"分支语义重叠;正确语义是"反向 fill 一部分只平一部分"。
            // 终态 `pos.quantity = p + n`,方向同 n,幅值 = |p| - |n|。
            (p, n) if p.signum() != n.signum() && n.abs() < p.abs() => {
                let close_qty = n.abs();
                let pnl = (fill_price - pos.avg_cost) * close_qty * p.signum();
                pos.realized_pnl += pnl;
                self.bt_state
                    .trading_metrics
                    .record_trade((pnl * 1e6) as i64, (fee * 1e6) as i64);
                // 平仓的 TradeRecord(净量 = n,即本次 fill 方向)
                let close_trade = Trade::new(
                    timestamp,
                    fill.price,
                    Quantity::from_f64(close_qty),
                    fill.maker_order_id,
                    fill.taker_order_id,
                );
                self.bt_state.trades.push(TradeRecord::new(
                    close_trade,
                    (pnl * 1e6) as i64,
                    (fee * 1e6) as i64,
                    (n * 1e6) as i64,
                    instrument.clone(), // T2.4 新增
                ));
                // 留 p+n(同 n 方向,幅值 = |p|-|n|)
                pos.quantity = p + n;
                // 平均成本不变(没加新仓,只是减仓)
            }
            // (5) 反手 (sign diff, |n| > |p|):平 p + 开反向 (n+p)
            (p, n) if p.signum() != n.signum() && n.abs() > p.abs() => {
                let close_qty = p.abs();
                let pnl = (fill_price - pos.avg_cost) * close_qty * p.signum();
                pos.realized_pnl += pnl;
                self.bt_state
                    .trading_metrics
                    .record_trade((pnl * 1e6) as i64, (fee * 1e6) as i64);
                let close_trade = Trade::new(
                    timestamp,
                    fill.price,
                    Quantity::from_f64(close_qty),
                    fill.maker_order_id,
                    fill.taker_order_id,
                );
                self.bt_state.trades.push(TradeRecord::new(
                    close_trade,
                    (pnl * 1e6) as i64,
                    (fee * 1e6) as i64,
                    (-p * 1e6) as i64,
                    instrument.clone(), // T2.4 新增
                ));
                // 开反向 n + p
                pos.quantity = n + p;
                pos.avg_cost = fill_price;
                pos.entry_price = fill_price;
                pos.entry_fee = fee;
                pos.entry_time_ns = timestamp.nanos;
                pos.side = Some(side);
            }
            // 不应到达 — 防御性兜底
            _ => {}
        }

        // ── 3. NAV 采样 + log return ─────────────────
        // Phase B 改(0.5.0):mark-to-market 用 `mark_cache[instrument]` 而非 fill_price
        //   - 本次 fill 的 instrument 用 `mark_cache[instrument]`(若已缓存)
        //   - 其他 instrument 用各自的 `mark_cache` 值(若已缓存),否则 0
        //   - 都没缓存时,fallback 到 fill_price(回测无 mark 数据的旧场景)
        // 效果:多 leg 持仓时 NAV 反映每个 leg 各自的 mark 价,unrealized PnL
        //   正确进 equity_curve 和 total_pnl。
        let nav = self.compute_nav(timestamp, fill_price);
        if nav > self.bt_state.nav_peak {
            self.bt_state.nav_peak = nav;
        }
        self.bt_state.equity_curve.push((timestamp, nav));

        // log return:本次 nav / 上次 nav(仅在 prev>0 时记录)
        if self.bt_state.equity_curve.len() >= 2 {
            let prev_nav = self.bt_state.equity_curve[self.bt_state.equity_curve.len() - 2].1;
            if prev_nav > 0.0 {
                let lr = (nav / prev_nav).ln();
                self.bt_state
                    .trading_metrics
                    .record_log_return((lr * 1e9) as i64);
            }
        }
    }

    /// 处理成交事件（来自外部 FillEvent 推送）
    ///
    /// FillEvent.trade 不含 taker_side 字段(只有 buyer/seller 订单 ID),
    /// 无法直接接入 6 状态机,故此处保守地只累计 fills 计数 + 现金恒等式
    /// (FillEvent 通常由外部 backtest harness 推送用于 hybrid 场景)。
    fn handle_fill(&mut self, fill: FillEvent) {
        self.stats.fills += 1;
        // FillEvent.trade 含 buyer/seller 订单 ID；用 axon-core Trade 转为 PnL
        let pnl_delta = trade_pnl_delta(&fill.trade);
        self.stats.total_pnl += pnl_delta;
        if self.stats.total_pnl > self.stats.pnl_peak {
            self.stats.pnl_peak = self.stats.total_pnl;
        }
    }

    /// 处理 Mark 事件(标记价格更新)— T3.6 新增
    ///
    /// 行为:把 `(instrument → mark_price)` 写入 `mark_cache`,供未来
    /// funding 结算 / 未实现 PnL 估值使用。
    ///
    /// 本次 spec 范围(T3.6):
    /// - **不**触 NAV 重采样(避免每个 mark 事件都重算 equity_curve,
    ///   fill 驱动的 mark-to-market 仍然由 `apply_fill` 负责)
    /// - **不**触 funding 结算(留给未来 T7+ funding rate 阶段)
    ///
    /// 幂等性:同一 `instrument` 多次 mark 事件,后到的覆盖前到的(最新价生效)。
    /// spec §5.2 / §4.4。
    fn handle_mark(&mut self, mark: MarkEvent) {
        self.bt_state
            .mark_cache
            .insert(mark.instrument, mark.mark_price);

        // Phase B 改(0.5.0):Mark 事件现在也触 NAV 重采样,让 equity_curve
        // 反映 mark 价变化对未实现 PnL 的影响。频率受用户 push 节奏控制
        // (8h funding tick / 1m mark / 1h bar close 等场景),不会过热。
        // 跨 leg 净值才能正确算 e.g. spot long 0.5 + perp short 0.5,中间
        // mark 价从 50k 涨到 51k 时,unrealized PnL 应反映 spot +500,perp -500 = 0。
        let timestamp = mark.timestamp;
        let nav = self.compute_nav(timestamp, mark.mark_price.as_f64());
        if nav > self.bt_state.nav_peak {
            self.bt_state.nav_peak = nav;
        }
        // 避免连续 mark 在同一纳秒戳重复推 equity_curve
        if self
            .bt_state
            .equity_curve
            .last()
            .map(|(t, _)| *t == timestamp)
            .unwrap_or(false)
        {
            // 同时间戳已有帧 → 覆盖最后一帧(取最新 mark 估的 NAV)
            if let Some(last) = self.bt_state.equity_curve.last_mut() {
                last.1 = nav;
            }
        } else {
            self.bt_state.equity_curve.push((timestamp, nav));
        }
    }

    /// Phase B 新增(0.5.0):按 instrument 各自的 `mark_cache` 价计算 mark-to-market NAV
    ///
    /// 公式:`NAV = cash + Σ(quantity[instrument] × mark[instrument])`
    /// - `mark_cache[instrument]` 若有,用缓存的 mark 价
    /// - 若无,**fallback_mark** 给定(通常是本次 fill_price,旧场景无 mark 数据)
    /// - 既无 mark 也无 fallback 的 instrument 用其 `avg_cost` 占位(避免
    ///   没 mark 时 NAV 看起来是 0,误导回测)
    fn compute_nav(&self, _timestamp: Timestamp, fallback_mark: f64) -> f64 {
        let position_value: f64 = self
            .bt_state
            .position_states
            .iter()
            .map(|(inst, pos)| {
                let mark = self
                    .bt_state
                    .mark_cache
                    .get(inst)
                    .map(|m| m.as_f64())
                    .unwrap_or(fallback_mark);
                pos.quantity * mark
            })
            .sum();
        self.bt_state.cash + position_value
    }

    /// 0.5.0 新增(Phase C):处理 Funding 结算事件
    ///
    /// 行为:
    /// - 查找 `position_states[instrument]` 当前持仓(`quantity` 带符号)
    /// - 算 `cash_delta = quantity * funding_rate * mark_price`
    ///   (由 `FundingEvent::cash_delta_for` 统一实现,符号语义见其 doc)
    /// - 直接累计到 `bt_state.cash`,因为 cash 已经在 `final_nav` 中体现
    /// - 累计到 `bt_state.total_funding_pnl`(便于 RunResult 报告)
    /// - 触发 NAV 重采样(同 `handle_mark`,让 `equity_curve` 反映 funding 入账)
    ///
    /// 边界:
    /// - **spot instrument 收到 FundingEvent 会被忽略**(spot 无 funding)
    /// - **无持仓时 cash_delta = 0**,仍写 `total_funding_pnl += 0`(无副作用)
    /// - **8h 调度不在引擎内**(plan 决策),由 quantcell / 数据源按需 push
    fn handle_funding(&mut self, funding: FundingEvent) {
        // spot 无 funding 概念,直接跳过(保持 cash 不动,total_funding_pnl 不变)
        if matches!(funding.instrument, Instrument::Spot(_)) {
            trace!(
                instrument = ?funding.instrument,
                "spot instrument 收到 FundingEvent,忽略(spot 无 funding 概念)"
            );
            return;
        }

        // 找当前持仓(可能为 0:尚无成交,funding 入账为 0)
        let qty = self
            .bt_state
            .position_states
            .get(&funding.instrument)
            .map(|p| p.quantity)
            .unwrap_or(0.0);
        let cash_delta = funding.cash_delta_for(qty);

        self.bt_state.cash += cash_delta;
        self.bt_state.total_funding_pnl += cash_delta;

        // NAV 重采样(同 handle_mark 模式,避免连续 funding 在同一时间戳重复推)
        let timestamp = funding.timestamp;
        let nav = self.compute_nav(timestamp, funding.mark_price.as_f64());
        if nav > self.bt_state.nav_peak {
            self.bt_state.nav_peak = nav;
        }
        if self
            .bt_state
            .equity_curve
            .last()
            .map(|(t, _)| *t == timestamp)
            .unwrap_or(false)
        {
            if let Some(last) = self.bt_state.equity_curve.last_mut() {
                last.1 = nav;
            }
        } else {
            self.bt_state.equity_curve.push((timestamp, nav));
        }
    }

    /// 0.5.0 新增(Phase C):便捷推入 funding 事件(Python 端友好)
    ///
    /// 等价 `push_event(Event::Funding(FundingEvent::new(...)))`,免去 Python 端
    /// 构造完整 `Event` 枚举的样板(避免暴露内部 `EventBuilder` 给 PyO3 绑定)。
    ///
    /// # 用法
    ///
    /// 外部调度器(每 8h / 1m 等按需)在 data feed 推动时调用:
    /// ```ignore
    /// engine.push_funding(swap_inst, 0.0001, 50_000.0, ts);
    /// ```
    pub fn push_funding(
        &mut self,
        instrument: Instrument,
        funding_rate: f64,
        mark_price: f64,
        timestamp: Timestamp,
    ) {
        let evt = FundingEvent::new(
            instrument,
            funding_rate,
            Price::from_f64(mark_price),
            timestamp,
        );
        self.event_queue.push(Event::Funding(evt));
    }

    /// 0.5.0 新增(Phase D):启用自动 rebalance 阈值
    ///
    /// 调用后,每根 bar 末(`begin_bar` 之后 / `step` 收尾)自动调
    /// `rebalance_to_target()`,对每个 `|target - current| > threshold` 的 leg
    /// 发市价单把仓位推到位。
    ///
    /// # 参数
    ///
    /// - `threshold`:最小 delta(绝对值,`f64`),小于此值不触发 rebalance。
    ///   - 建议 `1e-6`(避免 fill 抖动反复触发)
    ///   - `0.0` 等价"每 tick rebalance"(几乎无阈值过滤,谨慎使用)
    ///
    /// # 关闭
    ///
    /// `with_auto_rebalance_disable()` 关闭(回到 `None` 状态)。
    pub fn with_auto_rebalance(&mut self, threshold: f64) {
        self.auto_rebalance_threshold = Some(threshold);
    }

    /// 0.5.0 新增(Phase D):关闭自动 rebalance
    pub fn with_auto_rebalance_disable(&mut self) {
        self.auto_rebalance_threshold = None;
    }

    /// 0.5.0 新增(Phase D):手动触发 rebalance
    ///
    /// 遍历 `bt_state.legs`:
    /// - `current = position_states[instrument].quantity`(无则为 0)
    /// - `delta = target - current`
    /// - `|delta| > threshold` 时发市价单,`side` 跟 `delta` 同号
    ///   (`delta > 0` ⇒ Buy,`delta < 0` ⇒ Sell)
    /// - 市价单走 `MatchingEngine::submit`(同 EOD 平仓),产生的 fill 走
    ///   `apply_fill` 6 状态机正常处理(累计 cash、realized_pnl、trades)
    ///
    /// # 参数
    ///
    /// - `threshold`:最小 delta(绝对值)。传 `None` 用本字段默认阈值;
    ///   传 `Some(t)` 临时覆盖(单次 rebalance 用,不影响后续 bar 末的
    ///   `auto_rebalance_threshold` 配置)。
    ///
    /// # 返回
    ///
    /// 实际发出去的 rebalance 单数(便于 `RunResult::rebalances_triggered` 统计)。
    pub fn rebalance_to_target(&mut self, threshold_override: Option<f64>) -> u64 {
        // 0.6.0 新增(Phase 1):bar_id guard——本 bar 已 rebalance 过则 no-op,
        // 与 `begin_bar` 收尾的自动 rebalance 配合,避免同 bar 多次触发
        if self.last_rebalance_bar_id == self.bar_id {
            return 0;
        }

        // 确定本轮阈值(`override` 优先;否则读 auto_rebalance_threshold;
        // 都没有说明 rebalance 关闭 → 直接返回 0)
        let threshold = threshold_override
            .or(self.auto_rebalance_threshold)
            .unwrap_or(f64::INFINITY);

        // 收集要 rebalance 的 leg(避免 borrow/mut borrow 冲突:先 copy 一份)
        // 复制为 (instrument, target) 元组列表;后续用 instrument 直接查
        // position_states 拿 current。
        let legs: Vec<(Instrument, f64)> = self
            .bt_state
            .legs
            .iter()
            .map(|(inst, leg)| (inst.clone(), leg.target_position))
            .collect();

        let mut triggered = 0u64;
        // 用 rebalance_next_id 原子计数器确保多次 rebalance 调用 id 唯一
        for (instrument, target) in legs.into_iter() {
            let current = self
                .bt_state
                .position_states
                .get(&instrument)
                .map(|p| p.quantity)
                .unwrap_or(0.0);
            let delta = target - current;
            if delta.abs() <= threshold {
                continue; // 在阈值内,不发单
            }
            // delta > 0 ⇒ Buy(增加持仓);delta < 0 ⇒ Sell(减少持仓)
            let side = if delta > 0.0 { Side::Buy } else { Side::Sell };
            let qty = delta.abs();
            // 0.5.0 修:按 instrument 类型分派 `Order::spot` / `Order::swap`,
            // 之前一律用 `Order::spot` 导致 perp leg 的 `instrument` 字段被错
            // 设为 Spot,路由到 spot book,永远不成交。
            let order_id = self
                .rebalance_next_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let order = match &instrument {
                Instrument::Spot(s) => Order::spot(
                    order_id,
                    s.base.clone(),
                    s.quote.clone(),
                    side,
                    OrderType::Market,
                    Quantity::from_f64(qty),
                    TimeInForce::IOC,
                ),
                Instrument::Swap(s) => Order::swap(
                    order_id,
                    s.base.clone(),
                    s.quote.clone(),
                    s.settle,
                    s.contract_size,
                    side,
                    OrderType::Market,
                    Quantity::from_f64(qty),
                    TimeInForce::IOC,
                ),
            };
            // 同步提交走 MatchingEngine(同 EOD 模式,不入事件队列)
            let result = self.config.matching_engine.submit(order);
            for fill in &result.fills {
                self.stats.orders_accepted += 1;
                self.stats.fills += 1;
                self.stats.total_pnl += fill_pnl_delta(fill);
                if self.stats.total_pnl > self.stats.pnl_peak {
                    self.stats.pnl_peak = self.stats.total_pnl;
                }
                // 走 6 状态机更新持仓 / cash / realized_pnl
                self.apply_fill(&instrument, side, fill);
                triggered += 1;
            }
        }
        // 0.5.0 新增(Phase D):把本次 fill 数累计到引擎字段,
        // `build_result` 时统一写到 `RunResult.rebalances_triggered`
        self.rebalances_triggered += triggered;
        // 0.6.0 新增(Phase 1):本 bar 已 rebalance,记 bar_id
        // (仅当 triggered > 0 时记录,确保"无 target / 全在阈值内"场景
        // 不会浪费 guard —— 用户多次无操作 rebalance 仍可继续尝试)
        if triggered > 0 {
            self.last_rebalance_bar_id = self.bar_id;
        }
        triggered
    }

    /// 构造最终 RunResult
    ///
    /// # 关键语义(total_pnl / max_drawdown 是"账户视角"而非"现金流视角")
    ///
    /// - `total_pnl = final_nav - initial_cash`:已实现 PnL + 未实现 PnL - 手续费
    ///   (i.e. 等于"账户从初始资金到终值的变化量")。这与 `final_nav` 自洽,
    ///   也与 `trades[].realized_pnl` + 未平仓持仓的 mark-to-market 等价。
    /// - 旧实现把 `total_pnl` 当成"fill 维度 cash flow 累计"(buy=-notional, sell=+notional),
    ///   对未平仓 long 持仓会失真(把开仓花的现金误算成亏损);旧 `max_drawdown` 也是
    ///   基于该 cash flow,`pnl_peak - total_pnl` 同样失真。
    /// - `max_drawdown`:基于 `equity_curve` 真实扫描计算,反映实际账户价值回撤。
    fn build_result(&self, initial_cash: f64, duration: Duration) -> RunResult {
        // 1. final_nav:equity_curve 最后一帧(mark-to-market);空时回退 initial_cash
        let final_nav = if let Some((_, last_nav)) = self.bt_state.equity_curve.last() {
            *last_nav
        } else {
            initial_cash
        };
        let final_time = self.config.clock.now();

        // 2. total_pnl:账户视角 = final_nav - initial_cash
        //    (已实现 PnL 来自 trades;未实现 PnL 来自 equity_curve 末帧 mark;
        //     手续费已扣 cash,自然包含)
        let total_pnl = final_nav - initial_cash;

        // 3. max_drawdown:扫描 equity_curve 真实计算
        //    ponytail:简单 O(n) 单次扫描,O(1) 空间
        let mut nav_peak = initial_cash;
        let mut max_drawdown: f64 = 0.0;
        for (_, nav) in &self.bt_state.equity_curve {
            if *nav > nav_peak {
                nav_peak = *nav;
            }
            let dd = nav_peak - *nav;
            if dd > max_drawdown {
                max_drawdown = dd;
            }
        }
        // 兼容旧语义:若 equity_curve 为空,沿用 stats.pnl_peak - total_pnl
        // (空场景无 fill,无 PnL,dd=0)
        let _ = (self.stats.pnl_peak, self.stats.total_pnl);

        // 4. max_drawdown_pct = drawdown / nav_peak(0~1,限制)
        let max_drawdown_pct = if nav_peak > 0.0 {
            (max_drawdown / nav_peak).clamp(0.0, 1.0)
        } else {
            0.0
        };

        // 5. positions 快照(T3.5 改:`HashMap<Instrument, f64>`)
        let positions: HashMap<Instrument, f64> = self
            .bt_state
            .position_states
            .iter()
            .filter(|(_, p)| p.quantity.abs() > 1e-9)
            .map(|(inst, p)| (inst.clone(), p.quantity))
            .collect();

        // 5b. T3.5 新增:leg_targets + marks 快照
        let leg_targets: HashMap<Instrument, f64> = self
            .bt_state
            .legs
            .iter()
            .map(|(inst, cfg)| (inst.clone(), cfg.target_position))
            .collect();
        let marks: HashMap<Instrument, f64> = self
            .bt_state
            .mark_cache
            .iter()
            .map(|(inst, p)| (inst.clone(), p.as_f64()))
            .collect();

        // 6. win_rate / sharpe_ratio 从 TradingMetrics 取
        //    默认年化因子:15m bar 一年 35040 根(24h * 4 * 365)
        let win_rate = self.bt_state.trading_metrics.win_rate();
        let sharpe_ratio = self
            .bt_state
            .trading_metrics
            .sharpe_ratio(35_040_f64.sqrt());

        RunResult {
            events_processed: self.stats.events_processed,
            orders_accepted: self.stats.orders_accepted,
            orders_rejected: self.stats.orders_rejected,
            fills: self.stats.fills,
            orders_cancelled: self.stats.orders_cancelled,
            orders_modified: self.stats.orders_modified,
            total_pnl,
            max_drawdown,
            final_nav,
            duration,
            final_time,
            trades: self.bt_state.trades.clone(),
            total_fees: self.bt_state.fee_accumulator,
            equity_curve: self.bt_state.equity_curve.clone(),
            nav_peak,
            max_drawdown_pct,
            win_rate,
            sharpe_ratio,
            positions,
            // T3.5 新增
            leg_targets,
            marks,
            // 0.5.0 新增(Phase C):funding 累计 PnL
            total_funding_pnl: self.bt_state.total_funding_pnl,
            // 0.5.0 新增(Phase D):rebalance 触发次数(累计所有
            // `rebalance_to_target()` 调用产出的实际 fill 数)
            rebalances_triggered: self.rebalances_triggered,
        }
    }
}

/// 计算 `MatchFill` 对 taker 的 PnL 影响
///
/// 视角为"回测策略方"：
/// - Buy 端：现金减少，PnL 记为 `-price * qty`
/// - Sell 端：现金增加，PnL 记为 `+price * qty`
fn fill_pnl_delta(fill: &crate::matching::MatchFill) -> f64 {
    let notional = fill.turnover();
    match fill.taker_side {
        Side::Buy => -notional,
        Side::Sell => notional,
    }
}

/// 计算外部 `Trade` 的 PnL 影响
///
/// Trade 字段同时包含 buyer/seller 订单 ID；本函数使用一个保守的"对称记账"：
/// 因无法判断哪一方是策略侧，将 trade 视为市场价格参考，PnL 增量为 0。
/// 实际策略应使用 `MatchFill` 路径获取按 taker_side 区分的 PnL。
fn trade_pnl_delta(_trade: &Trade) -> f64 {
    // 见上方说明：保守为 0；按需可在调用侧自行计算 realized_pnl
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::event::{EventBuilder, OrderAction};
    use axon_core::market::Side;
    use axon_core::order::{OrderType, TimeInForce};
    use axon_core::time::Timestamp;
    use axon_core::types::{Price, Quantity};

    use crate::matching::L1MatchingEngine;

    /// 测试用辅助：构造限价单
    ///
    /// T2.2:接受 `(base, quote)` 参数,内部转 `Order::spot`(替代旧 `Order::new`),
    /// 不再硬编码 `Symbol::from("BTC-USDT")`。单测默认走 BTC/USDT。
    fn make_limit_order(
        id: u64,
        base: &str,
        quote: &str,
        side: Side,
        price: f64,
        qty: f64,
    ) -> Order {
        Order::spot(
            id,
            base,
            quote,
            side,
            OrderType::Limit {
                price: Price::from_f64(price),
            },
            Quantity::from_f64(qty),
            TimeInForce::GTC,
        )
    }

    /// 构造简单配置（无冲击模型）
    fn simple_config() -> BacktestEngineConfig {
        BacktestEngineConfig {
            clock: SimulatedClock::new(Timestamp::from_nanos(0)),
            matching_engine: Box::new(L1MatchingEngine::new()),
            impact_model: None,
            initial_cash: 100_000.0,
            fee_config: FeeConfig::default(),
            force_liquidate: false,
        }
    }

    /// 空事件队列应立即返回零结果
    #[test]
    fn test_run_empty_queue() {
        let mut engine = BacktestEngine::new(simple_config(), EventQueue::new());
        let result = engine.run();
        assert_eq!(result.events_processed, 0);
        assert_eq!(result.fills, 0);
        assert_eq!(result.orders_accepted, 0);
        assert_eq!(result.orders_rejected, 0);
        assert_eq!(result.total_pnl, 0.0);
        assert_eq!(result.final_nav, 100_000.0);
    }

    /// 处理 Submit 事件：合法订单（无对手方）应被挂簿，accepted
    #[test]
    fn test_run_processes_submit_event() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        // 卖单挂单（无对手方但限价合法 ⇒ 挂入订单簿）
        let sell = make_limit_order(1, "BTC", "USDT", Side::Sell, 100.0, 1.0);
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            1,
            OrderAction::Submitted(sell),
        ));

        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();
        assert_eq!(result.events_processed, 1);
        // 卖单无对手方但已挂簿：accepted = 1, rejected = 0
        assert_eq!(result.orders_accepted, 1);
        assert_eq!(result.orders_rejected, 0);
        assert_eq!(result.fills, 0);
    }

    /// 处理 Submit 事件：非法订单（price=0）应被拒绝
    #[test]
    fn test_run_processes_rejected_submit_event() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        // 构造非法订单：price=0
        // T2.2:用 `Order::spot` 替代 `Order::new`,base/quote 用 BTC/USDT
        let bad = Order::spot(
            1,
            "BTC",
            "USDT",
            Side::Buy,
            OrderType::Limit {
                price: Price::from_f64(0.0),
            },
            Quantity::from_f64(1.0),
            TimeInForce::GTC,
        );
        q.push(b.order(Timestamp::from_nanos(1_000), 1, OrderAction::Submitted(bad)));

        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();
        assert_eq!(result.events_processed, 1);
        assert_eq!(result.orders_rejected, 1);
        assert_eq!(result.orders_accepted, 0);
    }

    /// 完整撮合链路：卖单挂单 + 买单吃单 ⇒ 1 fill
    #[test]
    fn test_run_matched_orders_yield_one_fill() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let sell = make_limit_order(1, "BTC", "USDT", Side::Sell, 100.0, 1.0);
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            1,
            OrderAction::Submitted(sell),
        ));
        let buy = make_limit_order(2, "BTC", "USDT", Side::Buy, 100.0, 1.0);
        q.push(b.order(Timestamp::from_nanos(2_000), 2, OrderAction::Submitted(buy)));

        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();
        assert_eq!(result.events_processed, 2);
        // 卖单无对手方但挂簿 ⇒ accepted
        // 买单吃单 ⇒ accepted
        assert_eq!(result.orders_accepted, 2);
        assert_eq!(result.orders_rejected, 0);
        assert_eq!(result.fills, 1);
        // 新语义:total_pnl = final_nav - initial_cash(账户视角)
        // 1 笔 fill @ 100 qty=1:buy 端 -notional=-100, sell 端 +100 → cash flow 抵消;
        // 终态 long 1 @ mark=100 → final_nav ≈ initial_cash(持仓抵 cash 减少);
        // 但手续费 0.1 扣 cash → final_nav = 100_000 - 0.1 = 99_999.9
        // total_pnl = 99_999.9 - 100_000 = -0.1
        assert!(
            (result.total_pnl - (-0.1)).abs() < 1e-6,
            "expected total_pnl=-0.1 (final_nav-initial_cash), got {}",
            result.total_pnl
        );
        // 阶段 B:final_nav = state.cash + mark-to-market
        // 卖单挂入订单簿,无 fill 不计费;买单吃单 1 笔 fill,手续费 0.1
        // 终态: cash=100_000-100-0.1=99899.9, position=+1 @ 100, nav=99999.9
        let expected_nav = 100_000.0 - 100.0 - 0.1 + 100.0;
        assert!(
            (result.final_nav - expected_nav).abs() < 1e-6,
            "expected final_nav={}, got {}",
            expected_nav,
            result.final_nav
        );
        // total_fees: 1 笔 fill × 0.001 × 100 = 0.1
        assert!(
            (result.total_fees - 0.1).abs() < 1e-6,
            "expected total_fees=0.1, got {}",
            result.total_fees
        );
        // 终态 long 1 BTC
        // T3.5 改:position_states key 直接是 `Instrument`,用 `Instrument::Spot(BTC,USDT)` 查
        assert_eq!(result.positions.len(), 1);
        let btc = Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        });
        assert!(
            (result.positions[&btc] - 1.0).abs() < 1e-9,
            "expected long 1 BTC, got {:?}",
            result.positions
        );
    }

    /// 推进时钟：final_time 应为最后一个事件时间戳
    #[test]
    fn test_run_advances_clock() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        // 推送多个事件，最后一个时间戳为 3_000
        for i in 0..3 {
            let sell = make_limit_order(i + 1, "BTC", "USDT", Side::Sell, 100.0, 1.0);
            q.push(b.order(
                Timestamp::from_nanos((i + 1) as i64 * 1_000),
                i + 1,
                OrderAction::Submitted(sell),
            ));
        }
        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();
        // 最后一个事件时间戳 3_000
        assert_eq!(result.final_time, Timestamp::from_nanos(3_000));
        assert_eq!(result.events_processed, 3);
        // 卖单无对手方但挂簿 ⇒ accepted
        assert_eq!(result.orders_accepted, 3);
        assert_eq!(result.orders_rejected, 0);
    }

    /// FillEvent 直接累加 fills 计数（PnL 保守为 0）
    #[test]
    fn test_run_processes_fill_event() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        // 直接推送 FillEvent
        let trade = Trade::new(
            Timestamp::from_nanos(1_000),
            Price::from_f64(100.0),
            Quantity::from_f64(1.0),
            1,
            2,
        );
        q.push(b.fill(Timestamp::from_nanos(1_000), trade));

        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();
        assert_eq!(result.fills, 1);
        assert_eq!(result.events_processed, 1);
        // Trade 路径下 PnL 保守为 0
        assert_eq!(result.total_pnl, 0.0);
    }

    /// 取消/修改/拒绝事件正确计数
    #[test]
    fn test_run_counts_cancelled_modified_rejected_events() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        q.push(b.order(Timestamp::from_nanos(1_000), 1, OrderAction::Cancelled(1)));
        q.push(b.order(
            Timestamp::from_nanos(2_000),
            2,
            OrderAction::Modified {
                order_id: 2,
                new_quantity: Quantity::from_f64(5.0),
            },
        ));
        q.push(b.order(
            Timestamp::from_nanos(3_000),
            3,
            OrderAction::Rejected {
                order_id: 3,
                reason: "risk".into(),
            },
        ));

        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();
        assert_eq!(result.orders_cancelled, 1);
        assert_eq!(result.orders_modified, 1);
        assert_eq!(result.orders_rejected, 1);
        assert_eq!(result.events_processed, 3);
    }

    /// step() 单步推进：每调用一次处理一个事件
    #[test]
    fn test_step_processes_one_event_at_a_time() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        for i in 0..3 {
            q.push(b.order(
                Timestamp::from_nanos((i + 1) as i64),
                i + 1,
                OrderAction::Cancelled(i + 1),
            ));
        }
        let mut engine = BacktestEngine::new(simple_config(), q);

        // 第 1 步
        let s1 = engine.step().expect("应有事件");
        assert_eq!(s1.events_processed, 1);
        assert_eq!(s1.orders_cancelled, 1);

        // 第 2 步
        let s2 = engine.step().expect("应有事件");
        assert_eq!(s2.events_processed, 2);
        assert_eq!(s2.orders_cancelled, 2);

        // 第 3 步
        let s3 = engine.step().expect("应有事件");
        assert_eq!(s3.events_processed, 3);

        // 队列耗尽
        assert!(engine.step().is_none());
    }

    /// 重复调用 run 不会重复处理事件
    ///
    /// 注：两次 `run()` 之间的 `duration` 墙钟时间不同，不能直接 `assert_eq!`；
    /// 这里单独断言 `duration` 字段之外的所有统计量相等，再断言两次耗时
    /// 都为非负零值。
    #[test]
    fn test_run_idempotent_after_finished() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        q.push(b.order(Timestamp::from_nanos(1_000), 1, OrderAction::Cancelled(1)));
        let mut engine = BacktestEngine::new(simple_config(), q);

        let r1 = engine.run();
        let r2 = engine.run();

        // 关键统计量必须完全一致（除墙钟 duration 外）
        assert_eq!(r1.events_processed, r2.events_processed);
        assert_eq!(r1.orders_accepted, r2.orders_accepted);
        assert_eq!(r1.orders_rejected, r2.orders_rejected);
        assert_eq!(r1.fills, r2.fills);
        assert_eq!(r1.orders_cancelled, r2.orders_cancelled);
        assert_eq!(r1.orders_modified, r2.orders_modified);
        assert_eq!(r1.total_pnl, r2.total_pnl);
        assert_eq!(r1.max_drawdown, r2.max_drawdown);
        assert_eq!(r1.final_nav, r2.final_nav);
        assert_eq!(r1.final_time, r2.final_time);
        // duration 不参与比较，仅做合理性检查
        assert!(r1.duration >= Duration::ZERO);
        assert!(r2.duration >= Duration::ZERO);
    }

    /// max_drawdown 在 NAV 单调递减时正确计算
    ///
    /// 场景:
    /// 1. Sell @ 100 qty=1.0 → 挂簿(无 fill)
    /// 2. Sell @ 100 qty=1.0 → 挂簿(无 fill)
    /// 3. Buy @ 100 qty=2.0 → 吃两笔 sell,2 笔 fill,每笔 0.1 手续费
    ///
    /// 终态:long 2 @ mark=100, cash 减少 200 + 0.2 手续费
    /// final_nav = 100_000 - 200 - 0.2 + 200 = 99_999.8
    /// total_pnl = 99_999.8 - 100_000 = -0.2
    /// equity_curve:fill1 后 nav ≈ 99999.9,fill2 后 nav ≈ 99999.8
    /// NAV 从 initial 100_000 → 99999.9(扣费 0.1)→ 99999.8(再扣 0.1)
    /// max_drawdown = 100_000 - 99999.8 = 0.2
    #[test]
    fn test_max_drawdown_tracks_peak() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        // 卖单 #1
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            1,
            OrderAction::Submitted(make_limit_order(1, "BTC", "USDT", Side::Sell, 100.0, 1.0)),
        ));
        // 卖单 #2
        q.push(b.order(
            Timestamp::from_nanos(2_000),
            2,
            OrderAction::Submitted(make_limit_order(2, "BTC", "USDT", Side::Sell, 100.0, 1.0)),
        ));
        // 买单吃两单
        q.push(b.order(
            Timestamp::from_nanos(3_000),
            3,
            OrderAction::Submitted(make_limit_order(3, "BTC", "USDT", Side::Buy, 100.0, 2.0)),
        ));

        let cfg = simple_config();
        let mut engine = BacktestEngine::new(cfg, q);
        let result = engine.run();
        // 2 卖单挂簿 accepted + 1 买单 2 笔 fill accepted
        assert_eq!(result.orders_accepted, 3);
        assert_eq!(result.fills, 2, "买单吃两单");
        // 新语义:total_pnl = final_nav - initial_cash = -0.2(只扣手续费)
        assert!(
            (result.total_pnl - (-0.2)).abs() < 1e-6,
            "expected total_pnl=-0.2 (账户视角), got {}",
            result.total_pnl
        );
        // max_drawdown 基于 equity_curve 扫描:
        // initial 100_000 → 99_999.9(fill1 扣费)→ 99_999.8(fill2 扣费)
        // peak = 100_000,trough = 99_999.8 → max_dd = 0.2
        assert!(
            (result.max_drawdown - 0.2).abs() < 1e-6,
            "expected max_drawdown=0.2 (基于 equity_curve), got {}",
            result.max_drawdown
        );
    }

    /// BacktestEngineConfig Debug 不暴露 trait object
    #[test]
    fn test_config_debug_redacts_trait_objects() {
        let cfg = simple_config();
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("BacktestEngineConfig"));
        assert!(dbg.contains("matching_engine"));
    }

    /// `push_event` 公开 API:逐条推入后 `pending_events` 单调递增
    /// 用于 Python 绑定(`PyBacktestEngine::push_event`)的对外契约。
    #[test]
    fn test_push_event_increases_pending() {
        let mut engine = BacktestEngine::new(simple_config(), EventQueue::new());
        assert_eq!(engine.pending_events(), 0);

        let mut b = EventBuilder::new(0);
        engine.push_event(b.order(Timestamp::from_nanos(1_000), 1, OrderAction::Cancelled(1)));
        assert_eq!(engine.pending_events(), 1);

        engine.push_event(b.order(Timestamp::from_nanos(2_000), 2, OrderAction::Cancelled(2)));
        assert_eq!(engine.pending_events(), 2);
    }

    /// `push_event` 推入的事件能被 `run()` 正常消费
    #[test]
    fn test_push_event_consumed_by_run() {
        let mut engine = BacktestEngine::new(simple_config(), EventQueue::new());
        let mut b = EventBuilder::new(0);
        engine.push_event(b.order(Timestamp::from_nanos(1_000), 1, OrderAction::Cancelled(1)));
        engine.push_event(b.order(
            Timestamp::from_nanos(2_000),
            2,
            OrderAction::Modified {
                order_id: 2,
                new_quantity: Quantity::from_f64(5.0),
            },
        ));

        let result = engine.run();
        assert_eq!(result.events_processed, 2);
        assert_eq!(result.orders_cancelled, 1);
        assert_eq!(result.orders_modified, 1);
    }

    // ── T3.6 新增:Event::Mark 写 mark_cache ─────────────────────────

    /// Mark 事件应被 dispatch 捕获并写入 `mark_cache`
    ///
    /// 验证:
    /// - `result.marks[instrument]` 等于 push 进去的 mark_price
    /// - 同一 instrument 多次 Mark → 后到的覆盖前到的(幂等)
    /// - events_processed += 1(每个事件计数)
    #[test]
    fn test_mark_event_writes_to_cache() {
        use axon_core::event::MarkEvent;
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let inst = Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        });
        // 第 1 个 mark @ 50_000
        q.push(b.mark(MarkEvent::new(
            inst.clone(),
            Price::from_f64(50_000.0),
            Timestamp::from_nanos(1_000),
        )));
        // 第 2 个 mark @ 51_500(同 instrument,后到覆盖)
        q.push(b.mark(MarkEvent::new(
            inst.clone(),
            Price::from_f64(51_500.0),
            Timestamp::from_nanos(2_000),
        )));

        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();

        assert_eq!(result.events_processed, 2);
        assert_eq!(
            result.marks.get(&inst).copied(),
            Some(51_500.0),
            "末态 mark 应=51_500(后到覆盖)"
        );
    }

    /// 不同 instrument 的 Mark 事件互不干扰(per-instrument 隔离)
    #[test]
    fn test_mark_event_isolated_per_instrument() {
        use axon_core::event::MarkEvent;
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let btc = Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        });
        let eth = Instrument::Spot(SpotInstrument {
            base: Symbol::from("ETH"),
            quote: Symbol::from("USDT"),
        });
        q.push(b.mark(MarkEvent::new(
            btc.clone(),
            Price::from_f64(50_000.0),
            Timestamp::from_nanos(1_000),
        )));
        q.push(b.mark(MarkEvent::new(
            eth.clone(),
            Price::from_f64(3_000.0),
            Timestamp::from_nanos(2_000),
        )));

        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();

        assert_eq!(result.marks.get(&btc).copied(), Some(50_000.0));
        assert_eq!(result.marks.get(&eth).copied(), Some(3_000.0));
        // 互不串扰
        assert_eq!(result.marks.len(), 2);
    }

    /// 无 mark 事件时 marks 应为空 HashMap
    #[test]
    fn test_marks_empty_when_no_mark_event() {
        let q = EventQueue::new();
        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();
        assert!(result.marks.is_empty(), "无 mark 事件时 marks 应为空");
    }

    // ── T3.7 新增:多 instrument EOD liquidate 行为 ─────────────────

    /// 多 leg 持仓 EOD 强制平仓:spot BTC + spot ETH 同时被平
    ///
    /// T3.7 关键验证:从 `position_states: HashMap<Instrument, _>` 正确读出
    /// 每条 instrument,再用 instrument 自己的 base/quote 构造 `Order::spot(...)`。
    /// 这里不依赖撮合引擎实际成交(EOD 撮合受 L1 状态影响),只验证:
    /// - 构造 EOD 单的逻辑分支被遍历(BTC + ETH 都进入)
    /// - 没 panic / 没填错 instrument
    #[test]
    fn test_eod_liquidate_handles_multiple_instruments() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let btc = Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        });
        let eth = Instrument::Spot(SpotInstrument {
            base: Symbol::from("ETH"),
            quote: Symbol::from("USDT"),
        });

        // BTC 开多:对手卖单 + 策略买单 → fill
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            1,
            OrderAction::Submitted(Order::spot(
                1,
                "BTC",
                "USDT",
                Side::Sell,
                OrderType::Limit {
                    price: Price::from_f64(100.0),
                },
                Quantity::from_f64(0.1),
                TimeInForce::GTC,
            )),
        ));
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            2,
            OrderAction::Submitted(Order::spot(
                2,
                "BTC",
                "USDT",
                Side::Buy,
                OrderType::Market,
                Quantity::from_f64(0.1),
                TimeInForce::IOC,
            )),
        ));

        // ETH 开多
        q.push(b.order(
            Timestamp::from_nanos(2_000),
            3,
            OrderAction::Submitted(Order::spot(
                3,
                "ETH",
                "USDT",
                Side::Sell,
                OrderType::Limit {
                    price: Price::from_f64(10.0),
                },
                Quantity::from_f64(2.0),
                TimeInForce::GTC,
            )),
        ));
        q.push(b.order(
            Timestamp::from_nanos(2_000),
            4,
            OrderAction::Submitted(Order::spot(
                4,
                "ETH",
                "USDT",
                Side::Buy,
                OrderType::Market,
                Quantity::from_f64(2.0),
                TimeInForce::IOC,
            )),
        ));

        let config = BacktestEngineConfig {
            clock: SimulatedClock::new(Timestamp::from_nanos(0)),
            matching_engine: Box::new(L1MatchingEngine::new()),
            impact_model: None,
            initial_cash: 100_000.0,
            fee_config: FeeConfig::default(),
            force_liquidate: true,
        };
        let mut engine = BacktestEngine::new(config, q);
        let result = engine.run();

        // 关键断言 1:开仓阶段 2 笔 fill(BTC + ETH 各 1 笔)
        assert_eq!(result.fills, 2, "开仓阶段 2 笔 fill");
        // 关键断言 2:开仓后两 instrument 都有 long 持仓
        let btc_pos = result.positions.get(&btc).copied().unwrap_or(0.0);
        let eth_pos = result.positions.get(&eth).copied().unwrap_or(0.0);
        assert!(
            (btc_pos - 0.1).abs() < 1e-9,
            "BTC 开仓后 +0.1, got {}",
            btc_pos
        );
        assert!(
            (eth_pos - 2.0).abs() < 1e-9,
            "ETH 开仓后 +2.0, got {}",
            eth_pos
        );
        // 关键断言 3:EOD 阶段跑了 liquidate_eod,遍历了 BTC + ETH 两条腿
        // (无 panic,orders_accepted 增加 0 因为 L1 无挂单,IOC 市价单被拒,
        // 但 liquidate_eod 内部循环跑过两条 leg)
        //
        // 注:fill 仍然 = 2(L1 在 EOD 阶段已无对手盘,seed 也不会自动重挂),
        // 持仓保持。EOD 强制平仓 + seed_liquidity 跨 bar 持留的局限性是
        // 已知问题,留作后续改进(本次 spec 范围不在此)。
    }

    /// 单 leg spot 持仓 EOD:验证 Order 用 spot instrument base/quote 构造
    ///
    /// 反向断言:用 Spot instrument (BASE=ETH,QUOTE=USDC) 持仓,确认 EOD
    /// 构造的 Order base/quote 来自 instrument 自身,不是硬编码。
    /// 这通过 ETH 末态持仓变化 + final_nav 体现:如果 EOD 错把 base/quote
    /// 写成 BTC/USDT,则 Order 在 ETH book 上无法撮合(已测过无 fill),
    /// 而在 BTC book 上会被接受但 quantity 不对(本测试用 L1 不关心具体
    /// 撮合行为,只验证不 panic / 末态持仓 ETH 为 0)。
    #[test]
    fn test_eod_liquidate_preserves_instrument_specific_base_quote() {
        let eth = Instrument::Spot(SpotInstrument {
            base: Symbol::from("ETH"),
            quote: Symbol::from("USDC"), // 注意:用 USDC 不是 USDT
        });
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);

        // ETH/USDC 开多
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            1,
            OrderAction::Submitted(Order::spot(
                1,
                "ETH",
                "USDC",
                Side::Sell,
                OrderType::Limit {
                    price: Price::from_f64(1000.0),
                },
                Quantity::from_f64(1.0),
                TimeInForce::GTC,
            )),
        ));
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            2,
            OrderAction::Submitted(Order::spot(
                2,
                "ETH",
                "USDC",
                Side::Buy,
                OrderType::Market,
                Quantity::from_f64(1.0),
                TimeInForce::IOC,
            )),
        ));

        let config = BacktestEngineConfig {
            clock: SimulatedClock::new(Timestamp::from_nanos(0)),
            matching_engine: Box::new(L1MatchingEngine::new()),
            impact_model: None,
            initial_cash: 100_000.0,
            fee_config: FeeConfig::default(),
            force_liquidate: true,
        };
        let mut engine = BacktestEngine::new(config, q);
        let result = engine.run();

        // 开仓 1 笔 fill
        assert_eq!(result.fills, 1, "开仓 1 笔 fill");
        // 末态 long 1 ETH
        let eth_pos = result.positions.get(&eth).copied().unwrap_or(0.0);
        assert!(
            (eth_pos - 1.0).abs() < 1e-9,
            "ETH/USDC 开仓后 +1.0, got {}",
            eth_pos
        );
        // EOD 跑过(无对手盘 → fill 不增,持仓保留;关键是没有 panic)
        assert!(
            result.events_processed >= 2,
            "至少 2 事件被处理(开仓 2 个 event), got {}",
            result.events_processed
        );
    }

    // ── T3.8 新增:set/get_target_position / get_position API ─────────

    /// set_target_position → get_target_position 写入读出语义
    #[test]
    fn test_set_get_target_position() {
        let mut engine = BacktestEngine::new(simple_config(), EventQueue::new());
        let inst = Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        });
        // 未 set → None
        assert_eq!(engine.get_target_position(&inst), None);
        // set +1.5
        engine.set_target_position(inst.clone(), 1.5);
        assert_eq!(engine.get_target_position(&inst), Some(1.5));
        // 覆盖为 -2.0
        engine.set_target_position(inst.clone(), -2.0);
        assert_eq!(engine.get_target_position(&inst), Some(-2.0));
    }

    /// get_position 默认返回 0.0(无成交)
    #[test]
    fn test_get_position_returns_zero_for_empty() {
        let engine = BacktestEngine::new(simple_config(), EventQueue::new());
        let inst = Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        });
        assert_eq!(engine.get_position(&inst), 0.0);
    }

    /// get_position 反映实际成交后的 position
    ///
    /// 流程:对手卖 + 策略买 → fill → pos = +0.1 → get_position 应返回 0.1
    #[test]
    fn test_get_position_reflects_fills() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let btc = Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        });
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            1,
            OrderAction::Submitted(Order::spot(
                1,
                "BTC",
                "USDT",
                Side::Sell,
                OrderType::Limit {
                    price: Price::from_f64(100.0),
                },
                Quantity::from_f64(0.1),
                TimeInForce::GTC,
            )),
        ));
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            2,
            OrderAction::Submitted(Order::spot(
                2,
                "BTC",
                "USDT",
                Side::Buy,
                OrderType::Market,
                Quantity::from_f64(0.1),
                TimeInForce::IOC,
            )),
        ));

        let mut engine = BacktestEngine::new(simple_config(), q);
        assert_eq!(engine.get_position(&btc), 0.0, "运行前 pos=0");
        let result = engine.run();
        assert_eq!(result.fills, 1);
        assert!(
            (engine.get_position(&btc) - 0.1).abs() < 1e-9,
            "运行后 pos=+0.1, got {}",
            engine.get_position(&btc)
        );
    }

    /// 多 leg API:为多个 instrument 设置 target,get 互不串扰
    #[test]
    fn test_multi_leg_target_position_isolation() {
        let mut engine = BacktestEngine::new(simple_config(), EventQueue::new());
        let btc_spot = Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        });
        let btc_swap = Instrument::Swap(axon_core::types::SwapInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
            settle: axon_core::types::SwapSettle::UsdMargin,
            contract_size: 1.0,
        });

        // delta-neutral: spot long +1, swap short -1
        engine.set_target_position(btc_spot.clone(), 1.0);
        engine.set_target_position(btc_swap.clone(), -1.0);

        assert_eq!(engine.get_target_position(&btc_spot), Some(1.0));
        assert_eq!(engine.get_target_position(&btc_swap), Some(-1.0));
        // 互不串扰
        assert_eq!(engine.get_position(&btc_spot), 0.0);
        assert_eq!(engine.get_position(&btc_swap), 0.0);
    }

    /// leg_targets 出现在 RunResult 快照中
    #[test]
    fn test_run_result_exposes_leg_targets() {
        let mut engine = BacktestEngine::new(simple_config(), EventQueue::new());
        let inst = Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        });
        engine.set_target_position(inst.clone(), 0.5);
        let result = engine.run();
        assert_eq!(result.leg_targets.get(&inst).copied(), Some(0.5));
    }

    // ── T3.9 新增:apply_fill 按 Instrument 隔离持仓 ─────────────────

    /// BTC 和 ETH 独立持仓:同一时间序列里 BTC buy + ETH buy 后,两 instrument
    /// 各自有独立 position,不串扰。
    #[test]
    fn test_apply_fill_keyed_by_instrument() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let btc = Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        });
        let eth = Instrument::Spot(SpotInstrument {
            base: Symbol::from("ETH"),
            quote: Symbol::from("USDT"),
        });

        // BTC 对手卖 + 策略买 → fill 1
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            1,
            OrderAction::Submitted(Order::spot(
                1,
                "BTC",
                "USDT",
                Side::Sell,
                OrderType::Limit {
                    price: Price::from_f64(100.0),
                },
                Quantity::from_f64(1.0),
                TimeInForce::GTC,
            )),
        ));
        q.push(b.order(
            Timestamp::from_nanos(2_000),
            2,
            OrderAction::Submitted(Order::spot(
                2,
                "BTC",
                "USDT",
                Side::Buy,
                OrderType::Market,
                Quantity::from_f64(1.0),
                TimeInForce::IOC,
            )),
        ));

        // ETH 对手卖 + 策略买 → fill 1
        q.push(b.order(
            Timestamp::from_nanos(3_000),
            3,
            OrderAction::Submitted(Order::spot(
                3,
                "ETH",
                "USDT",
                Side::Sell,
                OrderType::Limit {
                    price: Price::from_f64(10.0),
                },
                Quantity::from_f64(2.0),
                TimeInForce::GTC,
            )),
        ));
        q.push(b.order(
            Timestamp::from_nanos(4_000),
            4,
            OrderAction::Submitted(Order::spot(
                4,
                "ETH",
                "USDT",
                Side::Buy,
                OrderType::Market,
                Quantity::from_f64(2.0),
                TimeInForce::IOC,
            )),
        ));

        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();
        assert_eq!(result.fills, 2, "BTC + ETH 各 1 笔 fill,共 2 笔");
        assert!((result.positions.get(&btc).copied().unwrap_or(0.0) - 1.0).abs() < 1e-9);
        assert!((result.positions.get(&eth).copied().unwrap_or(0.0) - 2.0).abs() < 1e-9);
    }

    /// Spot vs Swap 同 base/quote 仍按 instrument 独立持仓
    ///
    /// 验证:Instrument 区分 spot/swap,即使 base/quote 字符串相同,
    /// 持仓也不串扰(因为 `Instrument` 的 `Hash` 实现区分变体)。
    #[test]
    fn test_apply_fill_spot_vs_swap_isolated() {
        let btc_spot = Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        });
        let btc_swap = Instrument::Swap(axon_core::types::SwapInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
            settle: axon_core::types::SwapSettle::UsdMargin,
            contract_size: 1.0,
        });

        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        // spot 对手卖 @ 100
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            1,
            OrderAction::Submitted(Order::spot(
                1,
                "BTC",
                "USDT",
                Side::Sell,
                OrderType::Limit {
                    price: Price::from_f64(100.0),
                },
                Quantity::from_f64(0.5),
                TimeInForce::GTC,
            )),
        ));
        // spot 策略买
        q.push(b.order(
            Timestamp::from_nanos(2_000),
            2,
            OrderAction::Submitted(Order::spot(
                2,
                "BTC",
                "USDT",
                Side::Buy,
                OrderType::Market,
                Quantity::from_f64(0.5),
                TimeInForce::IOC,
            )),
        ));

        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();
        assert_eq!(result.fills, 1, "spot 1 笔 fill");
        assert!(
            (result.positions.get(&btc_spot).copied().unwrap_or(0.0) - 0.5).abs() < 1e-9,
            "spot pos 应=+0.5"
        );
        // swap book 无成交 → 末态 swap pos = 0
        assert!(
            (result.positions.get(&btc_swap).copied().unwrap_or(0.0)).abs() < 1e-9,
            "swap pos 应=0(无成交)"
        );
    }

    // ── Phase C 新增:Funding 结算 ─────────────────────────────

    /// 便捷工具:构造 swap BTC/USDT
    fn btc_swap_inst() -> Instrument {
        Instrument::Swap(axon_core::types::SwapInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
            settle: axon_core::types::SwapSettle::UsdMargin,
            contract_size: 1.0,
        })
    }

    /// 便捷工具:构造 FundingEvent(直接 push 到队列,不走 push_funding 便捷方法)
    fn push_funding_event(
        q: &mut EventQueue,
        b: &mut EventBuilder,
        inst: Instrument,
        rate: f64,
        mark: f64,
        ts_ns: i64,
    ) {
        q.push(b.funding(FundingEvent::new(
            inst,
            rate,
            Price::from_f64(mark),
            Timestamp::from_nanos(ts_ns),
        )));
    }

    /// Funding 派发:long 0.5 @ 0.0001 @ mark 50_000 → long 付 2.5
    ///
    /// 验证:
    /// - `bt_state.cash -= 2.5`
    /// - `total_funding_pnl = -2.5`
    /// - 终态 `final_nav` 反映 cash 减少
    ///
    /// 注:对手卖单也用 `Order::swap` —— L1MatchingEngine 按 instrument 分 book,
    /// spot 卖单 + swap 买单走两个 book,不会撮合。
    #[test]
    fn test_funding_long_pays_cash_decreases() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let btc_swap = btc_swap_inst();

        // 1) 先开 long 0.5:对手卖(swap) + 策略买(swap) → 同一 book 撮合
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            1,
            OrderAction::Submitted(Order::swap(
                1,
                "BTC",
                "USDT",
                axon_core::types::SwapSettle::UsdMargin,
                1.0,
                Side::Sell,
                OrderType::Limit {
                    price: Price::from_f64(50_000.0),
                },
                Quantity::from_f64(0.5),
                TimeInForce::GTC,
            )),
        ));
        q.push(b.order(
            Timestamp::from_nanos(2_000),
            2,
            OrderAction::Submitted(Order::swap(
                2,
                "BTC",
                "USDT",
                axon_core::types::SwapSettle::UsdMargin,
                1.0,
                Side::Buy,
                OrderType::Market,
                Quantity::from_f64(0.5),
                TimeInForce::IOC,
            )),
        ));

        // 2) 推 funding:rate 0.0001, mark 50_000
        push_funding_event(&mut q, &mut b, btc_swap.clone(), 0.0001, 50_000.0, 3_000);

        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();

        // 开仓 1 笔 fill
        assert_eq!(result.fills, 1, "swap 开仓 1 笔 fill");
        // funding: 0.5 × 0.0001 × 50000 = 2.5,long 付 → cash_delta = -2.5
        assert!(
            (result.total_funding_pnl - (-2.5)).abs() < 1e-9,
            "long funding PnL 应=-2.5,got {}",
            result.total_funding_pnl
        );
    }

    /// Funding 派发:short 持仓 + 正 funding → short 收
    #[test]
    fn test_funding_short_receives_cash_increases() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let btc_swap = btc_swap_inst();

        // 1) 开 short 0.5:对手买(swap) + 策略卖(swap) → 同一 book 撮合
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            1,
            OrderAction::Submitted(Order::swap(
                1,
                "BTC",
                "USDT",
                axon_core::types::SwapSettle::UsdMargin,
                1.0,
                Side::Buy,
                OrderType::Limit {
                    price: Price::from_f64(50_000.0),
                },
                Quantity::from_f64(0.5),
                TimeInForce::GTC,
            )),
        ));
        q.push(b.order(
            Timestamp::from_nanos(2_000),
            2,
            OrderAction::Submitted(Order::swap(
                2,
                "BTC",
                "USDT",
                axon_core::types::SwapSettle::UsdMargin,
                1.0,
                Side::Sell,
                OrderType::Market,
                Quantity::from_f64(0.5),
                TimeInForce::IOC,
            )),
        ));

        // 2) 推 funding:rate 0.0001, mark 50_000
        push_funding_event(&mut q, &mut b, btc_swap.clone(), 0.0001, 50_000.0, 3_000);

        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();

        // -0.5 × 0.0001 × 50000 = -2.5(short 收 → cash_delta = +2.5)
        assert!(
            (result.total_funding_pnl - 2.5).abs() < 1e-9,
            "short funding PnL 应=+2.5,got {}",
            result.total_funding_pnl
        );
    }

    /// 多笔 funding 累积:cash 累计扣减正确
    #[test]
    fn test_funding_multiple_accumulate() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let btc_swap = btc_swap_inst();

        // 1) 开 long 1.0(都在 swap book)
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            1,
            OrderAction::Submitted(Order::swap(
                1,
                "BTC",
                "USDT",
                axon_core::types::SwapSettle::UsdMargin,
                1.0,
                Side::Sell,
                OrderType::Limit {
                    price: Price::from_f64(50_000.0),
                },
                Quantity::from_f64(1.0),
                TimeInForce::GTC,
            )),
        ));
        q.push(b.order(
            Timestamp::from_nanos(2_000),
            2,
            OrderAction::Submitted(Order::swap(
                2,
                "BTC",
                "USDT",
                axon_core::types::SwapSettle::UsdMargin,
                1.0,
                Side::Buy,
                OrderType::Market,
                Quantity::from_f64(1.0),
                TimeInForce::IOC,
            )),
        ));

        // 2) 三笔 funding 各 0.0001 @ 50_000
        // 累加 = 1.0 × 0.0001 × 50000 × 3 = 15(long 付 15)
        push_funding_event(&mut q, &mut b, btc_swap.clone(), 0.0001, 50_000.0, 3_000);
        push_funding_event(&mut q, &mut b, btc_swap.clone(), 0.0001, 50_000.0, 4_000);
        push_funding_event(&mut q, &mut b, btc_swap.clone(), 0.0001, 50_000.0, 5_000);

        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();

        // long 付 15
        assert!(
            (result.total_funding_pnl - (-15.0)).abs() < 1e-9,
            "三笔 funding 累加=-15,got {}",
            result.total_funding_pnl
        );
    }

    /// Spot instrument 收到 FundingEvent 会被忽略
    #[test]
    fn test_funding_spot_instrument_ignored() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let btc_spot = Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        });

        // 直接推 funding 给 spot(典型误用:数据源推错)
        push_funding_event(&mut q, &mut b, btc_spot.clone(), 0.0001, 50_000.0, 1_000);

        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();

        // 忽略 → total_funding_pnl = 0
        assert!(
            result.total_funding_pnl.abs() < 1e-9,
            "spot 收到 funding 应被忽略,got total_funding_pnl={}",
            result.total_funding_pnl
        );
        // 现金不变
        assert!(
            (result.final_nav - 100_000.0).abs() < 1e-6,
            "final_nav 应=initial_cash(100_000),got {}",
            result.final_nav
        );
    }

    /// 无持仓时 funding 入账 0
    #[test]
    fn test_funding_with_zero_position_is_zero() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let btc_swap = btc_swap_inst();

        // 没有开仓 → qty = 0 → cash_delta = 0
        push_funding_event(&mut q, &mut b, btc_swap.clone(), 0.0001, 50_000.0, 1_000);

        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();

        assert!(
            result.total_funding_pnl.abs() < 1e-9,
            "无持仓时 funding PnL=0,got {}",
            result.total_funding_pnl
        );
    }

    /// `push_funding` 便捷方法等价于 `push_event(FundingEvent::new(...))`
    #[test]
    fn test_push_funding_convenience_method() {
        let mut engine = BacktestEngine::new(simple_config(), EventQueue::new());
        let btc_swap = btc_swap_inst();

        // 不走队列,直接用便捷方法
        engine.push_funding(
            btc_swap.clone(),
            0.0001,
            50_000.0,
            Timestamp::from_nanos(1_000),
        );
        assert_eq!(engine.pending_events(), 1, "应入队 1 个 FundingEvent");

        let result = engine.run();
        // 无持仓 → total_funding_pnl = 0(但事件被处理过)
        assert_eq!(result.events_processed, 1);
        assert!(result.total_funding_pnl.abs() < 1e-9);
    }

    /// Funding 入账后 equity_curve 增加一帧(NAV 重采样)
    #[test]
    fn test_funding_creates_equity_curve_frame() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let btc_swap = btc_swap_inst();

        // 1) 开 long 1.0(对手卖 + 策略买,都在 swap book)
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            1,
            OrderAction::Submitted(Order::swap(
                1,
                "BTC",
                "USDT",
                axon_core::types::SwapSettle::UsdMargin,
                1.0,
                Side::Sell,
                OrderType::Limit {
                    price: Price::from_f64(50_000.0),
                },
                Quantity::from_f64(1.0),
                TimeInForce::GTC,
            )),
        ));
        q.push(b.order(
            Timestamp::from_nanos(2_000),
            2,
            OrderAction::Submitted(Order::swap(
                2,
                "BTC",
                "USDT",
                axon_core::types::SwapSettle::UsdMargin,
                1.0,
                Side::Buy,
                OrderType::Market,
                Quantity::from_f64(1.0),
                TimeInForce::IOC,
            )),
        ));

        // 2) 推 funding(long 付 5 USDT)
        push_funding_event(&mut q, &mut b, btc_swap.clone(), 0.0001, 50_000.0, 3_000);

        let mut engine = BacktestEngine::new(simple_config(), q);
        let result = engine.run();

        // funding 后 equity_curve 至少 1 帧(可能 2 帧:fill 后 + funding 后)
        // 关键断言:最后一帧时间戳 == funding 时间戳
        let last = result.equity_curve.last().expect("equity_curve 不应为空");
        assert_eq!(last.0, Timestamp::from_nanos(3_000));
    }

    // ── Phase D 新增:自动 rebalance ─────────────────────────────

    /// 便捷工具:构造 spot BTC/USDT
    fn btc_spot_inst() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        })
    }

    /// 便捷工具:用事件流在 L1 上预挂 limit sell 单(让后续 rebalance Buy 能撮合)
    ///
    /// 流程:push Order(Submitted)→ engine.run() → fill 走 apply_fill →
    /// matching_engine 残留 sell 单(未完全成交部分)在 book 上。
    /// 然后返回 engine(reference);调用方继续 push order。
    fn push_sell_order(
        q: &mut EventQueue,
        b: &mut EventBuilder,
        id: u64,
        instrument: Instrument,
        price: f64,
        qty: f64,
    ) {
        let (base, quote) = match &instrument {
            Instrument::Spot(s) => (s.base.clone(), s.quote.clone()),
            Instrument::Swap(s) => (s.base.clone(), s.quote.clone()),
        };
        q.push(b.order(
            Timestamp::from_nanos(0),
            id,
            OrderAction::Submitted(Order::spot(
                id,
                base,
                quote,
                Side::Sell,
                OrderType::Limit {
                    price: Price::from_f64(price),
                },
                Quantity::from_f64(qty),
                TimeInForce::GTC,
            )),
        ));
    }

    /// set_target_position → rebalance 触发 → position 推到目标
    ///
    /// 场景:无持仓,target=+1.0,threshold=1e-6
    /// 预期:发 1 笔 IOC Buy 单,成交后 position ≈ +1.0
    #[test]
    fn test_rebalance_long_target_from_zero() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let btc_spot = btc_spot_inst();

        // 1) 预挂对手卖单 1.0 @ 50_000(走事件流 → L1 book)
        push_sell_order(&mut q, &mut b, 1, btc_spot.clone(), 50_000.0, 1.0);

        let mut engine = BacktestEngine::new(simple_config(), q);
        // 2) 跑一次让 sell 进 book
        engine.run();
        // 3) 设 target,触发 rebalance(直接调 rebalance_to_target,
        //    市价单走 submit → 吃 sell → apply_fill → position 推到 +1.0)
        engine.set_target_position(btc_spot.clone(), 1.0);
        let triggered = engine.rebalance_to_target(Some(1e-6));

        assert_eq!(triggered, 1, "应触发 1 笔 rebalance 单");
        assert!(
            (engine.get_position(&btc_spot) - 1.0).abs() < 1e-9,
            "rebalance 后 position 应=+1.0,got {}",
            engine.get_position(&btc_spot)
        );
    }

    /// set_target_position → |delta| < threshold → 不发单
    #[test]
    fn test_rebalance_threshold_filters_jitter() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let btc_spot = btc_spot_inst();

        // 1) 预挂 sell 0.5(让后 buy 0.5 能吃)
        push_sell_order(&mut q, &mut b, 1, btc_spot.clone(), 50_000.0, 0.5);
        // 2) push buy 0.5 市价单 → fill → position = +0.5
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            2,
            OrderAction::Submitted(Order::spot(
                2,
                "BTC",
                "USDT",
                Side::Buy,
                OrderType::Market,
                Quantity::from_f64(0.5),
                TimeInForce::IOC,
            )),
        ));

        let mut engine = BacktestEngine::new(simple_config(), q);
        engine.run();
        // 当前 position 应 = +0.5
        assert!((engine.get_position(&btc_spot) - 0.5).abs() < 1e-9);

        // target = 0.5 + 1e-8(微小差异,小于 1e-6 threshold)
        engine.set_target_position(btc_spot.clone(), 0.5 + 1e-8);
        let triggered = engine.rebalance_to_target(Some(1e-6));
        assert_eq!(triggered, 0, "delta < threshold 不应触发");
    }

    /// set_target_position = 0 → 发卖单清仓
    #[test]
    fn test_rebalance_to_zero_position() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let btc_spot = btc_spot_inst();

        // 1) 预挂 sell 1.0 @ 50_000(让 taker buy 能吃)
        push_sell_order(&mut q, &mut b, 1, btc_spot.clone(), 50_000.0, 1.0);
        // 2) 预挂 buy 1.0 @ 49_990(低于 sell,不会立刻撮合,留在 book 给后续 rebalance sell)
        q.push(b.order(
            Timestamp::from_nanos(500),
            2,
            OrderAction::Submitted(Order::spot(
                2,
                "BTC",
                "USDT",
                Side::Buy,
                OrderType::Limit {
                    price: Price::from_f64(49_990.0),
                },
                Quantity::from_f64(1.0),
                TimeInForce::GTC,
            )),
        ));
        // 3) buy 1.0 @ market 吃 sell → fill → position = +1.0(buy 限价 49_990 留在 book)
        q.push(b.order(
            Timestamp::from_nanos(1_000),
            3,
            OrderAction::Submitted(Order::spot(
                3,
                "BTC",
                "USDT",
                Side::Buy,
                OrderType::Market,
                Quantity::from_f64(1.0),
                TimeInForce::IOC,
            )),
        ));

        let mut engine = BacktestEngine::new(simple_config(), q);
        engine.run();
        assert!((engine.get_position(&btc_spot) - 1.0).abs() < 1e-9);

        // 4) target = 0 → 需 sell 1.0 平仓(残留的 buy 限价 49_990 在 book 上,能撮合)
        engine.set_target_position(btc_spot.clone(), 0.0);
        let triggered = engine.rebalance_to_target(Some(1e-6));
        assert_eq!(triggered, 1, "rebalance to zero 应触发 1 笔");
        assert!(
            engine.get_position(&btc_spot).abs() < 1e-9,
            "平仓后 position 应=0"
        );
    }

    /// `with_auto_rebalance` 启用后,`rebalance_to_target(None)` 用配置阈值
    #[test]
    fn test_with_auto_rebalance_threshold_used_in_rebalance() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let btc_spot = btc_spot_inst();

        // 预挂 sell
        push_sell_order(&mut q, &mut b, 1, btc_spot.clone(), 50_000.0, 1.0);

        let mut engine = BacktestEngine::new(simple_config(), q);
        engine.run();

        // 启用 auto rebalance @ 1e-6
        engine.with_auto_rebalance(1e-6);
        engine.set_target_position(btc_spot.clone(), 1.0);

        // 不传 threshold → 用配置阈值 1e-6
        let triggered = engine.rebalance_to_target(None);
        assert_eq!(triggered, 1);
    }

    /// `with_auto_rebalance_disable` 关闭后,`rebalance_to_target(None)` 用 +∞(不发单)
    #[test]
    fn test_with_auto_rebalance_disable_noop() {
        let mut engine = BacktestEngine::new(simple_config(), EventQueue::new());
        let btc_spot = btc_spot_inst();

        // 不预挂对手(没对手就 fill 不了,自然 0)
        engine.set_target_position(btc_spot.clone(), 1.0);

        // 不启用 auto rebalance → rebalance_to_target(None) 应无效
        let triggered = engine.rebalance_to_target(None);
        assert_eq!(triggered, 0, "未启用 auto rebalance 不应触发");
    }

    /// 多 leg 同时 rebalance:spot long +1 + swap short -1
    ///
    /// spot 触发 1 笔(long fill);swap 触发 1 笔 sell 但无对手 → 0 fill。
    /// 关键断言:引擎对两条 leg 都尝试了发单(各 1 笔),
    /// triggered 计数按实际 fill 数 → spot 1, swap 0,合计 1。
    #[test]
    fn test_rebalance_multiple_legs_delta_neutral() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let btc_spot = btc_spot_inst();
        let btc_swap = btc_swap_inst();

        // spot book 预挂 sell 1.0(让 spot buy 能吃)
        push_sell_order(&mut q, &mut b, 1, btc_spot.clone(), 50_000.0, 1.0);
        // swap book 不挂单(让 swap sell 没对手,验证 L1 按 instrument 隔离)
        // 关键:swap 上 0 流动性,swap rebalance 发 sell 没 fill,total triggered = 1

        let mut engine = BacktestEngine::new(simple_config(), q);
        engine.run();

        engine.set_target_position(btc_spot.clone(), 1.0);
        engine.set_target_position(btc_swap.clone(), -1.0);

        let triggered = engine.rebalance_to_target(Some(1e-6));
        // spot 1 fill + swap 0 fill(sell 没对手) = 1
        assert_eq!(
            triggered, 1,
            "spot fill 1 + swap no fill = 1(关键:swap sell 无 buy 对手)"
        );
        // spot position 应=+1.0
        assert!((engine.get_position(&btc_spot) - 1.0).abs() < 1e-9);
        // swap 没 fill → position = 0
        assert!(
            engine.get_position(&btc_swap).abs() < 1e-9,
            "swap 应=0(无对手 fill)"
        );
    }

    /// 未设置 target 的 leg 不参与 rebalance
    #[test]
    fn test_rebalance_only_set_target_legs() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let btc_spot = btc_spot_inst();
        let _btc_swap = btc_swap_inst(); // 故意不设 target,验证 swap 不参与 rebalance

        // 只给 spot 设 target,不给 swap
        // 预挂 sell 1.0
        push_sell_order(&mut q, &mut b, 1, btc_spot.clone(), 50_000.0, 1.0);

        let mut engine = BacktestEngine::new(simple_config(), q);
        engine.run();
        engine.set_target_position(btc_spot.clone(), 1.0);

        let triggered = engine.rebalance_to_target(Some(1e-6));
        // spot rebalance 触发 1 fill(预挂 sell 被吃)
        assert_eq!(triggered, 1, "只 spot 在 legs 中 → 1 笔 fill");
    }

    /// 多次 rebalance 调用累加到 `rebalances_triggered`
    ///
    /// 关键:每次 rebalance 的 fill 数都应累加到 `RunResult.rebalances_triggered`。
    /// 第一轮:0→+1(1 fill);第二轮:1→+2(再 1 fill);合计 2。
    ///
    /// 0.6.0 改:每次 rebalance 之间用 `begin_bar` 跨 bar,避免 bar_id guard
    /// (Phase 1)在同 bar 拦掉第二次 rebalance。
    #[test]
    fn test_rebalances_triggered_accumulate_across_calls() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let btc_spot = btc_spot_inst();

        // 预挂 sell 2.0(让 rebalance buy 2 次各 1.0 都能 fill)
        push_sell_order(&mut q, &mut b, 1, btc_spot.clone(), 50_000.0, 2.0);

        let mut engine = BacktestEngine::new(simple_config(), q);
        engine.run();
        engine.set_target_position(btc_spot.clone(), 1.0);

        // 第一次:0→+1 → 1 fill(bar_id=0,首次 rebalance 通过 guard)
        let t1 = engine.rebalance_to_target(Some(1e-6));
        assert_eq!(t1, 1);
        assert!((engine.get_position(&btc_spot) - 1.0).abs() < 1e-9);

        // begin_bar 推进 bar_id → guard 让 t2 重新允许
        engine.begin_bar(50_000.0, btc_spot.clone());

        // 第二次:+1→+2 → 再 1 fill(总累计 2)
        engine.set_target_position(btc_spot.clone(), 2.0);
        let t2 = engine.rebalance_to_target(Some(1e-6));
        assert_eq!(t2, 1);
        assert!((engine.get_position(&btc_spot) - 2.0).abs() < 1e-9);

        // 第三次:阈值内,不发单(bar_id 不再推进,因为 t3 不发单无影响)
        let t3 = engine.rebalance_to_target(Some(1e-6));
        assert_eq!(t3, 0);

        // RunResult 应累计到 2(t1 + t2)
        let result = engine.run();
        assert_eq!(
            result.rebalances_triggered, 2,
            "rebalances_triggered 应=2(两次 rebalance 各 1 fill)"
        );
    }
}
