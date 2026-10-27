//! 账务核心:内核侧的 `BacktestEngine` 代理
//!
//! 内核自己**不记账**——撮合、现金、持仓、保证金、资金费全部复用
//! [`BacktestEngine`]。本模块只补三样内核需要的
//! 编排状态:
//!
//! 1. 前向填充的最新价 `last_price`(某品种本帧无真实 bar 时沿用旧价);
//! 2. `effective_targets`:本轮**最终生效**的目标数量(Hold 沿用、预算缩放后),
//!    单位是绝对数量而不是权益占比;
//! 3. `liquidated_before`:上一根 bar 被强平的品种快照,挡住 Hold 复活旧目标。
//!
//! 「占比 → 数量」的换算不在这里,由 [`BacktestSink`](crate::sink::BacktestSink)
//! 在唯一换算点完成(设计文档 4.4)。

use std::collections::{BTreeMap, HashMap, HashSet};

use axon_backtest::L1MatchingEngine;
use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig, RunResult};
use axon_backtest::margin::MarginSpec;
use axon_core::portfolio::FillRecord;
use axon_core::types::{Instrument, SwapInstrument, SwapSettle, Symbol};
use axon_core::{Event, EventQueue, SimulatedClock, Timestamp};

/// 品种撮合口径(现货 / 永续)
///
/// 与 `axon-worker::backtest::InstrumentKind` 同义,但在内核侧重新定义,
/// 避免把 `axon-worker`(依赖 `axon-strategy` / `axon-llm`)拉进内核依赖图。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum InstrumentMode {
    /// 现货
    #[default]
    Spot,
    /// 永续合约(保证金结算方式 + 合约乘数)
    Swap {
        /// 结算方式(USD 保证金 / 币本位)
        settle: SwapSettle,
        /// 合约乘数
        contract_size: f64,
    },
}

impl InstrumentMode {
    /// 由交易所 symbol 字符串构造 `Instrument`
    ///
    /// 先按 `BASE/QUOTE` / `BASE-QUOTE` 解析(复用 `Instrument::from_symbol`),
    /// 再在 `Swap` 模式下把解析出的 Spot 改造成 Swap。symbol 无分隔符时
    /// `from_symbol` 退化为默认品种,此处原样返回。
    pub fn instrument(&self, symbol: &str) -> Instrument {
        let parsed = Instrument::from_symbol(&Symbol::from(symbol));
        match (*self, parsed) {
            (
                InstrumentMode::Swap {
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
}

/// 撮合种子流动性配置(回测「瞬时对手盘」语义)
///
/// 默认值与 `BacktestRunner` 的模块常量逐位一致(parity 要求):
/// `half_spread = 0.0005`、`depth_levels = 5`、`size_per_level = 100.0`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SeedLiquidity {
    /// 每层价差(相对 mid 的比例)
    pub half_spread: f64,
    /// 每侧挂单层数
    pub depth_levels: usize,
    /// 每层挂单数量
    pub size_per_level: f64,
}

impl Default for SeedLiquidity {
    fn default() -> Self {
        Self {
            half_spread: 0.0005,
            depth_levels: 5,
            size_per_level: 100.0,
        }
    }
}

/// 内核配置
///
/// 字段口径与 `axon-worker::BacktestRunner::run()` 的参数一一对应,
/// 默认值即 `BacktestRunner` 的硬编码常量(见 `backtest.rs` 模块顶部)。
#[derive(Debug, Clone)]
pub struct KernelConfig {
    /// 初始资金(所有 instrument 共享同一笔)
    pub initial_cash: f64,
    /// 手续费率(taker 单费率,与 `BacktestEngine` 的 `FeeConfig` 同义)
    ///
    /// 默认取引擎默认值,保证不传时与旧回测逐位一致;RL 训练侧用
    /// `transaction_cost` 口径时需要显式传入以对齐。
    pub fee_rate: f64,
    /// 回测结束 EOD 是否强制市价平仓(透传引擎)
    pub force_liquidate: bool,
    /// 每品种保证金规格(key = `"BTC/USDT"`);空 = 不启用保证金模拟
    pub margin_specs: HashMap<String, MarginSpec>,
    /// 品种撮合口径(决定 symbol → Instrument 的构造)
    pub instrument_mode: InstrumentMode,
    /// K 线周期(`"1m"` / `"15m"` / `"1h"`),用于推导收市时刻
    pub interval: String,
    /// 自动 rebalance 阈值(绝对数量);`BacktestRunner` 固定 0.001
    pub rebalance_threshold: f64,
    /// 撮合种子流动性
    pub seed: SeedLiquidity,
    /// 组合资金约束(调用方传 `initial_cash × leverage`);
    /// 启用保证金(`margin_specs` 非空)时强制停用,口径见 `BacktestRunner::run`
    pub capital_budget: Option<f64>,
    /// 资金费查找表(`symbol -> (纳秒时间戳 -> (rate, mark))`)
    ///
    /// 历史数据链路按交易所原始 symbol(如 `BTCUSDT`)建表,因此查找时同时
    /// 尝试 `BTC/USDT` / `BTCUSDT` / `BTC-USDT` 三种写法。
    pub funding_by_symbol: BTreeMap<String, BTreeMap<i64, (f64, f64)>>,
    /// 网格屏障的期望品种全集(回测可精确知道;为空时由 `load_frames` 推导)
    pub expected_instruments: HashSet<Instrument>,
}

impl Default for KernelConfig {
    fn default() -> Self {
        Self {
            initial_cash: 100_000.0,
            // 不在这里重复硬编码 0.001:费率口径的唯一来源是引擎的 `FeeConfig`
            fee_rate: FeeConfig::default().taker_rate,
            force_liquidate: false,
            margin_specs: HashMap::new(),
            instrument_mode: InstrumentMode::Spot,
            interval: "1m".to_string(),
            rebalance_threshold: 0.001,
            seed: SeedLiquidity::default(),
            capital_budget: None,
            funding_by_symbol: BTreeMap::new(),
            expected_instruments: HashSet::new(),
        }
    }
}

/// instrument → 账务 key(`"BASE/QUOTE"`)
///
/// 与 `axon-backtest` 内部 `instrument_key` 同一口径,保证金 / 强平快照都用它,
/// 内核必须逐字对齐否则 `liquidated_before` 永远命中不了。
pub(crate) fn symbol_key(instrument: &Instrument) -> String {
    format!(
        "{}/{}",
        instrument.base().as_str(),
        instrument.quote().as_str()
    )
}

/// 内核账务核心
///
/// 持有一个 `BacktestEngine`(L1 撮合 + `SimulatedClock` + `EventQueue`),
/// 以及编排层需要的状态映射。所有方法都是同步的,不做异步包装。
pub struct KernelCore {
    engine: BacktestEngine,
    /// 各品种最近一次真实 bar 的 close(前向填充,用于盯市与名义价值折算)
    last_price: BTreeMap<String, f64>,
    /// 已知品种(key = `"BASE/QUOTE"`),`last_price` 与目标位都要靠它回指
    instruments: BTreeMap<String, Instrument>,
    /// 本轮最终生效的目标数量(绝对数量,含预算缩放结果)
    effective_targets: BTreeMap<String, f64>,
    /// 上一根 bar 被强平的品种 key 快照
    liquidated_before: HashSet<String>,
    /// 本帧的网格时间戳(bar 起始时间,不是收市时刻)
    frame_ts: Timestamp,
}

impl KernelCore {
    /// 按配置构造账务核心
    ///
    /// 引擎的初始时钟置 0,真正的起始时间由 [`Self::set_clock`] 在每帧第一步
    /// 写入(`load_frames` 也会把时钟预置到首帧),因此空跑时 `final_time` 为 0。
    pub fn new(config: &KernelConfig) -> Self {
        let mut engine = BacktestEngine::new(
            BacktestEngineConfig {
                clock: SimulatedClock::new(Timestamp::from_nanos(0)),
                matching_engine: Box::new(L1MatchingEngine::new()),
                impact_model: None,
                initial_cash: config.initial_cash,
                fee_config: FeeConfig {
                    taker_rate: config.fee_rate,
                },
                force_liquidate: config.force_liquidate,
                margin_specs: config.margin_specs.clone(),
            },
            EventQueue::new(),
        );
        // 与 `BacktestRunner::run()` 一致的撮合配置(parity 常量)
        engine.with_auto_rebalance(config.rebalance_threshold);
        engine.with_seed_liquidity(
            config.seed.half_spread,
            config.seed.depth_levels,
            config.seed.size_per_level,
        );
        Self {
            engine,
            last_price: BTreeMap::new(),
            instruments: BTreeMap::new(),
            effective_targets: BTreeMap::new(),
            liquidated_before: HashSet::new(),
            frame_ts: Timestamp::from_nanos(0),
        }
    }

    /// 把引擎时钟与内核帧时间同时推进到 `ts`
    ///
    /// `ts` 是**网格时间戳(bar 起始时间)**,与 `BacktestRunner` 的
    /// `engine.set_clock(grid_ts)` 逐字对齐;`sample_bar_nav` 的打点也用它,
    /// 因此这里不能传收市时刻。
    pub fn set_clock(&mut self, ts: Timestamp) {
        self.frame_ts = ts;
        self.engine.set_clock(ts);
    }

    /// 本帧的网格时间戳
    pub fn frame_ts(&self) -> Timestamp {
        self.frame_ts
    }

    /// 账户当前权益(NAV)
    ///
    /// 口径 = `BacktestEngine::nav(frame_ts)` = 现金 + 逐仓保证金 + 持仓按本 bar
    /// mid 的市值 / 浮盈亏。这是只读查询,不推进任何账务状态。
    pub fn equity(&self) -> f64 {
        self.engine.nav(self.frame_ts)
    }

    /// 当前持仓快照(只含非零仓位,正多负空)
    ///
    /// 引擎不暴露整张 `position_states`,这里按已知品种逐个查询拼出快照,
    /// 开销 O(品种数),每 bar 一次可忽略。
    pub fn positions(&self) -> HashMap<Instrument, f64> {
        self.instruments
            .values()
            .filter_map(|inst| {
                let qty = self.engine.get_position(inst);
                (qty.abs() > 1e-12).then(|| (inst.clone(), qty))
            })
            .collect()
    }

    /// 某品种的当前持仓(无持仓返回 0)
    pub fn get_position(&self, instrument: &Instrument) -> f64 {
        self.engine.get_position(instrument)
    }

    /// 成交明细累计条数（paper 运行期增量读取的游标锚点）
    pub fn fills_len(&self) -> usize {
        self.engine.fills_len()
    }

    /// 自游标 `cursor` 起的成交明细（append-only、不消费，语义见引擎同名方法）
    pub fn fills_since(&self, cursor: usize) -> &[FillRecord] {
        self.engine.fills_since(cursor)
    }

    /// 更新某品种的最新价(前向填充的写入点)
    pub fn update_last_price(&mut self, instrument: &Instrument, price: f64) {
        let key = symbol_key(instrument);
        self.instruments.insert(key.clone(), instrument.clone());
        self.last_price.insert(key, price);
    }

    /// 某品种的最新价
    pub fn last_price(&self, instrument: &Instrument) -> Option<f64> {
        self.last_price.get(&symbol_key(instrument)).copied()
    }

    /// 按账务 key 查最新价(预算缩放 / 资金费兜底用)
    pub fn last_price_by_key(&self, key: &str) -> Option<f64> {
        self.last_price.get(key).copied()
    }

    /// 全部已知品种的最新价(`Instrument -> price`,供 `HostCtx` 回填)
    pub fn last_price_map(&self) -> HashMap<Instrument, f64> {
        self.instruments
            .iter()
            .filter_map(|(key, inst)| self.last_price.get(key).map(|p| (inst.clone(), *p)))
            .collect()
    }

    /// `begin_bar_multi` 的 legs:所有已有最新价的品种
    ///
    /// 尚未出现真实 bar(未上市 / 数据缺口)的品种不参与 seed 与盯市,与
    /// `BacktestRunner` 的 `legs` 过滤条件一致。
    pub fn legs(&self) -> Vec<(Instrument, f64)> {
        self.instruments
            .iter()
            .filter_map(|(key, inst)| self.last_price.get(key).map(|p| (inst.clone(), *p)))
            .collect()
    }

    /// 快照引擎最近一次保证金检查中被强平的品种
    ///
    /// 引擎只保留「最近一次 `check_margin`」的结果,所以必须在
    /// `begin_bar_multi` 的自动 rebalance 之前取,否则本 bar 的强平信息会被
    /// 上一 bar 的清空覆盖。
    pub fn snapshot_liquidated(&mut self) {
        self.liquidated_before = self.engine.liquidated_symbols().clone();
    }

    /// 上一根 bar 被强平的品种 key 集合
    pub fn liquidated_before(&self) -> &HashSet<String> {
        &self.liquidated_before
    }

    /// bar 开始:seed 流动性 + 自动 rebalance + funding 调度 + NAV 采样
    ///
    /// 全部复用引擎实现;`sample_bar_nav` 在引擎内部完成,内核不要再调一次。
    pub fn begin_bar_multi(&mut self) {
        let legs = self.legs();
        self.engine.begin_bar_multi(legs);
    }

    /// 登记某品种本轮生效的目标数量(由 sink 的换算点写入)
    pub fn set_effective_target(&mut self, instrument: &Instrument, quantity: f64) {
        let key = symbol_key(instrument);
        self.instruments.insert(key.clone(), instrument.clone());
        self.effective_targets.insert(key, quantity);
    }

    /// 本轮生效目标数量(`"BASE/QUOTE" -> qty`)
    pub fn effective_targets(&self) -> &BTreeMap<String, f64> {
        &self.effective_targets
    }

    /// 按比例缩放全部目标数量(组合资金约束)
    pub fn scale_effective_targets(&mut self, scale: f64) {
        for qty in self.effective_targets.values_mut() {
            *qty *= scale;
        }
    }

    /// Hold 决策的目标解析
    ///
    /// - 未命中 `liquidated_before`:什么都不做,沿用上一轮生效目标;
    /// - 命中(刚被强平):目标清零,避免 Hold 把旧目标原样写回去复活仓位。
    pub fn resolve_hold(&mut self, instrument: &Instrument) {
        let key = symbol_key(instrument);
        if self.liquidated_before.contains(&key) {
            self.set_effective_target(instrument, 0.0);
        }
    }

    /// 把本轮全部生效目标写入引擎并触发一次 rebalance
    ///
    /// `set_target_position` 只登记意图,必须再调 `rebalance_to_target(None)`
    /// 才真正撮合。bar_id guard 会让同 bar 的第二次 rebalance 变成 no-op,
    /// 这是「目标下一根 bar 生效」时序的一部分,不要绕过。
    pub fn apply_effective_targets(&mut self) {
        let targets: Vec<(Instrument, f64)> = self
            .effective_targets
            .iter()
            .filter_map(|(key, qty)| self.instruments.get(key).map(|inst| (inst.clone(), *qty)))
            .collect();
        for (instrument, qty) in targets {
            self.engine.set_target_position(instrument, qty);
        }
        self.engine.rebalance_to_target(None);
    }

    /// 推入资金费结算事件
    pub fn push_funding(
        &mut self,
        instrument: Instrument,
        rate: f64,
        mark_price: f64,
        ts: Timestamp,
    ) {
        self.engine.push_funding(instrument, rate, mark_price, ts);
    }

    /// 保证金检查与强平(未启用保证金时引擎侧 no-op)
    pub fn check_margin(&mut self) {
        self.engine.check_margin();
    }

    /// 引擎内部队列还剩多少事件
    pub fn pending_events(&self) -> usize {
        self.engine.pending_events()
    }

    /// 单步处理引擎队列中的一个事件
    pub fn step(&mut self) {
        let _ = self.engine.step();
    }

    /// 排空引擎队列(与 `BacktestRunner` 的 `while pending > 0 { step }` 同义)
    pub fn drain(&mut self) {
        while self.pending_events() > 0 {
            self.step();
        }
    }

    /// 推入事件(订单 / 成交 / mark / funding 的账务入口)
    pub fn push_event(&mut self, event: Event) {
        self.engine.push_event(event);
    }

    /// 收尾:消费残余事件、EOD 强平、构造 `RunResult`
    ///
    /// 只由 `Kernel::finalize` 调用一次;`finished` 只由引擎的 `run()` 置位,
    /// 因此重复调用是幂等的。
    pub fn run(&mut self) -> RunResult {
        self.engine.run()
    }

    /// 测试专用:直接注入强平快照(真实强平路径的构造成本过高)
    #[cfg(test)]
    pub(crate) fn force_liquidated_for_test(&mut self, keys: HashSet<String>) {
        self.liquidated_before = keys;
    }
}
