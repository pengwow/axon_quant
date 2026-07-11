//! 流式报告导出
//!
//! 支持 JSON / CSV / HTML 三种格式导出 `StreamingSnapshot` + `equity_curve`。
//!
//! ## 使用示例
//!
//! ```rust,no_run
//! use axon_backtest::streaming::{StreamingEngine, TradingMode, ReportFormat};
//!
//! let engine = StreamingEngine::new(TradingMode::Backtest);
//! let report = engine.report();
//! let json = report.export(ReportFormat::Json).unwrap();
//! ```

use std::fmt;
use std::fs;
use std::io::Write;
use std::path::Path;

use super::engine::StreamingSnapshot;
use super::metrics::EquityPoint;

/// 报告导出格式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    /// JSON 格式(pretty-print)
    Json,
    /// CSV 格式(snapshot 扁平化 + equity_curve 单独输出)
    Csv,
    /// HTML 格式(内嵌模板,纯 HTML,无外部依赖)
    Html,
}

/// 报告导出错误
#[derive(Debug)]
pub enum ReportError {
    /// JSON 序列化失败
    SerializeError(String),
    /// 文件写入失败
    IoError(std::io::Error),
}

impl fmt::Display for ReportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SerializeError(e) => write!(f, "serialize error: {e}"),
            Self::IoError(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for ReportError {}

impl From<std::io::Error> for ReportError {
    fn from(e: std::io::Error) -> Self {
        Self::IoError(e)
    }
}

/// 流式报告（组合 snapshot + equity_curve）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StreamingReport {
    /// 引擎快照（含 metrics）
    pub snapshot: StreamingSnapshot,
    /// 权益曲线
    pub equity_curve: Vec<EquityPoint>,
}

impl StreamingReport {
    /// 从 engine 构造报告
    pub fn from_engine(engine: &super::engine::StreamingEngine) -> Self {
        Self {
            snapshot: engine.metrics_snapshot(),
            equity_curve: engine.equity_curve(),
        }
    }

    /// 导出为指定格式的字节流
    pub fn export(&self, format: ReportFormat) -> Result<Vec<u8>, ReportError> {
        match format {
            ReportFormat::Json => self.export_json(),
            ReportFormat::Csv => self.export_csv(),
            ReportFormat::Html => self.export_html(),
        }
    }

    /// 导出到文件
    pub fn export_to_file(&self, path: &Path, format: ReportFormat) -> Result<(), ReportError> {
        let data = self.export(format)?;
        let mut file = fs::File::create(path)?;
        file.write_all(&data)?;
        Ok(())
    }

    fn export_json(&self) -> Result<Vec<u8>, ReportError> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| ReportError::SerializeError(e.to_string()))?;
        Ok(json.into_bytes())
    }

    fn export_csv(&self) -> Result<Vec<u8>, ReportError> {
        let mut wtr = csv::Writer::from_writer(vec![]);

        // snapshot 扁平化: 表头 + 1 行数据
        let headers = [
            "portfolio_nav",
            "active_orders",
            "total_trades",
            "mode",
            "total_pnl",
            "total_fees",
            "win_rate",
            "sharpe_ratio",
            "max_drawdown",
            "max_drawdown_pct",
            "nav_peak",
            "final_nav",
            "equity_curve_len",
        ];
        let mode_str = match &self.snapshot.mode {
            super::engine::TradingMode::Backtest => "backtest",
            super::engine::TradingMode::PaperTrading => "paper_trading",
            super::engine::TradingMode::LiveTrading => "live_trading",
        };
        let values = [
            self.snapshot.portfolio_nav.to_string(),
            self.snapshot.active_orders.to_string(),
            self.snapshot.total_trades.to_string(),
            mode_str.to_string(),
            format!("{:.6}", self.snapshot.total_pnl),
            format!("{:.6}", self.snapshot.total_fees),
            format!("{:.6}", self.snapshot.win_rate),
            format!("{:.6}", self.snapshot.sharpe_ratio),
            format!("{:.6}", self.snapshot.max_drawdown),
            format!("{:.6}", self.snapshot.max_drawdown_pct),
            format!("{:.6}", self.snapshot.nav_peak),
            format!("{:.6}", self.snapshot.final_nav),
            self.snapshot.equity_curve_len.to_string(),
        ];
        wtr.write_record(headers)
            .map_err(|e| ReportError::SerializeError(e.to_string()))?;
        wtr.write_record(values)
            .map_err(|e| ReportError::SerializeError(e.to_string()))?;

        // equity_curve 独立输出(补齐空字段以匹配 snapshot 列数)
        if !self.equity_curve.is_empty() {
            let empty = "";
            wtr.write_record([
                "equity_timestamp_ns",
                "equity_nav",
                empty,
                empty,
                empty,
                empty,
                empty,
                empty,
                empty,
                empty,
                empty,
                empty,
                empty,
            ])
            .map_err(|e| ReportError::SerializeError(e.to_string()))?;
            for pt in &self.equity_curve {
                wtr.write_record([
                    pt.timestamp.nanos.to_string(),
                    format!("{:.6}", pt.nav),
                    empty.into(),
                    empty.into(),
                    empty.into(),
                    empty.into(),
                    empty.into(),
                    empty.into(),
                    empty.into(),
                    empty.into(),
                    empty.into(),
                    empty.into(),
                    empty.into(),
                ])
                .map_err(|e| ReportError::SerializeError(e.to_string()))?;
            }
        }

        wtr.flush()
            .map_err(|e| ReportError::SerializeError(e.to_string()))?;
        let data = wtr
            .into_inner()
            .map_err(|e| ReportError::SerializeError(e.to_string()))?;
        Ok(data)
    }

    fn export_html(&self) -> Result<Vec<u8>, ReportError> {
        let s = &self.snapshot;
        let mode_str = match s.mode {
            super::engine::TradingMode::Backtest => "Backtest",
            super::engine::TradingMode::PaperTrading => "Paper Trading",
            super::engine::TradingMode::LiveTrading => "Live Trading",
        };

        let mut rows = String::new();
        for pt in &self.equity_curve {
            rows.push_str(&format!(
                "<tr><td>{}</td><td>{:.6}</td></tr>\n",
                pt.timestamp.nanos, pt.nav,
            ));
        }

        let html = format!(
            r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>Streaming Report</title>
<style>
  body {{ font-family: sans-serif; margin: 2rem; }}
  h1 {{ color: #333; }}
  table {{ border-collapse: collapse; margin: 1rem 0; }}
  th, td {{ border: 1px solid #ddd; padding: 8px 12px; text-align: right; }}
  th {{ background: #f5f5f5; text-align: left; }}
  .metric {{ margin: 0.3rem 0; }}
  .label {{ font-weight: bold; display: inline-block; width: 180px; }}
</style>
</head>
<body>
<h1>Streaming Report</h1>
<div class="metric"><span class="label">Mode:</span> {mode}</div>
<div class="metric"><span class="label">Portfolio NAV:</span> {nav}</div>
<div class="metric"><span class="label">Active Orders:</span> {active}</div>
<div class="metric"><span class="label">Total Trades:</span> {trades}</div>
<hr>
<div class="metric"><span class="label">Total PnL:</span> {pnl:.6}</div>
<div class="metric"><span class="label">Total Fees:</span> {fees:.6}</div>
<div class="metric"><span class="label">Win Rate:</span> {win_rate:.6}</div>
<div class="metric"><span class="label">Sharpe Ratio:</span> {sharpe:.6}</div>
<div class="metric"><span class="label">Max Drawdown:</span> {max_dd:.6}</div>
<div class="metric"><span class="label">Max Drawdown %:</span> {max_dd_pct:.6}</div>
<div class="metric"><span class="label">NAV Peak:</span> {nav_peak:.6}</div>
<div class="metric"><span class="label">Final NAV:</span> {final_nav:.6}</div>
<div class="metric"><span class="label">Equity Curve Points:</span> {eq_len}</div>
<hr>
<h2>Equity Curve</h2>
<table>
<tr><th>Timestamp (ns)</th><th>NAV</th></tr>
{rows}
</table>
</body>
</html>"#,
            mode = mode_str,
            nav = s.portfolio_nav,
            active = s.active_orders,
            trades = s.total_trades,
            pnl = s.total_pnl,
            fees = s.total_fees,
            win_rate = s.win_rate,
            sharpe = s.sharpe_ratio,
            max_dd = s.max_drawdown,
            max_dd_pct = s.max_drawdown_pct,
            nav_peak = s.nav_peak,
            final_nav = s.final_nav,
            eq_len = s.equity_curve_len,
            rows = rows,
        );

        Ok(html.into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::engine::TradingMode;

    fn test_snapshot() -> StreamingSnapshot {
        StreamingSnapshot {
            portfolio_nav: 100_000_000,
            active_orders: 0,
            total_trades: 5,
            mode: TradingMode::Backtest,
            total_pnl: 1500.0,
            total_fees: 12.5,
            win_rate: 0.6,
            sharpe_ratio: 1.8,
            max_drawdown: 200.0,
            max_drawdown_pct: 0.02,
            nav_peak: 101_500.0,
            final_nav: 101_500.0,
            equity_curve_len: 5,
        }
    }

    fn test_curve() -> Vec<EquityPoint> {
        use axon_core::time::Timestamp;
        vec![
            EquityPoint {
                timestamp: Timestamp::from_nanos(1_000),
                nav: 100_000.0,
            },
            EquityPoint {
                timestamp: Timestamp::from_nanos(2_000),
                nav: 101_500.0,
            },
        ]
    }

    fn test_report() -> StreamingReport {
        StreamingReport {
            snapshot: test_snapshot(),
            equity_curve: test_curve(),
        }
    }

    #[test]
    fn json_export_is_valid_json() {
        let report = test_report();
        let data = report.export(ReportFormat::Json).unwrap();
        let json_str = String::from_utf8(data).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert!(parsed.is_object());
        assert!(parsed.get("snapshot").is_some());
        assert!(parsed.get("equity_curve").is_some());
    }

    #[test]
    fn csv_export_has_headers_and_data() {
        let report = test_report();
        let data = report.export(ReportFormat::Csv).unwrap();
        let csv_str = String::from_utf8(data).unwrap();
        let lines: Vec<&str> = csv_str.trim().lines().collect();
        // 至少: snapshot header + snapshot data + equity header + 2 equity rows
        assert!(lines.len() >= 5);
        assert!(lines[0].contains("total_pnl"));
    }

    #[test]
    fn html_export_contains_metrics() {
        let report = test_report();
        let data = report.export(ReportFormat::Html).unwrap();
        let html = String::from_utf8(data).unwrap();
        assert!(html.contains("Sharpe Ratio"));
        assert!(html.contains("Max Drawdown"));
        assert!(html.contains("Win Rate"));
        assert!(html.contains("Equity Curve"));
    }

    #[test]
    fn export_to_file_writes_json() {
        let report = test_report();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        report.export_to_file(&path, ReportFormat::Json).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(parsed.is_object());
    }
}
