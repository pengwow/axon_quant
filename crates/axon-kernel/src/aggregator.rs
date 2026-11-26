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
//! 分桶，并维护该 key 的**水位线**（迄今最大的收市时刻，单调推进）。两层机制
//! 主次分明：
//!
//! - **去重回溯窗 = 最近 `SEEN_PER_KEY`（16）个收市戳**，这是判断"重复"的
//!   实际依据：重发的戳只要还在桶里，就照旧拦截（`is_duplicate = true`）；
//! - **水位线剪枝只负责长回放的内存上界**：`mark_seen` 推进水位线后剪掉
//!   `< barrier - WATERMARK_LAG * interval` 的旧戳。`WATERMARK_LAG`（64）远大于
//!   `SEEN_PER_KEY`（16），常态下剪枝不会先于桶长淘汰生效；只有**单次 barrier
//!   大幅跳变（> 48 个周期）**时才会一次性批量剪掉旧戳。
//!
//! 因此：**去重窗口内**的放行 / 拦截形态不变；窗口边界外（被水位线剪掉或被桶长
//! 淘汰后重发）的 bar 按迟到处理——`is_duplicate = false` 且起始时间已在水位线
//! 之下时无法判重，**照旧放行**，但会 `warn!` 并按放行消息数累加
//! [`BarAggregator::duplicates_missed`]，供数据质量报告消费。
//!
//! 交易所的 Kline 消息没有 `seq` 字段，只有时间戳，因此无法做序号去重；水位线
//! 是「无 `seq`」前提下可做到的最强近似。

use std::collections::{HashMap, VecDeque};

use axon_core::{
    Bar, Event, Instrument, MarketDataEvent, MarketDataPayload, Quantity, Tick, Timestamp,
};

use crate::event::{close_time, interval_to_nanos};

/// 去重回溯窗 = 每个 `(instrument, interval)` 保留的最近收市戳个数
/// （同时是桶长的硬上限）
const SEEN_PER_KEY: usize = 16;

/// 水位线以下的保留窗口，单位 = 周期数
///
/// 水位线为「迄今最大的收市时刻」；`mark_seen` 会剪掉
/// `< barrier - WATERMARK_LAG * interval` 的历史戳。
///
/// 定位：**只负责长回放的内存上界，不是去重窗口本身**。真正的去重回溯窗是
/// [`SEEN_PER_KEY`]（16 个收市戳）；`WATERMARK_LAG`（64）远大于它，所以只有
/// 单次 barrier 跳变超过 `WATERMARK_LAG - SEEN_PER_KEY = 48` 个周期时，剪枝才会
/// 先于桶长淘汰生效（一次性批量剪掉旧戳）。取值参照「重连补发通常只覆盖最近
/// 数十根」的交易所行为。
const WATERMARK_LAG: i64 = 64;

/// 逐品种、逐周期的 K 线聚合器
///
/// 一个实例可以同时服务多个 `(instrument, interval)`，互不干扰。
#[derive(Debug, Default)]
pub struct BarAggregator {
    /// 进行中的 bar（key = `(instrument, interval)`）
    open: HashMap<(Instrument, String), Bar>,
    /// 已收市的 bar 起始时间（key = `(instrument, interval)`，按时间升序的环形队列）
    seen: HashMap<(Instrument, String), VecDeque<i64>>,
    /// 去重水位线 = 该 key 迄今最大的收市时刻（`bar.timestamp + interval`，纳秒）
    barriers: HashMap<(Instrument, String), i64>,
    /// 越过水位线、无法判定为重复而放行的迟到 bar 计数
    duplicates_missed: u64,
    /// 负成交量 tick 被夹紧丢弃的数量（既有 `.max(0.0)` 行为的计数化）
    invalid_qty_dropped: u64,
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

        // 负成交量 tick 是脏数据：累加 / 开新 bar 时按 0 夹紧（行为不变），
        // 这里只把它计数化，供数据质量报告消费（不做拦截）
        if tick.quantity.as_f64() < 0.0 {
            self.invalid_qty_dropped += 1;
        }

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
                if !self.check_duplicate(instrument, interval, bar_start) {
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
                if self.check_duplicate(instrument, interval, bar_start) {
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

        if !is_closed {
            if self.check_duplicate(instrument, interval, bar.timestamp.nanos) {
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

        if self.check_duplicate(instrument, interval, bar.timestamp.nanos) {
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
        vec![self.finish_at(instrument, interval, bar, ts, interval_ns)]
    }

    /// 该 `(instrument, interval, bar.timestamp)` 是否已经收市过
    pub fn is_duplicate(&self, key: &(Instrument, String, i64)) -> bool {
        self.seen_contains(&(key.0.clone(), key.1.clone()), key.2)
    }

    /// 按引用查询去重桶(内部热路径入口)
    ///
    /// 调用方(如 [`Self::check_duplicate`])已持有拼好的 key 时用它,避免为了
    /// [`Self::is_duplicate`] 的三元组签名再 clone 一份 `instrument` /
    /// `interval`。公开 API 与查询结果都和 `is_duplicate` 完全一致。
    fn seen_contains(&self, key: &(Instrument, String), bar_start: i64) -> bool {
        self.seen
            .get(key)
            .is_some_and(|stamps| stamps.contains(&bar_start))
    }

    /// 越过水位线、无法判定为重复而放行的迟到 bar 计数
    ///
    /// 每次放行这样的 bar 都会同时 `warn!`；计数用于数据质量报告。
    pub fn duplicates_missed(&self) -> u64 {
        self.duplicates_missed
    }

    /// 负成交量 tick 被夹紧丢弃的数量
    ///
    /// 与 [`Self::duplicates_missed`] 一样是只读观测值：放行 / 丢弃形态与计数前
    /// 完全一致，数字只进数据质量报告。
    pub fn invalid_qty_dropped(&self) -> u64 {
        self.invalid_qty_dropped
    }

    /// 判重并让漏判可见：与 [`Self::is_duplicate`] 同义（返回值完全相同），
    /// 只在「未判为重复但起始时间已在水位线之下」时告警并计数。
    ///
    /// 无 `seq` 时这是近似判断：缺口回补（此前从未收到的旧 bar）也会命中，
    /// 但那同样是「迟到数据被放行」，值得进质量报告。
    ///
    /// 热路径：key 只拼一次（`instrument` clone + `interval` 转 `String` 各一份），
    /// 去重桶与水位线都用这同一份 key 查询。
    fn check_duplicate(&mut self, instrument: &Instrument, interval: &str, bar_start: i64) -> bool {
        let key = (instrument.clone(), interval.to_string());
        if self.seen_contains(&key, bar_start) {
            return true;
        }
        let Some(&barrier) = self.barriers.get(&key) else {
            return false;
        };
        if bar_start < barrier {
            self.duplicates_missed += 1;
            tracing::warn!(
                instrument = %instrument,
                interval,
                bar_timestamp = bar_start,
                barrier,
                "kernel aggregator: 重复 bar 超出去重窗口仍被放行"
            );
        }
        false
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
        self.finish_at(instrument, interval, bar, ts, interval_ns)
    }

    /// 产出收市事件并登记去重
    fn finish_at(
        &mut self,
        instrument: &Instrument,
        interval: &str,
        bar: Bar,
        ts: Timestamp,
        interval_ns: i64,
    ) -> Event {
        self.mark_seen(instrument, interval, bar.timestamp.nanos, interval_ns);
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

    /// 登记一个已收市时间戳：推进水位线、按水位线剪枝、超出 `SEEN_PER_KEY` 时
    /// 弹出最旧的
    ///
    /// 先剪枝再压入。剪枝阈值 `barrier - WATERMARK_LAG * interval`（见
    /// [`WATERMARK_LAG`]）比 `SEEN_PER_KEY` 的桶长宽一倍以上，因此常态下桶里始终
    /// 留着最近 `SEEN_PER_KEY` 个收市戳（**去重窗口 = 最近 16 个收市戳**）；水位线
    /// 只在单次大幅跳变（> 48 周期）时批量清掉更旧的戳，把长回放的内存钉在上界。
    fn mark_seen(
        &mut self,
        instrument: &Instrument,
        interval: &str,
        bar_start: i64,
        interval_ns: i64,
    ) {
        let key = (instrument.clone(), interval.to_string());
        // 水位线 = 迄今最大的收市时刻（bar 起始 + 周期），单调推进
        let close = bar_start.saturating_add(interval_ns);
        let barrier = self.barriers.entry(key.clone()).or_insert(close);
        if close > *barrier {
            *barrier = close;
        }
        // 水位线以下 WATERMARK_LAG 根周期之外的戳不再可能有重连补发
        let cutoff = barrier.saturating_sub(WATERMARK_LAG.saturating_mul(interval_ns));
        let stamps = self.seen.entry(key).or_default();
        stamps.retain(|stamp| *stamp >= cutoff);
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

    /// 负量 tick 是脏数据：既有行为是按 0 夹紧（不参与累加），C6 只把它计数化
    #[test]
    fn test_negative_tick_quantity_is_counted_and_clamped() {
        let mut agg = BarAggregator::new();
        assert_eq!(agg.invalid_qty_dropped(), 0, "干净 tick 不计数");

        agg.observe_tick(&btc(), "1m", &tick(0, 100.0, 2.0));
        agg.observe_tick(&btc(), "1m", &tick(10 * SEC, 100.0, -5.0));
        assert_eq!(agg.invalid_qty_dropped(), 1, "负量 tick 必须计数");

        // 跨周期收市：负量不参与累加（行为与计数前完全一致）
        let events = agg.observe_tick(&btc(), "1m", &tick(MIN, 101.0, 1.0));
        let (_, _, closed, _) = bar_closed_of(&events[0]).expect("必须收市");
        assert!(
            (closed.volume.as_f64() - 2.0).abs() < 1e-9,
            "负量被夹紧后不累加: {}",
            closed.volume.as_f64()
        );

        // 零量既不夹紧也不算脏：不计入 invalid_qty_dropped
        agg.observe_tick(&btc(), "1m", &tick(MIN, 101.0, 0.0));
        assert_eq!(agg.invalid_qty_dropped(), 1, "零量不是负量");
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
        assert_eq!(agg.duplicates_missed(), 0, "窗口内重复不算漏判");
    }

    /// 回放 `0..=last` 的收市 kline，每根都必须产出事件
    fn replay_closed(agg: &mut BarAggregator, last: i64) {
        for i in 0..=last {
            let events = agg.observe_kline(
                &btc(),
                "1m",
                bar(i * MIN, 100.0, 101.0, 99.0, 100.5),
                true,
                Timestamp::from_nanos((i + 1) * MIN),
            );
            assert_eq!(events.len(), 1, "回放 bar {i} 必须收市");
        }
    }

    #[test]
    fn test_late_duplicate_beyond_bucket_is_released_and_counted() {
        let mut agg = BarAggregator::new();
        replay_closed(&mut agg, 20);
        let key = (btc(), "1m".to_string());
        assert_eq!(
            agg.seen.get(&key).expect("桶必须存在").len(),
            SEEN_PER_KEY,
            "桶长受 SEEN_PER_KEY 兜底"
        );
        assert!(
            !agg.seen.get(&key).expect("桶必须存在").contains(&(3 * MIN)),
            "bar 3 已超出桶窗口"
        );
        assert_eq!(agg.duplicates_missed(), 0);

        // 迟到 17 个周期的重复（> SEEN_PER_KEY 且 ≤ WATERMARK_LAG）：无法判定为重复，
        // 仍按现有路径放行，但必须计数（生产环境同时 warn!）
        let late = bar(3 * MIN, 100.0, 101.0, 99.0, 100.5);
        let events = agg.observe_kline(&btc(), "1m", late, true, Timestamp::from_nanos(21 * MIN));
        assert_eq!(
            events.len(),
            1,
            "越过桶窗口的迟到重复仍照旧放行（事件流不变）"
        );
        let (_, _, released, ts) = bar_closed_of(&events[0]).expect("必须是 BarClosed");
        assert_eq!(released.timestamp.nanos, 3 * MIN);
        assert_eq!(ts.nanos, 4 * MIN, "事件时间戳仍是收市时刻");
        assert_eq!(agg.duplicates_missed(), 1, "漏判必须可见化");
    }

    #[test]
    fn test_duplicate_beyond_watermark_is_pruned_and_released() {
        let mut agg = BarAggregator::new();
        replay_closed(&mut agg, 90);
        let key = (btc(), "1m".to_string());
        let barrier = *agg.barriers.get(&key).expect("水位线必须存在");
        assert_eq!(barrier, 91 * MIN, "水位线 = 迄今最大收市时刻");
        let cutoff = barrier - WATERMARK_LAG * MIN;
        assert!(
            agg.seen[&key].iter().all(|stamp| *stamp >= cutoff),
            "水位线以下 WATERMARK_LAG 根之外的历史戳必须已剪枝"
        );
        assert!(
            !agg.seen[&key].contains(&(2 * MIN)),
            "88 个周期前的戳不可能还在桶里"
        );

        // 超过水位线的迟到重复（> WATERMARK_LAG + SEEN_PER_KEY 周期）：剪枝后视为
        // 新鲜 bar 放行，同时命中漏判可见化
        let late = bar(2 * MIN, 100.0, 101.0, 99.0, 100.5);
        let events = agg.observe_kline(&btc(), "1m", late, true, Timestamp::from_nanos(91 * MIN));
        assert_eq!(events.len(), 1, "剪枝后的迟到 bar 仍放行（事件流不变）");
        assert_eq!(agg.duplicates_missed(), 1, "水位线之上的迟到仍计数");
    }

    #[test]
    fn test_seen_bucket_is_watermark_bounded() {
        let mut agg = BarAggregator::new();
        replay_closed(&mut agg, 499);
        let key = (btc(), "1m".to_string());
        let barrier = *agg.barriers.get(&key).expect("水位线必须存在");
        let cutoff = barrier - WATERMARK_LAG * MIN;
        let bucket = &agg.seen[&key];
        assert!(
            bucket.len() <= SEEN_PER_KEY,
            "长回放后桶长不得增长（实际 {}）",
            bucket.len()
        );
        assert!(
            bucket.iter().all(|stamp| *stamp >= cutoff),
            "桶内不得残留水位线以下的历史戳"
        );
        assert_eq!(agg.duplicates_missed(), 0, "单调回放不产生漏判");
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
