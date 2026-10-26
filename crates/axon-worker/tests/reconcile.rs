//! 对账报告与恢复守门的契约测试（设计文档 4.10）
//!
//! 这些测试只验证「报告什么 / 不自动改账」的语义，不接真实交易所。

use std::collections::{BTreeMap, BTreeSet};

use axon_worker::reconcile::{
    LocalSnapshot, ReconcileConfig, ReconcileReport, VenueSnapshot, reconcile, resume_guard,
};

fn positions(entries: &[(&str, f64)]) -> BTreeMap<String, f64> {
    entries
        .iter()
        .map(|(k, v)| ((*k).to_string(), *v))
        .collect()
}

fn fills(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|id| (*id).to_string()).collect()
}

fn snapshot(entries: &[(&str, f64)], cash: f64, fill_ids: &[&str]) -> LocalSnapshot {
    LocalSnapshot {
        positions: positions(entries),
        cash,
        fill_ids: fills(fill_ids),
        open_order_ids: BTreeSet::new(),
    }
}

fn venue_snapshot(entries: &[(&str, f64)], cash: f64, fill_ids: &[&str]) -> VenueSnapshot {
    VenueSnapshot {
        positions: positions(entries),
        cash,
        fill_ids: fills(fill_ids),
        open_order_ids: BTreeSet::new(),
    }
}

/// 1) 完全一致 → 干净、无冻结、允许续跑
#[test]
fn test_identical_snapshots_are_clean() {
    let local = snapshot(&[("BTC/USDT", 1.5)], 10_000.0, &["t1", "t2"]);
    let venue = venue_snapshot(&[("BTC/USDT", 1.5)], 10_000.0, &["t1", "t2"]);

    let report = reconcile(&local, &venue, &ReconcileConfig::default());

    assert!(report.is_clean(), "完全一致必须判干净: {report:?}");
    assert!(report.position_breaks.is_empty());
    assert!(report.frozen_instruments.is_empty());
    assert!(report.missing_fills.is_empty());
    assert!(report.extra_fills.is_empty());
    assert!(report.cash_break.is_none());
    assert!(resume_guard(&report).is_ok());
}

/// 2) 持仓差异超阈值 → 该品种冻结，续跑守卫返回 Err
///
/// 同时覆盖「交易所有、本地无」的品种：双向差异都必须报出来。
#[test]
fn test_position_break_freezes_and_blocks_resume() {
    let local = snapshot(&[("BTC/USDT", 1.5)], 10_000.0, &[]);
    let venue = venue_snapshot(&[("BTC/USDT", 1.6), ("ETH/USDT", 0.5)], 10_000.0, &[]);

    let report = reconcile(&local, &venue, &ReconcileConfig::default());

    assert_eq!(report.position_breaks.len(), 2, "双方差异都要报");
    let btc = &report.position_breaks[0];
    assert_eq!(btc.instrument, "BTC/USDT");
    assert_eq!(btc.local, 1.5);
    assert_eq!(btc.venue, 1.6);
    let eth = &report.position_breaks[1];
    assert_eq!(eth.instrument, "ETH/USDT");
    assert_eq!(eth.local, 0.0, "本地缺失按 0 处理");
    assert_eq!(eth.venue, 0.5);
    assert_eq!(
        report.frozen_instruments,
        vec!["BTC/USDT".to_string(), "ETH/USDT".to_string()]
    );
    assert!(!report.is_clean());

    let err = resume_guard(&report).expect_err("持仓 break 必须阻断续跑");
    assert!(err.contains("持仓差异"), "错误信息应说明持仓差异: {err}");
}

/// 3) 交易所有、本地漏记的成交 → 只进 `missing_fills`，持仓 / 现金一致时仍允许续跑
#[test]
fn test_missing_fills_reported_without_blocking_resume() {
    let local = snapshot(&[("BTC/USDT", 1.0)], 5_000.0, &["t1"]);
    let venue = venue_snapshot(&[("BTC/USDT", 1.0)], 5_000.0, &["t1", "t2"]);

    let report = reconcile(&local, &venue, &ReconcileConfig::default());

    assert_eq!(report.missing_fills, vec!["t2".to_string()]);
    assert!(report.extra_fills.is_empty());
    assert!(report.position_breaks.is_empty());
    assert!(!report.is_clean(), "成交差异不算干净");
    assert!(
        resume_guard(&report).is_ok(),
        "只有成交差异时不阻断续跑（由调用方告警人工核对）"
    );
}

/// 4) 现金差异超阈值 → `cash_break` 有值，不做自动归因抵扣，续跑守卫 Err
#[test]
fn test_cash_break_blocks_resume() {
    let local = snapshot(&[], 10_000.0, &[]);
    let venue = venue_snapshot(&[], 9_990.0, &[]);

    let report = reconcile(&local, &venue, &ReconcileConfig::default());

    let cash = report.cash_break.as_ref().expect("现金差异必须 break");
    assert_eq!(cash.local, 10_000.0);
    assert_eq!(cash.venue, 9_990.0);
    assert!((cash.diff + 10.0).abs() < 1e-12, "diff = venue - local");

    let err = resume_guard(&report).expect_err("现金 break 必须阻断续跑");
    assert!(err.contains("现金差异"), "错误信息应说明现金差异: {err}");
}

/// 5) 交易所在挂、本地无记录 → 幽灵单进报告，续跑守卫 Err
#[test]
fn test_ghost_order_blocks_resume() {
    let mut local = snapshot(&[], 10_000.0, &[]);
    let mut venue = venue_snapshot(&[], 10_000.0, &[]);
    venue.open_order_ids = fills(&["ghost-1", "ghost-2"]);
    // 只有 ghost-2 是幽灵单：ghost-1 两侧都有（本地也有该单）
    local.open_order_ids.insert("ghost-1".to_string());

    let report = reconcile(&local, &venue, &ReconcileConfig::default());

    assert_eq!(report.ghost_orders, vec!["ghost-2".to_string()]);
    assert!(report.orphan_local_orders.is_empty());
    assert!(!report.is_clean(), "幽灵单不算干净（恢复窗口内可能成交）");
    let err = resume_guard(&report).expect_err("幽灵单必须阻断续跑");
    assert!(err.contains("幽灵单"), "错误信息应说明幽灵单: {err}");
}

/// 6) 本地在挂、交易所没有 → 孤儿单只进报告，不阻断续跑
#[test]
fn test_orphan_local_order_reported_without_blocking_resume() {
    let mut local = snapshot(&[], 10_000.0, &[]);
    local.open_order_ids = fills(&["local-1"]);
    let venue = venue_snapshot(&[], 10_000.0, &[]);

    let report = reconcile(&local, &venue, &ReconcileConfig::default());

    assert_eq!(report.orphan_local_orders, vec!["local-1".to_string()]);
    assert!(report.ghost_orders.is_empty());
    assert!(!report.is_clean());
    assert!(
        resume_guard(&report).is_ok(),
        "孤儿单是本地状态陈旧，不会凭空产生交易，不阻断续跑"
    );
}

/// 7) 持仓差异在容差内 → 不算 break，也不冻结
#[test]
fn test_position_within_tolerance_is_clean() {
    let local = snapshot(&[("BTC/USDT", 1.0)], 1_000.0, &[]);
    let venue = venue_snapshot(&[("BTC/USDT", 1.0 + 1e-9)], 1_000.0, &[]);

    let cfg = ReconcileConfig {
        position_tolerance: 1e-8,
        cash_tolerance: 1e-6,
    };
    let report = reconcile(&local, &venue, &cfg);

    assert!(report.position_breaks.is_empty(), "容差内不算 break");
    assert!(report.frozen_instruments.is_empty());
    assert!(report.is_clean());
}

/// 8) 报告 JSON 落盘后可解析回读（语义不丢字段）
#[test]
fn test_report_json_roundtrip() {
    let local = snapshot(&[("BTC/USDT", 1.0)], 1_000.0, &["t1"]);
    let venue = venue_snapshot(&[("BTC/USDT", 1.2)], 999.0, &["t1", "t2"]);
    let report = reconcile(&local, &venue, &ReconcileConfig::default());

    let unique = format!(
        "axon-reconcile-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系统时间应晚于 epoch")
            .as_nanos()
    );
    let dir = std::env::temp_dir().join(unique);

    let path = report.write_json(&dir).expect("报告应能落盘");
    assert!(path.exists());
    let content = std::fs::read_to_string(&path).expect("落盘文件应可读");
    let parsed: ReconcileReport = serde_json::from_str(&content).expect("报告 JSON 应可解析回读");
    assert_eq!(parsed, report);
    assert_eq!(parsed.frozen_instruments, vec!["BTC/USDT".to_string()]);

    std::fs::remove_dir_all(&dir).expect("清理临时目录");
}
