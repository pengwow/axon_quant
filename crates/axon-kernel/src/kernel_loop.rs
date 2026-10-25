//! 唯一主回路
//!
//! 回测 / paper / live 共用同一段循环，没有 mode 分支：差异只在装配时的
//! 数据泵与 sink 实现（设计文档 4.3）。模块名用 `kernel_loop`，因为 `loop`
//! 是 Rust 关键字。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use axon_backtest::engine::RunResult;
use axon_core::{
    Event, EventQueue, Instrument, MarketDataEvent, MarketDataPayload, SystemAction, SystemEvent,
    Timestamp,
};

use crate::KernelError;
use crate::core::{KernelConfig, KernelCore};
use crate::event::{close_time, interval_to_nanos};
use crate::host::{HostCtx, StrategyHost};
use crate::journal::{Journal, JournalEntry};
use crate::lifecycle::BarLifecycle;
use crate::persist::{JOURNAL_FORMAT_VERSION, RunManifest};
use crate::pump::{BarGridBarrier, GridFrame};
use crate::sink::ExecutionSink;

/// 一次运行的完整结果
///
/// `run` 是账务引擎的聚合结果（PnL / 成交 / NAV / 回撤 / 持仓），字段口径与
/// `axon_worker::BacktestResult` 一致；`duration` 由内核自己的墙钟起点重算
/// （引擎 `run()` 内部的 `started.elapsed()` 只覆盖收尾瞬间）。
#[derive(Debug, Clone)]
pub struct KernelResult {
    /// 账务引擎的完整运行结果
    pub run: RunResult,
    /// 整跑墙钟耗时（内核起点 → finalize）
    pub duration: Duration,
    /// 已处理的网格帧数
    pub bars_processed: u64,
    /// 提交给 sink 的意图总数（不是成交数）
    pub orders_submitted: u64,
}

/// 交易内核
pub struct Kernel {
    config: KernelConfig,
    queue: EventQueue,
    core: KernelCore,
    barrier: BarGridBarrier,
    hosts: Vec<Box<dyn StrategyHost>>,
    sink: Box<dyn ExecutionSink>,
    journal: Journal,
    lifecycle: BarLifecycle,
    manifest: RunManifest,
    /// 每帧逐品种 state 预注册表(key = 网格时间戳纳秒)
    ///
    /// state 不进 `MarketDataPayload`(市场数据契约),屏障攒齐后由 [`Self::dispatch`]
    /// 按 `(帧时间戳, 品种)` 回填到 `BarClosedEvent.state`。
    frame_states: BTreeMap<i64, HashMap<Instrument, serde_json::Value>>,
    ctx: HostCtx,
    started: Instant,
    next_seq: u64,
    /// 重演进行中：`enqueue_external` / `drain_external` 由此抑制 journal 写入
    replaying: bool,
}

impl Kernel {
    /// 用给定配置与执行出口创建内核
    ///
    /// 内核自己建队列与账务核心；调用方只需装配 Host 与 sink。
    pub fn new(config: KernelConfig, sink: Box<dyn ExecutionSink>) -> Self {
        let barrier = BarGridBarrier::new(config.expected_instruments.clone());
        let core = KernelCore::new(&config);
        Self {
            config,
            queue: EventQueue::new(),
            core,
            barrier,
            hosts: Vec::new(),
            sink,
            journal: Journal::new(),
            lifecycle: BarLifecycle::new(),
            manifest: RunManifest::default(),
            frame_states: Default::default(),
            ctx: HostCtx::default(),
            started: Instant::now(),
            next_seq: 0,
            replaying: false,
        }
    }

    /// 注册一个策略 Host（多个 Host 按注册顺序依次决策）
    pub fn add_host(&mut self, host: Box<dyn StrategyHost>) {
        self.hosts.push(host);
    }

    /// 构造本内核时固化的配置（只读）
    ///
    /// 内核配置在 [`Self::new`] 时就固定，之后的装配（网格帧的品种类型、资金费表、
    /// 预算）都必须与它一致。`axon-worker` 的 shadow 双跑需要读
    /// [`KernelConfig::instrument_mode`] 才能把同一份网格展平成帧，因此提供本访问器。
    pub fn config(&self) -> &KernelConfig {
        &self.config
    }

    /// 设置本次运行的重演凭证
    pub fn set_manifest(&mut self, manifest: RunManifest) {
        self.manifest = manifest;
    }

    /// 当前重演凭证
    pub fn manifest(&self) -> &RunManifest {
        &self.manifest
    }

    /// 事件日志
    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    /// 把 journal 落盘到 `path`（截断并写入首行 manifest）
    ///
    /// 必须在 [`Self::set_manifest`] 之后调用：首行记的是调用时刻的 manifest，
    /// 之后再改 manifest 会让文件头与内核指纹不一致，重演必然被拒绝。
    pub fn attach_journal(&mut self, path: &Path) -> Result<(), KernelError> {
        // 格式版本以本次实际写出的为准，同步回内存 manifest：
        // 否则调用方传入旧版本时，文件头（v1）与内核指纹（v0）不一致，
        // 重演会因逐字段比对失败被误拒
        self.manifest.journal_format = JOURNAL_FORMAT_VERSION;
        self.journal = Journal::create(path, &self.manifest)?;
        Ok(())
    }

    /// 记录一条**外部输入**（含当帧逐 bar state）并压入主队列
    ///
    /// 这是 journal 的唯一写入入口。只有内核**收到**的输入才走这里：
    /// 行情（`BarClosed` / tick）与 live 的交易所回报。sink 派生出来的
    /// 订单 / 成交事件一律用 `queue.push` 直接入队，**不要**经过本方法——
    /// 它们重演时会被同一段逻辑再派生一次，落盘就会让重演重复计数。
    ///
    /// `state` 是 `BarClosed` 在当帧的逐 bar state（无状态传 `None`）。它是
    /// 编排层注入的策略输入、不在 `MarketDataPayload` 里，因此必须随事件一起
    /// 落盘：否则依赖 state 决策的策略重演时只能拿到 `Value::Null`，结果静默改变。
    pub fn enqueue_external(
        &mut self,
        ev: Event,
        state: Option<serde_json::Value>,
    ) -> Result<(), KernelError> {
        // 重演期间输入本来就来自 journal，不能写回去：否则同一条输入在文件与
        // 内存里翻倍，崩溃恢复接管的续写 journal 会记重复条目
        if !self.replaying {
            self.journal.append(&ev, state)?;
        }
        self.queue.push(ev);
        Ok(())
    }

    /// 把历史帧展开成收市 `BarClosed` 事件压入队列
    ///
    /// 事件时间戳 = 帧时间戳 + 周期（**收市时刻**，防前视）；生命周期再把引擎
    /// 时钟回退到 `ev.ts - interval`（即网格时间戳），与
    /// `BacktestRunner::set_clock(grid_ts)` 逐字对齐，`final_time_ns` 等时间戳
    /// 字段的 parity 依赖这条。
    ///
    /// 屏障期望集合为空时用本批帧的品种并集补齐。
    pub fn load_frames(&mut self, frames: Vec<GridFrame>) -> Result<(), KernelError> {
        let interval_ns = interval_to_nanos(&self.config.interval)
            .ok_or_else(|| KernelError::Pump(format!("非法 interval: {}", self.config.interval)))?;
        if let Some(first) = frames.first() {
            // 与引擎构造时的 first_ts 对齐（空跑时 final_time 才有意义）
            self.core.set_clock(first.ts);
        }
        let instruments: HashSet<_> = frames
            .iter()
            .flat_map(|frame| frame.bars.iter().map(|(instrument, _)| instrument.clone()))
            .collect();
        self.barrier.set_expected_if_empty(instruments);

        for frame in frames {
            // 每帧的期望品种集合在灌数据时就精确已知:缺口(None)的品种
            // 本帧不出现,但这一帧仍然要触发一次生命周期(价格前向填充)。
            let frame_instruments = frame
                .bars
                .iter()
                .map(|(instrument, _)| instrument.clone())
                .collect();
            self.barrier.register_frame(frame.ts, frame_instruments);
            // state 是编排层注入的策略输入,不进 MarketDataPayload(市场数据契约)。
            // 预注册到本表,屏障攒齐后按 (帧时间戳, 品种) 回填到 BarClosedEvent;
            // 同时随事件一起落 journal,重演才能复现同一份输入。
            let states = frame.states;
            if !states.is_empty() {
                self.frame_states.insert(frame.ts.nanos, states.clone());
            }

            let close_ts = Timestamp::from_nanos(frame.ts.nanos + interval_ns);
            for (instrument, bar) in frame.bars {
                // 逐 bar state 跟着事件落盘:空 map 里查不到 / 显式 null 都当作无 state
                let state = states
                    .get(&instrument)
                    .filter(|value| !value.is_null())
                    .cloned();
                let event = Event::MarketData(MarketDataEvent::new(
                    self.next_seq,
                    close_ts,
                    MarketDataPayload::BarClosed {
                        instrument,
                        interval: self.config.interval.clone(),
                        bar,
                    },
                ));
                self.next_seq += 1;
                self.enqueue_external(event, state)?;
            }
        }
        Ok(())
    }

    /// 跑完整队列
    ///
    /// `Paused` 模式下队列 `next()` 直接返回 `None`，本次调用立即返回且不消费
    /// 任何事件；`StepOnce` 模式下每轮只前进一个事件（天然的单步调试钩子）。
    pub fn run(&mut self) -> Result<KernelResult, KernelError> {
        self.drain_queue()?;
        Ok(self.finalize())
    }

    /// 排空主队列（`run` 的循环体，重演逐条驱动共用）
    fn drain_queue(&mut self) -> Result<(), KernelError> {
        while let Some(ev) = self.queue.next() {
            self.dispatch(ev)?;
            self.drain_external()?;
        }
        Ok(())
    }

    /// 历史回测入口：灌帧 + 跑完 + 收尾
    pub fn run_history(&mut self, frames: Vec<GridFrame>) -> Result<KernelResult, KernelError> {
        self.load_frames(frames)?;
        self.run()
    }

    /// 从 journal 重演：校验 manifest 后把外部输入重新入队并跑完
    ///
    /// 语义边界（设计文档 4.7）：
    /// - journal 只含外部输入，派生事件在重演时由同一段逻辑重新产生；
    /// - manifest 与 `self.manifest` **逐字段相等**才允许重演，任一字段不同直接
    ///   报错，不做"尽力恢复"（配置 / 数据 / 种子变了，重演结果本就不可信）；
    /// - 每条事件自带当帧逐 bar state，重演按 `(帧时间戳, 品种)` 重建
    ///   `frame_states`，带 state 决策的策略也能逐字复现；
    /// - 缺 `state` 键的旧格式 journal 由 [`Journal::open`] 直接报错，这里不做
    ///   二次兜底；
    /// - 重演期间 `replaying` 标志抑制 journal 写入（输入本来就是从 journal 读出
    ///   的，不能写回去）；调用方应使用**未跑过任何输入的内核**。
    pub fn replay_from(&mut self, path: &Path) -> Result<KernelResult, KernelError> {
        let (manifest, entries) = Journal::open(path)?;
        self.ensure_manifest_match(&manifest)?;
        self.restore_frame_metadata(&entries)?;
        self.replay_entries(entries)?;
        Ok(self.finalize())
    }

    /// 崩溃恢复：读回 journal（容忍尾半行）→ 重放到断点 → 接管续写
    ///
    /// 与 [`Self::replay_from`] 的差异：
    /// - 用 [`Journal::resume`] 读回：崩溃时没收尾的尾半行被截掉，那条输入不重演
    ///   （它是否已被交易所处理由上层三路对账兜底，这里不猜）；
    /// - 恢复出的 journal **接管续写**：重演完成后内核照常收新输入，继续追加到
    ///   同一文件，内存计数从已恢复条数连续增长；
    /// - **不做 finalize**：恢复要停在「可继续跑」的状态，收尾由调用方续跑后统一做。
    ///
    /// 返回后调用方（live driver）的职责：对 sink 做三路对账（positions / fills /
    /// cash 快照 vs 交易所），对账干净、恢复护栏通过后才允许继续下单——重演只是
    /// 「把 journal 记过的输入再喂一遍」，交易所侧在崩溃窗口内可能已发生 journal
    /// 之外的状态变化。
    pub fn resume_replay(&mut self, path: &Path) -> Result<(), KernelError> {
        let (manifest, entries, journal) = Journal::resume(path)?;
        self.ensure_manifest_match(&manifest)?;
        self.journal = journal;
        self.restore_frame_metadata(&entries)?;
        self.replay_entries(entries)
    }

    /// manifest 逐字段比对，任一字段不同直接拒绝
    fn ensure_manifest_match(&self, manifest: &RunManifest) -> Result<(), KernelError> {
        if *manifest != self.manifest {
            return Err(KernelError::Journal(format!(
                "manifest 不一致，拒绝重演: journal={manifest:?} kernel={:?}",
                self.manifest
            )));
        }
        Ok(())
    }

    /// 逐条重演 journal 条目（`replay_from` / `resume_replay` 共用的内层）
    ///
    /// 每条处理完立即排空一次队列再进下一条（`dispatch` 又会在每个事件后跑
    /// [`Self::drain_external`]），与首跑 live 的「到达即驱动」节奏一致。分类：
    /// - 行情条目：走 [`Self::enqueue_external`]，屏障攒帧 / state 回填照常发生，
    ///   `replaying` 标志保证不再写 journal；
    /// - live 回报条目（`state` 槽非空）：`event` 是落盘占位 Heartbeat，不使用；
    ///   把记录的回报状态经 `sink.replay_external` 注回 sink 重建 OMS 状态；
    /// - 非行情且无 `state` 槽：格式不可信，直接拒绝——正常 journal 只有前两种行。
    ///
    /// 重演中途出错不复位 `replaying`：实例已半新半旧，只能整体放弃。
    fn replay_entries(&mut self, entries: Vec<JournalEntry>) -> Result<(), KernelError> {
        self.replaying = true;
        for JournalEntry { event, state } in entries {
            if matches!(event, Event::MarketData(_)) {
                self.enqueue_external(event, state)?;
            } else if let Some(state) = state {
                let out = self.sink.replay_external(&state)?;
                self.queue.push_batch(out);
            } else {
                return Err(KernelError::Journal(
                    "journal 含非行情条目且缺回报 state，格式不可信，拒绝重演".to_string(),
                ));
            }
            self.drain_queue()?;
        }
        self.replaying = false;
        Ok(())
    }

    /// 从 journal 重建网格屏障期望与逐 bar state
    ///
    /// 两者都是编排元数据（不在 `MarketDataPayload` 里）：屏障期望不落 journal，
    /// 重演若拿不到它，多品种回测会退化成"每个品种各自触发一次生命周期"，与首次
    /// 运行不同；逐 bar state 随每条事件落盘，重演时按同一口径回填到
    /// `frame_states`，`dispatch` 才能把同一份 state 交给 Host。
    ///
    /// 时间口径与 `dispatch` 一致：收市时刻 − 周期 = 网格时间戳。
    fn restore_frame_metadata(&mut self, entries: &[JournalEntry]) -> Result<(), KernelError> {
        let interval = self.config.interval.clone();
        let interval_ns = interval_to_nanos(&interval)
            .ok_or_else(|| KernelError::Pump(format!("非法 interval: {interval}")))?;
        let mut per_frame: BTreeMap<i64, HashSet<Instrument>> = BTreeMap::new();
        for entry in entries {
            if let Event::MarketData(md) = &entry.event
                && let MarketDataPayload::BarClosed {
                    instrument, bar, ..
                } = &md.payload
            {
                let close_ts = close_time(bar.timestamp, &interval).unwrap_or(bar.timestamp);
                let grid_ts = close_ts
                    .nanos
                    .checked_sub(interval_ns)
                    .unwrap_or(bar.timestamp.nanos);
                per_frame
                    .entry(grid_ts)
                    .or_default()
                    .insert(instrument.clone());
                if let Some(state) = &entry.state {
                    self.frame_states
                        .entry(grid_ts)
                        .or_default()
                        .insert(instrument.clone(), state.clone());
                }
            }
        }
        let instruments: HashSet<Instrument> = per_frame.values().flatten().cloned().collect();
        self.barrier.set_expected_if_empty(instruments);
        for (ts, frame_instruments) in per_frame {
            self.barrier
                .register_frame(Timestamp::from_nanos(ts), frame_instruments);
        }
        Ok(())
    }

    /// 单步：处理一个事件后返回
    ///
    /// 内部用 `EventQueue::step()`，出队一个事件后队列自动转 `Paused`。
    /// 返回是否消费了事件：崩溃恢复编排用 `while step()? {}` 驱动到队列耗尽
    /// （`run()` / `result()` 会做 EOD 强平，bar 间不能调）。
    pub fn step(&mut self) -> Result<bool, KernelError> {
        if let Some(ev) = self.queue.step() {
            self.dispatch(ev)?;
            self.drain_external()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// 读取执行出口（崩溃恢复对账用）
    ///
    /// 借用具体 sink 做本地快照（如 `LiveSink` 的 OMS 状态），主回路不转移
    /// 所有权——journal 已由内核持有，sink 的生命周期归内核。
    pub fn sink(&self) -> &dyn ExecutionSink {
        self.sink.as_ref()
    }

    /// 可变借用执行出口（恢复编排用）
    ///
    /// 恢复护栏通过后要把重演期的假网关换成真实网关，可变 downcast 需要经由
    /// trait object，规则与 [`Self::sink`] 一致：不转移所有权。
    pub fn sink_mut(&mut self) -> &mut dyn ExecutionSink {
        self.sink.as_mut()
    }

    /// 主动拉取一次 sink 外部回报并回流主队列
    ///
    /// [`Self::step`] 只在队列有事件时才排空回报——live 驱动循环的节奏是
    /// 「注入行情 → `while step` → 交易所回报陆续到达」，队列耗尽后到达的
    /// 回报不会自己被 poll。调度间隔（bar 间等待）由调用方控制，这里只
    /// 负责「拉一次 + 落 journal + 回流」，与 `drain_external` 同一套语义。
    pub fn poll_external(&mut self) -> Result<(), KernelError> {
        self.drain_external()
    }

    /// 收尾快照（幂等）
    ///
    /// 会执行与 `run()` 相同的 finalize（残余事件消费 + EOD 强平 + 结果构造），
    /// 因此只应在队列耗尽后调用。
    pub fn result(&mut self) -> KernelResult {
        self.finalize()
    }

    /// 队列耗尽后的收尾
    ///
    /// `BacktestEngine::step()` 从不置 `finished`，EOD 强平与 `RunResult`
    /// 构造都在 `run()` 内（设计文档 4.6），所以这里必须显式调一次。
    fn finalize(&mut self) -> KernelResult {
        let run = self.core.run();
        KernelResult {
            run,
            duration: self.started.elapsed(),
            bars_processed: self.lifecycle.bars_processed,
            orders_submitted: self.lifecycle.orders_submitted(),
        }
    }

    /// 排空 sink 的外部回报队列并回流主队列
    ///
    /// live 档 sink（`records_external_responses()`）的 poll 会消费交易所回报，
    /// 回报是**外部输入**：不落 journal，崩溃后重演就少处理它们。回流之前把
    /// sink 攒下的回报状态逐条落盘（sink trait 约定：poll 内每应用一条就攒一条，
    /// 含被丢弃的）。journal 行的 event 槽是占位 Heartbeat——一条回报可能产出
    /// 0 个内核事件（非法迁移 / 去重），行粒度跟回报走而不是跟事件走；重演由
    /// `replay_entries` 调 `sink.replay_external` 注回。落盘失败向上抛。
    fn drain_external(&mut self) -> Result<(), KernelError> {
        let out = self.sink.poll()?;
        if self.sink.records_external_responses() && !self.replaying {
            let placeholder = Event::System(SystemEvent::new(
                0,
                Timestamp::from_nanos(0),
                SystemAction::Heartbeat,
            ));
            for state in self.sink.take_update_states() {
                self.journal.append(&placeholder, Some(state))?;
            }
        }
        self.queue.push_batch(out);
        Ok(())
    }

    /// 分发单个事件
    ///
    /// **journal 不在这里写**：本方法处理的多是 sink 派生出来的事件（订单 / 成交 /
    /// 资金费），它们重演时会被 `sink` + 引擎重新派生；若也落盘，重演会把同一笔
    /// 成交计两次。外部输入统一由 [`Self::enqueue_external`] 落盘。
    ///
    /// 顺序固定（设计文档 4.3）：
    /// 1. 行情先给 sink（live 更新盘口），产出事件回流队列（普通 push，不记 journal）；
    /// 2. `BarClosed` 进网格屏障，帧齐才走 per-bar 编排；
    /// 3. 订单 / 成交 / 标记 / 资金费转交账务本体（push + step 排空）；
    /// 4. 系统事件只记录，不决策。
    fn dispatch(&mut self, ev: Event) -> Result<(), KernelError> {
        if let Event::MarketData(md) = &ev {
            let out = self.sink.on_market(&ev)?;
            self.queue.push_batch(out);

            if let MarketDataPayload::BarClosed {
                instrument,
                interval,
                bar,
            } = &md.payload
            {
                // 事件 ts 是收市时刻；生命周期需要的网格时间戳 = 收市时刻 − 周期
                let ts = close_time(bar.timestamp, interval).unwrap_or(bar.timestamp);
                let frame_ts = interval_to_nanos(interval)
                    .and_then(|nanos| ts.nanos.checked_sub(nanos))
                    .map(Timestamp::from_nanos)
                    .unwrap_or(bar.timestamp);
                // 屏障按网格时间戳(bar 起始时间)分组,与 `BacktestRunner`
                // 的 `set_clock(grid_ts)` 对齐
                if let Some(mut frame) = self.barrier.admit(instrument.clone(), *bar, frame_ts)? {
                    // 屏障只攒 bar;本帧 state 在这里按帧时间戳回填(查不到保持空表,
                    // 下游统一按 `Value::Null` 处理)。用 remove 而不是 get:每帧只
                    // 消费一次,顺便释放本轮不再需要的 state。
                    if let Some(states) = self.frame_states.remove(&frame.ts.nanos) {
                        frame.states = states;
                    }
                    let out = self.lifecycle.on_bar_closed(
                        &frame,
                        &self.config,
                        &mut self.core,
                        &mut self.ctx,
                        &mut self.hosts,
                        self.sink.as_mut(),
                    )?;
                    self.queue.push_batch(out);
                }
            }
            return Ok(());
        }

        if matches!(
            ev,
            Event::Order(_) | Event::Fill(_) | Event::Mark(_) | Event::Funding(_)
        ) {
            self.core.push_event(ev);
            self.core.drain();
        }
        // Event::System / 未来新增变体：记录不决策
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axon_core::market::Side;
    use axon_core::types::{SpotInstrument, Symbol};
    use axon_core::{Bar, Instrument, Price, Quantity, SystemAction, SystemEvent, Tick};

    use super::*;
    use crate::InstrumentMode;
    use crate::event::BarClosedEvent;
    use crate::host::{HostCtx, OrderIntent};
    use crate::pump::{GridFrame, build_frames};
    use crate::sink::{BacktestSink, NullSink};

    const START: i64 = 1_700_000_000_000_000_000;
    const ONE_MIN: i64 = 60_000_000_000;

    /// 只记录「被调用了多少次 / 最后一次拿到的 ts」的测试 Host
    struct CountingHost {
        calls: Arc<Mutex<usize>>,
        last_ts: Arc<Mutex<i64>>,
    }

    impl StrategyHost for CountingHost {
        fn name(&self) -> &str {
            "counting"
        }

        fn on_event(&mut self, ev: &BarClosedEvent<'_>, _ctx: &HostCtx) -> Vec<OrderIntent> {
            *self.calls.lock().unwrap() += 1;
            *self.last_ts.lock().unwrap() = ev.ts.nanos;
            // Hold：空 vec = 维持上次目标
            Vec::new()
        }
    }

    /// 恒定给出权益占比目标的 Host
    struct TargetHost {
        ratio: f64,
    }

    impl StrategyHost for TargetHost {
        fn name(&self) -> &str {
            "target"
        }

        fn on_event(&mut self, ev: &BarClosedEvent<'_>, _ctx: &HostCtx) -> Vec<OrderIntent> {
            vec![OrderIntent::SetTarget {
                instrument: ev.instrument.clone(),
                target_ratio: self.ratio,
            }]
        }
    }

    fn btc() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        })
    }

    fn bar(start: i64) -> Bar {
        Bar {
            timestamp: Timestamp::from_nanos(start),
            open: Price::from_f64(100.0),
            high: Price::from_f64(101.0),
            low: Price::from_f64(99.0),
            close: Price::from_f64(100.0),
            volume: Quantity::from_f64(10_000.0),
        }
    }

    fn market(ts: i64, payload: MarketDataPayload) -> Event {
        Event::MarketData(MarketDataEvent::new(0, Timestamp::from_nanos(ts), payload))
    }

    fn bar_closed(start: i64) -> Event {
        // 事件时间戳 = 收市时刻
        market(
            start + ONE_MIN,
            MarketDataPayload::BarClosed {
                instrument: btc(),
                interval: "1m".to_string(),
                bar: bar(start),
            },
        )
    }

    fn empty_kernel() -> Kernel {
        Kernel::new(KernelConfig::default(), Box::new(NullSink))
    }

    #[test]
    fn test_empty_queue_run_is_noop() {
        let mut kernel = empty_kernel();
        let result = kernel.run().expect("空跑不应失败");
        assert_eq!(result.run.events_processed, 0);
        assert_eq!(result.bars_processed, 0);
        assert_eq!(result.orders_submitted, 0);
        assert!(kernel.journal().is_empty());
    }

    #[test]
    fn test_only_bar_closed_triggers_host() {
        let calls = Arc::new(Mutex::new(0));
        let last_ts = Arc::new(Mutex::new(0));

        let mut kernel = Kernel::new(KernelConfig::default(), Box::new(NullSink));
        // bar 内的 tick：不应触发决策
        kernel
            .enqueue_external(
                market(
                    START + 30_000_000_000,
                    MarketDataPayload::Tick(Tick::new(
                        Timestamp::from_nanos(START + 30_000_000_000),
                        Price::from_f64(100.2),
                        Quantity::from_f64(1.0),
                        Side::Buy,
                    )),
                ),
                None,
            )
            .expect("外部输入入队");
        kernel
            .enqueue_external(bar_closed(START), None)
            .expect("外部输入入队");
        kernel.add_host(Box::new(CountingHost {
            calls: calls.clone(),
            last_ts: last_ts.clone(),
        }));

        let result = kernel.run().expect("跑队列不应失败");

        assert_eq!(
            *calls.lock().unwrap(),
            1,
            "只有 BarClosed 触发一次决策，Tick 不触发"
        );
        assert_eq!(
            *last_ts.lock().unwrap(),
            START + ONE_MIN,
            "视图 ts 应是收市时刻"
        );
        assert_eq!(kernel.journal().len(), 2, "两个外部输入都进 journal");
        assert_eq!(result.bars_processed, 1);
        assert_eq!(result.orders_submitted, 0, "Hold 不产生意图");
        assert_eq!(result.run.fills, 0);
    }

    #[test]
    fn test_paused_run_does_not_consume() {
        let mut queue = EventQueue::new();
        queue.push(bar_closed(START));
        queue.pause();

        let calls = Arc::new(Mutex::new(0));
        let last_ts = Arc::new(Mutex::new(0));
        let mut kernel = Kernel::new(KernelConfig::default(), Box::new(NullSink));
        kernel.queue = queue;
        kernel.add_host(Box::new(CountingHost {
            calls: calls.clone(),
            last_ts,
        }));

        let result = kernel.run().expect("暂停模式不应失败");

        assert_eq!(result.bars_processed, 0, "Paused 下不消费任何事件");
        assert!(kernel.journal().is_empty());
        assert_eq!(*calls.lock().unwrap(), 0, "Paused 下不触发决策");
    }

    fn frames_3bars() -> Vec<GridFrame> {
        (0..3)
            .map(|idx| GridFrame {
                ts: Timestamp::from_nanos(START + idx as i64 * ONE_MIN),
                bars: vec![(btc(), bar(START + idx as i64 * ONE_MIN))],
                states: Default::default(),
            })
            .collect()
    }

    /// 记录每根 bar 拿到的 state 的 Host
    struct StateHost {
        seen: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    impl StrategyHost for StateHost {
        fn name(&self) -> &str {
            "state"
        }

        fn on_event(&mut self, ev: &BarClosedEvent<'_>, _ctx: &HostCtx) -> Vec<OrderIntent> {
            self.seen.lock().unwrap().push(ev.state.clone());
            Vec::new()
        }
    }

    #[test]
    fn test_frame_state_reaches_bar_closed_event() {
        let bars = BTreeMap::from([(
            "BTC/USDT".to_string(),
            vec![Some(bar(START)), Some(bar(START + ONE_MIN))],
        )]);
        let states = BTreeMap::from([(
            "BTC/USDT".to_string(),
            vec![
                serde_json::json!({"features": {"ret_1": 0.1}}),
                serde_json::json!({"features": {"ret_1": -0.1}}),
            ],
        )]);
        let frames =
            build_frames(&bars, &InstrumentMode::Spot, Some(&states)).expect("网格应能展平成帧");

        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut kernel = empty_kernel();
        kernel.add_host(Box::new(StateHost { seen: seen.clone() }));
        kernel.run_history(frames).expect("回测应跑通");

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "每帧一次决策");
        assert_eq!(
            seen[0],
            serde_json::json!({"features": {"ret_1": 0.1}}),
            "第一帧 state 必须按 (帧时间戳, 品种) 回填"
        );
        assert_eq!(seen[1], serde_json::json!({"features": {"ret_1": -0.1}}));
    }

    #[test]
    fn test_end_to_end_small_backtest() {
        let config = KernelConfig {
            initial_cash: 10_000.0,
            expected_instruments: [btc()].into_iter().collect(),
            ..KernelConfig::default()
        };
        let mut kernel = Kernel::new(config, Box::new(BacktestSink::new()));
        kernel.add_host(Box::new(TargetHost { ratio: 0.5 }));

        let result = kernel.run_history(frames_3bars()).expect("回测应跑通");

        assert_eq!(result.bars_processed, 3, "三根 bar = 三帧");
        assert_eq!(result.orders_submitted, 3, "每帧一个 SetTarget");
        assert!(result.run.fills >= 1, "0.5 权益占比必须真的成交");
    }

    /// 用指定费率跑同一段网格(3 根 bar、恒定 0.5 权益占比)
    fn run_with_fee_rate(fee_rate: f64) -> KernelResult {
        let config = KernelConfig {
            initial_cash: 10_000.0,
            fee_rate,
            expected_instruments: [btc()].into_iter().collect(),
            ..KernelConfig::default()
        };
        let mut kernel = Kernel::new(config, Box::new(BacktestSink::new()));
        kernel.add_host(Box::new(TargetHost { ratio: 0.5 }));
        kernel.run_history(frames_3bars()).expect("回测应跑通")
    }

    #[test]
    fn test_fee_rate_is_applied() {
        let free = run_with_fee_rate(0.0);
        let cheap = run_with_fee_rate(0.001);
        let pricey = run_with_fee_rate(0.01);

        assert!(free.run.fills > 0, "没有成交则手续费断言无意义");
        assert_eq!(free.run.total_fees, 0.0, "fee_rate = 0 不应产生手续费");

        // 手续费口径 = Σ(成交价 × 成交数量) × 费率,名义按该跑自己的 fills 累加
        let notional: f64 = pricey
            .run
            .fills_detail
            .iter()
            .map(|fill| fill.price.as_f64() * fill.quantity.as_f64())
            .sum();
        assert!(notional > 0.0, "必须有成交量才能校验名义");
        let expected = 0.01 * notional;
        assert!(
            (pricey.run.total_fees - expected).abs() < expected * 1e-9 + 1e-9,
            "0.01 费率应收到 {} ,实际 {}",
            expected,
            pricey.run.total_fees
        );
        assert!(
            pricey.run.total_fees > cheap.run.total_fees && cheap.run.total_fees > 0.0,
            "费率越高手续费越高: 0.001 -> {}, 0.01 -> {}",
            cheap.run.total_fees,
            pricey.run.total_fees
        );
    }

    #[test]
    fn test_default_fee_rate_matches_engine_default() {
        use axon_backtest::engine::FeeConfig;

        assert_eq!(
            KernelConfig::default().fee_rate,
            FeeConfig::default().taker_rate,
            "内核默认费率必须与引擎 FeeConfig 默认值同源"
        );
    }

    #[test]
    fn test_multi_symbol_grid_with_gap_still_frames_every_point() {
        let eth = Instrument::Spot(SpotInstrument {
            base: Symbol::from("ETH"),
            quote: Symbol::from("USDT"),
        });
        let config = KernelConfig {
            initial_cash: 10_000.0,
            expected_instruments: [btc(), eth.clone()].into_iter().collect(),
            ..KernelConfig::default()
        };
        let mut kernel = Kernel::new(config, Box::new(BacktestSink::new()));
        kernel.add_host(Box::new(TargetHost { ratio: 0.1 }));

        // 中间一帧 ETH 缺口:该帧仍需触发一次生命周期(只决策 BTC)
        let frames = vec![
            GridFrame {
                ts: Timestamp::from_nanos(START),
                bars: vec![(btc(), bar(START)), (eth.clone(), bar(START))],
                states: Default::default(),
            },
            GridFrame {
                ts: Timestamp::from_nanos(START + ONE_MIN),
                bars: vec![(btc(), bar(START + ONE_MIN))],
                states: Default::default(),
            },
            GridFrame {
                ts: Timestamp::from_nanos(START + 2 * ONE_MIN),
                bars: vec![
                    (btc(), bar(START + 2 * ONE_MIN)),
                    (eth, bar(START + 2 * ONE_MIN)),
                ],
                states: Default::default(),
            },
        ];
        let result = kernel.run_history(frames).expect("缺口帧不应死锁或报错");

        assert_eq!(result.bars_processed, 3, "三个网格点各触发一次");
        assert_eq!(result.orders_submitted, 5, "2 + 1 + 2 个 SetTarget");
    }

    #[test]
    fn test_finalize_liquidates_open_positions_at_eod() {
        let config = KernelConfig {
            initial_cash: 10_000.0,
            force_liquidate: true,
            expected_instruments: [btc()].into_iter().collect(),
            ..KernelConfig::default()
        };
        let mut kernel = Kernel::new(config, Box::new(BacktestSink::new()));
        kernel.add_host(Box::new(TargetHost { ratio: 1.0 }));

        let result = kernel
            .run_history(vec![GridFrame {
                ts: Timestamp::from_nanos(START),
                bars: vec![(btc(), bar(START))],
                states: Default::default(),
            }])
            .expect("回测应跑通");

        assert!(
            result.run.positions.is_empty(),
            "EOD 强平后不应残留持仓: {:?}",
            result.run.positions
        );
        assert!(
            result.run.fills >= 2,
            "开仓 1 笔 + EOD 平仓 1 笔,实际 {}",
            result.run.fills
        );
        // final_nav 取 bar_nav_curve 末帧(最后一根 bar 采样点),EOD 平仓不新增
        // bar 采样,因此仍是清仓前的 mark-to-market 权益(与 BacktestRunner 一致)
        assert!(
            (result.run.final_nav - 10_000.0).abs() < 1.0,
            "final_nav 应回到初始资金附近,实际 {}",
            result.run.final_nav
        );
    }

    // ─────────────────────────────────────────────────────────────────
    // journal 落盘 / 重演(M1 Task 4)
    // ─────────────────────────────────────────────────────────────────

    /// 会把派生事件回流主队列的 sink(委托真实撮合)
    ///
    /// 用来证明「sink 产出的事件不落 journal」:如果 dispatch 里还留着
    /// `journal.append`,这些 System 事件就会混进去,测试立刻失败。
    struct ChattySink {
        inner: BacktestSink,
    }

    impl ChattySink {
        fn new() -> Self {
            Self {
                inner: BacktestSink::new(),
            }
        }

        /// 每次调用都造一个新的派生事件(seq/ts 取 0,系统事件不参与决策)
        fn derived(&self) -> Event {
            Event::System(SystemEvent::new(
                0,
                Timestamp::from_nanos(0),
                SystemAction::Heartbeat,
            ))
        }
    }

    impl ExecutionSink for ChattySink {
        fn submit(
            &mut self,
            intent: OrderIntent,
            core: &mut KernelCore,
            ctx: &HostCtx,
        ) -> Result<Vec<Event>, KernelError> {
            let mut out = self.inner.submit(intent, core, ctx)?;
            out.push(self.derived());
            Ok(out)
        }

        fn on_market(&mut self, ev: &Event) -> Result<Vec<Event>, KernelError> {
            let mut out = self.inner.on_market(ev)?;
            out.push(self.derived());
            Ok(out)
        }

        fn poll(&mut self) -> Result<Vec<Event>, KernelError> {
            self.inner.poll()
        }

        fn finish_bar(&mut self, core: &mut KernelCore) -> Result<Vec<Event>, KernelError> {
            let mut out = self.inner.finish_bar(core)?;
            out.push(self.derived());
            Ok(out)
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    fn run_manifest() -> RunManifest {
        RunManifest {
            journal_format: JOURNAL_FORMAT_VERSION,
            code_version: "0.22.0".to_string(),
            config_hash: "cfg-abc".to_string(),
            data_hash: "data-def".to_string(),
            seed: 42,
            model_artifacts: Vec::new(),
        }
    }

    /// 跑一次会成交的小回测并把 journal 落到 `path`，返回首次运行结果
    fn run_with_journal(path: &std::path::Path, manifest: &RunManifest) -> KernelResult {
        let config = KernelConfig {
            initial_cash: 10_000.0,
            ..KernelConfig::default()
        };
        let mut kernel = Kernel::new(config, Box::new(BacktestSink::new()));
        kernel.set_manifest(manifest.clone());
        kernel.attach_journal(path).expect("打开落盘 journal");
        kernel.add_host(Box::new(TargetHost { ratio: 0.5 }));
        kernel.run_history(frames_3bars()).expect("回测应跑通")
    }

    fn f64_close(a: f64, b: f64) -> bool {
        const REL_TOL: f64 = 1e-9;
        a == b || (a - b).abs() <= REL_TOL * a.abs().max(b.abs())
    }

    /// 逐字段比对两次运行的 `RunResult`(只排除墙钟 `duration`)
    fn assert_same_run(label: &str, a: &KernelResult, b: &KernelResult) {
        assert_eq!(
            a.bars_processed, b.bars_processed,
            "{label}: bars_processed"
        );
        assert_eq!(
            a.orders_submitted, b.orders_submitted,
            "{label}: orders_submitted"
        );
        let (x, y) = (&a.run, &b.run);

        assert_eq!(
            x.events_processed, y.events_processed,
            "{label}: events_processed"
        );
        assert_eq!(
            x.orders_accepted, y.orders_accepted,
            "{label}: orders_accepted"
        );
        assert_eq!(
            x.orders_rejected, y.orders_rejected,
            "{label}: orders_rejected"
        );
        assert_eq!(
            x.orders_cancelled, y.orders_cancelled,
            "{label}: orders_cancelled"
        );
        assert_eq!(
            x.orders_modified, y.orders_modified,
            "{label}: orders_modified"
        );
        assert_eq!(x.fills, y.fills, "{label}: fills");
        assert_eq!(
            x.rebalances_triggered, y.rebalances_triggered,
            "{label}: rebalances_triggered"
        );
        assert_eq!(x.final_time, y.final_time, "{label}: final_time");
        assert_eq!(
            x.margin_applied, y.margin_applied,
            "{label}: margin_applied"
        );
        assert_eq!(
            x.liquidation_count, y.liquidation_count,
            "{label}: liquidation_count"
        );
        assert_eq!(
            x.margin_shortfall_count, y.margin_shortfall_count,
            "{label}: margin_shortfall_count"
        );
        assert_eq!(x.price_basis, y.price_basis, "{label}: price_basis");

        for (name, left, right) in [
            ("total_pnl", x.total_pnl, y.total_pnl),
            ("max_drawdown", x.max_drawdown, y.max_drawdown),
            ("max_drawdown_pct", x.max_drawdown_pct, y.max_drawdown_pct),
            ("final_nav", x.final_nav, y.final_nav),
            ("nav_peak", x.nav_peak, y.nav_peak),
            ("total_fees", x.total_fees, y.total_fees),
            (
                "total_funding_pnl",
                x.total_funding_pnl,
                y.total_funding_pnl,
            ),
            ("win_rate", x.win_rate, y.win_rate),
            ("sharpe_ratio", x.sharpe_ratio, y.sharpe_ratio),
            (
                "insurance_fund_loss",
                x.insurance_fund_loss,
                y.insurance_fund_loss,
            ),
        ] {
            assert!(
                f64_close(left, right),
                "{label}: {name} 不一致 {left} vs {right}"
            );
        }

        for (name, left, right) in [
            ("bar_nav_curve", &x.bar_nav_curve, &y.bar_nav_curve),
            (
                "bar_balance_curve",
                &x.bar_balance_curve,
                &y.bar_balance_curve,
            ),
            ("equity_curve", &x.equity_curve, &y.equity_curve),
        ] {
            assert_eq!(left.len(), right.len(), "{label}: {name} 长度不一致");
            for (i, (l, r)) in left.iter().zip(right.iter()).enumerate() {
                assert_eq!(l.0, r.0, "{label}: {name}[{i}] 时间戳不一致");
                assert!(f64_close(l.1, r.1), "{label}: {name}[{i}] 值不一致");
            }
        }

        assert_eq!(x.positions, y.positions, "{label}: positions");
        assert_eq!(x.leg_targets, y.leg_targets, "{label}: leg_targets");
        assert_eq!(x.marks, y.marks, "{label}: marks");
        assert_eq!(x.fills_detail, y.fills_detail, "{label}: fills_detail");
        assert_eq!(x.trades, y.trades, "{label}: trades");
        assert_eq!(
            x.liquidation_events, y.liquidation_events,
            "{label}: liquidation_events"
        );
        assert_eq!(
            x.margin_specs_effective, y.margin_specs_effective,
            "{label}: margin_specs_effective"
        );
    }

    #[test]
    fn test_journal_records_only_external_inputs() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("run.jsonl");

        let config = KernelConfig {
            initial_cash: 10_000.0,
            ..KernelConfig::default()
        };
        let mut kernel = Kernel::new(config, Box::new(ChattySink::new()));
        kernel.set_manifest(run_manifest());
        kernel.attach_journal(&path).expect("打开落盘 journal");
        kernel.add_host(Box::new(TargetHost { ratio: 0.5 }));
        let result = kernel.run_history(frames_3bars()).expect("回测应跑通");
        assert!(
            result.run.fills >= 1,
            "用例必须真的成交,实际 {}",
            result.run.fills
        );

        let (_, entries) = Journal::open(&path).expect("读回 journal");
        assert_eq!(entries.len(), 3, "journal 只应有三条外部 BarClosed");
        for entry in &entries {
            match &entry.event {
                Event::MarketData(md) => assert!(
                    matches!(md.payload, MarketDataPayload::BarClosed { .. }),
                    "journal 里只允许 BarClosed"
                ),
                other => panic!("journal 混入了派生事件: {other:?}"),
            }
        }
        assert_eq!(kernel.journal().len(), 3, "内存日志与落盘内容同源");
    }

    #[test]
    fn test_replay_reproduces_same_result() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("run.jsonl");
        let manifest = run_manifest();

        let first = run_with_journal(&path, &manifest);

        // 重演:同配置、同 manifest、同 Host,不 attach 落盘 journal
        let config = KernelConfig {
            initial_cash: 10_000.0,
            ..KernelConfig::default()
        };
        let mut kernel = Kernel::new(config, Box::new(BacktestSink::new()));
        kernel.set_manifest(manifest);
        kernel.add_host(Box::new(TargetHost { ratio: 0.5 }));
        let second = kernel.replay_from(&path).expect("重演应跑通");

        assert!(first.run.fills >= 1, "用例必须真的成交,否则比较没有力度");
        assert_same_run("replay", &first, &second);
    }

    /// 直接拿逐 bar state 决策的 Host:state 里的比值就是权益占比
    ///
    /// 没有 state 时按 0.0（空仓）——若重演丢了 state,三条 bar 全部空仓,
    /// 成交数会从「有成交」变成 0,`assert_same_run` 立刻失败。
    struct StateRatioHost {
        seen: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    impl StrategyHost for StateRatioHost {
        fn name(&self) -> &str {
            "state_ratio"
        }

        fn on_event(&mut self, ev: &BarClosedEvent<'_>, _ctx: &HostCtx) -> Vec<OrderIntent> {
            self.seen.lock().unwrap().push(ev.state.clone());
            let ratio = ev
                .state
                .get("sma5_vs_sma20")
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0);
            vec![OrderIntent::SetTarget {
                instrument: ev.instrument.clone(),
                target_ratio: ratio,
            }]
        }
    }

    /// 3 根 bar,逐 bar state 各不相同(仓位 0.5 → 0.1 → 0.0)
    fn state_frames() -> Vec<GridFrame> {
        let bars = BTreeMap::from([(
            "BTC/USDT".to_string(),
            vec![
                Some(bar(START)),
                Some(bar(START + ONE_MIN)),
                Some(bar(START + 2 * ONE_MIN)),
            ],
        )]);
        let states = BTreeMap::from([(
            "BTC/USDT".to_string(),
            vec![
                serde_json::json!({"sma5_vs_sma20": 0.5}),
                serde_json::json!({"sma5_vs_sma20": 0.1}),
                serde_json::json!({"sma5_vs_sma20": 0.0}),
            ],
        )]);
        build_frames(&bars, &InstrumentMode::Spot, Some(&states)).expect("网格应能展平成帧")
    }

    #[test]
    fn test_replay_with_state_reproduces_same_result() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("state_run.jsonl");
        let manifest = run_manifest();
        let config = || KernelConfig {
            initial_cash: 10_000.0,
            ..KernelConfig::default()
        };

        let first_seen = Arc::new(Mutex::new(Vec::new()));
        let mut kernel = Kernel::new(config(), Box::new(BacktestSink::new()));
        kernel.set_manifest(manifest.clone());
        kernel.attach_journal(&path).expect("打开落盘 journal");
        kernel.add_host(Box::new(StateRatioHost {
            seen: first_seen.clone(),
        }));
        let first = kernel.run_history(state_frames()).expect("首跑应跑通");
        assert!(
            first.run.fills >= 1,
            "用例必须真的成交,实际 {}",
            first.run.fills
        );

        // 每一条 BarClosed 都必须带 state(本用例逐 bar 都有非空 state)
        let (_, entries) = Journal::open(&path).expect("读回 journal");
        assert_eq!(entries.len(), 3);
        assert!(
            entries.iter().all(|entry| entry.state.is_some()),
            "逐 bar state 必须落盘: {entries:?}"
        );

        let second_seen = Arc::new(Mutex::new(Vec::new()));
        let mut replayed = Kernel::new(config(), Box::new(BacktestSink::new()));
        replayed.set_manifest(manifest);
        replayed.add_host(Box::new(StateRatioHost {
            seen: second_seen.clone(),
        }));
        let second = replayed.replay_from(&path).expect("重演应跑通");

        assert_eq!(
            *first_seen.lock().unwrap(),
            *second_seen.lock().unwrap(),
            "重演时 Host 收到的 state 必须与首跑逐条相同"
        );
        assert_eq!(
            first_seen.lock().unwrap()[0],
            serde_json::json!({"sma5_vs_sma20": 0.5}),
            "首帧 state 应按 (帧时间戳, 品种) 回填"
        );
        assert_same_run("replay-state", &first, &second);
    }

    #[test]
    fn test_replay_manifest_mismatch_is_rejected() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("run.jsonl");
        let manifest = run_manifest();
        run_with_journal(&path, &manifest);

        let replay_with = |changed: RunManifest| {
            let config = KernelConfig {
                initial_cash: 10_000.0,
                ..KernelConfig::default()
            };
            let mut kernel = Kernel::new(config, Box::new(BacktestSink::new()));
            kernel.set_manifest(changed);
            kernel.add_host(Box::new(TargetHost { ratio: 0.5 }));
            kernel.replay_from(&path)
        };

        // config_hash 改一个字节
        let mut config_changed = manifest.clone();
        config_changed.config_hash = "cfg-abd".to_string();
        let err = replay_with(config_changed).expect_err("config_hash 不一致必须拒绝");
        assert!(
            err.to_string().contains("manifest 不一致"),
            "错误信息应说明 manifest 不一致: {err}"
        );

        // seed 改一个字节
        let mut seed_changed = manifest.clone();
        seed_changed.seed = 43;
        assert!(
            replay_with(seed_changed).is_err(),
            "seed 不一致必须拒绝重演"
        );
    }

    /// `resume_replay` 与 `replay_from` 共用 manifest 校验，崩溃恢复路径同样
    /// 必须逐字段拒绝：配置 / 种子变了，重演续跑出来的仓位与订单不可信
    #[test]
    fn test_resume_replay_rejects_manifest_mismatch() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("run.jsonl");
        let manifest = run_manifest();
        run_with_journal(&path, &manifest);

        let resume_with = |changed: RunManifest| {
            let config = KernelConfig {
                initial_cash: 10_000.0,
                ..KernelConfig::default()
            };
            let mut kernel = Kernel::new(config, Box::new(BacktestSink::new()));
            kernel.set_manifest(changed);
            kernel.add_host(Box::new(TargetHost { ratio: 0.5 }));
            kernel.resume_replay(&path)
        };

        // config_hash 改一个字节
        let mut config_changed = manifest.clone();
        config_changed.config_hash = "cfg-abd".to_string();
        let err = resume_with(config_changed).expect_err("config_hash 不一致必须拒绝恢复");
        assert!(
            err.to_string().contains("manifest 不一致"),
            "错误信息应说明 manifest 不一致: {err}"
        );

        // seed 改一个字节
        let mut seed_changed = manifest.clone();
        seed_changed.seed = 43;
        assert!(
            resume_with(seed_changed).is_err(),
            "seed 不一致必须拒绝恢复"
        );
    }

    /// 崩溃恢复全链路：首段跑 2 bar 即「崩溃」→ 新内核重放到断点 → 续跑第 3 根
    ///
    /// 分段（恢复续跑）与一次性跑完的 `RunResult` 必须逐字段一致，这是
    /// 「恢复后行为与不间断运行等价」的直接证据。分段驱动不能调 `run()` /
    /// `result()`——它们会做 EOD 收尾强平，bar 间只允许 `while step`。
    #[test]
    fn test_resume_replay_continues_and_matches_full_run() {
        let dir = tempfile::tempdir().expect("临时目录");
        let manifest = run_manifest();
        let config = || KernelConfig {
            initial_cash: 10_000.0,
            ..KernelConfig::default()
        };
        let frames = frames_3bars();

        // 对照组：不间断跑完 3 根的内核，全程无崩溃
        let full = run_with_journal(&dir.path().join("full.jsonl"), &manifest);

        // 崩溃组首段：只灌前 2 根，队列耗尽后立刻丢弃内核（模拟进程消失）
        let crash_path = dir.path().join("crash.jsonl");
        let mut kernel = Kernel::new(config(), Box::new(BacktestSink::new()));
        kernel.set_manifest(manifest.clone());
        kernel
            .attach_journal(&crash_path)
            .expect("打开落盘 journal");
        kernel.add_host(Box::new(TargetHost { ratio: 0.5 }));
        kernel
            .load_frames(frames[..2].to_vec())
            .expect("灌入前两根 bar");
        while kernel.step().expect("首段步进") {}
        drop(kernel);

        // 崩溃组恢复：新内核重放 journal 到断点（不 finalize），再续跑第 3 根
        let mut recovered = Kernel::new(config(), Box::new(BacktestSink::new()));
        recovered.set_manifest(manifest);
        recovered.add_host(Box::new(TargetHost { ratio: 0.5 }));
        recovered.resume_replay(&crash_path).expect("恢复应跑通");
        recovered
            .load_frames(frames[2..].to_vec())
            .expect("续跑灌入第 3 根 bar");
        while recovered.step().expect("续跑步进") {}
        let resumed = recovered.result();

        assert!(full.run.fills >= 1, "用例必须真的成交,否则比较没有力度");
        assert_eq!(resumed.bars_processed, 3, "恢复续跑必须处理完整 3 根 bar");
        assert_same_run("resume", &full, &resumed);
    }
}
