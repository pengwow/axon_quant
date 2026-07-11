//! 监管报送生成
//!
//! 生成监管报送并支持 JSON/CSV 格式导出。

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::ComplianceConfig;
use crate::error::{ComplianceError, ComplianceResult};
use crate::types::TradeRecord;

use super::metrics::RegulatoryMetricsCalculator;
use super::{RegulatorFormat, RegulatorySubmission, SubmissionType};

/// 监管报送生成器
pub struct SubmissionGenerator<'a> {
    /// 合规配置（保留用于未来扩展）
    #[allow(dead_code)]
    config: &'a ComplianceConfig,
    /// 指标计算器
    calculator: RegulatoryMetricsCalculator<'a>,
}

impl<'a> SubmissionGenerator<'a> {
    /// 创建新的生成器
    pub fn new(config: &'a ComplianceConfig, trades: &'a [TradeRecord]) -> Self {
        Self {
            config,
            calculator: RegulatoryMetricsCalculator::new(config, trades),
        }
    }

    /// 生成监管报送
    pub fn generate(
        &self,
        regulator: &str,
        submission_type: SubmissionType,
        period_start: DateTime<Utc>,
        period_end: DateTime<Utc>,
        format: RegulatorFormat,
    ) -> ComplianceResult<RegulatorySubmission> {
        // 计算监管指标
        let data = self.calculator.calculate_all();

        // 检查是否有违规
        let has_breach = data.position_limits.iter().any(|p| p.breach)
            || data.concentration_limits.iter().any(|c| c.breach);

        // 如果有违规，记录日志
        if has_breach {
            tracing::warn!("监管指标违规 detected for regulator: {}", regulator);
        }

        Ok(RegulatorySubmission {
            submission_id: Uuid::new_v4(),
            regulator: regulator.to_string(),
            submission_type,
            period_start,
            period_end,
            data,
            format,
            generated_at: Utc::now(),
            submitted_at: None,
        })
    }

    /// 导出为指定格式
    pub fn export(submission: &RegulatorySubmission) -> ComplianceResult<Vec<u8>> {
        match submission.format {
            RegulatorFormat::JSON => Self::export_json(submission),
            RegulatorFormat::CSV => Self::export_csv(submission),
        }
    }

    /// 导出为 JSON
    fn export_json(submission: &RegulatorySubmission) -> ComplianceResult<Vec<u8>> {
        let json = serde_json::to_string_pretty(submission)
            .map_err(|e| ComplianceError::SerializationError(e.to_string()))?;
        Ok(json.into_bytes())
    }

    /// 导出为 CSV
    fn export_csv(submission: &RegulatorySubmission) -> ComplianceResult<Vec<u8>> {
        let mut wtr = csv::Writer::from_writer(vec![]);

        // 写入持仓限制
        for limit in &submission.data.position_limits {
            wtr.serialize(limit)
                .map_err(|e| ComplianceError::SerializationError(e.to_string()))?;
        }

        // 写入集中度检查
        for check in &submission.data.concentration_limits {
            wtr.serialize(check)
                .map_err(|e| ComplianceError::SerializationError(e.to_string()))?;
        }

        // 写入大额交易报告
        for report in &submission.data.large_trade_reports {
            wtr.serialize(report)
                .map_err(|e| ComplianceError::SerializationError(e.to_string()))?;
        }

        wtr.flush()
            .map_err(|e| ComplianceError::SerializationError(e.to_string()))?;

        wtr.into_inner()
            .map_err(|e| ComplianceError::SerializationError(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LiquidityType, OrderType, TradeSide, TradeStatus};
    use uuid::Uuid;

    /// 创建测试配置
    fn create_test_config() -> ComplianceConfig {
        ComplianceConfig {
            account_id: "test".into(),
            base_currency: "USDT".into(),
            large_trade_threshold: 10000.0,
            position_limit: 100.0,
            max_portfolio_concentration: 30.0,
            data_retention_years: 7,
            regulators: vec!["SEC".into()],
        }
    }

    /// 创建测试交易
    fn create_test_trade(symbol: &str, quantity: f64, price: f64) -> TradeRecord {
        TradeRecord {
            trade_id: Uuid::new_v4(),
            order_id: Uuid::new_v4(),
            strategy_id: "test".into(),
            symbol: symbol.into(),
            side: TradeSide::Buy,
            quantity,
            price,
            notional_value: quantity * price,
            fee: 0.0,
            fee_currency: "USDT".into(),
            exchange: "Binance".into(),
            execution_time: Utc::now(),
            settlement_time: None,
            status: TradeStatus::Filled,
            order_type: OrderType::Market,
            exchange_trade_id: None,
            liquidity: LiquidityType::Taker,
            realized_pnl: None,
            funding_rate: None,
            slippage: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn test_submission_generation() {
        let config = create_test_config();
        let trades = [create_test_trade("BTCUSDT", 1.0, 50000.0)];
        let generator = SubmissionGenerator::new(&config, &trades);

        let now = Utc::now();
        let submission = generator
            .generate(
                "SEC",
                SubmissionType::Daily,
                now,
                now,
                RegulatorFormat::JSON,
            )
            .unwrap();

        assert_eq!(submission.regulator, "SEC");
        assert_eq!(submission.data.total_turnover, 50000.0);
    }

    #[test]
    fn test_json_export() {
        let config = create_test_config();
        let trades = [create_test_trade("BTCUSDT", 1.0, 50000.0)];
        let generator = SubmissionGenerator::new(&config, &trades);

        let now = Utc::now();
        let submission = generator
            .generate(
                "SEC",
                SubmissionType::Daily,
                now,
                now,
                RegulatorFormat::JSON,
            )
            .unwrap();

        let data = SubmissionGenerator::export(&submission).unwrap();
        let json_str = String::from_utf8(data).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        assert!(parsed.is_object());
        assert_eq!(parsed["regulator"], "SEC");
    }

    #[test]
    fn test_csv_export() {
        let config = create_test_config();
        let trades = [create_test_trade("BTCUSDT", 1.0, 50000.0)];
        let generator = SubmissionGenerator::new(&config, &trades);

        let now = Utc::now();
        let submission = generator
            .generate("SEC", SubmissionType::Daily, now, now, RegulatorFormat::CSV)
            .unwrap();

        let data = SubmissionGenerator::export(&submission).unwrap();
        let csv_str = String::from_utf8(data).unwrap();

        // CSV 应包含数据行
        let lines: Vec<&str> = csv_str.trim().lines().collect();
        assert!(!lines.is_empty());
    }
}
