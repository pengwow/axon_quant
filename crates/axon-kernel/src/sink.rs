//! 执行出口
//!
//! [`ExecutionSink`] 是「意图 → 成交/回报」的唯一出口：回测 / paper / live
//! 三档撮合精度的差异全部收敛在实现里，主回路不感知。三个方法返回的事件
//! 一律由主回路 push 回队列，sink 不允许自己消费（否则 fill / 回报会绕过
//! journal 与后续决策）。
//!
//! 回测实现 [`BacktestSink`]：目标调仓攒到 bar 末统一 rebalance，与
//! `BacktestRunner` 的 per-bar 编排逐字对齐；[`PaperSink`] 复用同一份撮合与
//! 虚拟账，额外吸收实时报价。交易所直连的 `LiveSink` 属于 M5。

use std::collections::HashMap;

use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::{Event, Instrument, MarketDataPayload, OrderAction, OrderEvent};

use crate::KernelError;
use crate::core::KernelCore;
use crate::host::{HostCtx, OrderIntent};

/// 执行出口
pub trait ExecutionSink {
    /// 提交一个策略意图，返回因此产生的事件（订单确认 / 成交等）
    ///
    /// `core` 是账务核心：`SetTarget` 换算数量需要当前权益与最新价，而换算点
    /// 是 sink 的唯一职责；`ctx` 是只读账户视图（供 live 侧风控 / 日志使用）。
    fn submit(
        &mut self,
        intent: OrderIntent,
        core: &mut KernelCore,
        ctx: &HostCtx,
    ) -> Result<Vec<Event>, KernelError>;

    /// 行情到达时更新内部状态（live 更新盘口 / 挂单簿），返回产出的事件
    fn on_market(&mut self, ev: &Event) -> Result<Vec<Event>, KernelError>;

    /// 排空外部回报队列（live 用；回测 / paper 恒为空）
    fn poll(&mut self) -> Result<Vec<Event>, KernelError>;

    /// bar 末批量落地
    ///
    /// `SetTarget` 只在 [`Self::submit`] 里登记目标（数量已换算），真正的
    /// `set_target_position + rebalance_to_target` 在这里统一执行一次 ——
    /// 多品种同一帧只允许一次 rebalance，否则资金约束与 NAV 采样会重复。
    fn finish_bar(&mut self, core: &mut KernelCore) -> Result<Vec<Event>, KernelError>;

    /// 是否由 sink 记录外部交易所回报（live 档 = `true`）
    ///
    /// 打开此标志后，主回路在 [`Self::poll`] 之后调 [`Self::take_update_states`]，
    /// 把 poll 拉到的每条原始回报写进 journal（broker 侧的外部输入，重演必需）。
    /// 回测 / paper 的 poll 恒空，默认 `false` 即可。
    fn records_external_responses(&self) -> bool {
        false
    }

    /// 取走 poll 阶段攒下的原始回报（journal 落盘用）
    ///
    /// 只在 [`Self::records_external_responses`] 为 `true` 时被调用。约定：`poll`
    /// 内每 apply 一条交易所回报就同步攒一条对应的 JSON 状态，两者一一对应、
    /// 顺序一致；被 sink 丢弃的回报（未知订单 / 去重）也照攒——重演时同样的
    /// 丢弃决策会再次发生，条数与顺序才能逐字对齐。本方法取走后清空。
    fn take_update_states(&mut self) -> Vec<serde_json::Value> {
        Vec::new()
    }

    /// 重演：把 journal 里记录的一条外部回报注回 sink
    ///
    /// 与首跑 `poll → apply` 等价地更新内部账本（OMS 状态机 / 成交去重表），
    /// 返回派生事件（回流主队列）。默认拒绝：非 live 实现的 journal 里不应
    /// 出现回报条目，出现即为文件被写坏，宁可拒绝恢复也不静默跳过。
    fn replay_external(&mut self, _state: &serde_json::Value) -> Result<Vec<Event>, KernelError> {
        Err(KernelError::Sink(
            "该 sink 不支持重演外部回报（非 live 实现）".to_string(),
        ))
    }

    /// downcast 到具体类型（崩溃恢复对账用）
    ///
    /// 主回路持有 `Box<dyn ExecutionSink>`，恢复编排需要拿具体 sink（如
    /// `LiveSink`）的本地快照做三路对账，trait object 只能经此桥回去。
    fn as_any(&self) -> &dyn std::any::Any;
}

/// 空实现：什么都不做，永远返回空事件
///
/// M0 空跑与单测用；也便于验证「主回路在零执行器下不 panic」。
#[derive(Debug, Default)]
pub struct NullSink;

impl ExecutionSink for NullSink {
    fn submit(
        &mut self,
        _intent: OrderIntent,
        _core: &mut KernelCore,
        _ctx: &HostCtx,
    ) -> Result<Vec<Event>, KernelError> {
        Ok(Vec::new())
    }

    fn on_market(&mut self, _ev: &Event) -> Result<Vec<Event>, KernelError> {
        Ok(Vec::new())
    }

    fn poll(&mut self) -> Result<Vec<Event>, KernelError> {
        Ok(Vec::new())
    }

    fn finish_bar(&mut self, _core: &mut KernelCore) -> Result<Vec<Event>, KernelError> {
        Ok(Vec::new())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// 回测执行出口：L1 撮合 + `BacktestEngine` 全量账务
///
/// 与 `axon-worker::BacktestRunner` 共用同一份引擎语义；差异只在于目标位来自
/// `OrderIntent::SetTarget` 的权益占比，而不是策略自报的绝对数量。
#[derive(Debug)]
pub struct BacktestSink {
    /// 订单号计数器（`Submit` 路径用；与引擎内部的 rebalance/seed id 段错开）
    next_order_id: u64,
}

impl Default for BacktestSink {
    fn default() -> Self {
        Self::new()
    }
}

impl BacktestSink {
    /// 创建空 sink
    pub fn new() -> Self {
        Self { next_order_id: 1 }
    }
}

impl ExecutionSink for BacktestSink {
    fn submit(
        &mut self,
        intent: OrderIntent,
        core: &mut KernelCore,
        _ctx: &HostCtx,
    ) -> Result<Vec<Event>, KernelError> {
        match intent {
            // 唯一换算点：权益占比 → 绝对数量。只登记，不立即 rebalance；
            // 同一帧多个品种的目标在 finish_bar 里一次性落地。
            OrderIntent::SetTarget {
                instrument,
                target_ratio,
            } => {
                let Some(price) = core.last_price(&instrument) else {
                    tracing::debug!(
                        instrument = ?instrument,
                        "backtest sink: 无最新价，跳过目标登记"
                    );
                    return Ok(Vec::new());
                };
                if price <= 0.0 {
                    tracing::debug!(
                        instrument = ?instrument,
                        price,
                        "backtest sink: 价格非正，跳过目标登记"
                    );
                    return Ok(Vec::new());
                }
                let quantity = target_ratio * core.equity() / price;
                core.set_effective_target(&instrument, quantity);
                Ok(Vec::new())
            }
            // 指定数量下单：直接进引擎队列并排空，账务/统计口径与
            // `BacktestEngine::handle_submit` 完全一致。
            OrderIntent::Submit(spec) => {
                let order_id = self.next_order_id;
                self.next_order_id += 1;
                let (order_type, tif) = match spec.price {
                    Some(price) => (OrderType::Limit { price }, TimeInForce::GTC),
                    None => (OrderType::Market, TimeInForce::IOC),
                };
                let order = match &spec.instrument {
                    axon_core::Instrument::Spot(s) => Order::spot(
                        order_id,
                        s.base.clone(),
                        s.quote.clone(),
                        spec.side,
                        order_type,
                        spec.quantity,
                        tif,
                    ),
                    axon_core::Instrument::Swap(s) => Order::swap(
                        order_id,
                        s.base.clone(),
                        s.quote.clone(),
                        s.settle,
                        s.contract_size,
                        spec.side,
                        order_type,
                        spec.quantity,
                        tif,
                    ),
                };
                let event = Event::Order(OrderEvent {
                    seq: 0,
                    // 用网格时间戳而不是收市时刻：引擎 dispatch 会把时钟设为
                    // 事件时间戳，写收市时刻会让本 bar 的后续账务跨到下一帧。
                    timestamp: core.frame_ts(),
                    order_id,
                    action: OrderAction::Submitted(order),
                });
                core.push_event(event);
                core.drain();
                Ok(Vec::new())
            }
            // M1 不做撤单（回测撮合是全同步的，没有可撤的挂单状态）。
            OrderIntent::Cancel(_) => {
                Err(KernelError::Sink("cancel not supported in M1".to_string()))
            }
        }
    }

    fn on_market(&mut self, _ev: &Event) -> Result<Vec<Event>, KernelError> {
        Ok(Vec::new())
    }

    fn poll(&mut self) -> Result<Vec<Event>, KernelError> {
        Ok(Vec::new())
    }

    fn finish_bar(&mut self, core: &mut KernelCore) -> Result<Vec<Event>, KernelError> {
        core.apply_effective_targets();
        core.drain();
        Ok(Vec::new())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Paper 执行出口：L1 撮合 + 实时报价驱动的虚拟账
///
/// 账务与撮合完全复用 [`BacktestSink`]（同一个 `BacktestEngine` 虚拟账），差异
/// 只在「最新价」的来源：回测用收市 bar 的 close 前向填充，paper 额外吸收实时
/// 成交价与盘口 mid，下一次 `SetTarget` 换算数量时用最新报价。
///
/// `poll` 恒为空：真实交易所回报（订单 / 成交推送）由 M5 的 `LiveSink` 接入；
/// 真实余额对账在 M5/M6 接。设计文档 4.5 的 `PaperSink = L1 撮合 + 实时盘口报价`。
///
/// ponytail: `MarketDataPayload::Tick` / `OrderBookSnapshot` 不带 instrument
/// （市场数据契约），报价只能归因到最近一根 `BarClosed` 的品种。单品种 paper
/// 完全正确；多品种需要 M5 接 WS 时在泵侧补一条带品种的报价通道，而不是改
/// 市场数据契约。
#[derive(Debug, Default)]
pub struct PaperSink {
    /// L1 撮合 + 全量账务（与回测同一实现）
    inner: BacktestSink,
    /// 各品种最新报价（BarClosed 的 close / Tick 的价 / 盘口 mid）
    last_price: HashMap<Instrument, f64>,
    /// 最近一根收市 bar 的品种——Tick / Depth 载荷不带 instrument，用它归因
    active: Option<Instrument>,
}

impl PaperSink {
    /// 创建 paper sink
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一条报价（仅对 `active` 品种生效）
    fn record(&mut self, price: f64) {
        if !price.is_finite() || price <= 0.0 {
            return;
        }
        if let Some(instrument) = &self.active {
            self.last_price.insert(instrument.clone(), price);
        }
    }
}

impl ExecutionSink for PaperSink {
    fn submit(
        &mut self,
        intent: OrderIntent,
        core: &mut KernelCore,
        ctx: &HostCtx,
    ) -> Result<Vec<Event>, KernelError> {
        // 目标换算点仍是 BacktestSink；先把 paper 记到的最新报价写进账务核心，
        // 换算与盯市都用它，而不是回测路径的前向填充价。
        if let OrderIntent::SetTarget { instrument, .. } = &intent
            && let Some(price) = self.last_price.get(instrument).copied()
        {
            core.update_last_price(instrument, price);
        }
        self.inner.submit(intent, core, ctx)
    }

    fn on_market(&mut self, ev: &Event) -> Result<Vec<Event>, KernelError> {
        if let Event::MarketData(md) = ev {
            match &md.payload {
                MarketDataPayload::BarClosed {
                    instrument, bar, ..
                } => {
                    self.active = Some(instrument.clone());
                    self.record(bar.close.as_f64());
                }
                MarketDataPayload::Tick(tick) => self.record(tick.price.as_f64()),
                MarketDataPayload::OrderBookSnapshot(book) => {
                    if let Some(mid) = book.mid_price() {
                        self.record(mid.as_f64());
                    }
                }
                // 其它载荷（Bar）与 paper 的报价驱动无关
                _ => {}
            }
        }
        self.inner.on_market(ev)
    }

    fn poll(&mut self) -> Result<Vec<Event>, KernelError> {
        // paper 没有真实交易所回报队列；LiveSink（M5）才需要实现
        Ok(Vec::new())
    }

    fn finish_bar(&mut self, core: &mut KernelCore) -> Result<Vec<Event>, KernelError> {
        self.inner.finish_bar(core)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use axon_core::market::Side;
    use axon_core::types::{SpotInstrument, Symbol};
    use axon_core::{Bar, Instrument, MarketDataEvent, Price, Quantity, Tick, Timestamp};

    use super::*;
    use crate::core::{KernelConfig, symbol_key};

    fn btc() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        })
    }

    #[test]
    fn test_set_target_converts_ratio_to_quantity() {
        let config = KernelConfig {
            initial_cash: 10_000.0,
            ..KernelConfig::default()
        };
        let mut core = KernelCore::new(&config);
        core.update_last_price(&btc(), 100.0);

        let mut sink = BacktestSink::new();
        sink.submit(
            OrderIntent::SetTarget {
                instrument: btc(),
                target_ratio: 0.5,
            },
            &mut core,
            &HostCtx::default(),
        )
        .expect("换算不应失败");

        let target = core.effective_targets()[&symbol_key(&btc())];
        assert!(
            (target - 50.0).abs() < 1e-9,
            "0.5 × 10000 / 100 = 50，实际 {target}"
        );
        assert_eq!(core.get_position(&btc()), 0.0, "登记目标不应立即成交");
    }

    #[test]
    fn test_set_target_without_price_is_skipped() {
        let mut core = KernelCore::new(&KernelConfig::default());
        let mut sink = BacktestSink::new();
        sink.submit(
            OrderIntent::SetTarget {
                instrument: btc(),
                target_ratio: 1.0,
            },
            &mut core,
            &HostCtx::default(),
        )
        .expect("缺价应静默跳过");
        assert!(core.effective_targets().is_empty());
    }

    /// 护栏：`set_target_position` 只记录意图，必须 `rebalance_to_target` 才成交
    ///
    /// 如果后来人删掉 `BacktestSink::finish_bar` 里的落地动作（或拆掉
    /// `KernelCore::apply_effective_targets` 中的 rebalance），本测试会失败：
    /// 目标登记后持仓与成交数都必须仍为 0，`finish_bar` 之后才允许 > 0。
    #[test]
    fn test_finish_bar_is_required_to_fill() {
        let config = KernelConfig {
            initial_cash: 10_000.0,
            ..KernelConfig::default()
        };
        let mut core = KernelCore::new(&config);
        core.set_clock(Timestamp::from_nanos(1_700_000_000_000_000_000));
        core.update_last_price(&btc(), 100.0);
        // seed 流动性（默认 0.0005 / 5 层 / 每层 100）让市价单有对手盘
        core.begin_bar_multi();

        let mut sink = BacktestSink::new();
        // 1) 只登记目标，不 rebalance：不得成交
        core.set_effective_target(&btc(), 10.0);
        core.drain();
        assert_eq!(
            core.get_position(&btc()),
            0.0,
            "set_target_position 只记录意图"
        );
        assert_eq!(core.run().fills, 0, "没有 rebalance 就没有成交");

        // 2) finish_bar = set_target_position + rebalance_to_target → 必须成交
        sink.finish_bar(&mut core).expect("finish_bar 不应失败");
        assert!(
            core.get_position(&btc()) > 0.0,
            "finish_bar 必须触发 rebalance 并把仓位撮合到位"
        );
        assert!(core.run().fills > 0, "rebalance 后必须产生成交");
    }

    fn market(ts: i64, payload: MarketDataPayload) -> Event {
        Event::MarketData(MarketDataEvent::new(0, Timestamp::from_nanos(ts), payload))
    }

    /// PaperSink 的实时报价必须参与下一次 `SetTarget` 的换算
    ///
    /// 若 `submit` 里不再把最新报价写回 `KernelCore`，换算会退回 bar close
    /// （100），目标数量变成 50 而不是 25，本测试立刻失败。
    #[test]
    fn test_paper_sink_quote_drives_target_conversion() {
        let config = KernelConfig {
            initial_cash: 10_000.0,
            ..KernelConfig::default()
        };
        let mut core = KernelCore::new(&config);
        let mut sink = PaperSink::new();

        let bar = Bar {
            timestamp: Timestamp::from_nanos(0),
            open: Price::from_f64(100.0),
            high: Price::from_f64(100.0),
            low: Price::from_f64(100.0),
            close: Price::from_f64(100.0),
            volume: Quantity::from_f64(1.0),
        };
        // 收市 bar 建立 active 品种
        sink.on_market(&market(
            60_000_000_000,
            MarketDataPayload::BarClosed {
                instrument: btc(),
                interval: "1m".to_string(),
                bar,
            },
        ))
        .expect("on_market 不应失败");
        // 实时成交价 200（Tick 载荷不带 instrument，归因到 active 品种）
        sink.on_market(&market(
            90_000_000_000,
            MarketDataPayload::Tick(Tick::new(
                Timestamp::from_nanos(90_000_000_000),
                Price::from_f64(200.0),
                Quantity::from_f64(1.0),
                Side::Buy,
            )),
        ))
        .expect("on_market 不应失败");

        sink.submit(
            OrderIntent::SetTarget {
                instrument: btc(),
                target_ratio: 0.5,
            },
            &mut core,
            &HostCtx::default(),
        )
        .expect("换算不应失败");

        assert_eq!(core.last_price(&btc()), Some(200.0), "最新报价必须写回账务");
        let target = core.effective_targets()[&symbol_key(&btc())];
        assert!(
            (target - 25.0).abs() < 1e-9,
            "0.5 × 10000 / 200 = 25，实际 {target}"
        );
    }

    #[test]
    fn test_paper_sink_poll_is_empty() {
        let mut sink = PaperSink::new();
        assert!(sink.poll().expect("poll 不应失败").is_empty());
    }
}
