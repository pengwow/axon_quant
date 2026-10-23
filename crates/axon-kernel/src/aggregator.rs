//! tick / kline → 收市 `BarClosed` 事件的聚合器
//!
//! 实盘数据泵（`axon_worker::pump::KernelPump`）把交易所 WS 消息转成本模块的输入：
//!
//! - tick（逐笔成交）走 [`BarAggregator::observe_tick`]，跨周期时收市上一根 bar；
//! - 未收市 kline 走 [`BarAggregator::observe_kline`]（`is_closed = false`），
//!   只更新进行中 bar 的 OHLCV；
//! - 已收市 kline 走 `observe_kline`（`is_closed = true`），直接产出 `BarClosed`。
//!
//! 语义与回测路径完全一致：事件时间戳 = **收市时刻**（`bar.timestamp + interval`），
//! 不是 bar 起始时间（防前视，与 `Kernel::load_frames` 同口径）。
//!
//! # 为什么不依赖 axon-exchange
//!
//! `axon-kernel` 的依赖图里没有 `axon-exchange`（内核只认 `axon_core` 类型）。
//! 因此本模块收的是 `Bar` / `Tick`，交易所消息字段的搬运（`Decimal` → `f64`、
//! `DateTime` → 纳秒）由 `axon-worker` 的泵完成。
//!
//! # 去重
//!
//! 同一 `(instrument, interval, bar.timestamp)` 只放行一次。已收市时间戳按 key
//! 分桶只保留最近 [`SEEN_PER_KEY`] 个：断线重连补发的旧 bar 仍能识别，内存占用
//! 有上限。
//!
//! ponytail: 分桶只保留最近 16 根，极端乱序（重复 bar 迟到超过 16 个周期）会漏判。
//! 上限来自「重连补发通常只覆盖最近几根」的交易所行为；升级路径是按水位线剪枝
//! 或改用交易所自带的 `seq`。

use std::collections::{HashMap, VecDeque};

use axon_core::{
    Bar, Event, Instrument, MarketDataEvent, MarketDataPayload, Quantity, Tick, Timestamp,
};

use crate::event::{close_time, interval_to_nanos};

/// 每个 `(instrument, interval)` 保留的去重时间戳个数
const SEEN_PER_KEY: usize = 16;

/// 逐品种、逐周期的 K 线聚合器
///
/// 一个实例可以同时服务多个 `(instrument, interval)`，互不干扰。
#[derive(Debug, Default)]
pub struct BarAggregator {
    /// 进行中的 bar（key = `(instrument, interval)`）
    open: HashMap<(Instrument, String), Bar>,
    /// 已收市的 bar 起始时间（key = `(instrument, interval)`，按时间升序的环形队列）
    seen: HashMap<(Instrument, String), VecDeque<i64>>,
}

impl BarAggregator {
    /// 创建空聚合器
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂一条逐笔成交
    ///
    /// - 同一周期的 tick：更新 high / low / close 并累加 volume；
    /// - 跨到下一个周期：先收市上一根（返回 `BarClosed`），再开新 bar；
    /// - 早于进行中 bar 的乱序 tick：丢弃。
    ///
    /// `interval` 非法时返回空 vec 并告警，不 panic。
    pub fn observe_tick(
        &mut self,
        instrument: &Instrument,
        interval: &str,
        tick: &Tick,
    ) -> Vec<Event> {
        let Some(interval_ns) = interval_to_nanos(interval) else {
            tracing::warn!(interval, "kernel aggregator: 非法 interval，丢弃 tick");
            return Vec::new();
        };
        // 向下取整到周期边界 = tick 所属 bar 的起始时间
        let bar_start = tick.timestamp.nanos.div_euclid(interval_ns) * interval_ns;
        let key = (instrument.clone(), interval.to_string());

        match self.open.get(&key).map(|bar| bar.timestamp.nanos) {
            Some(current_start) if current_start == bar_start => {
                if let Some(current) = self.open.get_mut(&key) {
                    let price = tick.price;
                    if price.as_f64() > current.high.as_f64() {
                        current.high = price;
                    }
                    if price.as_f64() < current.low.as_f64() {
                        current.low = price;
                    }
                    current.close = price;
                    // 成交量为负的 tick 是脏数据，不参与累加
                    let volume = current.volume.as_f64() + tick.quantity.as_f64().max(0.0);
                    current.volume = Quantity::from_f64(volume);
                }
                Vec::new()
            }
            Some(current_start) if bar_start > current_start => {
                let closed = self
                    .open
                    .remove(&key)
                    .expect("已命中进行中 bar，remove 必须成功");
                let out = vec![self.finish(&key.0, interval, closed, interval_ns)];
                if !self.is_duplicate(&(instrument.clone(), interval.to_string(), bar_start)) {
                    self.open.insert(key, bar_from_tick(bar_start, tick));
                }
                out
            }
            Some(_) => {
                tracing::debug!(
                    instrument = %instrument,
                    interval,
                    "kernel aggregator: tick 早于进行中 bar，忽略"
                );
                Vec::new()
            }
            None => {
                if self.is_duplicate(&(instrument.clone(), interval.to_string(), bar_start)) {
                    tracing::debug!(
                        instrument = %instrument,
                        interval,
                        "kernel aggregator: 该周期已收市，忽略迟到 tick"
                    );
                } else {
                    self.open.insert(key, bar_from_tick(bar_start, tick));
                }
                Vec::new()
            }
        }
    }

    /// 喂一条 kline
    ///
    /// - `is_closed = false`：用交易所的进行中 OHLCV 覆盖本地进行中 bar；若它的
    ///   起始时间已经跨周期，先收市上一根（收市 kline 消息丢失时的兜底）；
    /// - `is_closed = true`：产出收市事件（`bar.timestamp` 是起始时间，事件 ts
    ///   取收市时刻）；
    /// - 同一 bar 重复收市：返回空 vec。
    ///
    /// `event_ts` 是交易所消息自身的到达时间，只在收市时刻无法推导（溢出）时兜底。
    /// `interval` 非法时返回空 vec 并告警，不 panic。
    pub fn observe_kline(
        &mut self,
        instrument: &Instrument,
        interval: &str,
        bar: Bar,
        is_closed: bool,
        event_ts: Timestamp,
    ) -> Vec<Event> {
        let Some(interval_ns) = interval_to_nanos(interval) else {
            tracing::warn!(interval, "kernel aggregator: 非法 interval，丢弃 kline");
            return Vec::new();
        };
        let key = (instrument.clone(), interval.to_string());
        let dup_key = (
            instrument.clone(),
            interval.to_string(),
            bar.timestamp.nanos,
        );

        if !is_closed {
            if self.is_duplicate(&dup_key) {
                tracing::debug!(
                    instrument = %instrument,
                    interval,
                    "kernel aggregator: 该周期已收市，忽略迟到未收市 kline"
                );
                return Vec::new();
            }
            if let Some(current_start) = self.open.get(&key).map(|open| open.timestamp.nanos)
                && bar.timestamp.nanos > current_start
            {
                let closed = self
                    .open
                    .remove(&key)
                    .expect("已命中进行中 bar，remove 必须成功");
                let out = vec![self.finish(instrument, interval, closed, interval_ns)];
                self.open.insert(key, bar);
                return out;
            }
            // 交易所的进行中 OHLCV 是权威值（tick 聚合缺 volume），直接覆盖
            self.open.insert(key, bar);
            return Vec::new();
        }

        if self.is_duplicate(&dup_key) {
            tracing::debug!(
                instrument = %instrument,
                interval,
                bar_timestamp = bar.timestamp.nanos,
                "kernel aggregator: 重复收市 bar，丢弃"
            );
            return Vec::new();
        }
        // 收市 kline 覆盖同一周期的进行中 bar（tick 聚合的近似值不再需要）
        self.open.remove(&key);
        let ts = close_time(bar.timestamp, interval).unwrap_or(event_ts);
        vec![self.finish_at(instrument, interval, bar, ts)]
    }

    /// 该 `(instrument, interval, bar.timestamp)` 是否已经收市过
    pub fn is_duplicate(&self, key: &(Instrument, String, i64)) -> bool {
        self.seen
            .get(&(key.0.clone(), key.1.clone()))
            .is_some_and(|stamps| stamps.contains(&key.2))
    }

    /// tick 路径：按周期推导收市时刻后产出事件
    fn finish(
        &mut self,
        instrument: &Instrument,
        interval: &str,
        bar: Bar,
        interval_ns: i64,
    ) -> Event {
        let ts = Timestamp::from_nanos(bar.timestamp.nanos + interval_ns);
        self.finish_at(instrument, interval, bar, ts)
    }

    /// 产出收市事件并登记去重
    fn finish_at(
        &mut self,
        instrument: &Instrument,
        interval: &str,
        bar: Bar,
        ts: Timestamp,
    ) -> Event {
        self.mark_seen(instrument, interval, bar.timestamp.nanos);
        Event::MarketData(MarketDataEvent::new(
            0,
            ts,
            MarketDataPayload::BarClosed {
                instrument: instrument.clone(),
                interval: interval.to_string(),
                bar,
            },
        ))
    }

    /// 登记一个已收市时间戳（超出 [`SEEN_PER_KEY`] 时弹出最旧的）
    fn mark_seen(&mut self, instrument: &Instrument, interval: &str, bar_start: i64) {
        let stamps = self
            .seen
            .entry((instrument.clone(), interval.to_string()))
            .or_default();
        if stamps.contains(&bar_start) {
            return;
        }
        stamps.push_back(bar_start);
        if stamps.len() > SEEN_PER_KEY {
            stamps.pop_front();
        }
    }
}

/// 由 tick 开一根新 bar
fn bar_from_tick(bar_start: i64, tick: &Tick) -> Bar {
    Bar {
        timestamp: Timestamp::from_nanos(bar_start),
        open: tick.price,
        high: tick.price,
        low: tick.price,
        close: tick.price,
        volume: Quantity::from_f64(tick.quantity.as_f64().max(0.0)),
    }
}

#[cfg(test)]
mod tests {
    use axon_core::Price;
    use axon_core::market::Side;
    use axon_core::types::{SpotInstrument, Symbol};

    use super::*;

    const SEC: i64 = 1_000_000_000;
    const MIN: i64 = 60 * SEC;

    fn btc() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        })
    }

    fn tick(nanos: i64, price: f64, quantity: f64) -> Tick {
        Tick::new(
            Timestamp::from_nanos(nanos),
            Price::from_f64(price),
            Quantity::from_f64(quantity),
            Side::Buy,
        )
    }

    fn bar(start: i64, open: f64, high: f64, low: f64, close: f64) -> Bar {
        Bar {
            timestamp: Timestamp::from_nanos(start),
            open: Price::from_f64(open),
            high: Price::from_f64(high),
            low: Price::from_f64(low),
            close: Price::from_f64(close),
            volume: Quantity::from_f64(10.0),
        }
    }

    /// 拆一个 `BarClosed` 事件（返回拥有所有权的字段，避免测试里的借用纠缠）
    fn bar_closed_of(ev: &Event) -> Option<(Instrument, String, Bar, Timestamp)> {
        match ev {
            Event::MarketData(md) => match &md.payload {
                MarketDataPayload::BarClosed {
                    instrument,
                    interval,
                    bar,
                } => Some((instrument.clone(), interval.clone(), *bar, md.timestamp)),
                _ => None,
            },
            _ => None,
        }
    }

    #[test]
    fn test_observe_tick_closes_previous_bar_at_close_time() {
        let mut agg = BarAggregator::new();
        assert!(
            agg.observe_tick(&btc(), "1m", &tick(0, 100.0, 2.0))
                .is_empty()
        );
        assert!(
            agg.observe_tick(&btc(), "1m", &tick(30 * SEC, 105.0, 3.0))
                .is_empty()
        );
        assert!(
            agg.observe_tick(&btc(), "1m", &tick(40 * SEC, 98.0, 1.0))
                .is_empty()
        );

        let events = agg.observe_tick(&btc(), "1m", &tick(MIN, 101.0, 5.0));
        assert_eq!(events.len(), 1, "跨周期必须收市上一根");

        let (instrument, interval, closed, ts) =
            bar_closed_of(&events[0]).expect("必须是 BarClosed 事件");
        assert_eq!(instrument, btc());
        assert_eq!(interval, "1m");
        assert_eq!(closed.timestamp.nanos, 0, "bar 起始时间不变");
        assert_eq!(closed.open.as_f64(), 100.0);
        assert_eq!(closed.high.as_f64(), 105.0);
        assert_eq!(closed.low.as_f64(), 98.0);
        assert_eq!(closed.close.as_f64(), 98.0);
        assert!((closed.volume.as_f64() - 6.0).abs() < 1e-9, "成交量累加");
        assert_eq!(ts.nanos, MIN, "事件时间戳必须是收市时刻");
    }

    #[test]
    fn test_observe_kline_unclosed_updates_in_progress_only() {
        let mut agg = BarAggregator::new();
        let in_progress = bar(0, 100.0, 101.0, 99.0, 100.5);
        assert!(
            agg.observe_kline(
                &btc(),
                "1m",
                in_progress,
                false,
                Timestamp::from_nanos(30 * SEC)
            )
            .is_empty(),
            "未收市 kline 不产出事件"
        );

        let closed = bar(0, 100.0, 102.0, 99.0, 101.0);
        let events = agg.observe_kline(&btc(), "1m", closed, true, Timestamp::from_nanos(MIN));
        assert_eq!(events.len(), 1);
        let (_, _, payload, ts) = bar_closed_of(&events[0]).unwrap();
        assert_eq!(payload.close.as_f64(), 101.0);
        assert_eq!(ts.nanos, MIN, "收市事件 ts = bar 起始 + 周期");
    }

    #[test]
    fn test_duplicate_closed_bar_is_dropped() {
        let mut agg = BarAggregator::new();
        let closed = bar(0, 100.0, 101.0, 99.0, 100.5);
        let key = (btc(), "1m".to_string(), 0);

        assert_eq!(
            agg.observe_kline(&btc(), "1m", closed, true, Timestamp::from_nanos(MIN))
                .len(),
            1
        );
        assert!(agg.is_duplicate(&key), "收市后必须登记去重键");
        assert!(
            agg.observe_kline(&btc(), "1m", closed, true, Timestamp::from_nanos(MIN))
                .is_empty(),
            "同一 key 的第二个收市 bar 必须被丢弃"
        );
    }

    #[test]
    fn test_invalid_interval_returns_empty_without_panic() {
        let mut agg = BarAggregator::new();
        assert!(
            agg.observe_tick(&btc(), "1x", &tick(0, 100.0, 1.0))
                .is_empty()
        );
        assert!(
            agg.observe_kline(
                &btc(),
                "",
                bar(0, 100.0, 100.0, 100.0, 100.0),
                true,
                Timestamp::from_nanos(0)
            )
            .is_empty()
        );
    }
}
