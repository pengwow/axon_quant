//! 进程内增量市场数据源(测试 / 单元回测用)
//!
//! 0.8.0 Phase 2 B1 新增,提供 [`MarketDataSource`] 的内存实现。
//! 与 `axon-backtest::BacktestEngine` 配合:`dispatch MarkEvent` 时
//! `push_mark` 写入,回测结束后 `compute_report` 读取。
//!
//! # 线程安全
//!
//! 内部用 `std::sync::Mutex`(与 Scheduler 0.8.0 重构一致,无 unsafe),
//! 保证 `Send + Sync`。`mark_history` / `implied_vol` 持有锁期间完成
//! 数据 clone / 拷贝,锁粒度小。

use std::collections::HashMap;
use std::sync::Mutex;

use super::source::{MarkPoint, MarketDataSource};
use crate::time::Timestamp;
use crate::types::Instrument;

/// 进程内增量市场数据源
///
/// 用 `HashMap<Instrument, Vec<MarkPoint>>` 存 mark 历史,
/// `HashMap<Instrument, f64>` 存 IV。
/// 写操作需持锁;读操作也持锁(临界区内仅做 clone / 拷贝,锁粒度小)。
#[derive(Debug)]
pub struct InMemoryMarketData {
    /// mark 历史:`instrument -> Vec<(ts, mark)>`(按 ts 升序)
    marks: Mutex<HashMap<Instrument, Vec<MarkPoint>>>,
    /// 隐含波动率:`instrument -> iv`
    ivs: Mutex<HashMap<Instrument, f64>>,
}

impl Default for InMemoryMarketData {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryMarketData {
    /// 创建空数据源
    pub fn new() -> Self {
        Self {
            marks: Mutex::new(HashMap::new()),
            ivs: Mutex::new(HashMap::new()),
        }
    }

    /// 推入一帧 mark(增量,按 ts 升序追加)
    ///
    /// 若 instrument 已有数据,新帧的 `ts` 必须 **≥ 最后一帧 ts**(单调性)。
    /// 违反单调性 → 静默丢弃并 `eprintln!` 警告(0.8.0 范围:不 panic,留
    /// `Result<(), MarketDataError>` 升级空间给 0.9.0)。
    pub fn push_mark(&self, instrument: Instrument, ts: Timestamp, mark: f64) {
        let mut marks = self
            .marks
            .lock()
            .expect("InMemoryMarketData::marks Mutex poisoned");
        let entry = marks.entry(instrument).or_default();
        if let Some((last_ts, _)) = entry.last()
            && ts < *last_ts
        {
            eprintln!(
                "InMemoryMarketData::push_mark: ts out of order ({} < {}), dropped",
                ts, last_ts
            );
            return;
        }
        entry.push((ts, mark));
    }

    /// 推入 IV(覆盖)
    pub fn set_iv(&self, instrument: Instrument, iv: f64) {
        if !iv.is_finite() || iv < 0.0 {
            eprintln!(
                "InMemoryMarketData::set_iv: invalid iv {} (must be finite ≥ 0), dropped",
                iv
            );
            return;
        }
        self.ivs
            .lock()
            .expect("InMemoryMarketData::ivs Mutex poisoned")
            .insert(instrument, iv);
    }

    /// 当前持有的 instrument 数量(mark 或 IV 任一非空都计)
    pub fn instrument_count(&self) -> usize {
        let marks = self
            .marks
            .lock()
            .expect("InMemoryMarketData::marks Mutex poisoned");
        let ivs = self
            .ivs
            .lock()
            .expect("InMemoryMarketData::ivs Mutex poisoned");
        let mut all: std::collections::HashSet<&Instrument> = marks.keys().collect();
        all.extend(ivs.keys());
        all.len()
    }

    /// mark 历史总帧数(跨所有 instrument)
    pub fn total_mark_points(&self) -> usize {
        self.marks
            .lock()
            .expect("InMemoryMarketData::marks Mutex poisoned")
            .values()
            .map(|v| v.len())
            .sum()
    }
}

impl MarketDataSource for InMemoryMarketData {
    fn mark_history(&self, instrument: &Instrument, lookback: usize) -> Vec<MarkPoint> {
        let marks = self
            .marks
            .lock()
            .expect("InMemoryMarketData::marks Mutex poisoned");
        match marks.get(instrument) {
            None => Vec::new(),
            Some(history) => {
                // 末尾取 lookback 帧,O(1) 切片
                let start = history.len().saturating_sub(lookback);
                history[start..].to_vec()
            }
        }
    }

    fn implied_vol(&self, instrument: &Instrument) -> Option<f64> {
        self.ivs
            .lock()
            .expect("InMemoryMarketData::ivs Mutex poisoned")
            .get(instrument)
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Timestamp;
    use crate::types::{SpotInstrument, Symbol};

    fn btc_spot() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        })
    }

    #[test]
    fn empty_source_returns_empty() {
        let src = InMemoryMarketData::new();
        assert_eq!(src.instrument_count(), 0);
        assert_eq!(src.total_mark_points(), 0);
        assert!(src.mark_history(&btc_spot(), 10).is_empty());
        assert!(src.implied_vol(&btc_spot()).is_none());
        assert!(src.latest_mark(&btc_spot()).is_none());
    }

    #[test]
    fn push_mark_appends_in_order() {
        let src = InMemoryMarketData::new();
        let inst = btc_spot();
        src.push_mark(inst.clone(), Timestamp::from_nanos(100), 50_000.0);
        src.push_mark(inst.clone(), Timestamp::from_nanos(200), 50_100.0);
        src.push_mark(inst.clone(), Timestamp::from_nanos(300), 50_200.0);
        let hist = src.mark_history(&inst, 10);
        assert_eq!(hist.len(), 3);
        assert_eq!(hist[0], (Timestamp::from_nanos(100), 50_000.0));
        assert_eq!(hist[2], (Timestamp::from_nanos(300), 50_200.0));
        assert_eq!(src.total_mark_points(), 3);
    }

    #[test]
    fn push_mark_out_of_order_drops() {
        let src = InMemoryMarketData::new();
        let inst = btc_spot();
        src.push_mark(inst.clone(), Timestamp::from_nanos(200), 1.0);
        src.push_mark(inst.clone(), Timestamp::from_nanos(100), 0.5); // out of order, dropped
        let hist = src.mark_history(&inst, 10);
        assert_eq!(hist.len(), 1, "out-of-order push should be dropped");
    }

    #[test]
    fn mark_history_respects_lookback() {
        let src = InMemoryMarketData::new();
        let inst = btc_spot();
        for i in 0..5 {
            src.push_mark(
                inst.clone(),
                Timestamp::from_nanos((i + 1) as i64 * 100),
                i as f64,
            );
        }
        let last_3 = src.mark_history(&inst, 3);
        assert_eq!(last_3.len(), 3);
        // 末尾 3 帧
        assert_eq!(last_3[0], (Timestamp::from_nanos(300), 2.0));
        assert_eq!(last_3[2], (Timestamp::from_nanos(500), 4.0));
    }

    #[test]
    fn latest_mark_returns_last_frame() {
        let src = InMemoryMarketData::new();
        let inst = btc_spot();
        src.push_mark(inst.clone(), Timestamp::from_nanos(100), 50_000.0);
        src.push_mark(inst.clone(), Timestamp::from_nanos(200), 51_000.0);
        let latest = src.latest_mark(&inst);
        assert_eq!(latest, Some((Timestamp::from_nanos(200), 51_000.0)));
    }

    #[test]
    fn set_iv_and_get() {
        let src = InMemoryMarketData::new();
        let inst = btc_spot();
        src.set_iv(inst.clone(), 0.65);
        assert_eq!(src.implied_vol(&inst), Some(0.65));
    }

    #[test]
    fn set_iv_rejects_invalid() {
        let src = InMemoryMarketData::new();
        let inst = btc_spot();
        src.set_iv(inst.clone(), -0.1); // dropped
        src.set_iv(inst.clone(), f64::NAN); // dropped
        assert_eq!(src.implied_vol(&inst), None);
    }

    #[test]
    fn instrument_count_dedupes_marks_and_ivs() {
        let src = InMemoryMarketData::new();
        let inst = btc_spot();
        src.push_mark(inst.clone(), Timestamp::from_nanos(100), 1.0);
        src.set_iv(inst.clone(), 0.5);
        assert_eq!(
            src.instrument_count(),
            1,
            "same instrument in marks + ivs counts once"
        );
    }

    #[test]
    fn send_sync_compiles() {
        // 编译期断言:InMemoryMarketData 是 Send + Sync
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<InMemoryMarketData>();
    }

    // ─── 0.8.0 B3 新增测试:`mark_returns` / `latest_return` ───

    #[test]
    fn mark_returns_basic() {
        let src = InMemoryMarketData::new();
        let inst = btc_spot();
        // 5 帧 mark,价格序列 100, 110, 121, 121, 110
        src.push_mark(inst.clone(), Timestamp::from_nanos(100), 100.0);
        src.push_mark(inst.clone(), Timestamp::from_nanos(200), 110.0);
        src.push_mark(inst.clone(), Timestamp::from_nanos(300), 121.0);
        src.push_mark(inst.clone(), Timestamp::from_nanos(400), 121.0);
        src.push_mark(inst.clone(), Timestamp::from_nanos(500), 110.0);
        let r = src.mark_returns(&inst, 10);
        assert_eq!(r.len(), 4);
        assert!((r[0] - 0.10).abs() < 1e-9, "110/100 - 1 = 0.10");
        assert!((r[1] - 0.10).abs() < 1e-9, "121/110 - 1 ≈ 0.10");
        assert!(r[2].abs() < 1e-9, "121/121 - 1 = 0");
        assert!((r[3] - (-1.0 / 11.0)).abs() < 1e-9, "110/121 - 1 ≈ -0.0909");
    }

    #[test]
    fn mark_returns_insufficient_data() {
        let src = InMemoryMarketData::new();
        let inst = btc_spot();
        // 0 帧 → 空
        assert!(src.mark_returns(&inst, 10).is_empty());
        // 1 帧 → 空(无法计算 return)
        src.push_mark(inst.clone(), Timestamp::from_nanos(100), 100.0);
        assert!(src.mark_returns(&inst, 10).is_empty());
    }

    #[test]
    fn mark_returns_lookback_trims() {
        let src = InMemoryMarketData::new();
        let inst = btc_spot();
        // 10 帧,价格线性 100, 110, 120, ..., 190
        for i in 0..10 {
            src.push_mark(
                inst.clone(),
                Timestamp::from_nanos((i + 1) as i64 * 100),
                100.0 + 10.0 * i as f64,
            );
        }
        // lookback=3 → 末 3 帧 mark = (170, 180, 190)
        // returns: 180/170-1 ≈ 0.0588, 190/180-1 ≈ 0.0556(2 个 returns)
        let r = src.mark_returns(&inst, 3);
        assert_eq!(r.len(), 2);
        assert!((r[0] - 10.0 / 170.0).abs() < 1e-9);
        assert!((r[1] - 10.0 / 180.0).abs() < 1e-9);
    }

    #[test]
    fn latest_return_returns_last() {
        let src = InMemoryMarketData::new();
        let inst = btc_spot();
        src.push_mark(inst.clone(), Timestamp::from_nanos(100), 100.0);
        src.push_mark(inst.clone(), Timestamp::from_nanos(200), 105.0);
        src.push_mark(inst.clone(), Timestamp::from_nanos(300), 110.0);
        // latest = 110/105 - 1 = 1/21
        let lr = src.latest_return(&inst);
        assert!(lr.is_some());
        assert!((lr.unwrap() - (1.0 / 21.0)).abs() < 1e-9);
    }

    #[test]
    fn latest_return_insufficient_data_is_none() {
        let src = InMemoryMarketData::new();
        let inst = btc_spot();
        // 0 / 1 帧 → None
        assert!(src.latest_return(&inst).is_none());
        src.push_mark(inst.clone(), Timestamp::from_nanos(100), 100.0);
        assert!(src.latest_return(&inst).is_none());
    }
}
