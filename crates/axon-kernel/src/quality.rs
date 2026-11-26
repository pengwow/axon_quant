//! 数据质量报告（0.26.0 C6）
//!
//! 脏数据的处理是**分层**的：历史路径（`build_frames`）严格拒绝、fail-fast；
//! 实时路径（`axon_worker::KernelPump`）丢弃脏 bar 并旁路告警，主回路照常跑。
//! 两层共用的观测出口是本模块的 [`QualityReport`]：
//!
//! - `ohlc_rejected`：OHLC 非法被拒收的 bar 数（实时路径计数；历史路径直接
//!   返回错误，不产生报告）；
//! - `invalid_qty_dropped`：负成交量 tick 被夹紧丢弃的数量（聚合器既有
//!   `.max(0.0)` 行为的计数化，不新增拦截）；
//! - `duplicates_missed`：越过去重水位线、无法判重而放行的迟到 bar 数（C5）。
//!   口径 = **按放行的消息数**计数（同一 bar 的 `unclosed` 后跟 `closed`、
//!   tick 后跟 `closed` 各计一次），不做按 bar 去重。
//!
//! 报告**不进** `RunManifest`：manifest 是重演等值语义的指纹，统计字段随运行
//! 变化会让「同配置两次运行 manifest 相同」不再成立。它在 run 结束时随
//! `KernelResult` 上浮，并写成 journal 同目录的 `<stem>.quality.json` 侧车
//! （与 `*.reconcile_last_ok.json` 同目录惯例）。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::KernelError;

/// 一次运行的数据质量计数（全 0 = 输入干净）
///
/// 三个字段都是**顺序无关的求和式计数**，因此重跑同一份输入得到同一份报告，
/// 可以安全地用在双跑等值断言里。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityReport {
    /// OHLC 非法被拒收的 bar 数
    #[serde(default)]
    pub ohlc_rejected: u64,
    /// 负成交量 tick 被夹紧丢弃的数量（聚合器既有行为，不新增拦截）
    #[serde(default)]
    pub invalid_qty_dropped: u64,
    /// 越过去重水位线、无法判重而放行的迟到 bar 数
    ///
    /// 口径 = **按放行消息数**：一条迟到 bar 被重复推送 N 次就计 N 次
    /// （`unclosed` 之后跟一个 `closed`、tick 聚合之后又收到 `closed` 这类
    /// "同 bar 两种消息"也各算一次），本字段不做按 bar 去重，只反映"有多少条
    /// 消息没被判重"。
    #[serde(default)]
    pub duplicates_missed: u64,
}

impl QualityReport {
    /// 三项计数是否都为 0（输入干净）
    pub fn is_clean(&self) -> bool {
        self.ohlc_rejected == 0 && self.invalid_qty_dropped == 0 && self.duplicates_missed == 0
    }

    /// 把报告写成侧车 JSON，返回实际写出的路径
    ///
    /// 侧车命名 = 把 `run_path` 的扩展名替换成 `quality.json`，与
    /// `*.reconcile_last_ok.json` 同目录惯例一致：
    /// `runs/abc.journal.jsonl` → `runs/abc.journal.quality.json`。
    /// 序列化 / 落盘失败都返回错误，由调用方决定是硬失败还是只告警。
    pub fn write_sidecar(&self, run_path: &Path) -> Result<PathBuf, KernelError> {
        let sidecar = run_path.with_extension("quality.json");
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| KernelError::Journal(format!("quality 报告序列化失败: {e}")))?;
        std::fs::write(&sidecar, format!("{json}\n")).map_err(|e| {
            KernelError::Journal(format!("写 quality 报告失败 {}: {e}", sidecar.display()))
        })?;
        Ok(sidecar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_report_is_clean() {
        assert!(QualityReport::default().is_clean());
    }

    #[test]
    fn sidecar_is_written_beside_run_path() {
        let dir = tempfile::tempdir().expect("临时目录");
        let run_path = dir.path().join("run.journal.jsonl");
        let report = QualityReport {
            ohlc_rejected: 2,
            invalid_qty_dropped: 3,
            duplicates_missed: 4,
        };

        let written = report.write_sidecar(&run_path).expect("侧车应写出");
        assert_eq!(written, dir.path().join("run.journal.quality.json"));

        let text = std::fs::read_to_string(&written).expect("侧车应可读");
        let parsed: QualityReport = serde_json::from_str(&text).expect("侧车应是合法 JSON");
        assert_eq!(parsed, report);
    }
}
