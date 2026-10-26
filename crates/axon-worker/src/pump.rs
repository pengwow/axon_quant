//! 实时数据泵：交易所 WS 消息 → 内核事件
//!
//! 回测路径由 `axon_kernel::build_frames` 直接灌 `BarClosed`，paper / live 则
//! 把 `WsMessage` 泵成同一套事件（设计文档 5 章的装配矩阵）。本模块负责三件事：
//!
//! 1. **格式转换**：`Kline` → 收市 `BarClosed`（未收市只更新进行中 bar），
//!    `Trade` → `Tick`，`Depth` → `OrderBookSnapshot`，`OrderUpdate` → 系统事件；
//! 2. **去重**：沿用 `axon-kernel` `BarAggregator` 的 `(instrument, interval, bar.timestamp)` 去重；
//! 3. **缺口检测**：同一 `(instrument, interval)` 的 bar 起始时间跳过一个整周期
//!    以上时记一条 [`Gap`]，供上层断线后调 `fetch_ohlcv` 补齐。
//!
//! # 时间戳契约
//!
//! `WsMessage::Kline.timestamp` 必须是 **bar 的起始时间**（Binance `k.t`），
//! 不是消息接收时间：去重、缺口检测、收市时刻推导都以它为准。
//!
//! # 边界
//!
//! - `OrderUpdate` 先原样转成 `Event::System`，M5 接入 OMS 后再细化为
//!   `Event::Order` / `Event::Fill`；
//! - `Ticker` / `Ping` / `Pong` 不产生事件（paper 的报价由 `Trade` / `Depth` 驱动）；
//! - 品种解析口径由构造参数决定：缺省现货（`BTCUSDT` → `BTC/USDT`），
//!   live 跑永续时用 `with_instrument_mode` 传 `InstrumentMode::Swap`。

use std::collections::{HashMap, HashSet};

use axon_core::{
    Bar, Event, Instrument, MarketDataEvent, MarketDataPayload, OrderBookLevel, OrderBookSnapshot,
    Price, Quantity, Side, SystemAction, SystemEvent, Tick, Timestamp,
};
use axon_exchange::types::{Kline, Side as WsSide, Symbol as ExchangeSymbol, WsMessage};
use axon_kernel::{BarAggregator, InstrumentMode, interval_to_nanos};
use tokio::sync::mpsc;

use crate::backtest::instrument_key_for;

/// 一次排空的产物
#[derive(Debug, Default)]
pub struct KernelPumpBatch {
    /// 需要入队的市场 / 系统事件（`seq` 已按到达顺序重编号）
    pub events: Vec<Event>,
    /// 被去重丢弃的重复收市 bar 数
    pub skipped_duplicates: usize,
}

/// 断线补齐所需的缺口区间（半开 `[start, end)`）
///
/// `start` 是第一个缺失 bar 的起始时间，`end` 是缺口之后第一根已恢复 bar 的
/// 起始时间。上层按这两个边界调 `fetch_ohlcv` 补历史即可。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gap {
    /// 缺口的品种
    pub instrument: Instrument,
    /// K 线周期（`"1m"` / `"15m"` / …）
    pub interval: String,
    /// 缺口起点（缺失的第一根 bar 的起始时间）
    pub start: Timestamp,
    /// 缺口终点（恢复后的第一根 bar 的起始时间）
    pub end: Timestamp,
}

/// 把交易所 WS 消息泵成内核事件的实时数据泵
pub struct KernelPump {
    rx: mpsc::Receiver<WsMessage>,
    aggregator: BarAggregator,
    /// 品种解析口径：缺省 Spot，live 永续传 Swap（`with_instrument_mode`）
    instrument_mode: InstrumentMode,
    /// 每个 `(instrument, interval)` 已见到的最新 bar 起始时间（缺口检测用）
    last_bar_start: HashMap<(Instrument, String), i64>,
    /// 缺口补灌时回灌 bar 的起始时间记录（按 `(instrument, interval)` 分组）。
    /// 判缺口覆盖不用聚合器的 seen：它每 key 只保留最近 16 根，长缺口会丢记录；
    /// 这里自建全集，settle 后按剩余缺口窗口剪枝
    replayed: HashMap<(Instrument, String), HashSet<i64>>,
    gaps: Vec<Gap>,
    next_seq: u64,
}

impl KernelPump {
    /// 用交易所行情 channel 的接收端创建泵（缺省按现货解析品种）
    pub fn new(rx: mpsc::Receiver<WsMessage>) -> Self {
        Self::with_instrument_mode(rx, InstrumentMode::Spot)
    }

    /// 指定品种解析口径创建泵：live 永续链路传 `InstrumentMode::Swap`，
    /// 让泵产出的 bar 与内核屏障 / LiveSink 的 Swap 品种一致
    pub fn with_instrument_mode(
        rx: mpsc::Receiver<WsMessage>,
        instrument_mode: InstrumentMode,
    ) -> Self {
        Self {
            rx,
            aggregator: BarAggregator::new(),
            instrument_mode,
            last_bar_start: HashMap::new(),
            replayed: HashMap::new(),
            gaps: Vec::new(),
            next_seq: 0,
        }
    }

    /// 非阻塞排空：把当前可读消息转成事件
    ///
    /// 返回的 `events` 由调用方通过 `Kernel::enqueue_external` 入队（行情是外部
    /// 输入，必须进 journal）；`skipped_duplicates` 只是观测指标，不影响决策。
    pub fn drain(&mut self) -> KernelPumpBatch {
        let mut batch = KernelPumpBatch::default();
        while let Ok(msg) = self.rx.try_recv() {
            let events = match msg {
                WsMessage::Kline(kline) => self.on_kline(&kline, &mut batch.skipped_duplicates),
                WsMessage::Trade(trade) => vec![Event::MarketData(MarketDataEvent::new(
                    0,
                    nanos(trade.timestamp),
                    MarketDataPayload::Tick(Tick::new(
                        nanos(trade.timestamp),
                        Price::from_f64(trade.price.as_f64()),
                        Quantity::from_f64(trade.quantity.as_f64()),
                        match trade.side {
                            WsSide::Buy => Side::Buy,
                            WsSide::Sell => Side::Sell,
                        },
                    )),
                ))],
                WsMessage::Depth(depth) => self.on_depth(&depth),
                WsMessage::OrderUpdate(update) => {
                    // TODO(M5): 细化成 Event::Order / Event::Fill —— 需要 OMS
                    // 订单簿记与「交易所 order_id ↔ 内核 order_id」映射，M4 先
                    // 原样保留为系统事件，保证回报不丢。
                    let value = serde_json::json!({
                        "order_id": update.order_id,
                        "client_order_id": update.client_order_id.to_string(),
                        "status": update.status,
                        "filled_qty": update.filled_qty.to_string(),
                        "avg_price": update.avg_price.map(|price| price.to_string()),
                    });
                    vec![Event::System(SystemEvent::new(
                        0,
                        nanos(update.timestamp),
                        SystemAction::Custom {
                            key: "exchange_order_update".to_string(),
                            value: value.to_string(),
                        },
                    ))]
                }
                // Ticker 只反映 24h 统计，不构成逐笔报价；Ping / Pong 是链路保活
                WsMessage::Ticker(_) | WsMessage::Ping | WsMessage::Pong => Vec::new(),
            };
            batch.events.extend(events);
        }

        self.renumber(&mut batch.events);
        batch
    }

    /// 断线补齐所需的缺口区间（按发现顺序累积）
    pub fn gaps(&self) -> &[Gap] {
        &self.gaps
    }

    /// 按到达顺序重编号事件的 seq（`drain` 与 `replay_bars` 共用）
    fn renumber(&mut self, events: &mut [Event]) {
        for event in events {
            match event {
                Event::MarketData(md) => {
                    md.seq = self.next_seq;
                    self.next_seq += 1;
                }
                Event::System(sys) => {
                    sys.seq = self.next_seq;
                    self.next_seq += 1;
                }
                _ => {}
            }
        }
    }

    /// 断线补齐：把 `fetch_ohlcv` 拉回的历史 bar 灌回同一条事件管道
    ///
    /// 走与实时 kline 相同的聚合 / 去重路径（重复 bar 只计数不重放）；事件按
    /// 补灌到达顺序重编号，随缺口一并清除后由调用方 `enqueue_external` 进内核
    /// （与 `drain` 的下游约定一致）。事件时间戳是收市时刻，EventQueue 会把它们
    /// 排到正确的历史位置，调用方无需关心补灌顺序与实时 bar 的交错。
    pub fn replay_bars(
        &mut self,
        symbol: &ExchangeSymbol,
        interval: &str,
        bars: &[Bar],
    ) -> KernelPumpBatch {
        let mut batch = KernelPumpBatch::default();
        let instrument = self
            .instrument_mode
            .instrument(&instrument_key_for(&symbol.0));
        if instrument.base().as_str().is_empty() || instrument.quote().as_str().is_empty() {
            tracing::warn!(symbol = %symbol.0, "kernel pump: 无法解析交易对，丢弃补灌 bar");
            return batch;
        }
        let key = (instrument.clone(), interval.to_string());
        {
            // 先记录全量「回灌存在」（含将被去重的重复 bar）：重复 = 此前已处理
            // = 不缺，settle 靠这份完整集合判定缺口覆盖。块作用域结束借用，
            // 后面才能调 track_closed 等 &mut self 方法
            let seen = self.replayed.entry(key).or_default();
            seen.extend(bars.iter().map(|bar| bar.timestamp.nanos));
        }
        let mut events = Vec::with_capacity(bars.len());
        for bar in bars {
            // 与实时路径同一判重来源（BarAggregator），重复补灌只计数
            if self.aggregator.is_duplicate(&(
                instrument.clone(),
                interval.to_string(),
                bar.timestamp.nanos,
            )) {
                batch.skipped_duplicates += 1;
                continue;
            }
            let bar_events =
                self.aggregator
                    .observe_kline(&instrument, interval, *bar, true, bar.timestamp);
            self.track_closed(&bar_events);
            events.extend(bar_events);
        }
        self.renumber(&mut events);
        batch.events = events;
        self.settle_gaps(&instrument, interval);
        batch
    }

    /// 结算缺口：按补灌记录清掉已完全覆盖的缺口，并剪枝补灌时间戳集合
    ///
    /// 覆盖判定 = 缺口区间 `[start, end)` 内每个整周期时间戳都出现在补灌记录里；
    /// 空区间（`end <= start`）显式拒绝——`Range::step_by` 对空区间恒返回 true，
    /// 会把病态缺口误判成已补齐。
    fn settle_gaps(&mut self, instrument: &Instrument, interval: &str) {
        let Some(interval_ns) = interval_to_nanos(interval) else {
            return;
        };
        let key = (instrument.clone(), interval.to_string());
        let old_gaps = std::mem::take(&mut self.gaps);
        let mut kept = Vec::with_capacity(old_gaps.len());
        {
            let seen = self.replayed.get(&key);
            for gap in old_gaps {
                let covered = gap.instrument == *instrument
                    && gap.interval == interval
                    && seen.is_some_and(|set| {
                        gap.end.nanos > gap.start.nanos
                            && (gap.start.nanos..gap.end.nanos)
                                .step_by(interval_ns as usize)
                                .all(|ts| set.contains(&ts))
                    });
                if !covered {
                    kept.push(gap);
                }
            }
        }
        self.gaps = kept;
        // 剪枝：补灌记录只保留仍落在剩余缺口窗口内的时间戳，防无界增长
        let windows: Vec<(i64, i64)> = self
            .gaps
            .iter()
            .filter(|gap| gap.instrument == *instrument && gap.interval == interval)
            .map(|gap| (gap.start.nanos, gap.end.nanos))
            .collect();
        if let Some(seen) = self.replayed.get_mut(&key) {
            seen.retain(|ts| windows.iter().any(|&(lo, hi)| lo <= *ts && *ts < hi));
        }
        if self.replayed.get(&key).is_some_and(|set| set.is_empty()) {
            self.replayed.remove(&key);
        }
    }

    /// 单条 kline：未收市只更新聚合器，收市产出 `BarClosed` 并做缺口检测
    fn on_kline(&mut self, kline: &Kline, skipped_duplicates: &mut usize) -> Vec<Event> {
        // 先按交易所原始写法归一化成带分隔符的 key（复用 `instrument_key_for`，
        // 与 QuantCell 的 `_normalize_symbol` 同口径），再按构造口径解析成
        // Spot / Swap 品种——live 永续的屏障与账务都按 Swap instrument 走。
        let instrument = self
            .instrument_mode
            .instrument(&instrument_key_for(&kline.symbol.0));
        if instrument.base().as_str().is_empty() || instrument.quote().as_str().is_empty() {
            tracing::warn!(
                symbol = %kline.symbol,
                "kernel pump: 无法解析交易对，丢弃 kline"
            );
            return Vec::new();
        }
        let bar = Bar {
            timestamp: nanos(kline.timestamp),
            open: Price::from_f64(kline.open.as_f64()),
            high: Price::from_f64(kline.high.as_f64()),
            low: Price::from_f64(kline.low.as_f64()),
            close: Price::from_f64(kline.close.as_f64()),
            volume: Quantity::from_f64(kline.volume.as_f64()),
        };
        if kline.is_closed
            && self.aggregator.is_duplicate(&(
                instrument.clone(),
                kline.interval.clone(),
                bar.timestamp.nanos,
            ))
        {
            *skipped_duplicates += 1;
            return Vec::new();
        }

        let events = self.aggregator.observe_kline(
            &instrument,
            &kline.interval,
            bar,
            kline.is_closed,
            nanos(kline.timestamp),
        );
        self.track_closed(&events);
        events
    }

    /// 单条深度快照：转成 `OrderBookSnapshot` 事件，绝不伪造 bar
    fn on_depth(&mut self, depth: &axon_exchange::types::DepthSnapshot) -> Vec<Event> {
        if depth.bids.is_empty() || depth.asks.is_empty() {
            tracing::debug!(symbol = %depth.symbol, "kernel pump: 盘口单边为空，跳过");
            return Vec::new();
        }
        // 交易所给出的 bids 降序 / asks 升序与 `OrderBookSnapshot` 契约一致，不重排
        let bids = depth
            .bids
            .iter()
            .map(|(price, quantity)| {
                OrderBookLevel::new(
                    Price::from_f64(price.as_f64()),
                    Quantity::from_f64(quantity.as_f64()),
                )
            })
            .collect();
        let asks = depth
            .asks
            .iter()
            .map(|(price, quantity)| {
                OrderBookLevel::new(
                    Price::from_f64(price.as_f64()),
                    Quantity::from_f64(quantity.as_f64()),
                )
            })
            .collect();
        vec![Event::MarketData(MarketDataEvent::new(
            0,
            nanos(depth.timestamp),
            MarketDataPayload::OrderBookSnapshot(OrderBookSnapshot {
                timestamp: nanos(depth.timestamp),
                bids,
                asks,
            }),
        ))]
    }

    /// 用已产出的收市事件更新「每个 `(instrument, interval)` 的最新 bar 起点」
    ///
    /// 跳过一个整周期以上即记缺口。`Some(prev)` 且新 bar 不比 `prev` 新时不动
    /// （乱序补发不产生负向缺口，也不会把水位往回拉）。
    fn track_closed(&mut self, events: &[Event]) {
        for event in events {
            let Event::MarketData(md) = event else {
                continue;
            };
            let MarketDataPayload::BarClosed {
                instrument,
                interval,
                bar,
            } = &md.payload
            else {
                continue;
            };
            let Some(interval_ns) = interval_to_nanos(interval) else {
                continue;
            };
            let key = (instrument.clone(), interval.clone());
            let start = bar.timestamp.nanos;
            match self.last_bar_start.get(&key).copied() {
                Some(prev) if start > prev + interval_ns => {
                    self.gaps.push(Gap {
                        instrument: instrument.clone(),
                        interval: interval.clone(),
                        start: Timestamp::from_nanos(prev + interval_ns),
                        end: bar.timestamp,
                    });
                    self.last_bar_start.insert(key, start);
                }
                Some(prev) if start > prev => {
                    self.last_bar_start.insert(key, start);
                }
                Some(_) => {}
                None => {
                    self.last_bar_start.insert(key, start);
                }
            }
        }
    }
}

/// `chrono::DateTime<Utc>` → 纳秒 `Timestamp`
fn nanos(ts: chrono::DateTime<chrono::Utc>) -> Timestamp {
    Timestamp::from_nanos(
        ts.timestamp_nanos_opt()
            .unwrap_or_else(|| ts.timestamp_millis() * 1_000_000),
    )
}

#[cfg(test)]
mod tests {
    use axon_core::types::{SpotInstrument, SwapSettle, Symbol};

    use super::*;

    const MIN_MS: i64 = 60_000;
    const MIN_NS: i64 = 60_000_000_000;

    fn pump_channel() -> (mpsc::Sender<WsMessage>, KernelPump) {
        let (tx, rx) = mpsc::channel(8);
        (tx, KernelPump::new(rx))
    }

    fn btc() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        })
    }

    fn rfc3339(ms: i64) -> String {
        chrono::DateTime::from_timestamp_millis(ms)
            .expect("测试时间戳合法")
            .to_rfc3339()
    }

    /// 用交易所原始 JSON 形状构造消息（同时验证 serde 契约）
    fn kline(start_ms: i64, is_closed: bool) -> WsMessage {
        serde_json::from_value(serde_json::json!({
            "type": "Kline",
            "data": {
                "symbol": "BTCUSDT",
                "interval": "1m",
                "open": "100.0",
                "high": "101.0",
                "low": "99.0",
                "close": "100.5",
                "volume": "10.0",
                "timestamp": rfc3339(start_ms),
                "is_closed": is_closed,
            }
        }))
        .expect("Kline 消息应能反序列化")
    }

    #[test]
    fn test_duplicate_bar_is_dropped_and_counted() {
        let (tx, mut pump) = pump_channel();
        tx.try_send(kline(0, true)).expect("channel 容量足够");
        tx.try_send(kline(0, true)).expect("channel 容量足够");

        let batch = pump.drain();
        assert_eq!(batch.events.len(), 1, "重复收市 bar 只能放行一次");
        assert_eq!(batch.skipped_duplicates, 1);
    }

    #[test]
    fn test_missing_period_is_recorded_as_gap() {
        let (tx, mut pump) = pump_channel();
        tx.try_send(kline(0, true)).expect("channel 容量足够");
        // 跳过 00:01 整根 bar，直接到 00:02
        tx.try_send(kline(2 * MIN_MS, true))
            .expect("channel 容量足够");

        let batch = pump.drain();
        assert_eq!(batch.events.len(), 2);
        assert_eq!(
            pump.gaps(),
            &[Gap {
                instrument: btc(),
                interval: "1m".to_string(),
                start: Timestamp::from_nanos(MIN_NS),
                end: Timestamp::from_nanos(2 * MIN_NS),
            }],
            "缺一根 1m bar 必须记一条缺口"
        );
    }

    /// 构造内核 Bar（`replay_bars` 入参形状，非 WS JSON；价格主体随意，去重 /
    /// 缺口检测只用时间戳）
    fn bar(start_ns: i64, close: f64) -> Bar {
        Bar {
            timestamp: Timestamp::from_nanos(start_ns),
            open: Price::from_f64(100.0),
            high: Price::from_f64(101.0),
            low: Price::from_f64(99.0),
            close: Price::from_f64(close),
            volume: Quantity::from_f64(10.0),
        }
    }

    fn replay_btc(pump: &mut KernelPump, bars: &[Bar]) -> KernelPumpBatch {
        pump.replay_bars(&ExchangeSymbol("BTCUSDT".to_string()), "1m", bars)
    }

    #[test]
    fn test_replay_backfills_gap_once_and_dedups_on_repeat() {
        let (tx, mut pump) = pump_channel();
        tx.try_send(kline(0, true)).expect("channel 容量足够");
        tx.try_send(kline(2 * MIN_MS, true))
            .expect("channel 容量足够");
        pump.drain();
        assert_eq!(pump.gaps().len(), 1, "铺垫：缺 00:01 应记一条缺口");

        // 补灌缺失的 00:01：缺口收敛
        let batch = replay_btc(&mut pump, &[bar(MIN_NS, 100.5)]);
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.skipped_duplicates, 0);
        assert!(pump.gaps().is_empty(), "补齐后缺口必须清除");

        // 同一根 bar 二次回灌：去重丢弃、不产事件、也不重开缺口
        let batch = replay_btc(&mut pump, &[bar(MIN_NS, 100.5)]);
        assert!(batch.events.is_empty(), "重复补灌只应记 skipped: {batch:?}");
        assert_eq!(batch.skipped_duplicates, 1);
        assert!(pump.gaps().is_empty());
    }

    #[test]
    fn test_gap_survives_until_fully_covered() {
        let (tx, mut pump) = pump_channel();
        tx.try_send(kline(0, true)).expect("channel 容量足够");
        // 缺 00:01、00:02 两根，直接到 00:03
        tx.try_send(kline(3 * MIN_MS, true))
            .expect("channel 容量足够");
        pump.drain();
        assert_eq!(pump.gaps().len(), 1);

        // 只补 00:01：区间 [00:01, 00:03) 未全覆盖，缺口必须保留（防漏灌误清）
        let batch = replay_btc(&mut pump, &[bar(MIN_NS, 100.5)]);
        assert_eq!(batch.events.len(), 1);
        assert_eq!(pump.gaps().len(), 1, "缺口未补齐不能清除");

        // 补 00:02：全覆盖，缺口清除（00:01 已注册进聚合器，本次只产 1 事件）
        let batch = replay_btc(&mut pump, &[bar(2 * MIN_NS, 100.5)]);
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.skipped_duplicates, 0);
        assert!(pump.gaps().is_empty());
    }

    #[test]
    fn test_closed_kline_event_ts_is_close_time() {
        let (tx, mut pump) = pump_channel();
        tx.try_send(kline(0, true)).expect("channel 容量足够");

        let batch = pump.drain();
        assert_eq!(batch.events.len(), 1);
        let Event::MarketData(md) = &batch.events[0] else {
            panic!("Kline 必须转成市场数据事件");
        };
        assert_eq!(md.timestamp.nanos, MIN_NS, "事件 ts 必须是收市时刻");
        let MarketDataPayload::BarClosed {
            instrument, bar, ..
        } = &md.payload
        else {
            panic!("必须是 BarClosed");
        };
        assert_eq!(instrument, &btc(), "BTCUSDT 必须归一化成 BTC/USDT");
        assert_eq!(bar.timestamp.nanos, 0, "bar.timestamp 仍是起始时间");
    }

    #[test]
    fn test_swap_mode_resolves_swap_instrument() {
        let (tx, rx) = mpsc::channel(8);
        let mut pump = KernelPump::with_instrument_mode(
            rx,
            InstrumentMode::Swap {
                settle: SwapSettle::UsdMargin,
                contract_size: 1.0,
            },
        );
        tx.try_send(kline(0, true)).expect("channel 容量足够");

        let batch = pump.drain();
        let Event::MarketData(md) = &batch.events[0] else {
            panic!("Kline 必须转成市场数据事件");
        };
        let MarketDataPayload::BarClosed { instrument, .. } = &md.payload else {
            panic!("必须是 BarClosed");
        };
        assert!(
            matches!(instrument, Instrument::Swap(_)),
            "Swap 模式下 BTCUSDT 必须解析成永续品种：{instrument:?}"
        );
    }

    #[test]
    fn test_trade_and_depth_map_to_tick_and_book() {
        let (tx, mut pump) = pump_channel();
        tx.try_send(
            serde_json::from_value(serde_json::json!({
                "type": "Trade",
                "data": {
                    "symbol": "BTCUSDT",
                    "price": "100.0",
                    "quantity": "2.0",
                    "side": "buy",
                    "timestamp": rfc3339(30_000),
                }
            }))
            .expect("Trade 消息应能反序列化"),
        )
        .expect("channel 容量足够");
        tx.try_send(
            serde_json::from_value(serde_json::json!({
                "type": "Depth",
                "data": {
                    "symbol": "BTCUSDT",
                    "bids": [["100.0", "1.0"]],
                    "asks": [["101.0", "2.0"]],
                    "timestamp": rfc3339(31_000),
                }
            }))
            .expect("Depth 消息应能反序列化"),
        )
        .expect("channel 容量足够");

        let batch = pump.drain();
        assert_eq!(batch.events.len(), 2);

        let Event::MarketData(tick_event) = &batch.events[0] else {
            panic!("Trade 必须转成市场数据事件");
        };
        let MarketDataPayload::Tick(tick) = &tick_event.payload else {
            panic!("Trade 必须转成 Tick");
        };
        assert_eq!(tick.price.as_f64(), 100.0);
        assert_eq!(tick.quantity.as_f64(), 2.0);
        assert_eq!(tick.side, Side::Buy);

        let Event::MarketData(depth_event) = &batch.events[1] else {
            panic!("Depth 必须转成市场数据事件");
        };
        let MarketDataPayload::OrderBookSnapshot(book) = &depth_event.payload else {
            panic!("Depth 必须转成 OrderBookSnapshot，而不是伪造 bar");
        };
        assert_eq!(
            book.best_bid().map(|level| level.price.as_f64()),
            Some(100.0)
        );
        assert_eq!(
            book.best_ask().map(|level| level.price.as_f64()),
            Some(101.0)
        );
        assert_eq!(book.mid_price().map(|price| price.as_f64()), Some(100.5));
    }
}
