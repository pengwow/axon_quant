//! CSV 文件加载的市场数据源(离线 / 单元测试用)
//!
//! 0.8.0 Phase 2 B1 新增。提供 [`MarketDataSource`] 的 CSV 实现,
//! 适合从文件加载历史 mark / IV 做离线回放。
//!
//! # CSV 格式
//!
//! ```csv
//! instrument,ts_ns,mark,iv
//! BTC/USDT,1700000000000000000,50000.0,0.65
//! BTC/USDT,1700000001000000000,50100.0,0.64
//! BTC/USDT:SWAP,1700000000000000000,50010.0,
//! ```
//!
//! - `instrument`:使用 `Instrument` 的 `Display` 格式
//!   - spot:`BTC/USDT`
//!   - swap:`BTC/USDT:SWAP`
//! - `ts_ns`:纳秒时间戳(i64)
//! - `mark`:浮点 mark 价格
//! - `iv`:可空(空 = 无 IV)。IV 是年化小数(0.5 = 50%)
//!
//! # 加载策略
//!
//! 构造时一次性加载到内存(`HashMap<Instrument, Vec<MarkPoint>>` +
//! `HashMap<Instrument, f64>`)。对中小数据集(< 10MB)开销可忽略。
//! 流式加载(arrow/parquet)留 0.9.0。

use std::collections::HashMap;
use std::path::Path;

use thiserror::Error;

use super::source::{MarkPoint, MarketDataSource};
use crate::time::Timestamp;
use crate::types::{Instrument, SpotInstrument, SwapInstrument, SwapSettle, Symbol};

/// CSV 加载错误
#[derive(Debug, Error)]
pub enum CsvMarketDataError {
    /// 读取文件失败
    #[error("read CSV file failed: {0}")]
    Io(#[from] std::io::Error),
    /// 解析行失败(带行号,从 1 起)
    #[error("parse CSV row {line} failed: {detail}")]
    Parse {
        /// 行号
        line: usize,
        /// 错误详情
        detail: String,
    },
    /// 缺少必需列
    #[error("missing required column: {0}")]
    MissingColumn(String),
    /// 时间戳解析失败(带行号)
    #[error("invalid timestamp at row {line} '{raw}': {err}")]
    Timestamp {
        /// 行号
        line: usize,
        /// 原始字符串
        raw: String,
        /// 解析错误
        err: String,
    },
}

/// 从 `Display` 格式解析 instrument(0.8.0 B3 修复:接受 `line` 参数填充错误位置)
///
/// 支持:
/// - `"BTC/USDT"` → `Instrument::Spot(...)`(默认 settle)
/// - `"BTC/USDT:SWAP"` → `Instrument::Swap(...)`(默认 contract_size = 1.0,UsdMargin)
///
/// 注:CSV 不携带 `contract_size`(保持简洁),0.8.0 全部假设 `1.0`。
/// 多 leg perp(contract_size ≠ 1.0)的 CSV 留 0.9.0 扩展。
///
/// # Errors
///
/// - `line`:出错行号(由调用方传入,1-based),用于错误定位
fn parse_instrument(s: &str, line: usize) -> Result<Instrument, CsvMarketDataError> {
    if let Some(label) = s.strip_suffix(":SWAP") {
        // swap: "BTC/USDT:SWAP"
        let parts: Vec<&str> = label.splitn(2, '/').collect();
        match parts.as_slice() {
            [base, quote] if !base.is_empty() && !quote.is_empty() => {
                Ok(Instrument::Swap(SwapInstrument {
                    base: Symbol::from(*base),
                    quote: Symbol::from(*quote),
                    settle: SwapSettle::UsdMargin,
                    contract_size: 1.0,
                }))
            }
            _ => Err(CsvMarketDataError::Parse {
                line,
                detail: format!("invalid swap instrument label: {s}"),
            }),
        }
    } else {
        // spot: "BTC/USDT"
        let parts: Vec<&str> = s.splitn(2, '/').collect();
        match parts.as_slice() {
            [base, quote] if !base.is_empty() && !quote.is_empty() => {
                Ok(Instrument::Spot(SpotInstrument {
                    base: Symbol::from(*base),
                    quote: Symbol::from(*quote),
                }))
            }
            _ => Err(CsvMarketDataError::Parse {
                line,
                detail: format!("invalid spot instrument label: {s}"),
            }),
        }
    }
}

/// 解析时间戳字符串(i64 纳秒)(0.8.0 B3 修复:`line` 填充到 `Timestamp` 错误)
///
/// # Errors
///
/// - `Timestamp` 错误:解析失败时带 `line` 字段(由调用方传入)
fn parse_ts_ns(s: &str, line: usize) -> Result<Timestamp, CsvMarketDataError> {
    let n: i64 =
        s.trim()
            .parse()
            .map_err(|e: std::num::ParseIntError| CsvMarketDataError::Timestamp {
                line,
                raw: s.to_string(),
                err: e.to_string(),
            })?;
    Ok(Timestamp::from_nanos(n))
}

/// 解析可选 IV(空 = None)(0.8.0 B3 修复:接受 `line` 参数填充错误位置)
///
/// # Errors
///
/// - `Parse` 错误:IV 解析失败 / 非有限 / 负数时带 `line` 字段
fn parse_iv_opt(s: &str, line: usize) -> Result<Option<f64>, CsvMarketDataError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let v: f64 =
        trimmed
            .parse()
            .map_err(|e: std::num::ParseFloatError| CsvMarketDataError::Parse {
                line,
                detail: format!("invalid IV '{trimmed}': {e}"),
            })?;
    if !v.is_finite() || v < 0.0 {
        return Err(CsvMarketDataError::Parse {
            line,
            detail: format!("invalid IV '{v}' (must be finite ≥ 0)"),
        });
    }
    Ok(Some(v))
}

/// CSV 市场数据源
///
/// 构造时一次性加载整个 CSV 到内存。
/// 后续 `mark_history` / `implied_vol` 查询是纯 `HashMap` 查找 + Vec 切片,O(1) 摊销。
#[derive(Debug)]
pub struct CsvMarketData {
    /// mark 历史
    marks: HashMap<Instrument, Vec<MarkPoint>>,
    /// 隐含波动率
    ivs: HashMap<Instrument, f64>,
}

impl CsvMarketData {
    /// 从 CSV 文件路径构造
    ///
    /// # Errors
    ///
    /// - `Io`:文件读取失败
    /// - `Parse`:列数不对 / 数值解析失败
    /// - `MissingColumn`:表头缺列
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, CsvMarketDataError> {
        let content = std::fs::read_to_string(path)?;
        Self::parse(&content)
    }

    /// 从 CSV 字符串构造(便于测试,无需写文件)
    pub fn parse(content: &str) -> Result<Self, CsvMarketDataError> {
        let mut marks: HashMap<Instrument, Vec<MarkPoint>> = HashMap::new();
        let mut ivs: HashMap<Instrument, f64> = HashMap::new();

        let mut lines = content.lines().enumerate();
        let header = lines
            .next()
            .ok_or_else(|| CsvMarketDataError::MissingColumn("(empty file)".into()))?
            .1;
        let cols: Vec<&str> = header.split(',').map(|s| s.trim()).collect();

        // 找列索引
        let inst_idx = cols
            .iter()
            .position(|c| *c == "instrument")
            .ok_or_else(|| CsvMarketDataError::MissingColumn("instrument".into()))?;
        let ts_idx = cols
            .iter()
            .position(|c| *c == "ts_ns")
            .ok_or_else(|| CsvMarketDataError::MissingColumn("ts_ns".into()))?;
        let mark_idx = cols
            .iter()
            .position(|c| *c == "mark")
            .ok_or_else(|| CsvMarketDataError::MissingColumn("mark".into()))?;
        let iv_idx = cols.iter().position(|c| *c == "iv"); // 可选

        for (i, line) in lines {
            let line_no = i + 1; // 1-based 行号
            let row: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
            if row.len() < cols.len() {
                return Err(CsvMarketDataError::Parse {
                    line: line_no,
                    detail: format!("expected {} columns, got {}", cols.len(), row.len()),
                });
            }

            // 0.8.0 B3 修复:helper 直接接受 `line_no`,无需后填充 map_err hack
            let inst = parse_instrument(row[inst_idx], line_no)?;
            let ts = parse_ts_ns(row[ts_idx], line_no)?;
            let mark: f64 = row[mark_idx]
                .parse()
                .map_err(|e: std::num::ParseFloatError| CsvMarketDataError::Parse {
                    line: line_no,
                    detail: format!("invalid mark '{}': {e}", row[mark_idx]),
                })?;
            if !mark.is_finite() {
                return Err(CsvMarketDataError::Parse {
                    line: line_no,
                    detail: format!("mark not finite: {mark}"),
                });
            }

            marks.entry(inst.clone()).or_default().push((ts, mark));

            if let Some(iv_idx) = iv_idx
                && let Some(iv) = parse_iv_opt(row[iv_idx], line_no)?
            {
                ivs.insert(inst, iv);
            }
        }

        // 按 ts 升序排序(允许 CSV 乱序)
        for v in marks.values_mut() {
            v.sort_by_key(|(ts, _)| *ts);
        }

        Ok(Self { marks, ivs })
    }

    /// 当前持有的 instrument 数量
    pub fn instrument_count(&self) -> usize {
        let mut all: std::collections::HashSet<&Instrument> = self.marks.keys().collect();
        all.extend(self.ivs.keys());
        all.len()
    }

    /// mark 历史总帧数
    pub fn total_mark_points(&self) -> usize {
        self.marks.values().map(|v| v.len()).sum()
    }
}

impl MarketDataSource for CsvMarketData {
    fn mark_history(&self, instrument: &Instrument, lookback: usize) -> Vec<MarkPoint> {
        match self.marks.get(instrument) {
            None => Vec::new(),
            Some(history) => {
                let start = history.len().saturating_sub(lookback);
                history[start..].to_vec()
            }
        }
    }

    fn implied_vol(&self, instrument: &Instrument) -> Option<f64> {
        self.ivs.get(instrument).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_CSV: &str = "\
instrument,ts_ns,mark,iv
BTC/USDT,1700000000000000000,50000.0,0.65
BTC/USDT,1700000001000000000,50100.0,0.64
BTC/USDT,1700000002000000000,50200.0,0.63
BTC/USDT:SWAP,1700000000000000000,50010.0,
ETH/USDT,1700000000000000000,3000.0,0.70
";

    #[test]
    fn parse_basic_csv() {
        let src = CsvMarketData::parse(SAMPLE_CSV).expect("parse ok");
        assert_eq!(src.instrument_count(), 3);
        assert_eq!(src.total_mark_points(), 5);
    }

    #[test]
    fn mark_history_per_instrument() {
        let src = CsvMarketData::parse(SAMPLE_CSV).expect("parse ok");
        let btc_spot = parse_instrument("BTC/USDT", 0).unwrap();
        let hist = src.mark_history(&btc_spot, 10);
        assert_eq!(hist.len(), 3, "BTC spot has 3 marks");
        assert_eq!(hist[0].1, 50_000.0);
        assert_eq!(hist[2].1, 50_200.0);
    }

    #[test]
    fn swap_instrument_recognized() {
        let src = CsvMarketData::parse(SAMPLE_CSV).expect("parse ok");
        let btc_swap = parse_instrument("BTC/USDT:SWAP", 0).unwrap();
        assert!(matches!(btc_swap, Instrument::Swap(_)));
        let hist = src.mark_history(&btc_swap, 10);
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].1, 50_010.0);
        // swap 在 CSV 中 IV 列为空 → None
        assert!(src.implied_vol(&btc_swap).is_none());
    }

    #[test]
    fn implied_vol_per_instrument() {
        let src = CsvMarketData::parse(SAMPLE_CSV).expect("parse ok");
        let btc_spot = parse_instrument("BTC/USDT", 0).unwrap();
        let eth_spot = parse_instrument("ETH/USDT", 0).unwrap();
        // BTC/USDT 在 CSV 中有 3 行,最后一行 IV=0.63 覆盖前两行(HashMap insert 语义)
        assert_eq!(src.implied_vol(&btc_spot), Some(0.63));
        // ETH/USDT 只有 1 行,IV=0.70
        assert_eq!(src.implied_vol(&eth_spot), Some(0.70));
    }

    #[test]
    fn mark_history_lookback_caps() {
        let src = CsvMarketData::parse(SAMPLE_CSV).expect("parse ok");
        let btc_spot = parse_instrument("BTC/USDT", 0).unwrap();
        let last_2 = src.mark_history(&btc_spot, 2);
        assert_eq!(last_2.len(), 2);
        assert_eq!(last_2[0].1, 50_100.0);
        assert_eq!(last_2[1].1, 50_200.0);
    }

    #[test]
    fn out_of_order_csv_is_sorted() {
        let csv = "\
instrument,ts_ns,mark,iv
BTC/USDT,300,3.0,
BTC/USDT,100,1.0,
BTC/USDT,200,2.0,
";
        let src = CsvMarketData::parse(csv).expect("parse ok");
        let btc = parse_instrument("BTC/USDT", 0).unwrap();
        let hist = src.mark_history(&btc, 10);
        assert_eq!(hist.len(), 3);
        // 应按 ts 升序
        assert_eq!(hist[0].0, Timestamp::from_nanos(100));
        assert_eq!(hist[1].0, Timestamp::from_nanos(200));
        assert_eq!(hist[2].0, Timestamp::from_nanos(300));
    }

    #[test]
    fn missing_required_column_errors() {
        // 缺 mark 列(instrument + ts_ns + mark 是必需三列)
        let csv = "instrument,ts_ns\nBTC/USDT,100\n";
        let err = CsvMarketData::parse(csv).expect_err("should fail without mark column");
        assert!(matches!(err, CsvMarketDataError::MissingColumn(ref c) if c == "mark"));
        // 缺 instrument 列
        let csv = "ts_ns,mark\n100,1.0\n";
        let err = CsvMarketData::parse(csv).expect_err("should fail without instrument column");
        assert!(matches!(err, CsvMarketDataError::MissingColumn(ref c) if c == "instrument"));
    }

    #[test]
    fn invalid_mark_errors() {
        let csv = "instrument,ts_ns,mark,iv\nBTC/USDT,100,not_a_number,\n";
        let err = CsvMarketData::parse(csv).expect_err("invalid mark should fail");
        assert!(matches!(err, CsvMarketDataError::Parse { line: 2, .. }));
    }

    #[test]
    fn invalid_instrument_errors() {
        let csv = "instrument,ts_ns,mark,iv\nINVALID,100,1.0,\n";
        let err = CsvMarketData::parse(csv).expect_err("invalid instrument should fail");
        assert!(matches!(err, CsvMarketDataError::Parse { line: 2, .. }));
    }

    #[test]
    fn empty_file_errors() {
        let csv = "";
        let err = CsvMarketData::parse(csv).expect_err("empty file should fail");
        assert!(matches!(err, CsvMarketDataError::MissingColumn(_)));
    }

    #[test]
    fn iv_optional_column_absent() {
        // iv 列完全不存在 — 应该正常工作,所有 instrument 无 IV
        let csv = "\
instrument,ts_ns,mark
BTC/USDT,100,1.0
BTC/USDT,200,2.0
";
        let src = CsvMarketData::parse(csv).expect("parse ok without iv column");
        let btc = parse_instrument("BTC/USDT", 0).unwrap();
        assert_eq!(src.mark_history(&btc, 10).len(), 2);
        assert!(src.implied_vol(&btc).is_none());
    }

    // ─── 0.8.0 B3 新增测试:`mark_returns` / `latest_return`(继承自默认实现) ───

    #[test]
    fn csv_mark_returns_inherits_default() {
        // 默认实现:从 mark_history 派生 returns
        let csv = "\
instrument,ts_ns,mark,iv
BTC/USDT,100,100.0,0.5
BTC/USDT,200,110.0,0.5
BTC/USDT,300,121.0,0.5
";
        let src = CsvMarketData::parse(csv).expect("parse ok");
        let btc = parse_instrument("BTC/USDT", 0).unwrap();
        let r = src.mark_returns(&btc, 10);
        assert_eq!(r.len(), 2);
        assert!((r[0] - 0.10).abs() < 1e-9);
        assert!((r[1] - 0.10).abs() < 1e-9);
        // latest_return 与 mark_returns 末元素一致
        let lr = src.latest_return(&btc).expect("latest_return");
        assert!((lr - r[1]).abs() < 1e-12);
    }

    // ─── 0.8.0 B3 修复:helper 错误带行号 ───

    /// 验证 `parse_instrument` 错误带正确行号(直接调用 helper,不走 `parse` 主路径)
    #[test]
    fn parse_instrument_error_carries_line_param() {
        // 无效 spot 格式:line=42 应在错误中出现
        let err = parse_instrument("INVALID", 42).expect_err("invalid spot should fail");
        match err {
            CsvMarketDataError::Parse { line, detail } => {
                assert_eq!(line, 42, "错误 line 字段应=42,而非硬编码 0");
                assert!(
                    detail.contains("invalid spot instrument label"),
                    "detail={detail}"
                );
            }
            other => panic!("expected Parse error, got {other:?}"),
        }
        // 无效 swap 格式:line=99
        let err = parse_instrument("BTC/:SWAP", 99).expect_err("invalid swap should fail");
        match err {
            CsvMarketDataError::Parse { line, detail } => {
                assert_eq!(line, 99, "错误 line 字段应=99");
                assert!(
                    detail.contains("invalid swap instrument label"),
                    "detail={detail}"
                );
            }
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    /// 验证 `parse_iv_opt` 错误带正确行号
    #[test]
    fn parse_iv_opt_error_carries_line_param() {
        // 无效 IV(非数字)
        let err = parse_iv_opt("not_a_number", 17).expect_err("invalid IV should fail");
        match err {
            CsvMarketDataError::Parse { line, detail } => {
                assert_eq!(line, 17, "错误 line 字段应=17");
                assert!(detail.contains("invalid IV"), "detail={detail}");
            }
            other => panic!("expected Parse error, got {other:?}"),
        }
        // 负数 IV
        let err = parse_iv_opt("-0.5", 25).expect_err("negative IV should fail");
        match err {
            CsvMarketDataError::Parse { line, .. } => {
                assert_eq!(line, 25, "负数 IV 错误 line 应=25");
            }
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    /// 验证 `parse_ts_ns` 错误带正确行号(0.8.0 B3 修复:`Timestamp` 错误新增 `line` 字段)
    #[test]
    fn parse_ts_ns_error_carries_line_param() {
        let err = parse_ts_ns("not_a_number", 7).expect_err("invalid ts should fail");
        match err {
            CsvMarketDataError::Timestamp { line, raw, err } => {
                assert_eq!(line, 7, "Timestamp 错误 line 字段应=7");
                assert_eq!(raw, "not_a_number");
                assert!(!err.is_empty());
            }
            other => panic!("expected Timestamp error, got {other:?}"),
        }
    }

    /// 端到端验证:CSV 解析在多行错位时,错误能精确定位到出错行
    #[test]
    fn parse_csv_reports_correct_line_for_invalid_instrument() {
        // header 是第 1 行,BADFORMAT 是第 6 行(header + 4 valid + 1 invalid)
        let csv = "\
instrument,ts_ns,mark,iv
BTC/USDT,100,1.0,0.5
BTC/USDT,200,2.0,0.5
BTC/USDT,300,3.0,0.5
BTC/USDT,400,4.0,0.5
BADFORMAT,500,5.0,0.5
";
        let err = CsvMarketData::parse(csv).expect_err("invalid instrument should fail");
        match err {
            CsvMarketDataError::Parse { line, detail } => {
                assert_eq!(
                    line, 6,
                    "错误应定位到第 6 行(header=1 + 4 valid + 1 invalid)"
                );
                assert!(detail.contains("invalid spot instrument label"));
            }
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    /// 端到端验证:无效时间戳错误带正确行号(0.8.0 B3 修复)
    #[test]
    fn parse_csv_reports_correct_line_for_invalid_timestamp() {
        // header 是第 1 行;2 个 valid 数据行后 invalid ts 在第 4 行
        // (enumerate 在 header 被消费后继续从 1 计数,line_no = i + 1)
        let csv = "\
instrument,ts_ns,mark,iv
BTC/USDT,100,1.0,0.5
BTC/USDT,200,2.0,0.5
BTC/USDT,not_a_number,3.0,0.5
";
        let err = CsvMarketData::parse(csv).expect_err("invalid ts should fail");
        match err {
            CsvMarketDataError::Timestamp { line, raw, .. } => {
                assert_eq!(line, 4, "时间戳错误应定位到第 4 行");
                assert_eq!(raw, "not_a_number");
            }
            other => panic!("expected Timestamp error, got {other:?}"),
        }
    }

    /// 端到端验证:无效 IV 错误带正确行号
    #[test]
    fn parse_csv_reports_correct_line_for_invalid_iv() {
        // header 是第 1 行;2 个 valid 数据行后 invalid IV 在第 4 行
        let csv = "\
instrument,ts_ns,mark,iv
BTC/USDT,100,1.0,0.5
BTC/USDT,200,2.0,0.5
BTC/USDT,300,3.0,not_a_number
";
        let err = CsvMarketData::parse(csv).expect_err("invalid IV should fail");
        match err {
            CsvMarketDataError::Parse { line, detail } => {
                assert_eq!(line, 4, "IV 错误应定位到第 4 行");
                assert!(detail.contains("invalid IV"));
            }
            other => panic!("expected Parse error, got {other:?}"),
        }
    }
}
