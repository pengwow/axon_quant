//! axon-compliance: 金融交易合规审计模块
//!
//! 提供交易记录、不可变审计日志、报告生成和监管报送功能。
//!
//! # 特性
//!
//! - **交易记录**：完整的订单、成交记录
//! - **审计日志**：区块链式哈希链，不可篡改
//! - **报告生成**：日报、月报、年报
//! - **监管报送**：对接监管报送系统
//!
//! # 使用示例
//!
//! ```rust
//! use axon_compliance::{ComplianceModule, ComplianceConfig, TradeRecord, TradeSide, TradeStatus, OrderType, LiquidityType};
//! use chrono::Utc;
//! use uuid::Uuid;
//!
//! // 创建配置
//! let config = ComplianceConfig {
//!     account_id: "test_account".into(),
//!     base_currency: "USDT".into(),
//!     large_trade_threshold: 100000.0,
//!     position_limit: 1000000.0,
//!     max_portfolio_concentration: 0.4,
//!     data_retention_years: 7,
//!     regulators: vec!["SEC".into()],
//! };
//!
//! // 创建合规模块
//! let mut compliance = ComplianceModule::new(config, "/tmp/axon_compliance").unwrap();
//!
//! // 记录交易
//! let trade = TradeRecord {
//!     trade_id: Uuid::new_v4(),
//!     order_id: Uuid::new_v4(),
//!     strategy_id: "test_strategy".into(),
//!     symbol: "BTCUSDT".into(),
//!     side: TradeSide::Buy,
//!     quantity: 1.0,
//!     price: 50000.0,
//!     notional_value: 50000.0,
//!     fee: 50.0,
//!     fee_currency: "USDT".into(),
//!     exchange: "Binance".into(),
//!     execution_time: Utc::now(),
//!     settlement_time: None,
//!     status: TradeStatus::Filled,
//!     order_type: OrderType::Market,
//!     exchange_trade_id: None,
//!     liquidity: LiquidityType::Taker,
//!     realized_pnl: None,
//!     funding_rate: None,
//!     slippage: None,
//!     created_at: Utc::now(),
//! };
//!
//! compliance.record_trade(trade).unwrap();
//!
//! // 验证审计完整性
//! assert!(compliance.verify_audit_integrity());
//! ```

pub mod audit;
pub mod error;
#[cfg(feature = "python")]
pub mod python;
pub mod regulator;
pub mod report;
pub mod types;

// 重新导出常用类型
pub use audit::{AuditLog, FileStorage};
pub use error::{ComplianceError, ComplianceResult};
pub use regulator::{
    ConcentrationCheck, LargeTradeReport, PositionLimit, RegulatorFormat, RegulatoryData,
    RegulatorySubmission, SubmissionType,
};
pub use report::{AnnualReport, DailyReport, MonthlyReport, ReportExporter, ReportFormat};
pub use types::{
    AuditEvent, AuditEventType, ComplianceConfig, LiquidityType, OrderId, OrderType, TradeFilter,
    TradeId, TradeRecord, TradeSide, TradeStats, TradeStatus,
};

use chrono::{DateTime, Datelike, NaiveDate, Utc};

/// 合规模块主结构
pub struct ComplianceModule {
    /// 交易记录
    trade_records: Vec<TradeRecord>,
    /// 审计日志
    audit_log: AuditLog,
    /// 配置
    config: ComplianceConfig,
    /// 文件存储
    storage: FileStorage,
}

impl ComplianceModule {
    /// 创建新的合规模块
    pub fn new(
        config: ComplianceConfig,
        storage_path: impl AsRef<std::path::Path>,
    ) -> ComplianceResult<Self> {
        let storage = FileStorage::new(storage_path)?;

        Ok(Self {
            trade_records: Vec::new(),
            audit_log: AuditLog::new(),
            config,
            storage,
        })
    }

    /// 记录交易（自动生成审计事件）
    pub fn record_trade(&mut self, trade: TradeRecord) -> ComplianceResult<()> {
        // 验证交易数据
        self.validate_trade(&trade)?;

        // 检查监管阈值
        self.check_regulatory_thresholds(&trade)?;

        // 生成审计事件
        let event = AuditEvent {
            event_id: uuid::Uuid::new_v4(),
            timestamp: Utc::now(),
            event_type: AuditEventType::TradeExecuted,
            actor: trade.strategy_id.clone(),
            action: "trade_executed".into(),
            resource_type: "trade".into(),
            resource_id: trade.trade_id.to_string(),
            details: serde_json::json!({
                "symbol": trade.symbol,
                "side": format!("{:?}", trade.side),
                "quantity": trade.quantity,
                "price": trade.price,
                "notional": trade.notional_value,
                "fee": trade.fee,
            }),
            previous_hash: String::new(),
            event_hash: String::new(),
            ip_address: None,
            session_id: None,
        };

        // 记录审计事件
        self.log_audit_event(event)?;

        // 记录交易
        self.trade_records.push(trade);

        Ok(())
    }

    /// 记录审计事件
    pub fn log_audit_event(&mut self, event: AuditEvent) -> ComplianceResult<()> {
        // 添加到审计日志
        self.audit_log.log_event(event.clone())?;

        // 持久化到文件系统
        self.storage.save_event(&event)?;

        Ok(())
    }

    /// 验证审计日志完整性
    pub fn verify_audit_integrity(&self) -> bool {
        self.audit_log.verify_integrity()
    }

    /// 查询交易记录
    pub fn query_trades(&self, filter: &TradeFilter) -> Vec<&TradeRecord> {
        self.trade_records
            .iter()
            .filter(|t| {
                // 按交易对过滤
                if let Some(ref symbol) = filter.symbol
                    && t.symbol != *symbol
                {
                    return false;
                }

                // 按策略 ID 过滤
                if let Some(ref strategy_id) = filter.strategy_id
                    && t.strategy_id != *strategy_id
                {
                    return false;
                }

                // 按交易方向过滤
                if let Some(ref side) = filter.side
                    && t.side != *side
                {
                    return false;
                }

                // 按时间范围过滤
                if let Some(start) = filter.start_time
                    && t.execution_time < start
                {
                    return false;
                }
                if let Some(end) = filter.end_time
                    && t.execution_time > end
                {
                    return false;
                }

                // 按最小名义价值过滤
                if let Some(min_notional) = filter.min_notional
                    && t.notional_value < min_notional
                {
                    return false;
                }

                true
            })
            .collect()
    }

    /// 获取交易统计
    pub fn get_trade_stats(&self, start: DateTime<Utc>, end: DateTime<Utc>) -> TradeStats {
        let trades: Vec<&TradeRecord> = self
            .trade_records
            .iter()
            .filter(|t| t.execution_time >= start && t.execution_time <= end)
            .collect();

        let total_trades = trades.len() as u32;
        let total_volume: f64 = trades.iter().map(|t| t.notional_value).sum();
        let total_fees: f64 = trades.iter().map(|t| t.fee).sum();

        let winning_trades = trades
            .iter()
            .filter(|t| t.realized_pnl.unwrap_or(0.0) > 0.0)
            .count() as u32;

        let losing_trades = trades
            .iter()
            .filter(|t| t.realized_pnl.unwrap_or(0.0) < 0.0)
            .count() as u32;

        let win_rate = if total_trades > 0 {
            winning_trades as f64 / total_trades as f64
        } else {
            0.0
        };

        let avg_trade_size = if total_trades > 0 {
            total_volume / total_trades as f64
        } else {
            0.0
        };

        TradeStats {
            total_trades,
            total_volume,
            total_fees,
            winning_trades,
            losing_trades,
            win_rate,
            avg_trade_size,
        }
    }

    /// 获取配置
    pub fn config(&self) -> &ComplianceConfig {
        &self.config
    }

    /// 获取交易记录数量
    pub fn trade_count(&self) -> usize {
        self.trade_records.len()
    }

    /// 获取审计日志
    pub fn audit_log(&self) -> &AuditLog {
        &self.audit_log
    }

    /// 验证交易数据
    fn validate_trade(&self, trade: &TradeRecord) -> ComplianceResult<()> {
        if trade.quantity <= 0.0 {
            return Err(ComplianceError::InvalidTradeData(
                "Quantity must be positive".into(),
            ));
        }
        if trade.price <= 0.0 {
            return Err(ComplianceError::InvalidTradeData(
                "Price must be positive".into(),
            ));
        }

        // 验证名义价值（允许小的浮点误差）
        let expected_notional = trade.quantity * trade.price;
        if (trade.notional_value - expected_notional).abs() > 0.01 {
            return Err(ComplianceError::InvalidTradeData(
                "Notional value mismatch".into(),
            ));
        }

        Ok(())
    }

    /// 检查监管阈值
    fn check_regulatory_thresholds(&self, trade: &TradeRecord) -> ComplianceResult<()> {
        // 大额交易检查
        if trade.notional_value > self.config.large_trade_threshold {
            // 记录告警但不阻止交易
            tracing::warn!(
                "Large trade detected: {} > {}",
                trade.notional_value,
                self.config.large_trade_threshold
            );
        }

        Ok(())
    }

    /// 生成日报
    pub fn generate_daily_report(&self, date: NaiveDate, starting_balance: f64) -> DailyReport {
        // 过滤指定日期的交易
        let trades: Vec<&TradeRecord> = self
            .trade_records
            .iter()
            .filter(|t| t.execution_time.date_naive() == date)
            .collect();

        report::daily::DailyReportGenerator::generate(
            date,
            &self.config.account_id,
            starting_balance,
            &trades,
            &self.config.base_currency,
        )
    }

    /// 生成月报
    pub fn generate_monthly_report(
        &self,
        year: u32,
        month: u32,
    ) -> ComplianceResult<MonthlyReport> {
        // 过滤指定月份的交易
        let trades: Vec<&TradeRecord> = self
            .trade_records
            .iter()
            .filter(|t| {
                let d = t.execution_time.date_naive();
                d.year() as u32 == year && d.month() == month
            })
            .collect();

        // 统计活跃天数
        let active_days: u32 = trades
            .iter()
            .map(|t| t.execution_time.date_naive())
            .collect::<std::collections::HashSet<_>>()
            .len() as u32;

        report::monthly::MonthlyReportGenerator::generate(
            year,
            month,
            &self.config.account_id,
            &trades,
            active_days,
        )
    }

    /// 生成年报
    pub fn generate_annual_report(&self, year: u32, initial_balance: f64) -> AnnualReport {
        // 过滤指定年份的交易
        let trades: Vec<&TradeRecord> = self
            .trade_records
            .iter()
            .filter(|t| t.execution_time.date_naive().year() as u32 == year)
            .collect();

        // 统计活跃月数
        let active_months: u32 = trades
            .iter()
            .map(|t| t.execution_time.date_naive().month())
            .collect::<std::collections::HashSet<_>>()
            .len() as u32;

        report::annual::AnnualReportGenerator::generate(
            year,
            &self.config.account_id,
            initial_balance,
            &trades,
            active_months,
        )
    }

    /// 导出报告为指定格式
    pub fn export_report<T: serde::Serialize>(
        &self,
        report: &T,
        format: ReportFormat,
    ) -> ComplianceResult<Vec<u8>> {
        ReportExporter::export(report, format)
    }

    /// 计算监管指标
    pub fn calculate_regulatory_metrics(&self) -> regulator::RegulatoryData {
        let calculator =
            regulator::metrics::RegulatoryMetricsCalculator::new(&self.config, &self.trade_records);
        calculator.calculate_all()
    }

    /// 生成监管报送
    pub fn generate_submission(
        &self,
        regulator_name: &str,
        submission_type: regulator::SubmissionType,
        period_start: DateTime<Utc>,
        period_end: DateTime<Utc>,
        format: regulator::RegulatorFormat,
    ) -> ComplianceResult<regulator::RegulatorySubmission> {
        let generator =
            regulator::submission::SubmissionGenerator::new(&self.config, &self.trade_records);
        generator.generate(
            regulator_name,
            submission_type,
            period_start,
            period_end,
            format,
        )
    }

    /// 导出监管报送
    pub fn export_submission(
        submission: &regulator::RegulatorySubmission,
    ) -> ComplianceResult<Vec<u8>> {
        regulator::submission::SubmissionGenerator::export(submission)
    }

    /// 检查持仓限制
    pub fn check_position_limits(&self) -> Vec<regulator::PositionLimit> {
        let calculator =
            regulator::metrics::RegulatoryMetricsCalculator::new(&self.config, &self.trade_records);
        calculator.check_position_limits()
    }

    /// 检查集中度限制
    pub fn check_concentration_limits(&self) -> Vec<regulator::ConcentrationCheck> {
        let calculator =
            regulator::metrics::RegulatoryMetricsCalculator::new(&self.config, &self.trade_records);
        calculator.check_concentration_limits()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use uuid::Uuid;

    fn create_test_config() -> ComplianceConfig {
        ComplianceConfig {
            account_id: "test_account".into(),
            base_currency: "USDT".into(),
            large_trade_threshold: 100000.0,
            position_limit: 1000000.0,
            max_portfolio_concentration: 0.4,
            data_retention_years: 7,
            regulators: vec!["SEC".into()],
        }
    }

    fn create_test_trade() -> TradeRecord {
        TradeRecord {
            trade_id: Uuid::new_v4(),
            order_id: Uuid::new_v4(),
            strategy_id: "test_strategy".into(),
            symbol: "BTCUSDT".into(),
            side: TradeSide::Buy,
            quantity: 1.0,
            price: 50000.0,
            notional_value: 50000.0,
            fee: 50.0,
            fee_currency: "USDT".into(),
            exchange: "Binance".into(),
            execution_time: Utc::now(),
            settlement_time: None,
            status: TradeStatus::Filled,
            order_type: OrderType::Market,
            exchange_trade_id: None,
            liquidity: LiquidityType::Taker,
            realized_pnl: Some(100.0),
            funding_rate: None,
            slippage: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn test_compliance_module_creation() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config();
        let module = ComplianceModule::new(config, tmp.path()).unwrap();

        assert_eq!(module.trade_count(), 0);
        assert!(module.verify_audit_integrity());
    }

    #[test]
    fn test_record_trade() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config();
        let mut module = ComplianceModule::new(config, tmp.path()).unwrap();

        let trade = create_test_trade();
        module.record_trade(trade).unwrap();

        assert_eq!(module.trade_count(), 1);
        assert_eq!(module.audit_log().len(), 1);
        assert!(module.verify_audit_integrity());
    }

    #[test]
    fn test_validate_trade_negative_quantity() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config();
        let mut module = ComplianceModule::new(config, tmp.path()).unwrap();

        let trade = TradeRecord {
            quantity: -1.0,
            ..create_test_trade()
        };

        let result = module.record_trade(trade);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_trade_negative_price() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config();
        let mut module = ComplianceModule::new(config, tmp.path()).unwrap();

        let trade = TradeRecord {
            price: -1.0,
            notional_value: -1.0,
            ..create_test_trade()
        };

        let result = module.record_trade(trade);
        assert!(result.is_err());
    }

    #[test]
    fn test_query_trades_by_symbol() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config();
        let mut module = ComplianceModule::new(config, tmp.path()).unwrap();

        // 记录不同交易对的交易
        let trade1 = create_test_trade();
        let trade2 = TradeRecord {
            symbol: "ETHUSDT".into(),
            ..create_test_trade()
        };

        module.record_trade(trade1).unwrap();
        module.record_trade(trade2).unwrap();

        // 按交易对查询
        let filter = TradeFilter {
            symbol: Some("BTCUSDT".into()),
            ..Default::default()
        };
        let trades = module.query_trades(&filter);
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].symbol, "BTCUSDT");
    }

    #[test]
    fn test_trade_stats() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config();
        let mut module = ComplianceModule::new(config, tmp.path()).unwrap();

        let now = Utc::now();
        let trade1 = TradeRecord {
            execution_time: now,
            ..create_test_trade()
        };
        let trade2 = TradeRecord {
            execution_time: now,
            realized_pnl: Some(-50.0),
            ..create_test_trade()
        };

        module.record_trade(trade1).unwrap();
        module.record_trade(trade2).unwrap();

        let stats = module.get_trade_stats(
            now - chrono::Duration::hours(1),
            now + chrono::Duration::hours(1),
        );

        assert_eq!(stats.total_trades, 2);
        assert_eq!(stats.winning_trades, 1);
        assert_eq!(stats.losing_trades, 1);
    }
}
