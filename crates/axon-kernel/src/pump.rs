//! 历史数据泵与网格屏障
//!
//! 回测 loader 把「按网格对齐」的 per-symbol bar 序列交给 [`build_frames`],
//! 再由 `Kernel::load_frames` 展开成收市时刻的 `BarClosed` 事件。事件驱动的主
//! 回路无法天然知道「同一网格点的所有品种是否到齐」,所以用
//! [`BarGridBarrier`] 攒帧:同一 `ts` 的品种集齐后才触发一次生命周期。
//!
//! 没有屏障,多品种场景会每品种各 rebalance 一次,`budget` 缩减与 NAV 采样
//! 都会重复执行(设计文档 4.8)。

use std::collections::{BTreeMap, HashMap, HashSet};

use axon_core::Bar;
use axon_core::Timestamp;
use axon_core::types::Instrument;

use crate::KernelError;
use crate::core::InstrumentMode;

/// 一个网格点上所有有真实 bar 的品种
#[derive(Debug, Clone, PartialEq)]
pub struct GridFrame {
    /// 网格时间戳(本点第一个真实 bar 的 `bar.timestamp`,即 bar 起始时间)
    pub ts: Timestamp,
    /// 本点有真实 bar 的 `(instrument, bar)`;`None` 缺口的品种不出现在这里
    pub bars: Vec<(Instrument, Bar)>,
    /// 本点每个有真实 bar 品种的 state(策略因子/上下文)
    ///
    /// 只对本帧出现过的 instrument 建键;查不到时调用方按 `Value::Null` 处理。
    /// 来自 [`build_frames`] 的逐 bar 对齐结果,屏障重建帧时为空,由 `Kernel`
    /// 按 `(帧时间戳, 品种)` 查表回填。
    ///
    /// 用 `HashMap` 而不是 `BTreeMap`:`Instrument` 实现了 `Hash` / `Eq`
    /// (f64 字段按位比较),但没有 `Ord`,与 `HostCtx.positions` 同口径。
    pub states: HashMap<Instrument, serde_json::Value>,
}

/// 把 per-symbol 网格展平成帧序列
///
/// # 规则(与 `BacktestRunner::run()` 对齐)
///
/// - 所有 symbol 的向量必须等长,不等长返回错误而不是 panic;
/// - `ts` 取该网格点上按 `BTreeMap` 顺序第一个有真实 bar 的 symbol 的
///   `bar.timestamp`;
/// - `None` 不产生 bar(价格前向填充由内核的 `last_price` 负责);
/// - 每个网格点至少有一个真实 bar,否则返回错误;
/// - `states_by_symbol` 与 bars 逐网格点对齐:第 i 项对应第 i 个网格点,
///   长度不足按 `Value::Null` 补齐(下标取不到就是 Null),超长自动忽略。
pub fn build_frames(
    bars_by_symbol: &BTreeMap<String, Vec<Option<Bar>>>,
    mode: &InstrumentMode,
    states_by_symbol: Option<&BTreeMap<String, Vec<serde_json::Value>>>,
) -> Result<Vec<GridFrame>, KernelError> {
    if bars_by_symbol.is_empty() {
        return Ok(Vec::new());
    }
    let grid_len = bars_by_symbol
        .values()
        .map(Vec::len)
        .max()
        .expect("非空 BTreeMap 必有一个元素");
    for (symbol, bars) in bars_by_symbol {
        if bars.len() != grid_len {
            return Err(KernelError::Pump(format!(
                "bars_by_symbol[{symbol}] 长度 {} 与网格长度 {grid_len} 不一致",
                bars.len()
            )));
        }
    }

    let instruments: BTreeMap<&String, Instrument> = bars_by_symbol
        .keys()
        .map(|symbol| (symbol, mode.instrument(symbol)))
        .collect();

    let mut frames = Vec::with_capacity(grid_len);
    for grid_idx in 0..grid_len {
        let ts = bars_by_symbol
            .iter()
            .find_map(|(_, bars)| bars[grid_idx].as_ref().map(|bar| bar.timestamp))
            .ok_or_else(|| KernelError::Pump(format!("网格点 {grid_idx} 没有任何真实 bar")))?;
        let mut bars = Vec::with_capacity(bars_by_symbol.len());
        let mut states = HashMap::new();
        for (symbol, column) in bars_by_symbol {
            let Some(bar) = column[grid_idx].as_ref() else {
                continue;
            };
            let instrument = instruments[symbol].clone();
            bars.push((instrument.clone(), *bar));
            // 逐 bar state:下标取不到(长度不足)就是 Null,与 BacktestRunner
            // 的 `states.get(grid_idx).cloned().unwrap_or(Null)` 同口径
            let state = states_by_symbol
                .and_then(|table| table.get(symbol))
                .and_then(|column| column.get(grid_idx))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            states.insert(instrument, state);
        }
        frames.push(GridFrame { ts, bars, states });
    }
    Ok(frames)
}

/// 网格屏障:同一 `ts` 的 `BarClosed` 集齐期望品种后才放行一帧
///
/// 期望集合有两级来源:
/// 1. `Kernel::load_frames` 预注册的**每帧**品种集合(回测路径精确已知);
/// 2. `KernelConfig::expected_instruments` 全局兜底(手工灌事件的场景)。
///
/// 两级是必要的:多品种网格允许某一格某个品种 `None` 缺口,该帧仍然要触发
/// 一次生命周期(前向填充价格),所以「全集」不能当成每帧的期望。
#[derive(Debug, Default)]
pub struct BarGridBarrier {
    expected: HashSet<Instrument>,
    /// 每帧期望品种(key = 网格时间戳纳秒)
    per_frame: BTreeMap<i64, HashSet<Instrument>>,
    frame: Option<GridFrame>,
    seen: HashSet<Instrument>,
}

impl BarGridBarrier {
    /// 用全局期望品种创建屏障
    pub fn new(expected: HashSet<Instrument>) -> Self {
        Self {
            expected,
            per_frame: BTreeMap::new(),
            frame: None,
            seen: HashSet::new(),
        }
    }

    /// 仅当全局期望集合为空时用 `expected` 补齐(配空配置的测试仍能跑)
    pub fn set_expected_if_empty(&mut self, expected: HashSet<Instrument>) {
        if self.expected.is_empty() {
            self.expected = expected;
        }
    }

    /// 预注册某一帧的期望品种集合(回测 loader 用)
    pub fn register_frame(&mut self, ts: Timestamp, instruments: HashSet<Instrument>) {
        self.per_frame.insert(ts.nanos, instruments);
    }

    /// 收下一条收市事件;集齐时返回完整帧,否则返回 `None`
    ///
    /// 同一帧内重复品种按「先到先得」丢弃(去重),与 4.11 的 bar 去重语义一致。
    pub fn admit(
        &mut self,
        instrument: Instrument,
        bar: Bar,
        ts: Timestamp,
    ) -> Result<Option<GridFrame>, KernelError> {
        if self.frame.is_none() {
            let capacity = self
                .per_frame
                .get(&ts.nanos)
                .map(HashSet::len)
                .unwrap_or(self.expected.len())
                .max(1);
            self.frame = Some(GridFrame {
                ts,
                bars: Vec::with_capacity(capacity),
                // 屏障只攒 bar;state 由 `Kernel` 按帧时间戳查表回填
                states: HashMap::new(),
            });
        }
        if let Some(frame) = &self.frame
            && frame.ts != ts
        {
            return Err(KernelError::Pump(format!(
                "网格帧乱序:当前帧 ts={} 收到 ts={ts}",
                frame.ts.nanos
            )));
        }

        if !self.seen.insert(instrument.clone()) {
            tracing::debug!(instrument = %instrument, "kernel: duplicate bar in grid frame ignored");
            return Ok(None);
        }
        if let Some(frame) = self.frame.as_mut() {
            frame.bars.push((instrument, bar));
        }

        let complete = match self.per_frame.get(&ts.nanos) {
            Some(expected) => self.seen.is_superset(expected),
            None => self.seen.is_superset(&self.expected),
        };
        if complete {
            let frame = self.frame.take();
            self.seen.clear();
            self.per_frame.remove(&ts.nanos);
            Ok(frame)
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use axon_core::types::SpotInstrument;
    use axon_core::{Price, Quantity, Symbol};

    use super::*;

    fn bar(ts: i64, close: f64) -> Bar {
        Bar {
            timestamp: Timestamp::from_nanos(ts),
            open: Price::from_f64(close),
            high: Price::from_f64(close),
            low: Price::from_f64(close),
            close: Price::from_f64(close),
            volume: Quantity::from_f64(1.0),
        }
    }

    fn spot(base: &str) -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from(base),
            quote: Symbol::from("USDT"),
        })
    }

    #[test]
    fn test_build_frames_aligns_multi_symbol_grid() {
        let mut bars = BTreeMap::new();
        // BTC 中间一格缺口,ETH 第三格缺口
        bars.insert(
            "BTC/USDT".to_string(),
            vec![Some(bar(0, 100.0)), None, Some(bar(120, 102.0))],
        );
        bars.insert(
            "ETH/USDT".to_string(),
            vec![Some(bar(0, 10.0)), Some(bar(60, 11.0)), None],
        );

        let frames = build_frames(&bars, &InstrumentMode::Spot, None).expect("对齐网格应成功");
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].ts.nanos, 0);
        assert_eq!(frames[0].bars.len(), 2, "第一格两个品种都有真实 bar");
        assert_eq!(frames[1].ts.nanos, 60);
        assert_eq!(frames[1].bars.len(), 1, "BTC 缺口不产生 bar");
        assert_eq!(frames[1].bars[0].0, spot("ETH"));
        assert_eq!(frames[2].ts.nanos, 120);
        assert_eq!(frames[2].bars[0].0, spot("BTC"));
        assert!(
            frames
                .iter()
                .flat_map(|frame| frame.states.values())
                .all(|state| state.is_null()),
            "没传 state 时帧内 state 应为 Null（与 BacktestRunner 缺省口径一致）"
        );
    }

    #[test]
    fn test_build_frames_aligns_per_bar_states() {
        let mut bars = BTreeMap::new();
        bars.insert(
            "BTC/USDT".to_string(),
            vec![Some(bar(0, 100.0)), Some(bar(60, 101.0))],
        );
        // BTC 只给一格 state(长度不足),ETH 给三格(超长忽略)
        let mut states = BTreeMap::new();
        states.insert(
            "BTC/USDT".to_string(),
            vec![serde_json::json!({"features": {"ret_1": 0.1}})],
        );
        states.insert(
            "ETH/USDT".to_string(),
            vec![
                serde_json::json!({"features": {"ret_1": 0.3}}),
                serde_json::json!({"features": {"ret_1": 0.4}}),
                serde_json::json!({"features": {"ret_1": 0.5}}),
            ],
        );

        let frames =
            build_frames(&bars, &InstrumentMode::Spot, Some(&states)).expect("对齐网格应成功");
        assert_eq!(
            frames[0].states[&spot("BTC")],
            serde_json::json!({"features": {"ret_1": 0.1}})
        );
        assert_eq!(
            frames[1].states[&spot("BTC")],
            serde_json::Value::Null,
            "state 长度不足按 Null 补齐"
        );
    }

    #[test]
    fn test_build_frames_rejects_unequal_lengths() {
        let mut bars = BTreeMap::new();
        bars.insert("BTC/USDT".to_string(), vec![Some(bar(0, 100.0))]);
        bars.insert(
            "ETH/USDT".to_string(),
            vec![Some(bar(0, 10.0)), Some(bar(60, 11.0))],
        );
        let err = build_frames(&bars, &InstrumentMode::Spot, None);
        assert!(matches!(err, Err(KernelError::Pump(_))), "不等长必须报错");
    }

    #[test]
    fn test_barrier_waits_for_all_expected_instruments() {
        let mut barrier = BarGridBarrier::new([spot("BTC"), spot("ETH")].into_iter().collect());
        let ts = Timestamp::from_nanos(0);

        assert!(
            barrier
                .admit(spot("BTC"), bar(0, 100.0), ts)
                .unwrap()
                .is_none()
        );
        let frame = barrier
            .admit(spot("ETH"), bar(0, 10.0), ts)
            .unwrap()
            .expect("集齐后应放行");
        assert_eq!(frame.bars.len(), 2);
        assert_eq!(frame.ts.nanos, 0);

        // 下一帧重新攒
        assert!(
            barrier
                .admit(spot("BTC"), bar(60, 101.0), Timestamp::from_nanos(60))
                .unwrap()
                .is_none()
        );
    }
}
