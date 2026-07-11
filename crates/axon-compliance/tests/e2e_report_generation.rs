//! 端到端测试:axon-compliance 合规报告生成
//!
//! ## 4 个测试场景
//!
//! 1. `compliance_record_and_daily_report`:记录交易 → 生成日报 → 验证字段
//! 2. `compliance_daily_report_json_export`:日报 → JSON 导出 → 验证格式
//! 3. `compliance_monthly_report_generation`:记录多日交易 → 月报生成
//! 4. `compliance_audit_integrity`:记录交易 → 审计完整性验证
//!
//! 运行:`cargo test -p axon-compliance --test e2e_report_generation`

use axon_compliance::{
    ComplianceConfig, ComplianceModule, LiquidityType, OrderType, ReportFormat, TradeRecord,
    TradeSide, TradeStatus,
};
use chrono::{Datelike, Utc};

// ── helpers ────────────────────────────────────────────────────────────

fn test_config() -> ComplianceConfig {
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

fn test_trade(symbol: &str, price: f64, qty: f64, side: TradeSide) -> TradeRecord {
    TradeRecord {
        trade_id: uuid::Uuid::new_v4(),
        order_id: uuid::Uuid::new_v4(),
        strategy_id: "test_strategy".into(),
        symbol: symbol.into(),
        side,
        quantity: qty,
        price,
        notional_value: price * qty,
        fee: price * qty * 0.001,
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

// ── 1. 记录交易 → 生成日报 → 验证字段 ─────────────────────────────────

#[test]
fn compliance_record_and_daily_report() {
    let dir = tempfile::tempdir().unwrap();
    let mut compliance = ComplianceModule::new(test_config(), dir.path()).unwrap();

    // 记录 3 笔交易
    compliance
        .record_trade(test_trade("BTC-USDT", 50000.0, 0.1, TradeSide::Buy))
        .unwrap();
    compliance
        .record_trade(test_trade("BTC-USDT", 50100.0, 0.1, TradeSide::Sell))
        .unwrap();
    compliance
        .record_trade(test_trade("ETH-USDT", 3000.0, 1.0, TradeSide::Buy))
        .unwrap();

    // 生成日报
    let today = Utc::now().date_naive();
    let report = compliance.generate_daily_report(today, 100000.0);

    assert_eq!(report.account_id, "test_account");
    assert!(report.total_trades > 0);
    assert!(report.starting_balance > 0.0);
}

// ── 2. 日报 → JSON 导出 → 验证格式 ────────────────────────────────────

#[test]
fn compliance_daily_report_json_export() {
    let dir = tempfile::tempdir().unwrap();
    let mut compliance = ComplianceModule::new(test_config(), dir.path()).unwrap();

    compliance
        .record_trade(test_trade("BTC-USDT", 50000.0, 0.1, TradeSide::Buy))
        .unwrap();

    let today = Utc::now().date_naive();
    let report = compliance.generate_daily_report(today, 100000.0);

    // 导出为 JSON
    let json = compliance
        .export_report(&report, ReportFormat::JSON)
        .unwrap();
    let json_str = String::from_utf8(json).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

    assert!(parsed.is_object());
    assert!(parsed.get("account_id").is_some());
    assert!(parsed.get("total_trades").is_some());
}

// ── 3. 多日交易 → 月报生成 ────────────────────────────────────────────

#[test]
fn compliance_monthly_report_generation() {
    let dir = tempfile::tempdir().unwrap();
    let mut compliance = ComplianceModule::new(test_config(), dir.path()).unwrap();

    // 记录交易
    compliance
        .record_trade(test_trade("BTC-USDT", 50000.0, 0.1, TradeSide::Buy))
        .unwrap();
    compliance
        .record_trade(test_trade("ETH-USDT", 3000.0, 1.0, TradeSide::Sell))
        .unwrap();

    let now = Utc::now();
    let report = compliance
        .generate_monthly_report(now.year() as u32, now.month())
        .unwrap();

    assert_eq!(report.account_id, "test_account");
    assert!(report.total_trades > 0);
}

// ── 4. 审计完整性验证 ─────────────────────────────────────────────────

#[test]
fn compliance_audit_integrity() {
    let dir = tempfile::tempdir().unwrap();
    let mut compliance = ComplianceModule::new(test_config(), dir.path()).unwrap();

    // 记录交易后审计完整性应为 true
    compliance
        .record_trade(test_trade("BTC-USDT", 50000.0, 0.1, TradeSide::Buy))
        .unwrap();
    assert!(compliance.verify_audit_integrity());

    // 再记录一笔，完整性仍应保持
    compliance
        .record_trade(test_trade("ETH-USDT", 3000.0, 1.0, TradeSide::Sell))
        .unwrap();
    assert!(compliance.verify_audit_integrity());
}
