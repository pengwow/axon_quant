//! 实盘驱动：统一内核 × 真实交易所网关的接线层
//!
//! 装配状态机：`new`（构校验）→ `connect`（WS 握手 + 订阅）→ `start`（全新起跑）
//! 或 `resume`（journal 重演 + 对账守卫）→ 外层循环调 [`LiveDriver::tick`]
//! → `shutdown`（撤单 + 禁用下单）。
//!
//! # tick 节奏
//!
//! 外层（QuantCell 的 Python 线程）按固定周期（建议 1s）调 [`LiveDriver::tick`]。
//! 注意回报有「落定落后一拍」的语义：WS 消息在 tick 尾部 `poll_external`
//! 时才被拉取处理，其产生的回流事件要等下一 tick 的 `step()` 才被内核消化。
//! 行情断流期间的缺口由泵记录，tick 中段用 REST 网关拉历史 bar 补灌
//! （见 `backfill_gap`，与 `paper_driver` 同构）。
//!
//! # 实时数据通路（v1 无后台任务）
//!
//! 本模块不 spawn 任何任务，WS 消息全靠拉取：tick 尾部
//! `Kernel::poll_external` → `LiveSink::poll` → `ExchangeGateway::poll_updates`
//! （同步 try_recv）。行情类消息转 fanout 供下一 tick 的 pump 排空，
//! 回报类消息在 sink 内走 OMS 簿记并 push_batch 回流事件。
//!
//! # 边界（v1）
//!
//! - 仅支持永续合约（`InstrumentMode::Swap`）+ 钱包口径（`AccountingMode::Wallet`），
//!   现货请走原 StrategyLoop；
//! - 风控默认 `LiveSink` 的 `AllowAllRisk`；`with_risk` 配置后 start 装配
//!   `EngineRiskCheck`（B3），resume 恢复路径不注入；
//! - 行情 fanout 通道满时新行情被丢弃（容量 4096，正常 tick 节奏不会触顶）；
//! - 多品种共享一个内核一个账本，不同品种收市 bar 乱序会 `Err`——v1 以单品种为主；
//! - 交易所成交流水审计只翻最近 1000 笔（`TRADES_PAGE_LIMIT`），更长的停机窗口
//!   依赖 `resume_guard` 对账差异告警兜底。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use tokio::sync::mpsc;

use axon_core::Instrument;
use axon_exchange::ExchangeAdapter;
use axon_exchange::types::{Side as ExchangeSide, Symbol as ExchangeSymbol};
use axon_kernel::{
    InstrumentMode, Journal, Kernel, KernelConfig, KernelError, ManifestPosition, RunManifest,
    StrategyHost, interval_to_nanos,
};
use axon_oms::{AccountingMode, Position as OmsPosition};
use axon_risk::RiskConfig;

use crate::backtest::instrument_key_for;
use crate::exchange_gateway::ExchangeGateway;
use crate::live_sink::LiveSink;
use crate::pump::{Gap, KernelPump};
use crate::reconcile::{
    LocalSnapshot, ReconcileConfig, ReconcileReport, VenueSnapshot, reconcile, resume_guard,
};
use crate::risk_check::{EngineRiskCheck, RiskHandle};

/// 行情 fan-out 通道容量：满即丢新增行情（tick 节奏下不会触顶）
const FANOUT_CHANNEL_CAPACITY: usize = 4096;
/// ponytail: 精确审计需游标全量拉取，v1 只翻最近 1000 笔，超长停机靠 resume_guard 告警兜底
const TRADES_PAGE_LIMIT: u32 = 1000;
/// 单次缺口补灌的最大拉取根数（`fetch_ohlcv` 无 since 参数、只能取最近 N 根）
const MAX_GAP_FETCH: usize = 1000;

/// start 阶段对交易所已有持仓的策略
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExistingPositions {
    /// freqtrade 式恢复精神（默认）：把残留持仓抄进影子账本带仓起跑，
    /// 并随 `RunManifest` 持久化，resume 重演时种回同一初始持仓
    #[default]
    Adopt,
    /// 保守选项：残留持仓即拒绝起跑（强制干净起点）
    Reject,
}

/// 统一内核的实盘驱动器
///
/// 持有 pump（行情 → 事件）、可选内核（connect 后 start/resume 才建立）
/// 与网关（start/resume 时 move 进 `LiveSink`）。
pub struct LiveDriver {
    /// 内核配置（构造时校验必须 `InstrumentMode::Swap`）
    config: KernelConfig,
    /// 影子保证金：钱包口径下的记账起始现金（USDT）
    shadow_cash: Decimal,
    /// 记账口径（构造时校验必须 `AccountingMode::Wallet`）
    accounting: AccountingMode,
    /// 订阅品种
    symbols: Vec<ExchangeSymbol>,
    /// 交易所网关；`start` / `resume` 成功后 move 进内救 sink，
    /// `connect` 前后均保留在此，便于错误路径放回重试
    gateway: Option<ExchangeGateway>,
    /// 行情泵（fanout 接收端在构造时接好）
    pump: KernelPump,
    /// instrument → 交易所 symbol 反查表，缺口补灌按 `gap.instrument` 找 fetch 入参
    by_instrument: Vec<(Instrument, ExchangeSymbol)>,
    /// REST 专用网关（断线缺口补灌）：与 sink 内主网关共享 adapter，但不取
    /// `market_data_rx` 单消费者通道，也从不 poll
    rest_gateway: ExchangeGateway,
    /// 统一内核；`tick` / `snapshot` / `shutdown` 要求已启动
    kernel: Option<Kernel>,
    /// B3 风控：start 时注入 LiveSink 的配置（None = 默认 AllowAllRisk，
    /// 行为同接线前）；`f64` 是单品种仓名义额上限，`None` 表示不做名义额检查
    risk: Option<(RiskConfig, Option<f64>)>,
    /// start 成功后的引擎句柄，上层经它回灌当日盈亏（联动熔断）/ 跨日清零
    risk_handle: Option<RiskHandle>,
    /// 起跑时对交易所残留持仓的策略（Adopt 接管 / Reject 拒绝）
    existing_positions: ExistingPositions,
}

impl LiveDriver {
    /// 构造驱动器：校验品种口径与记账模式，接好行情 fan-out
    ///
    /// v1 仅支持永续 + 钱包口径，其余配置直接报错（现货请走原 StrategyLoop）。
    pub fn new(
        adapter: Arc<tokio::sync::Mutex<Box<dyn ExchangeAdapter>>>,
        config: KernelConfig,
        accounting: AccountingMode,
        shadow_cash: f64,
        symbols: Vec<String>,
        existing_positions: ExistingPositions,
    ) -> Result<Self, String> {
        if !matches!(config.instrument_mode, InstrumentMode::Swap { .. }) {
            return Err(
                "LiveDriver v1 仅支持永续合约（Swap），现货请走原 StrategyLoop".to_string(),
            );
        }
        if !matches!(accounting, AccountingMode::Wallet) {
            return Err("LiveDriver v1 仅支持钱包口径（Wallet）记账".to_string());
        }
        if symbols.is_empty() {
            return Err("至少需要一个交易品种".to_string());
        }
        // 行情订阅按 config.interval 参数化（见 BinanceAdapter::with_kline_interval），
        // 非法周期串会被 Binance 静默忽略（订阅成功、零推送），构造期拒绝
        if interval_to_nanos(&config.interval).is_none() {
            return Err(format!(
                "不支持的 interval 周期串（只认小写 s/m/h/d/w 后缀，如 \"1m\"/\"15m\"/\"1h\"）: {}",
                config.interval
            ));
        }
        let shadow_cash = Decimal::from_f64(shadow_cash)
            .ok_or_else(|| format!("影子保证金不是有效数字: {shadow_cash}"))?;
        // instrument → 交易所 symbol 反查表（缺口补灌按 gap.instrument 反查）
        let by_instrument = symbols
            .iter()
            .map(|sym| {
                let instrument = config.instrument_mode.instrument(&instrument_key_for(sym));
                (instrument, ExchangeSymbol(sym.clone()))
            })
            .collect();
        let symbols = symbols.into_iter().map(ExchangeSymbol).collect();

        // fan-out 接收端交给 pump（按构造口径解析品种），发送端交给网关；
        // 另备一个 REST 专用网关，只做断线缺口的历史 bar 补拉
        let (fanout_tx, fanout_rx) = mpsc::channel(FANOUT_CHANNEL_CAPACITY);
        let gateway = ExchangeGateway::shared(adapter.clone())?.with_market_data_fanout(fanout_tx);
        let rest_gateway = ExchangeGateway::shared_rest_only(adapter)?;
        let pump = KernelPump::with_instrument_mode(fanout_rx, config.instrument_mode);
        Ok(Self {
            config,
            shadow_cash,
            accounting,
            symbols,
            by_instrument,
            gateway: Some(gateway),
            rest_gateway,
            pump,
            kernel: None,
            risk: None,
            risk_handle: None,
            existing_positions,
        })
    }

    /// 注入 pre-trade 风控（B3）：start 时装配进 LiveSink
    ///
    /// `max_position_value` 是单品种目标净仓的金额上限（金额口径；None = 不查）。
    /// 不调用此方法时 LiveSink 用默认 `AllowAllRisk`，行为同接线前。
    /// 注意 resume 恢复路径不装配风控（恢复会话优先对账放行）。
    pub fn with_risk(mut self, config: RiskConfig, max_position_value: Option<f64>) -> Self {
        self.set_risk(config, max_position_value);
        self
    }

    /// 非消耗版装配（pyo3 绑定层复用）：driver 已在锁里，无法走 consuming builder
    pub fn set_risk(&mut self, config: RiskConfig, max_position_value: Option<f64>) {
        self.risk = Some((config, max_position_value));
    }

    /// start 成功后的风控引擎句柄（未注入风控时为 None）
    pub fn risk_handle(&self) -> Option<&RiskHandle> {
        self.risk_handle.as_ref()
    }

    /// 连接交易所并订阅品种（幂等安全：失败时网关放回可重试）
    pub fn connect(&mut self) -> Result<(), String> {
        let gateway = self
            .gateway
            .take()
            .ok_or_else(|| "驱动器网关缺失".to_string())?;
        let mut gateway = gateway;
        let outcome = gateway
            .connect_sync()
            .and_then(|()| gateway.subscribe(&self.symbols));
        // 成败都放回：调用方对失败可整体重试
        self.gateway = Some(gateway);
        outcome
    }

    /// 全新起跑：建内核、挂 journal、注册策略宿主
    ///
    /// start 前先做交易所侧体检（base 都要求 connect 已完成，走同一 REST 通道）：
    /// - 影子基线钉到真实钱包余额（`total_balance - unrealized_pnl`，与 resume
    ///   对账的 venue 口径一致）。拉取失败 fail-fast 放回网关可重试——影子基线
    ///   拒绝回退兜底值，基线错位正是重启后对账假阳性的根源；
    /// - 账户已有持仓时的处理由 [`ExistingPositions`] 决定：默认 `Adopt` 把残留持仓
    ///   抄进影子账本带仓起跑（随 manifest 持久化，resume 重演种回同一份）；
    ///   `Reject` 保留干净起点守卫（带仓跑会让 resume 对账必然断裂）。
    ///
    /// journal 建立失败属启动期硬故障（kernel 连 sink 一起 drop，网关随之释放），
    /// 需重建 LiveDriver 重 connect。
    pub fn start(
        &mut self,
        journal_path: &Path,
        hosts: Vec<Box<dyn StrategyHost>>,
    ) -> Result<(), String> {
        if self.kernel.is_some() {
            return Err("内核已启动，不可重复 start".to_string());
        }
        if hosts.is_empty() {
            return Err("至少需要一个策略宿主".to_string());
        }
        let gateway = self
            .gateway
            .take()
            .ok_or_else(|| "驱动器网关缺失，请先 connect".to_string())?;
        // 影子基线 = 交易所真实钱包余额；initial_cash 只是调用方的预算语义，
        // 直接当基线会让「影子账本起点 ≠ 交易所钱包」，resume 对账第一天就验不过
        let shadow_cash = match venue_cash(&gateway) {
            Ok(cash) => cash,
            Err(e) => {
                // 网关放回：重试走完整的 connect → start
                self.gateway = Some(gateway);
                return Err(format!(
                    "start 前拉取交易所钱包余额失败，拒绝以兜底值建立影子基线: {e}"
                ));
            }
        };
        // 带仓起跑策略：Reject 走干净起点守卫；Adopt（默认）把交易所残留持仓
        // 抄进影子账本。采集与解析都在 sink 构造（gateway move 进 sink）之前完成，
        // 失败时网关放回可整体重试
        let adopted = match self.existing_positions {
            ExistingPositions::Adopt => collect_positions(&gateway)
                .and_then(|collected| seed_positions(&collected).map(|oms| (collected, oms))),
            ExistingPositions::Reject => {
                check_no_positions(&gateway).map(|()| (Vec::new(), Vec::new()))
            }
        };
        let (manifest_positions, oms_positions) = match adopted {
            Ok(positions) => positions,
            Err(e) => {
                // 网关放回：重试走完整的 connect → start
                self.gateway = Some(gateway);
                return Err(format!("start 前交易所持仓体检失败: {e}"));
            }
        };
        let sink = LiveSink::new(Box::new(gateway), shadow_cash)
            .with_accounting_mode(self.accounting)
            .with_initial_positions(oms_positions);
        // B3：有风控配置则装配 EngineRiskCheck 并留出句柄供 pnl 回灌；
        // 无配置保持 AllowAllRisk（opt-in，行为不变）
        let sink = if let Some((risk_config, max_position_value)) = self.risk.take() {
            let (check, handle) = EngineRiskCheck::new(risk_config, max_position_value);
            self.risk_handle = Some(handle);
            sink.with_risk(Box::new(check))
        } else {
            sink
        };
        let mut kernel = Kernel::new(self.config.clone(), Box::new(sink));
        // 实际生效的影子基线随 manifest 落 journal 首行，resume 读回同一值：
        // 重演起点必须等于首跑起点，兜底值只在旧 journal 缺字段时凑数
        let manifest = RunManifest {
            shadow_cash: Some(shadow_cash.to_string()),
            initial_positions: (!manifest_positions.is_empty()).then_some(manifest_positions),
            ..RunManifest::default()
        };
        kernel.set_manifest(manifest);
        for host in hosts {
            kernel.add_host(host);
        }
        // journal 是崩溃恢复的根基，建立失败直接 fail-fast
        if let Err(e) = kernel.attach_journal(journal_path) {
            return Err(format!(
                "journal 建立失败（属启动期硬故障，需重建 LiveDriver 重 connect）: {e}"
            ));
        }
        self.kernel = Some(kernel);
        Ok(())
    }

    /// 崩溃恢复：重演 journal → 本地快照 → 交易所对账 → 守卫放行
    ///
    /// 守卫拒绝（持仓/资金断裂）时网关放回本结构，人工核对后可整体重试。
    pub fn resume(
        &mut self,
        journal_path: &Path,
        hosts: Vec<Box<dyn StrategyHost>>,
    ) -> Result<ReconcileReport, String> {
        if self.kernel.is_some() {
            return Err("内核已启动，不可重复 resume".to_string());
        }
        if hosts.is_empty() {
            return Err("至少需要一个策略宿主".to_string());
        }
        let mut gateway = self
            .gateway
            .take()
            .ok_or_else(|| "驱动器网关缺失，请先 connect".to_string())?;
        match Self::resume_attempt(
            &mut gateway,
            journal_path,
            hosts,
            &self.config,
            self.shadow_cash,
            self.accounting,
            &self.symbols,
        ) {
            Ok((mut kernel, report)) => {
                // Ok 之后无失败点：把真实网关挂回 sink，供后续下单与 poll_updates
                let sink = kernel
                    .sink_mut()
                    .as_any_mut()
                    .downcast_mut::<LiveSink>()
                    .expect("resume_attempt 构造的 sink 必为 LiveSink");
                sink.set_gateway(Box::new(gateway));
                self.kernel = Some(kernel);
                Ok(report)
            }
            Err(e) => {
                // 对账失败/守卫拒绝时放回网关，人工介入后可整体重试
                self.gateway = Some(gateway);
                Err(e)
            }
        }
    }

    /// resume 七步：预读 journal → ReplaySink → 重演 → 本地快照 → 交易所体检
    /// → 对账 → 守卫放行
    fn resume_attempt(
        gateway: &mut ExchangeGateway,
        journal_path: &Path,
        hosts: Vec<Box<dyn StrategyHost>>,
        config: &KernelConfig,
        shadow_cash: Decimal,
        accounting: AccountingMode,
        symbols: &[ExchangeSymbol],
    ) -> Result<(Kernel, ReconcileReport), String> {
        // ① 预读 journal：恢复 manifest 供 set_manifest
        let (manifest, _entries) =
            Journal::open(journal_path).map_err(|e| format!("journal 预读失败: {e}"))?;
        // ①' 重演基线：start 已把首跑实际生效的影子基线（交易所钱包余额）落进
        //     manifest 首行，优先取它；旧 journal 缺字段回退调用方传入值（行为
        //     不变）。字段非法 = 文件被手改，直接拒绝不猜。
        let replay_cash = match &manifest.shadow_cash {
            Some(raw) => raw
                .parse::<Decimal>()
                .map_err(|_| format!("journal manifest 的影子基线不是合法十进制数: {raw}"))?,
            None => shadow_cash,
        };
        // ②' adopt 接管的初始持仓：首跑已随 manifest 落 journal 首行，重演必须
        //     种回同一份，才能维持「影子持仓 = 初始持仓 + 成交效果 = 交易所持仓」；
        //     旧 journal 缺字段读回 None（行为不变），字段非法 = 文件被手改，拒绝不猜
        let replay_positions = match &manifest.initial_positions {
            Some(list) => seed_positions(list)?,
            None => Vec::new(),
        };
        // ② 重演期用假网关的 sink（避免重演的 place_order 打真实交易所）
        let sink = LiveSink::for_replay(replay_cash)
            .with_accounting_mode(accounting)
            .with_initial_positions(replay_positions);
        // ③ 建内核并重演
        let mut kernel = Kernel::new(config.clone(), Box::new(sink));
        kernel.set_manifest(manifest);
        for host in hosts {
            kernel.add_host(host);
        }
        kernel
            .resume_replay(journal_path)
            .map_err(|e| format!("journal 重演失败: {e}"))?;
        // ④ 本地快照（重演后 sink 内的账面）
        let local = Self::local_snapshot(&kernel)?;
        // ④' 从重演出的 OMS 历史重建网关「累计→增量」状态：last_filled 只在
        //     内存里，重启即空；不从重演恢复的话，恢复后同一订单的下一笔
        //     回报会以 0 为基准算出虚增的增量，把已恢复的成交重复记账。
        let replayed = kernel
            .sink()
            .as_any()
            .downcast_ref::<LiveSink>()
            .expect("LiveDriver 的 sink 必为 LiveSink");
        gateway.restore_last_filled(&replayed.oms().snapshot().order_history);
        // ⑤ 交易所体检（此时 gateway 未 move，失败可整体重试）
        let venue = fetch_venue_snapshot(gateway, symbols)?;
        // ⑥⑦ 对账 + 守卫
        let report = reconcile(&local, &venue, &ReconcileConfig::default());
        resume_guard(&report)
            .map_err(|e| format!("恢复守卫拒绝继续交易，请人工核对持仓与资金后重试 resume: {e}"))?;
        Ok((kernel, report))
    }

    /// 推进一拍：泵行情 → 断线补齐 → 步进 → 尾部拉 WS 消息
    ///
    /// 顺序依据数据通路：WS 消息只在尾部 `poll_external` 被拉取
    /// （行情转 fanout 供下一 tick、回报即时簿记并回流），
    /// 回流的消化发生在下一 tick 的 `step()` 循环里。
    pub fn tick(&mut self) -> Result<(), String> {
        let kernel = self
            .kernel
            .as_mut()
            .ok_or_else(|| "内核未启动，请先 start/resume".to_string())?;
        // 1. 泵走 fanout 里攒的行情，转成外部事件入队
        let batch = self.pump.drain();
        for event in batch.events {
            kernel.enqueue_external(event, None).map_err(kerr)?;
        }
        // 2. 断线补齐：行情断流期间的缺口由泵记录，REST 拉回历史 bar 灌进
        //    同一条管道（EventQueue 按收市时刻插到正确历史位置）
        for gap in self.pump.gaps().to_vec() {
            backfill_gap(
                &mut self.pump,
                &self.rest_gateway,
                kernel,
                &self.by_instrument,
                &gap,
            )?;
        }
        // 3. 步进消化（含上一 tick 回流的回报事件）
        while kernel.step().map_err(kerr)? {}
        // 4. 尾部拉取：WS 回报入 OMS 簿记 + 行情转 fanout
        kernel.poll_external().map_err(kerr)
    }

    /// 本地账面快照（钱包口径）
    pub fn snapshot(&self) -> Result<LocalSnapshot, String> {
        let kernel = self
            .kernel
            .as_ref()
            .ok_or_else(|| "内核未启动，请先 start/resume".to_string())?;
        Self::local_snapshot(kernel)
    }

    fn local_snapshot(kernel: &Kernel) -> Result<LocalSnapshot, String> {
        let sink = kernel
            .sink()
            .as_any()
            .downcast_ref::<LiveSink>()
            .expect("LiveDriver 的 sink 必为 LiveSink");
        Ok(LocalSnapshot::from_sink(sink))
    }

    /// 收尾：撤单并禁用后续下单（内核保留可继续 snapshot 审计）
    pub fn shutdown(&mut self) -> Result<(), String> {
        let kernel = self
            .kernel
            .as_mut()
            .ok_or_else(|| "内核未启动，请先 start/resume".to_string())?;
        let sink = kernel
            .sink_mut()
            .as_any_mut()
            .downcast_mut::<LiveSink>()
            .expect("LiveDriver 的 sink 必为 LiveSink");
        sink.shutdown()
    }
}

/// `KernelError` → `String` 的统一映射
fn kerr(e: KernelError) -> String {
    e.to_string()
}

/// 把一个缺口的最新历史 bar 拉回来灌进内核（`tick` 第 2 步的落点）
///
/// 自由函数而非 `LiveDriver` 方法：调用点同时借出 kernel / pump /
/// rest_gateway / by_instrument 四个字段，self 方法会立刻撞借用检查。
/// 与 `paper_driver::backfill_gap` 同构，差别只在 REST 走专用网关
/// （下单主网关已 move 进 sink，无法从此处借出）。品种不在反查表里
/// 只告警跳过——缺口理论不该来自未声明品种，兜底不炸 tick。
fn backfill_gap(
    pump: &mut KernelPump,
    gateway: &ExchangeGateway,
    kernel: &mut Kernel,
    by_instrument: &[(Instrument, ExchangeSymbol)],
    gap: &Gap,
) -> Result<(), String> {
    let Some((_, symbol)) = by_instrument
        .iter()
        .find(|(instrument, _)| *instrument == gap.instrument)
    else {
        tracing::warn!(instrument = ?gap.instrument, "kernel live: 缺口品种不在声明表里，跳过补灌");
        return Ok(());
    };
    let Some(interval_ns) = interval_to_nanos(&gap.interval) else {
        return Ok(());
    };
    // ponytail: naive heuristic — fetch_ohlcv 只能拉「最近 N 根」（无 since 参数），
    // limit 按缺失根数 +2 根余量；缺口跨度超 MAX_GAP_FETCH 时只覆盖最近一段，
    // 剩余缺口保留待后续 tick 继续补。升级路径 = fetch_ohlcv 加 since 按区间拉
    let missing = (((gap.end.nanos - gap.start.nanos) / interval_ns).clamp(1, MAX_GAP_FETCH as i64)
        as usize)
        + 2;
    let bars = gateway.fetch_ohlcv(symbol, &gap.interval, missing)?;
    if bars.is_empty() {
        return Ok(());
    }
    let batch = pump.replay_bars(symbol, &gap.interval, &bars);
    for event in batch.events {
        kernel.enqueue_external(event, None).map_err(kerr)?;
    }
    Ok(())
}

/// 拉取交易所钱包口径现金（`total_balance - unrealized_pnl`）
///
/// 与 `fetch_venue_snapshot` 的 cash 口径一致，start 用它钉影子基线
/// （Decimal 全程无 f64 往返，`to_string` 落 manifest 保精度）。
fn venue_cash(gateway: &ExchangeGateway) -> Result<Decimal, String> {
    let account = gateway.account_info()?;
    Ok(account.total_balance - account.unrealized_pnl)
}

/// 干净起点守卫：start 时交易所不应有任何持仓
///
/// 过滤口径与 `fetch_venue_snapshot` 一致（近零视为空仓），残留持仓进入
/// 报错清单供人工核对——影子 OMS 从零记仓，带仓起跑必然埋下对账断裂。
fn check_no_positions(gateway: &ExchangeGateway) -> Result<(), String> {
    let leftovers: Vec<String> = gateway
        .positions()?
        .iter()
        .filter(|p| p.quantity.abs().to_f64().unwrap_or(0.0) > f64::EPSILON)
        .map(|p| {
            let side = match p.side {
                ExchangeSide::Buy => "long",
                ExchangeSide::Sell => "short",
            };
            format!("{} {} {}", p.symbol.0, side, p.quantity)
        })
        .collect();
    if leftovers.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "start 要求干净起点，但交易所已有持仓（影子账本从零记仓，带仓起跑会让 resume 对账必然失败，请先人工处理持仓）: {leftovers:?}"
        ))
    }
}

/// 读取交易所残留持仓，折算成 adopt 接管的 manifest 单元
///
/// Buy 为正 / Sell 为负、同品种互抵取净额、近零过滤——口径与
/// `fetch_venue_snapshot` 严格一致（对账恒等式靠这个一致性成立）。
/// ponytail: 双向持仓互抵后均价取最后一腿，属启发式；单向真实持仓（常态）
/// 该值即交易所 avg_entry_price，精确。
fn collect_positions(gateway: &ExchangeGateway) -> Result<Vec<ManifestPosition>, String> {
    // key → (带符号净额, 均价)
    let mut net: BTreeMap<String, (Decimal, Decimal)> = BTreeMap::new();
    for position in gateway.positions()? {
        let signed = match position.side {
            ExchangeSide::Buy => position.quantity,
            ExchangeSide::Sell => -position.quantity,
        };
        let key = instrument_key_for(&position.symbol.0);
        let entry = net
            .entry(key)
            .or_insert((Decimal::ZERO, position.avg_entry_price));
        entry.0 += signed;
        entry.1 = position.avg_entry_price;
    }
    Ok(net
        .into_iter()
        .filter(|(_, (quantity, _))| quantity.abs().to_f64().unwrap_or(0.0) > f64::EPSILON)
        .map(|(symbol, (quantity, avg_price))| ManifestPosition {
            symbol,
            quantity: quantity.to_string(),
            avg_price: avg_price.to_string(),
        })
        .collect())
}

/// manifest 持仓单元 → OMS 持仓（String 解析 Decimal，不经 f64 往返）
///
/// 同一转换在 start 首跑与 resume 重演两个路径复用，保证两边账本口径一致。
fn seed_positions(manifest_positions: &[ManifestPosition]) -> Result<Vec<OmsPosition>, String> {
    manifest_positions
        .iter()
        .map(|p| {
            let quantity = p.quantity.parse::<Decimal>().map_err(|_| {
                format!(
                    "manifest 持仓数量不是合法十进制数: {} {}",
                    p.symbol, p.quantity
                )
            })?;
            let avg_price = p.avg_price.parse::<Decimal>().map_err(|_| {
                format!(
                    "manifest 持仓均价不是合法十进制数: {} {}",
                    p.symbol, p.avg_price
                )
            })?;
            Ok(OmsPosition {
                symbol: p.symbol.clone(),
                quantity,
                avg_price,
                realized_pnl: Decimal::ZERO,
                updated_at: Utc::now(),
            })
        })
        .collect()
}

/// 拉取交易所侧快照：带符号持仓 + 钱包现金 + 成交流水 id 集合
///
/// 口径与 `LocalSnapshot::from_sink` 严格对齐（|q| 过滤、BASE/QUOTE 键、
/// 现金扣除未实现盈亏），否则对账必然假阳性。
fn fetch_venue_snapshot(
    gateway: &ExchangeGateway,
    symbols: &[ExchangeSymbol],
) -> Result<VenueSnapshot, String> {
    // 持仓：Buy 为正、Sell 为负，同品种双向互抵后取净额
    let mut positions: BTreeMap<String, f64> = BTreeMap::new();
    for position in gateway.positions()? {
        let signed = match position.side {
            ExchangeSide::Buy => position.quantity,
            ExchangeSide::Sell => -position.quantity,
        };
        let key = instrument_key_for(&position.symbol.0);
        *positions.entry(key).or_insert(0.0) += signed.to_f64().unwrap_or(0.0);
    }
    // 与 LocalSnapshot 同样的近零过滤，避免 round-trip 残差干扰对账
    positions.retain(|_, quantity| quantity.abs() > f64::EPSILON);

    // 现金：total_balance 含未实现盈亏，钱包口径要扣回去
    let account = gateway.account_info()?;
    let cash = (account.total_balance - account.unrealized_pnl)
        .to_f64()
        .ok_or_else(|| "交易所现金快照不是有效数字".to_string())?;

    // 成交流水 id：重启窗口内疑似丢账的成交，交给 reconcile 按 fill_id 对齐
    let mut fill_ids = BTreeSet::new();
    for symbol in symbols {
        for trade in gateway.fetch_my_trades(symbol, None, TRADES_PAGE_LIMIT)? {
            fill_ids.insert(trade.trade_id);
        }
    }
    Ok(VenueSnapshot {
        positions,
        cash,
        fill_ids,
        // 交易所网关暂无 fetch_open_orders 接口，venue 侧在挂单填空集合：
        // 空集合不会产生幽灵单误报（resume_guard 只拦 ghost），本地在挂单会以
        // 孤儿单身份进报告供人工核对（与 python 侧 snapshot_from_dict 缺省语义一致）
        open_order_ids: BTreeSet::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use axon_core::types::SwapSettle;
    use axon_exchange::types::{
        ExchangeId, Kline, OrderId as ExchangeOrderId, OrderStatus as ExchangeOrderStatus,
        OrderUpdate as ExchangeOrderUpdate, Position, UserTrade, WsMessage,
    };
    use axon_kernel::{BarClosedEvent, HostCtx, OrderIntent};
    use axon_oms::Side as OmsSide;
    use chrono::{DateTime, Utc};

    use crate::test_support::{MockHandle, mock_exchange};

    const START: i64 = 1_700_000_000_000_000_000;
    const ONE_MIN: i64 = 60_000_000_000;
    const SYMBOL: &str = "BTCUSDT";
    /// 周期目标仓位比例序列：建仓 → 加仓 → 清零
    const CYCLE_RATIOS: [f64; 3] = [0.3, 0.6, 0.0];

    fn live_config() -> KernelConfig {
        KernelConfig {
            initial_cash: 10_000.0,
            interval: "1m".into(),
            instrument_mode: InstrumentMode::Swap {
                settle: SwapSettle::UsdMargin,
                contract_size: 1.0,
            },
            ..KernelConfig::default()
        }
    }

    /// 收市 K 线消息（pump 契约：timestamp 为 bar 起始时间）
    fn kline_closed(idx: i64) -> WsMessage {
        WsMessage::Kline(Kline {
            symbol: ExchangeSymbol(SYMBOL.into()),
            interval: "1m".into(),
            open: Decimal::from(100),
            high: Decimal::from(101),
            low: Decimal::from(99),
            close: Decimal::from(100),
            volume: Decimal::from(10),
            timestamp: DateTime::from_timestamp_nanos(START + idx * ONE_MIN),
            is_closed: true,
        })
    }

    /// 第 idx 根 bar 的收市后时刻（回报时间戳用）
    fn bar_close_ts(idx: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_nanos(START + (idx + 1) * ONE_MIN + ONE_MIN / 2)
    }

    /// 推两拍：第一拍尾部 poll 才把 WS 消息拉进来，第二拍消化
    fn two_ticks(driver: &mut LiveDriver) {
        driver.tick().expect("tick 应成功");
        driver.tick().expect("tick 应成功");
    }

    /// 交替加/减仓的测试宿主（蓝本：crash_recovery.rs）
    struct CycleHost {
        idx: usize,
    }

    impl CycleHost {
        fn new() -> Self {
            Self { idx: 0 }
        }
    }

    impl StrategyHost for CycleHost {
        fn name(&self) -> &str {
            "cycle"
        }

        fn on_event(&mut self, ev: &BarClosedEvent<'_>, _ctx: &HostCtx) -> Vec<OrderIntent> {
            let ratio = CYCLE_RATIOS[self.idx.min(CYCLE_RATIOS.len() - 1)];
            self.idx += 1;
            vec![OrderIntent::SetTarget {
                instrument: ev.instrument.clone(),
                target_ratio: ratio,
            }]
        }
    }

    /// 从 OMS 拿最近一笔订单，伪造交易所 WS 成交回报 + 同步 mock 真相
    fn drive_fill(driver: &mut LiveDriver, handle: &MockHandle, trade_id: &str, bar_idx: i64) {
        let kernel = driver.kernel.as_ref().expect("内核已启动");
        let sink = kernel
            .sink()
            .as_any()
            .downcast_ref::<LiveSink>()
            .expect("sink 必为 LiveSink");
        let record = sink
            .oms()
            .snapshot()
            .order_history
            .last()
            .expect("已下单")
            .clone();
        let timestamp = bar_close_ts(bar_idx);
        let (side, quantity) = match record.order.side {
            OmsSide::Buy => (ExchangeSide::Buy, record.order.quantity),
            OmsSide::Sell => (ExchangeSide::Sell, record.order.quantity),
        };
        handle.push_message(WsMessage::OrderUpdate(ExchangeOrderUpdate {
            order_id: "venue-1".into(),
            // OMS 与 exchange 的 OrderId 同形状（.0 都是 Uuid），直接搬运
            client_order_id: ExchangeOrderId(record.order.id.0),
            status: ExchangeOrderStatus::Filled {
                filled_qty: quantity,
                avg_price: Decimal::from(100),
            },
            filled_qty: quantity,
            avg_price: Some(Decimal::from(100)),
            fee: Decimal::from(1),
            // WS 回报与 REST 流水用同一个 trade id：对账按 fill_id 对齐
            trade_id: Some(trade_id.into()),
            timestamp,
        }));

        // 同步 mock 的交易所真相（钱包口径只扣手续费）
        let mut state = handle.state();
        state.account.total_balance -= Decimal::from(1);
        state.trades.push(UserTrade {
            trade_id: trade_id.into(),
            symbol: SYMBOL.into(),
            order_id: "venue-1".into(),
            side,
            price: Decimal::from(100),
            qty: quantity,
            quote_qty: quantity * Decimal::from(100),
            commission: Decimal::from(1),
            commission_asset: "USDT".into(),
            realized_pnl: Decimal::ZERO,
            time_ms: timestamp.timestamp_millis(),
        });

        // 单向持仓：现有净额 + 本笔带符号数量，归零即清空
        let signed = match side {
            ExchangeSide::Buy => quantity,
            ExchangeSide::Sell => -quantity,
        };
        let existing: Decimal = state
            .positions
            .iter()
            .filter(|p| p.symbol.0 == SYMBOL)
            .map(|p| match p.side {
                ExchangeSide::Buy => p.quantity,
                ExchangeSide::Sell => -p.quantity,
            })
            .sum();
        state.positions.retain(|p| p.symbol.0 != SYMBOL);
        let net = existing + signed;
        // 钱包口径下各仓合并（测试里同一品种只有一笔净持仓）
        if net != Decimal::ZERO {
            state.positions.push(Position {
                symbol: ExchangeSymbol(SYMBOL.into()),
                side: if net > Decimal::ZERO {
                    ExchangeSide::Buy
                } else {
                    ExchangeSide::Sell
                },
                quantity: net.abs(),
                avg_entry_price: Decimal::from(100),
                unrealized_pnl: Decimal::ZERO,
            });
        }
    }

    /// 合成交易所侧快照（网关已 move 进内核，从 mock 状态直接按
    /// `fetch_venue_snapshot` 同口径合成）
    fn manual_venue(handle: &MockHandle) -> VenueSnapshot {
        let state = handle.state();
        let mut positions: BTreeMap<String, f64> = BTreeMap::new();
        for position in &state.positions {
            let signed = match position.side {
                ExchangeSide::Buy => position.quantity,
                ExchangeSide::Sell => -position.quantity,
            };
            *positions
                .entry(instrument_key_for(&position.symbol.0))
                .or_insert(0.0) += signed.to_f64().unwrap_or(0.0);
        }
        positions.retain(|_, q| q.abs() > f64::EPSILON);
        VenueSnapshot {
            positions,
            cash: (state.account.total_balance - state.account.unrealized_pnl)
                .to_f64()
                .unwrap_or(0.0),
            fill_ids: state.trades.iter().map(|t| t.trade_id.clone()).collect(),
            open_order_ids: BTreeSet::new(),
        }
    }

    /// T1：起跑 → 行情 → 下单 → 成交 → 对账 clean
    #[test]
    fn live_start_roundtrip_reconciles() {
        let (adapter, handle) = mock_exchange(ExchangeId::Binance);
        let mut driver = LiveDriver::new(
            adapter,
            live_config(),
            AccountingMode::Wallet,
            10_000.0,
            vec![SYMBOL.into()],
            ExistingPositions::default(),
        )
        .expect("构造应成功");

        let dir = tempfile::tempdir().expect("tempdir");
        let journal = dir.path().join("live.journal");
        driver.connect().expect("connect");
        driver
            .start(&journal, vec![Box::new(CycleHost::new())])
            .expect("start");

        // bar0（0.3 目标）→ 成交回报
        handle.push_message(kline_closed(0));
        two_ticks(&mut driver);
        drive_fill(&mut driver, &handle, "t-0", 0);
        two_ticks(&mut driver);

        // 钱包口径：建仓不动现金、只扣 fee；目标 0.3×10000/100 = 30 张
        let local = driver.snapshot().expect("snapshot");
        assert!((local.cash - 9_999.0).abs() < 1e-6, "cash = {}", local.cash);
        assert_eq!(local.fill_ids.len(), 1);
        let position = local.positions.values().next().expect("应持多头");
        assert!((position - 30.0).abs() < 1e-9, "position = {position}");

        // 本地对账：终态一致即 clean
        let venue = manual_venue(&handle);
        let report = reconcile(&local, &venue, &ReconcileConfig::default());
        assert!(report.is_clean(), "对账应 clean: {report:?}");
        resume_guard(&report).expect("守卫应放行");

        driver.shutdown().expect("shutdown");
    }

    /// T2：kill -9 → resume 重演对账 → 网关接线 → 续跑到清零
    #[test]
    fn resume_replays_reconciles_and_wires_gateway() {
        // ── 首跑：bar0 建仓 + 成交，然后硬崩 ──
        let (adapter_a, handle_a) = mock_exchange(ExchangeId::Binance);
        let mut driver = LiveDriver::new(
            adapter_a,
            live_config(),
            AccountingMode::Wallet,
            10_000.0,
            vec![SYMBOL.into()],
            ExistingPositions::default(),
        )
        .expect("构造应成功");
        let dir = tempfile::tempdir().expect("tempdir");
        let journal = dir.path().join("live.journal");
        driver.connect().expect("connect");
        driver
            .start(&journal, vec![Box::new(CycleHost::new())])
            .expect("start");
        handle_a.push_message(kline_closed(0));
        two_ticks(&mut driver);
        drive_fill(&mut driver, &handle_a, "t-0", 0);
        two_ticks(&mut driver);
        // 模拟 kill -9：同步线程，plain drop 即崩溃（无 spawn 任务要泄漏）
        drop(driver);

        // ── 新建 mock：market_data_rx 只能 take 一次，必须换新 adapter ──
        let (adapter_b, handle_b) = mock_exchange(ExchangeId::Binance);
        {
            // 搬运交易所持久状态（订单簿记忆不搬：sent_orders 从零开始）
            let src = handle_a.state();
            let mut dst = handle_b.state();
            dst.positions = src.positions.clone();
            dst.account = src.account.clone();
            dst.trades = src.trades.clone();
        }

        // ── 恢复：重演 + 对账 + 守卫 ──
        let mut driver = LiveDriver::new(
            adapter_b.clone(),
            live_config(),
            AccountingMode::Wallet,
            10_000.0,
            vec![SYMBOL.into()],
            ExistingPositions::default(),
        )
        .expect("构造应成功");
        driver.connect().expect("connect");
        let report = driver
            .resume(&journal, vec![Box::new(CycleHost::new())])
            .expect("resume 应守卫放行");
        assert!(report.is_clean(), "恢复对账应 clean: {report:?}");

        // ── 续跑：bar1 加仓 0.6 → bar2 清零 ──
        handle_b.push_message(kline_closed(1));
        two_ticks(&mut driver);
        drive_fill(&mut driver, &handle_b, "t-1", 1);
        handle_b.push_message(kline_closed(2));
        two_ticks(&mut driver);
        drive_fill(&mut driver, &handle_b, "t-2", 2);
        two_ticks(&mut driver);

        // 终态：三笔各扣 1手续费；清零后持仓归零（round-trip 残差容忍）
        let local = driver.snapshot().expect("snapshot");
        assert!((local.cash - 9_997.0).abs() < 1e-6, "cash = {}", local.cash);
        assert!(
            local.positions.values().all(|q| q.abs() < 1e-9),
            "持仓应已清零: {local:?}"
        );
        let venue = manual_venue(&handle_b);
        let report = reconcile(&local, &venue, &ReconcileConfig::default());
        assert!(report.is_clean(), "终态对账应 clean: {report:?}");

        // bar0 重演走 ReplayGateway 不打 mock，bar1/bar2 走真实网关
        assert_eq!(handle_b.state().sent_orders.len(), 2);
        driver.shutdown().expect("shutdown");
    }

    /// T3：start 基线自动钉到交易所钱包余额（≠ initial_cash 兜底值），且
    /// resume 从 journal manifest 读回同一基线——兜底值错也不影响对账放行
    #[test]
    fn start_pins_shadow_baseline_to_venue_cash() {
        let (adapter, handle) = mock_exchange(ExchangeId::Binance);
        // 交易所真实余额 4881.9，明显 ≠ 构造传入的兜底 10_000
        handle.state().account.total_balance = Decimal::new(48819, 1);
        let mut driver = LiveDriver::new(
            adapter,
            live_config(),
            AccountingMode::Wallet,
            10_000.0,
            vec![SYMBOL.into()],
            ExistingPositions::default(),
        )
        .expect("构造应成功");
        let dir = tempfile::tempdir().expect("tempdir");
        let journal = dir.path().join("live.journal");
        driver.connect().expect("connect");
        driver
            .start(&journal, vec![Box::new(CycleHost::new())])
            .expect("start");

        // 未成交时影子现金 = 交易所钱包余额，而非 initial_cash
        let local = driver.snapshot().expect("snapshot");
        assert!((local.cash - 4881.9).abs() < 1e-6, "cash = {}", local.cash);

        // 一笔成交只扣手续费，与交易所真相同步
        handle.push_message(kline_closed(0));
        two_ticks(&mut driver);
        drive_fill(&mut driver, &handle, "t-0", 0);
        two_ticks(&mut driver);
        let local = driver.snapshot().expect("snapshot");
        assert!((local.cash - 4880.9).abs() < 1e-6, "cash = {}", local.cash);

        // 硬崩 → 新驱动（兜底基线仍是错的 10_000）→ resume 从 manifest 首行
        // 读回首跑基线 4881.9 → 对账 clean；若按兜底值重演必然 cash_break
        drop(driver);
        let (adapter_b, handle_b) = mock_exchange(ExchangeId::Binance);
        {
            let src = handle.state();
            let mut dst = handle_b.state();
            dst.positions = src.positions.clone();
            dst.account = src.account.clone();
            dst.trades = src.trades.clone();
        }
        let mut driver = LiveDriver::new(
            adapter_b,
            live_config(),
            AccountingMode::Wallet,
            10_000.0,
            vec![SYMBOL.into()],
            ExistingPositions::default(),
        )
        .expect("构造应成功");
        driver.connect().expect("connect");
        let report = driver
            .resume(&journal, vec![Box::new(CycleHost::new())])
            .expect("resume 应守卫放行");
        assert!(report.is_clean(), "恢复对账应 clean: {report:?}");
        driver.shutdown().expect("shutdown");
    }

    /// T4：start 前交易所已有持仓 → 干净起点守卫拒绝，网关放回后清仓可整体重试
    #[test]
    fn start_rejects_leftover_positions() {
        let (adapter, handle) = mock_exchange(ExchangeId::Binance);
        handle.state().positions.push(Position {
            symbol: ExchangeSymbol(SYMBOL.into()),
            side: ExchangeSide::Buy,
            quantity: Decimal::from(30),
            avg_entry_price: Decimal::from(100),
            unrealized_pnl: Decimal::ZERO,
        });
        let mut driver = LiveDriver::new(
            adapter,
            live_config(),
            AccountingMode::Wallet,
            10_000.0,
            vec![SYMBOL.into()],
            ExistingPositions::Reject,
        )
        .expect("构造应成功");
        let dir = tempfile::tempdir().expect("tempdir");
        let journal = dir.path().join("live.journal");
        driver.connect().expect("connect");
        let err = driver
            .start(&journal, vec![Box::new(CycleHost::new())])
            .expect_err("带仓起跑应被拒绝");
        assert!(err.contains("干净起点"), "报错应说明原因: {err}");

        // 清掉持仓后整体重试成功（网关确实放回了）
        handle.state().positions.clear();
        driver
            .start(&journal, vec![Box::new(CycleHost::new())])
            .expect("清仓后重试应成功");
        driver.shutdown().expect("shutdown");
    }

    /// T5：adopt 模式起跑 → 接管交易所残留持仓进影子账本 → manifest 落盘 → 对账 clean
    #[test]
    fn start_adopt_leftover_positions() {
        let (adapter, handle) = mock_exchange(ExchangeId::Binance);
        handle.state().positions.push(Position {
            symbol: ExchangeSymbol(SYMBOL.into()),
            side: ExchangeSide::Buy,
            quantity: Decimal::from(30),
            avg_entry_price: Decimal::from(100),
            unrealized_pnl: Decimal::ZERO,
        });
        let mut driver = LiveDriver::new(
            adapter,
            live_config(),
            AccountingMode::Wallet,
            10_000.0,
            vec![SYMBOL.into()],
            ExistingPositions::Adopt,
        )
        .expect("构造应成功");
        let dir = tempfile::tempdir().expect("tempdir");
        let journal = dir.path().join("live.journal");
        driver.connect().expect("connect");
        driver
            .start(&journal, vec![Box::new(CycleHost::new())])
            .expect("adopt 应允许带仓起跑");

        // 影子账本已接管持仓（无成交不动现金）
        let local = driver.snapshot().expect("snapshot");
        let position = local.positions.values().next().expect("应接管到持仓");
        assert!((position - 30.0).abs() < 1e-9, "position = {position}");
        let venue = manual_venue(&handle);
        let report = reconcile(&local, &venue, &ReconcileConfig::default());
        assert!(report.is_clean(), "带仓起点对账应 clean: {report:?}");
        resume_guard(&report).expect("守卫应放行");

        // manifest 首行应记录初始持仓（resume 播种的数据源）
        let (manifest, _) = Journal::open(&journal).expect("journal 可读");
        let initial = manifest.initial_positions.expect("应记录初始持仓");
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].symbol, "BTC/USDT");
        assert_eq!(initial[0].quantity, "30");
        assert_eq!(initial[0].avg_price, "100");

        driver.shutdown().expect("shutdown");
    }

    /// T6：adopt 带仓起跑 → 策略加仓成交 → kill -9 → resume 从 manifest 播种
    /// 初始持仓 + 重演成交 → 对账 clean 放行（核心等式：初始 30 + 成交 30 = 交易所 60）
    #[test]
    fn resume_after_adopt_seeds_initial_positions() {
        // ── 首跑：mock 残留 30 张多头 → adopt 起跑 → bar1 加仓成交 → 硬崩 ──
        let (adapter_a, handle_a) = mock_exchange(ExchangeId::Binance);
        handle_a.state().positions.push(Position {
            symbol: ExchangeSymbol(SYMBOL.into()),
            side: ExchangeSide::Buy,
            quantity: Decimal::from(30),
            avg_entry_price: Decimal::from(100),
            unrealized_pnl: Decimal::ZERO,
        });
        let mut driver = LiveDriver::new(
            adapter_a,
            live_config(),
            AccountingMode::Wallet,
            10_000.0,
            vec![SYMBOL.into()],
            ExistingPositions::Adopt,
        )
        .expect("构造应成功");
        let dir = tempfile::tempdir().expect("tempdir");
        let journal = dir.path().join("live.journal");
        driver.connect().expect("connect");
        driver
            .start(&journal, vec![Box::new(CycleHost::new())])
            .expect("adopt 起跑应成功");
        // bar0（target 0.3 = 30 张）与接管持仓一致 → 不应产生任何订单，
        // 这条断言同时守护「seed 持仓参与 SetTarget 差额计算」这个前提
        handle_a.push_message(kline_closed(0));
        two_ticks(&mut driver);
        assert!(
            handle_a.state().sent_orders.is_empty(),
            "bar0 目标与接管持仓一致，不应下单"
        );
        // bar1（target 0.6 = 60 张）→ 差额 +30 买单 → 成交
        handle_a.push_message(kline_closed(1));
        two_ticks(&mut driver);
        drive_fill(&mut driver, &handle_a, "t-1", 1);
        two_ticks(&mut driver);
        let local = driver.snapshot().expect("snapshot");
        let position = local.positions.values().next().expect("应持 60 张");
        assert!((position - 60.0).abs() < 1e-9, "position = {position}");
        assert!((local.cash - 9_999.0).abs() < 1e-6, "cash = {}", local.cash);
        // 模拟 kill -9：同步线程，plain drop 即崩溃
        drop(driver);

        // ── 新建 mock 搬运状态 → resume：重演 manifest 播种 30 + journal 成交 +30 ──
        let (adapter_b, handle_b) = mock_exchange(ExchangeId::Binance);
        {
            let src = handle_a.state();
            let mut dst = handle_b.state();
            dst.positions = src.positions.clone();
            dst.account = src.account.clone();
            dst.trades = src.trades.clone();
        }
        let mut driver = LiveDriver::new(
            adapter_b,
            live_config(),
            AccountingMode::Wallet,
            10_000.0,
            vec![SYMBOL.into()],
            ExistingPositions::Adopt,
        )
        .expect("构造应成功");
        driver.connect().expect("connect");
        let report = driver
            .resume(&journal, vec![Box::new(CycleHost::new())])
            .expect("resume 应守卫放行");
        assert!(report.is_clean(), "恢复对账应 clean: {report:?}");
        let local = driver.snapshot().expect("snapshot");
        let position = local.positions.values().next().expect("重演后应持 60 张");
        assert!((position - 60.0).abs() < 1e-9, "position = {position}");
        assert!((local.cash - 9_999.0).abs() < 1e-6, "cash = {}", local.cash);
        driver.shutdown().expect("shutdown");
    }
}
