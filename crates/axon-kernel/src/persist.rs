//! 重演凭证（RunManifest）
//!
//! 仅靠事件流不足以复现一次运行：配置、代码版本、数据版本、随机种子、模型产物
//! 任一变化都会改变结果。`RunManifest` 是 journal 的第一行，**任一字段不同，
//! 重演都不保证一致**，因此重演时逐字段比对，不一致直接拒绝（不做"尽力恢复"）。

use serde::{Deserialize, Serialize};

/// 当前 journal 落盘格式版本
///
/// 版本号描述的是**文件格式本身**（首行 manifest + 每行
/// `{"event": .., "state": ..}` 包裹），不是内核业务版本：
/// - v1：现行格式，事件行必须带 `state` 键（缺失视为更旧格式，读回时拒绝）；
/// - v0：v1 之前的历史文件，没有版本字段，只存在于 upgrade 前的归档里。
///
/// 新增格式时才递增；读回时高于本常量的 journal 一律拒绝重演。
pub const JOURNAL_FORMAT_VERSION: u32 = 1;

/// 模型产物指纹
///
/// 一个 `model_id` 对应一次训练产出的权重 + 预处理器。`normalizer_hash` 必须
/// 覆盖归一化参数（均值 / 方差 / 特征顺序），否则同权重不同归一化会静默漂移。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelArtifact {
    /// 模型标识（策略 / 训练任务内唯一）
    pub model_id: String,
    /// 模型规格（输入特征、网络结构、动作空间）的版本号
    pub spec_version: String,
    /// 归一化参数指纹
    pub normalizer_hash: String,
}

/// adopt 接管时抄进影子账本的初始持仓单元
///
/// live `start` 拉取交易所残留持仓后写入 [`RunManifest::initial_positions`]；
/// `resume` 重演时按它把同一份持仓种回影子 OMS，保证
/// 「影子持仓 = 初始持仓 + 成交效果」与交易所恒等。
/// 数量与均价用 Decimal 字符串落盘，避免 f64 往返丢精度。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestPosition {
    /// instrument_key（`BASE/QUOTE`，与 live 侧 `fetch_venue_snapshot` 的口径一致）
    pub symbol: String,
    /// 带符号净额（正 = 多，负 = 空），Decimal 字符串
    pub quantity: String,
    /// 交易所 avg_entry_price，Decimal 字符串，仅供 OMS 盈亏展示
    pub avg_price: String,
}

/// 一次运行的复现指纹
///
/// 作为 journal 首行落盘。**任一字段不同，重演结果不保证一致**：
/// - `code_version`：内核 / 策略代码版本（`Cargo.toml` 版本号）不同 → 逻辑可能已变；
/// - `config_hash`：配置指纹（初始资金、撮合参数、资金费表等）不同 → 账务口径不同；
/// - `data_hash`：输入 bar 集指纹不同 → 外部输入本身就不同；
/// - `seed`：撮合冲击 / 部分成交裁决的随机种子不同 → 裁决路径不同；
/// - `model_artifacts`：模型权重或归一化不同 → 决策不同。
///
/// `journal_format` 只用于读回时判断格式能否解析，不参与「重演结果是否一致」的
/// 语义比对（它描述文件格式，不描述运行输入）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunManifest {
    /// journal 落盘格式版本，见 [`JOURNAL_FORMAT_VERSION`]
    ///
    /// `serde(default)` 让升级前的归档（首行没有该字段）读成 0（v0 历史格式）；
    /// 0 低于当前版本，读回时按「格式够旧但仍是已知格式」放行。
    #[serde(default)]
    pub journal_format: u32,
    /// 代码版本（crate 版本，M0/M1 取 `CARGO_PKG_VERSION`）
    pub code_version: String,
    /// 配置指纹（序列化后哈希）
    pub config_hash: String,
    /// 数据指纹（输入 bar 集哈希）
    pub data_hash: String,
    /// 随机种子（冲击模型 / 部分成交裁决等）
    pub seed: u64,
    /// 本次运行用到的模型产物（无模型时为空 vec）
    pub model_artifacts: Vec<ModelArtifact>,
    /// start 时实际生效的影子账本基线现金（USDT，十进制字符串保精度）
    ///
    /// `serde(default)` 兼容旧 journal：缺字段读成 None，恢复方回退用自己的
    /// 兜底基线（行为不变）。live start 从交易所拉真实钱包余额作影子基线时
    /// 写入此字段，resume 按它重建重演起点，保证「重演基线 = 首跑基线」——
    /// 否则基线只存在于首跑进程内存里，重启后被兜底值顶替，对账必然假阳性。
    #[serde(default)]
    pub shadow_cash: Option<String>,
    /// adopt 接管时的初始持仓（fresh start 抄自交易所，resume 据此播种影子账本）
    ///
    /// `serde(default)` 兼容旧 journal：缺字段读成 None（行为不变：不播种）。
    /// 只有 `ExistingPositions::Adopt` 且交易所确实有持仓时才写入；
    /// 空 vec 与 None 等价，都表示「无接管持仓」。
    #[serde(default)]
    pub initial_positions: Option<Vec<ManifestPosition>>,
}

impl Default for RunManifest {
    fn default() -> Self {
        Self {
            journal_format: JOURNAL_FORMAT_VERSION,
            // 默认填编译期版本，避免调用方漏填导致 journal 头是空字符串
            code_version: env!("CARGO_PKG_VERSION").to_string(),
            config_hash: String::new(),
            data_hash: String::new(),
            seed: 0,
            model_artifacts: Vec::new(),
            shadow_cash: None,
            initial_positions: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// initial_positions 往返：序列化 → 读回逐字段相等
    #[test]
    fn manifest_positions_roundtrip() {
        let manifest = RunManifest {
            initial_positions: Some(vec![ManifestPosition {
                symbol: "BTC/USDT".into(),
                quantity: "-0.5".into(),
                avg_price: "100.25".into(),
            }]),
            ..RunManifest::default()
        };
        let raw = serde_json::to_string(&manifest).expect("序列化");
        let roundtrip: RunManifest = serde_json::from_str(&raw).expect("反序列化");
        assert_eq!(roundtrip, manifest);
    }

    /// 旧 journal 缺 initial_positions 字段读回 None（行为兼容）
    #[test]
    fn legacy_manifest_without_initial_positions() {
        let legacy = r#"{
            "journal_format": 1,
            "code_version": "0.0.0",
            "config_hash": "",
            "data_hash": "",
            "seed": 0,
            "model_artifacts": []
        }"#;
        let manifest: RunManifest = serde_json::from_str(legacy).expect("旧格式应可读回");
        assert_eq!(manifest.initial_positions, None);
        assert_eq!(manifest.shadow_cash, None);
    }
}
