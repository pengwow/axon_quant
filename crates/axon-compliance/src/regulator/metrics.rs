//! 监管指标计算
//!
//! 计算持仓限制、集中度检查、大额交易报告等监管指标。

use std::collections::HashMap;

use crate::ComplianceConfig;
use crate::types::{TradeRecord, TradeSide};

use super::{ConcentrationCheck, LargeTradeReport, PositionLimit, RegulatoryData};

/// 监管指标计算器
pub struct RegulatoryMetricsCalculator<'a> {
    /// 合规配置
    config: &'a ComplianceConfig,
    /// 交易记录
    trades: &'a [TradeRecord],
}

impl<'a> RegulatoryMetricsCalculator<'a> {
    /// 创建新的计算器
    pub fn new(config: &'a ComplianceConfig, trades: &'a [TradeRecord]) -> Self {
        Self { config, trades }
    }

    /// 计算总成交额
    pub fn calculate_total_turnover(&self) -> f64 {
        self.trades.iter().map(|t| t.notional_value).sum()
    }

    /// 检查持仓限制
    pub fn check_position_limits(&self) -> Vec<PositionLimit> {
        // 按 symbol 聚合持仓
        let mut positions: HashMap<String, f64> = HashMap::new();
        for trade in self.trades {
            let entry = positions.entry(trade.symbol.clone()).or_insert(0.0);
            match trade.side {
                TradeSide::Buy => *entry += trade.quantity,
                TradeSide::Sell => *entry -= trade.quantity,
            }
        }

        // 检查限制
        positions
            .iter()
            .map(|(symbol, &position)| {
                let limit = self.config.position_limit;
                let utilization = (position.abs() / limit) * 100.0;
                PositionLimit {
                    symbol: symbol.clone(),
                    current_position: position,
                    limit,
                    utilization_pct: utilization,
                    breach: utilization > 100.0,
                }
            })
            .collect()
    }

    /// 检查集中度限制
    pub fn check_concentration_limits(&self) -> Vec<ConcentrationCheck> {
        let total_turnover = self.calculate_total_turnover();
        if total_turnover == 0.0 {
            return vec![];
        }

        // 按 symbol 计算集中度
        let mut symbol_turnover: HashMap<String, f64> = HashMap::new();
        for trade in self.trades {
            *symbol_turnover.entry(trade.symbol.clone()).or_insert(0.0) += trade.notional_value;
        }

        let limit = self.config.max_portfolio_concentration;
        symbol_turnover
            .iter()
            .map(|(symbol, &turnover)| {
                let concentration = (turnover / total_turnover) * 100.0;
                ConcentrationCheck {
                    category: symbol.clone(),
                    exposure: turnover,
                    limit,
                    utilization_pct: concentration,
                    breach: concentration > limit,
                }
            })
            .collect()
    }

    /// 检测大额交易
    pub fn detect_large_trades(&self) -> Vec<LargeTradeReport> {
        let threshold = self.config.large_trade_threshold;
        self.trades
            .iter()
            .filter(|t| t.notional_value > threshold)
            .map(|t| LargeTradeReport {
                trade_id: t.trade_id,
                symbol: t.symbol.clone(),
                notional_value: t.notional_value,
                threshold,
                requires_report: true,
            })
            .collect()
    }

    /// 计算所有监管指标
    pub fn calculate_all(&self) -> RegulatoryData {
        RegulatoryData {
            total_turnover: self.calculate_total_turnover(),
            position_limits: self.check_position_limits(),
            concentration_limits: self.check_concentration_limits(),
            large_trade_reports: self.detect_large_trades(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LiquidityType, OrderType, TradeStatus};
    use chrono::Utc;
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
    fn test_total_turnover_calculation() {
        let config = create_test_config();
        let trades = [
            create_test_trade("BTCUSDT", 1.0, 50000.0),
            create_test_trade("ETHUSDT", 10.0, 3000.0),
        ];
        let calculator = RegulatoryMetricsCalculator::new(&config, &trades);
        assert_eq!(calculator.calculate_total_turnover(), 80000.0);
    }

    #[test]
    fn test_position_limit_breach() {
        let config = create_test_config();
        let trades = [create_test_trade("BTCUSDT", 150.0, 50000.0)];
        let calculator = RegulatoryMetricsCalculator::new(&config, &trades);
        let limits = calculator.check_position_limits();
        assert!(limits[0].breach);
    }

    #[test]
    fn test_concentration_limit_breach() {
        let config = create_test_config();
        let trades = [
            create_test_trade("BTCUSDT", 1.0, 50000.0),
            create_test_trade("BTCUSDT", 1.0, 50000.0),
        ];
        let calculator = RegulatoryMetricsCalculator::new(&config, &trades);
        let checks = calculator.check_concentration_limits();
        assert!(checks[0].breach);
    }

    #[test]
    fn test_large_trade_detection() {
        let config = create_test_config();
        let trades = [create_test_trade("BTCUSDT", 1.0, 50000.0)];
        let calculator = RegulatoryMetricsCalculator::new(&config, &trades);
        let reports = calculator.detect_large_trades();
        assert_eq!(reports.len(), 1);
    }
}
