//! M5 对账：内核 / OMS 侧快照 vs 交易所侧快照（设计文档 4.10 三路对账）
//!
//! [`reconcile`] 只做**比对与报告**，不修改任何一侧账务：
//!
//! - 持仓：双向差异都算 break。本地有、交易所有，或交易所有、本地无，都要报；
//! - 成交：`missing_fills`（交易所有、本地漏记）与 `extra_fills`（本地多记、
//!   交易所无）分开列，**不做自动补录**；
//! - 现金：差异在 [`ReconcileConfig::cash_tolerance`] 内视为干净；超出即 break，
//!   不做「按手续费 / 资金费归因后自动抵扣」——归因是人工动作。
//!
//! 差异超阈值的品种进 [`ReconcileReport::frozen_instruments`]，由调用方拿去阻止
//! 该品种开仓；本模块不持有任何交易状态，也不代替调用方做冻结。
//!
//! # 尚未接真实交易所
//!
//! 本模块没有 `fetch_positions` / `fetch_my_trades` / 余额查询的实现，两侧快照
//! 都由调用方填入。真实接线（`axon_exchange::ExchangeAdapter` → 快照）留到
//! testnet 阶段，与 [`crate::live_sink`] 的 gateway 桥接一起做。
//!
//! 崩溃恢复的用法：重放 journal 得到本地快照 → 拉交易所快照 → [`reconcile`] →
//! [`resume_guard`] 决定能否继续跑。本地侧快照可直接
//! [`LocalSnapshot::from_sink`] 从 [`crate::live_sink::LiveSink`] 的影子账本提取。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};

use crate::live_sink::LiveSink;

/// 内核 / OMS 侧快照
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LocalSnapshot {
    /// 品种持仓，key 是 `"BASE/QUOTE"`（与内核 `symbol_key` 同口径）
    pub positions: BTreeMap<String, f64>,
    /// 计价币现金（USDT）
    pub cash: f64,
    /// 已记账的成交编号（交易所 `trade_id`）
    pub fill_ids: BTreeSet<String>,
}

impl LocalSnapshot {
    /// 从 [`LiveSink`] 的 OMS 影子账本提取本地快照（崩溃恢复 / 冒烟测试共用）
    ///
    /// - 持仓：绝对值在 `f64::EPSILON` 内的近零仓不进快照，避免浮点残渣触发对账 break；
    /// - 现金：OMS `Portfolio` 把计价币硬编码为 USDT；
    /// - 成交：遍历全部订单历史取 `fill_id`，与交易所流水做差集。
    pub fn from_sink(sink: &LiveSink) -> Self {
        let balance = sink.oms().snapshot_balance();
        let positions = sink
            .oms()
            .snapshot_positions()
            .into_iter()
            .filter_map(|position| {
                let quantity = position.quantity.to_f64()?;
                (quantity.abs() > f64::EPSILON).then_some((position.symbol, quantity))
            })
            .collect();
        let fill_ids = sink
            .oms()
            .snapshot()
            .order_history
            .iter()
            .flat_map(|record| record.fills.iter().map(|fill| fill.fill_id.clone()))
            .collect();
        Self {
            positions,
            cash: balance
                .cash
                .get("USDT")
                .and_then(|v| v.to_f64())
                .unwrap_or(0.0),
            fill_ids,
        }
    }
}

/// 交易所侧快照
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VenueSnapshot {
    /// 品种持仓，key 是 `"BASE/QUOTE"`
    pub positions: BTreeMap<String, f64>,
    /// 交易所账户余额（计价币）
    pub cash: f64,
    /// 交易所成交流水里的成交编号
    pub fill_ids: BTreeSet<String>,
}

/// 对账阈值
#[derive(Debug, Clone, PartialEq)]
pub struct ReconcileConfig {
    /// 持仓数量绝对误差上限（默认 `1e-8`）
    pub position_tolerance: f64,
    /// 现金绝对误差上限（默认 `1e-6`）
    pub cash_tolerance: f64,
}

impl Default for ReconcileConfig {
    fn default() -> Self {
        Self {
            position_tolerance: 1e-8,
            cash_tolerance: 1e-6,
        }
    }
}

/// 一条持仓差异
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionBreak {
    /// 品种（`"BASE/QUOTE"`）
    pub instrument: String,
    /// 内核 / OMS 侧数量（缺失按 0）
    pub local: f64,
    /// 交易所侧数量（缺失按 0）
    pub venue: f64,
}

/// 一条现金差异
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CashBreak {
    /// 本地现金
    pub local: f64,
    /// 交易所现金
    pub venue: f64,
    /// `venue - local`（正数表示交易所比本地多）
    pub diff: f64,
}

/// 对账报告（落盘归档的唯一产物）
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReconcileReport {
    /// 超阈值的持仓差异（按品种名升序）
    pub position_breaks: Vec<PositionBreak>,
    /// 交易所有、本地没有的成交编号（漏记）
    pub missing_fills: Vec<String>,
    /// 本地有、交易所没有的成交编号（多记）
    pub extra_fills: Vec<String>,
    /// 现金差异；在容差内时为 `None`
    pub cash_break: Option<CashBreak>,
    /// 冻结开仓的品种（`position_breaks` 的品种集合，升序）
    pub frozen_instruments: Vec<String>,
}

impl ReconcileReport {
    /// 是否完全一致（无持仓 break、无成交差异、无现金 break）
    pub fn is_clean(&self) -> bool {
        self.position_breaks.is_empty()
            && self.missing_fills.is_empty()
            && self.extra_fills.is_empty()
            && self.cash_break.is_none()
    }

    /// 落盘为 `<dir>/reconcile-report.json`（`dir` 不存在时自动创建），返回文件路径
    pub fn write_json(&self, dir: &Path) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("reconcile-report.json");
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::other(format!("序列化对账报告失败: {e}")))?;
        std::fs::write(&path, json)?;
        Ok(path)
    }
}

/// 比对两侧快照，产出差异报告（只读，不改账）
///
/// 持仓按 `local` / `venue` 的并集逐品种比较，缺失一侧按 `0` 处理；成交按集合
/// 差集拆分；现金按 [`ReconcileConfig::cash_tolerance`] 判定。
///
/// 任何差异都不会被自动修复：调用方要么人工处理，要么按
/// [`ReconcileReport::frozen_instruments`] 阻止对应品种开仓。
pub fn reconcile(
    local: &LocalSnapshot,
    venue: &VenueSnapshot,
    cfg: &ReconcileConfig,
) -> ReconcileReport {
    let mut position_breaks = Vec::new();
    let mut frozen_instruments = Vec::new();

    // 并集：交易所有、本地无的品种也必须报出来（双向差异）
    let mut instruments: BTreeSet<&String> = local.positions.keys().collect();
    instruments.extend(venue.positions.keys());
    for instrument in instruments {
        let local_qty = local.positions.get(instrument).copied().unwrap_or(0.0);
        let venue_qty = venue.positions.get(instrument).copied().unwrap_or(0.0);
        if (venue_qty - local_qty).abs() > cfg.position_tolerance {
            position_breaks.push(PositionBreak {
                instrument: instrument.clone(),
                local: local_qty,
                venue: venue_qty,
            });
            frozen_instruments.push(instrument.clone());
        }
    }

    let missing_fills = venue
        .fill_ids
        .difference(&local.fill_ids)
        .cloned()
        .collect();
    let extra_fills = local
        .fill_ids
        .difference(&venue.fill_ids)
        .cloned()
        .collect();

    let cash_diff = venue.cash - local.cash;
    // 超出容差即 break：不按手续费 / 资金费自动归因抵扣。
    let cash_break = (cash_diff.abs() > cfg.cash_tolerance).then_some(CashBreak {
        local: local.cash,
        venue: venue.cash,
        diff: cash_diff,
    });

    ReconcileReport {
        position_breaks,
        missing_fills,
        extra_fills,
        cash_break,
        frozen_instruments,
    }
}

/// 崩溃恢复守门：重放 journal 后，用对账报告决定是否允许继续
///
/// - 有持仓 break 或现金 break → `Err`，账务不一致，必须人工介入；
/// - 只有成交差异（漏记 / 多记）→ `Ok`，但调用方必须告警并人工核对成交
///   （漏记可能是回报丢失，多记可能是重复记账的残余）。
pub fn resume_guard(report: &ReconcileReport) -> Result<(), String> {
    let mut reasons = Vec::new();
    if !report.position_breaks.is_empty() {
        reasons.push(format!("持仓差异 {} 个品种", report.position_breaks.len()));
    }
    if report.cash_break.is_some() {
        reasons.push("现金差异".to_string());
    }
    if reasons.is_empty() {
        return Ok(());
    }
    Err(format!(
        "对账不一致（{}），禁止自动续跑，必须人工介入",
        reasons.join("、")
    ))
}
