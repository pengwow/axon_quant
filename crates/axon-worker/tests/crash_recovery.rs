//! 崩溃恢复端到端 mock 演练（设计文档 4.7 重演边界 / 4.10 三路对账）
//!
//! 场景：内核跑完 bar0/bar1 后进程崩溃 → 用 journal 重建恢复 kernel
//! （重演外部输入恢复 OMS 状态）→ 三路对账（local OMS vs 模拟交易所账本）
//! → 从 bar2 续跑 → 与「从未崩溃的整跑」逐一比对 OMS 快照。
//!
//! 全部外部交互由 mock 承载（ScriptedGateway + VenueLedger），不触网；
//! 崩溃由显式 drop 模拟。断言覆盖四层语义：
//! 1. 重演保真：恢复 kernel 的 OMS 与崩溃瞬间完全一致
//! 2. 断点对账：重演后的 OMS vs 断点前交易所侧账本（三路 + resume 门禁）
//! 3. 续跑终态对账
//! 4. 续跑取齐：与从未崩溃的整跑 OMS 逐字段一致
//!
//! 全程只用 `while step()` 驱动（`result()` 是回测 EOD 收尾概念，bar 间不许调），
//! 终态断言直接比 LiveSink 的 OMS 快照——崩溃组与恢复组都不 finalize，两侧对称。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use axon_core::types::{Instrument, Price, Quantity, SpotInstrument, Symbol};
use axon_core::{Bar, Event, Timestamp};
use axon_kernel::persist::{JOURNAL_FORMAT_VERSION, RunManifest};
use axon_kernel::{
    BarClosedEvent, GridFrame, HostCtx, Journal, Kernel, KernelConfig, OrderIntent, StrategyHost,
};
use axon_oms::{
    AccountingMode, OmsSnapshot, Order as OmsOrder, OrderId as OmsOrderId,
    OrderStatus as OmsOrderStatus, Side as OmsSide,
};
use axon_worker::reconcile::{
    LocalSnapshot, ReconcileConfig, VenueSnapshot, reconcile, resume_guard,
};
use axon_worker::{GatewayAck, LiveOrderGateway, LiveSink, VenueUpdate};
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};

const START: i64 = 1_700_000_000_000_000_000;
const ONE_MIN: i64 = 60_000_000_000;
const PRICE: f64 = 100.0;
const INITIAL_CASH: f64 = 10_000.0;
const BASE_QUOTE: &str = "BTC/USDT";

/// 0.3 → 0.6 加仓（崩在 bar1 后 = 断点持仓非零）、0 平仓、0.5 反手、0 清仓
const CYCLE_RATIOS: [f64; 5] = [0.3, 0.6, 0.0, 0.5, 0.0];

// ── 市场 / 配置工厂（照抄 kernel_loop.rs 测试区惯例） ─────────────────

fn btc() -> Instrument {
    Instrument::Spot(SpotInstrument {
        base: Symbol::from("BTC"),
        quote: Symbol::from("USDT"),
    })
}

fn bar(start: i64) -> Bar {
    Bar {
        timestamp: Timestamp::from_nanos(start),
        open: Price::from_f64(PRICE),
        high: Price::from_f64(101.0),
        low: Price::from_f64(99.0),
        close: Price::from_f64(PRICE),
        volume: Quantity::from_f64(10_000.0),
    }
}

fn deterministic_price() -> Decimal {
    Decimal::from_f64(PRICE).expect("常量价转 Decimal")
}

fn kernel_config() -> KernelConfig {
    KernelConfig {
        initial_cash: INITIAL_CASH,
        expected_instruments: [btc()].into_iter().collect(),
        ..KernelConfig::default()
    }
}

/// 三个内核（整跑 / 崩溃 / 恢复）必须持同一份 manifest 才能互认
fn run_manifest() -> RunManifest {
    RunManifest {
        config_hash: "cfg-crash".to_string(),
        data_hash: "data-crash".to_string(),
        seed: 7,
        // journal_format / code_version 由 Default 给出（当前格式版本 + 编译期版本）
        ..RunManifest::default()
    }
}

fn fresh_sink(gateway: &ScriptedGateway) -> LiveSink {
    LiveSink::new(
        Box::new(gateway.clone()),
        Decimal::from_f64(INITIAL_CASH).expect("初始现金转 Decimal"),
    )
    .with_accounting_mode(AccountingMode::Cash)
}

/// 回报时间戳 = 该 bar 收市后半小时根，确定性可复现（LiveSink 构造 Fill
/// 用的是 `to_utc(update.timestamp)`，不吃墙钟）
fn bar_close_ts(bar_idx: usize) -> Timestamp {
    Timestamp::from_nanos(START + (bar_idx as i64 + 1) * ONE_MIN + ONE_MIN / 2)
}

// ── Mock：策略 / 网关 / 交易所账本 ───────────────────────────────────

/// 循环档位 Host：每次收市把目标权益占比推到 `CYCLE_RATIOS` 的下一档
///
/// 恢复 kernel 用全新实例（idx 从 0 起）——重演恰好按相同次序消耗前两档，
/// 决策与首跑一一对应，这是「重演保真」可成立的前提。
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
        // 越界挡板：超过档位数 = 用例设计错误，停在最后一档而非 panic
        let ratio = CYCLE_RATIOS[self.idx.min(CYCLE_RATIOS.len() - 1)];
        self.idx = (self.idx + 1).min(CYCLE_RATIOS.len() - 1);
        vec![OrderIntent::SetTarget {
            instrument: ev.instrument.clone(),
            target_ratio: ratio,
        }]
    }
}

/// 脚本化网关：place 返回 Ack（订单号自增），回报由测试手工 `inject` 再 poll
#[derive(Clone, Default)]
struct ScriptedGateway {
    queue: Arc<Mutex<Vec<VenueUpdate>>>,
    venue_seq: Arc<Mutex<u64>>,
}

impl ScriptedGateway {
    fn inject(&self, update: VenueUpdate) {
        self.queue.lock().expect("回报队列锁").push(update);
    }
}

impl LiveOrderGateway for ScriptedGateway {
    fn place(&mut self, _order: &OmsOrder) -> Result<GatewayAck, String> {
        let mut seq = self.venue_seq.lock().expect("venue 序号锁");
        *seq += 1;
        let venue_order_id = format!("venue-{seq}");
        Ok(GatewayAck {
            venue_order_id,
            status: OmsOrderStatus::Acknowledged,
        })
    }

    fn cancel(&mut self, _id: &OmsOrderId) -> Result<(), String> {
        Ok(())
    }

    fn poll_updates(&mut self) -> Result<Vec<VenueUpdate>, String> {
        Ok(std::mem::take(&mut *self.queue.lock().expect("回报队列锁")))
    }
}

/// 模拟交易所账本：与 LiveSink Cash 模式同一套记账公式，
/// 作为「交易所侧真相」供三路对账
#[derive(Debug)]
struct VenueLedger {
    cash: Decimal,
    positions: BTreeMap<String, Decimal>,
    fill_ids: BTreeSet<String>,
}

impl Default for VenueLedger {
    fn default() -> Self {
        // 交易所账户预存与 OMS shadow_cash 相同的初始资金，两侧起点对齐
        Self {
            cash: Decimal::from_f64(INITIAL_CASH).expect("初始现金转 Decimal"),
            positions: BTreeMap::new(),
            fill_ids: BTreeSet::new(),
        }
    }
}

impl VenueLedger {
    fn apply(&mut self, side: OmsSide, qty: Decimal, price: Decimal, fee: Decimal, trade_id: &str) {
        let gross = qty * price;
        let slot = self
            .positions
            .entry(BASE_QUOTE.to_string())
            .or_insert(Decimal::ZERO);
        match side {
            OmsSide::Buy => {
                self.cash -= gross + fee;
                *slot += qty;
            }
            OmsSide::Sell => {
                self.cash += gross - fee;
                *slot -= qty;
            }
        }
        self.fill_ids.insert(trade_id.to_string());
    }
}

// ── 断言 / 驱动工具 ──────────────────────────────────────────────────

/// Kernel 拥有 sink，用 `as_any` downcast 拿回具体 LiveSink
fn live_sink(kernel: &Kernel) -> &LiveSink {
    kernel
        .sink()
        .as_any()
        .downcast_ref::<LiveSink>()
        .expect("本测试的 ExecutionSink 应为 LiveSink")
}

/// 逐字段比对两份 OMS 快照
///
/// 刻意排除墙钟与回执类字段：created_at / updated_at / completed_at / meta /
/// order HistoryRecord 自身时间戳、fill 的 timestamp 与 symbol/instrument——
/// 这类字段在崩溃两侧无害且必然不同，比了反而脆弱。
fn assert_oms_equal(label: &str, left: &OmsSnapshot, right: &OmsSnapshot) {
    assert_eq!(
        left.order_history.len(),
        right.order_history.len(),
        "{label}: 订单数应一致"
    );
    for (l, r) in left.order_history.iter().zip(right.order_history.iter()) {
        assert_eq!(l.order.id, r.order.id, "{label}: 订单号应一致");
        assert_eq!(
            l.order.instrument_id, r.order.instrument_id,
            "{label}: 品种应一致"
        );
        assert_eq!(l.order.side, r.order.side, "{label}: 方向应一致");
        assert_eq!(
            l.order.order_type, r.order.order_type,
            "{label}: 订单类型应一致"
        );
        assert_eq!(l.order.quantity, r.order.quantity, "{label}: 数量应一致");
        assert_eq!(l.order.price, r.order.price, "{label}: 委托价应一致");
        assert_eq!(l.order.status, r.order.status, "{label}: 状态应一致");
        assert_eq!(
            l.order.idempotency_key, r.order.idempotency_key,
            "{label}: 幂等键应一致"
        );
        assert_eq!(l.fills.len(), r.fills.len(), "{label}: 成交笔数应一致");
        for (lf, rf) in l.fills.iter().zip(r.fills.iter()) {
            assert_eq!(lf.fill_id, rf.fill_id, "{label}: 成交号应一致");
            assert_eq!(lf.price, rf.price, "{label}: 成交价应一致");
            assert_eq!(lf.quantity, rf.quantity, "{label}: 成交量应一致");
            assert_eq!(lf.fee, rf.fee, "{label}: 手续费应一致");
        }
    }
    assert_eq!(
        left.active_orders.len(),
        right.active_orders.len(),
        "{label}: 活动订单数应一致"
    );
}

/// 三路对账（持仓 / 现金 / 成交流水）+ 恢复门禁
fn assert_reconciled(label: &str, sink: &LiveSink, ledger: &VenueLedger) {
    let local = LocalSnapshot::from_sink(sink);
    let mut positions: BTreeMap<String, f64> = BTreeMap::new();
    for (key, qty) in &ledger.positions {
        let value = qty.to_f64().expect("持仓转 f64");
        if value.abs() > 1e-9 {
            positions.insert(key.clone(), value);
        }
    }
    let venue = VenueSnapshot {
        positions,
        cash: ledger.cash.to_f64().expect("现金转 f64"),
        fill_ids: ledger.fill_ids.clone(),
    };
    let report = reconcile(&local, &venue, &ReconcileConfig::default());
    assert!(report.is_clean(), "{label}: 三路对账必须干净: {report:#?}");
    resume_guard(&report).unwrap_or_else(|err| panic!("{label}: 恢复门禁: {err}"));
}

/// 驱动一根 bar：行情入泵 → 主回路步进 → 构造确定性 Filled 回报 →
/// 网关注入 + 账本记账 → 显式排空回报 → 消化到队列收敛
fn drive_bar(
    kernel: &mut Kernel,
    gateway: &ScriptedGateway,
    ledger: &mut VenueLedger,
    frame: GridFrame,
    bar_idx: usize,
) {
    kernel.load_frames(vec![frame]).expect("bar 帧入泵");
    while kernel.step().expect("主回路步进") {}

    // 用例设计保证每根 bar 恰好一笔新订单，order_history.last() 即当前单
    let record = live_sink(kernel)
        .oms()
        .snapshot()
        .order_history
        .last()
        .expect("每根 bar 恰好触发一单")
        .clone();
    let qty = record.order.quantity;
    let price = deterministic_price();
    let fee = Decimal::from(1);
    let trade_id = format!("t-{bar_idx}");
    let update = VenueUpdate {
        order_id: record.order.id,
        status: OmsOrderStatus::Filled {
            filled_qty: qty,
            avg_price: price,
        },
        fill_quantity: qty,
        fill_price: price,
        fee,
        trade_id: Some(trade_id.clone()),
        timestamp: bar_close_ts(bar_idx),
    };
    gateway.inject(update);
    ledger.apply(record.order.side, qty, price, fee, &trade_id);

    // 回报晚于队列耗尽到达：必须显式 poll（live 驱动循环同款节奏）
    kernel.poll_external().expect("排空回报");
    while kernel.step().expect("回报消化") {}
}

// ── 主测试：崩溃 → 恢复 → 对账 → 续跑取齐 ────────────────────────────

#[test]
fn crash_resume_reconciles_and_matches_full_run() {
    let journal_dir = tempfile::tempdir().expect("创建 journal 临时目录");
    let journal_path = journal_dir.path().join("crash-resume.journal");

    let frames: Vec<GridFrame> = (0..5)
        .map(|idx| GridFrame {
            ts: Timestamp::from_nanos(START + idx as i64 * ONE_MIN),
            bars: vec![(btc(), bar(START + idx as i64 * ONE_MIN))],
            states: Default::default(),
        })
        .collect();

    // ── 对照组：从未崩溃的整跑 ─────────────────────────────────────
    let gateway_full = ScriptedGateway::default();
    let sink_full = fresh_sink(&gateway_full);
    let mut kernel_full = Kernel::new(kernel_config(), Box::new(sink_full));
    kernel_full.add_host(Box::new(CycleHost::new()));
    let mut ledger_full = VenueLedger::default();
    for (idx, frame) in frames.iter().cloned().enumerate() {
        drive_bar(
            &mut kernel_full,
            &gateway_full,
            &mut ledger_full,
            frame,
            idx,
        );
    }
    assert_reconciled("对照终态", live_sink(&kernel_full), &ledger_full);
    let oms_full = live_sink(&kernel_full).oms().snapshot();
    drop(kernel_full);

    // ── 崩溃组：跑完 bar0/bar1 后进程消失 ──────────────────────────
    let gateway1 = ScriptedGateway::default();
    let sink1 = fresh_sink(&gateway1);
    let mut kernel1 = Kernel::new(kernel_config(), Box::new(sink1));
    kernel1.set_manifest(run_manifest());
    kernel1.add_host(Box::new(CycleHost::new()));
    kernel1.attach_journal(&journal_path).expect("挂载 journal");
    let mut ledger1 = VenueLedger::default();
    drive_bar(&mut kernel1, &gateway1, &mut ledger1, frames[0].clone(), 0);
    drive_bar(&mut kernel1, &gateway1, &mut ledger1, frames[1].clone(), 1);
    let oms1 = live_sink(&kernel1).oms().snapshot();
    drop(kernel1); // 模拟 kill -9

    // ── 恢复组：journal 重演 ───────────────────────────────────────
    let gateway2 = ScriptedGateway::default();
    let sink2 = fresh_sink(&gateway2);
    let mut kernel2 = Kernel::new(kernel_config(), Box::new(sink2));
    // 恢复 kernel 不能 attach_journal（会截断既有文件），也不允许
    // journal_format=0（与文件头不匹配）——先 set_manifest 再 resume，
    // ensure_manifest_match 逐字段校验。
    kernel2.set_manifest(run_manifest());
    kernel2.add_host(Box::new(CycleHost::new()));
    kernel2.resume_replay(&journal_path).expect("恢复应跑通");

    // 重演保真：恢复 kernel 的 OMS 与崩溃瞬间逐字段一致
    let oms2 = live_sink(&kernel2).oms().snapshot();
    assert_oms_equal("重演保真", &oms1, &oms2);
    // 断点对账：重演后的 OMS vs 崩溃前交易所侧真实账本（含非零持仓）
    assert_reconciled("断点", live_sink(&kernel2), &ledger1);

    // ── 续跑：bar2/bar3/bar4 ───────────────────────────────────────
    let mut ledger2 = ledger1;
    for (idx, frame) in frames.iter().enumerate().skip(2) {
        drive_bar(&mut kernel2, &gateway2, &mut ledger2, frame.clone(), idx);
    }
    assert_reconciled("续跑终态", live_sink(&kernel2), &ledger2);
    let oms2_final = live_sink(&kernel2).oms().snapshot();
    drop(kernel2);

    // 续跑取齐：与从未崩溃的整跑逐字段一致
    assert_oms_equal("续跑取齐", &oms_full, &oms2_final);

    // ── journal 收口：10 条目、行情在前回报紧随其后交替 ────────────
    let (manifest_read, entries) = Journal::open(&journal_path).expect("journal 可读回");
    assert_eq!(
        manifest_read.journal_format, JOURNAL_FORMAT_VERSION,
        "journal 头应是当前格式版本"
    );
    assert_eq!(entries.len(), 10, "2 bar + 2 回报 + 3 bar + 3 回报");
    for (idx, entry) in entries.iter().enumerate() {
        if idx % 2 == 0 {
            assert!(
                matches!(entry.event, Event::MarketData(..)),
                "第 {idx} 条应是行情条目"
            );
        } else {
            // 回报条目：event 槽是 Heartbeat 占位（System 变体），state 槽才是 VenueUpdate JSON
            assert!(
                matches!(entry.event, Event::System(..)),
                "第 {idx} 条应是回报占位条目"
            );
            assert!(entry.state.is_some(), "回报条目必须携带 VenueUpdate JSON");
        }
    }
}
