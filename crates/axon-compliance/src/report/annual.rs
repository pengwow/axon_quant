//! 年报生成器
//!
//! 从交易记录生成年报，计算年度统计指标。

use crate::report::AnnualReport;
use crate::report::monthly::compute_sharpe_ratio;
use crate::types::TradeRecord;

/// 年报生成器
pub struct AnnualReportGenerator;

impl AnnualReportGenerator {
    /// 从交易记录生成年报
    ///
    /// # 参数
    /// - `year`: 年份
    /// - `account_id`: 账户 ID
    /// - `initial_balance`: 年初余额
    /// - `trades`: 当年所有交易
    /// - `active_months`: 活跃月数
    pub fn generate(
        year: u32,
        account_id: &str,
        initial_balance: f64,
        trades: &[&TradeRecord],
        active_months: u32,
    ) -> AnnualReport {
        // 计算总盈亏
        let total_pnl: f64 = trades.iter().filter_map(|t| t.realized_pnl).sum();

        // 计算总手续费
        let total_fees: f64 = trades.iter().map(|t| t.fee).sum();

        // 统计盈亏交易
        let winning_trades = trades
            .iter()
            .filter(|t| t.realized_pnl.unwrap_or(0.0) > 0.0)
            .count() as u32;

        let total_trades = trades.len() as u32;

        // 计算胜率
        let win_rate = if total_trades > 0 {
            winning_trades as f64 / total_trades as f64
        } else {
            0.0
        };

        // 计算总回报（净盈亏 - 手续费）
        let total_return = total_pnl - total_fees;

        // 计算年化回报率（百分比）
        let annual_return_pct = if initial_balance > 0.0 {
            (total_return / initial_balance) * 100.0
        } else {
            0.0
        };

        // 计算夏普比率
        let pnl_values: Vec<f64> = trades.iter().filter_map(|t| t.realized_pnl).collect();
        let sharpe_ratio = compute_sharpe_ratio(&pnl_values);

        // 生成合规评分（简化版：基于交易活跃度和胜率）
        let compliance_score = compute_compliance_score(total_trades, win_rate, active_months);

        // 生成监管备注
        let regulatory_notes = generate_regulatory_notes(total_trades, win_rate, annual_return_pct);

        AnnualReport {
            year,
            account_id: account_id.into(),
            total_return,
            annual_return_pct,
            total_fees,
            total_trades,
            win_rate,
            sharpe_ratio,
            max_drawdown: 0.0,
            compliance_score,
            regulatory_notes,
            active_months,
        }
    }
}

/// 计算合规评分（0-100）
fn compute_compliance_score(total_trades: u32, win_rate: f64, active_months: u32) -> f64 {
    // 活跃度分（40 分）：有交易即满分
    let activity_score = if total_trades > 0 { 40.0 } else { 0.0 };
    // 胜率分（40 分）
    let win_rate_score = win_rate * 40.0;
    // 一致性分（20 分）：活跃 6 个月以上满分
    let consistency_score = if active_months >= 6 {
        20.0
    } else {
        active_months as f64 * (20.0 / 6.0)
    };

    activity_score + win_rate_score + consistency_score
}

/// 生成监管备注
fn generate_regulatory_notes(
    total_trades: u32,
    win_rate: f64,
    annual_return_pct: f64,
) -> Vec<String> {
    let mut notes = Vec::new();

    if total_trades == 0 {
        notes.push("无交易活动".into());
    }

    if win_rate > 0.8 {
        notes.push("胜率异常高，建议人工审查".into());
    }

    if annual_return_pct > 100.0 {
        notes.push("年化回报率超过 100%，需额外合规审查".into());
    }

    if notes.is_empty() {
        notes.push("正常".into());
    }

    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LiquidityType, OrderType, TradeSide, TradeStatus};
    use chrono::Utc;
    use uuid::Uuid;

    fn make_trade(realized_pnl: Option<f64>, fee: f64) -> TradeRecord {
        TradeRecord {
            trade_id: Uuid::new_v4(),
            order_id: Uuid::new_v4(),
            strategy_id: "test".into(),
            symbol: "BTCUSDT".into(),
            side: TradeSide::Buy,
            quantity: 1.0,
            price: 50000.0,
            notional_value: 50000.0,
            fee,
            fee_currency: "USDT".into(),
            exchange: "Binance".into(),
            execution_time: Utc::now(),
            settlement_time: None,
            status: TradeStatus::Filled,
            order_type: OrderType::Market,
            exchange_trade_id: None,
            liquidity: LiquidityType::Taker,
            realized_pnl,
            funding_rate: None,
            slippage: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn test_annual_report_generation() {
        let trades = [
            make_trade(Some(1000.0), 100.0),
            make_trade(Some(-300.0), 50.0),
        ];
        let trade_refs: Vec<&TradeRecord> = trades.iter().collect();

        let report = AnnualReportGenerator::generate(2026, "test", 100000.0, &trade_refs, 12);

        assert_eq!(report.year, 2026);
        assert_eq!(report.total_trades, 2);
        assert!((report.total_return - 550.0).abs() < f64::EPSILON);
        assert!((report.annual_return_pct - 0.55).abs() < 0.01);
    }

    #[test]
    fn test_empty_annual_report() {
        let trades: Vec<&TradeRecord> = vec![];
        let report = AnnualReportGenerator::generate(2026, "test", 100000.0, &trades, 0);

        assert_eq!(report.total_trades, 0);
        assert!((report.total_return).abs() < f64::EPSILON);
        assert!(report.regulatory_notes.contains(&"无交易活动".to_string()));
    }

    #[test]
    fn test_high_win_rate_warning() {
        // 创建 10 笔盈利交易，胜率 100%
        let trades: Vec<TradeRecord> = (0..10).map(|_| make_trade(Some(100.0), 10.0)).collect();
        let trade_refs: Vec<&TradeRecord> = trades.iter().collect();

        let report = AnnualReportGenerator::generate(2026, "test", 100000.0, &trade_refs, 12);

        assert!(
            report
                .regulatory_notes
                .iter()
                .any(|n| n.contains("胜率异常"))
        );
    }
}
