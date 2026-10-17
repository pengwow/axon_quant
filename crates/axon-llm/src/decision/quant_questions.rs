//! SDM v2 状态与问题模板（quant_questions）。
//!
//! 与 QuantCell `backend/decision/quant_questions.py` 逐字段同构：
//! - 特征集：v1 全部 5 个 + 新增 3 个 = 8 个
//! - 问题：v1 全部 4 个（键名不变，cascade 层硬编码依赖）+ 新增 4 个
//!
//! 向后兼容：v1 问题/特征定义不变；v2 通过本模块的常量和构造函数暴露。

use std::collections::BTreeMap;

use serde_json::{Value, json};

/// v2 特征名冻结表（顺序与 Python 版 FEATURE_NAMES_V2 完全一致）
///
/// 前 5 个为 v1 既有（cascade / stub / RuleFallback 已硬编码读取）；
/// 后 3 个为 v2 新增。改名或增减特征必须同步 QuantCell Python 层。
pub const FEATURE_NAMES_V2: [&str; 8] = [
    // —— v1 既有 —— //
    "ret_1",
    "ret_5",
    "vol_20",
    "sma5_vs_sma20",
    "volume_z",
    // —— v2 新增 —— //
    "close_vs_sma20",
    "rsi_14",
    "volume_ratio",
];

/// 构造 v2 完整问题模板（JSON Value，直接嵌入 DecisionRequest.questions）
///
/// 共 8 个问题：v1 全部 4 个 + v2 新增 4 个。v1 的键名保持不变（cascade.rs
/// 硬编码读取 `"next_direction"` 做主决策 gate，`"trend_confirms"` /
/// `"risk_regime_bad"` / `"signal_clarity"` 做子信号归因）。
pub fn build_questions_v2(cost_threshold: f64) -> BTreeMap<String, Value> {
    let pct = format!("{:.2}%", cost_threshold * 100.0);

    let raw = json!({
        // ── Group 1: 主决策（影响 cascade gate）── //
        "next_direction": {
            "type": "choice",
            "instructions": {
                "question": "基于 state.window 最近 20 根 bar 与 state.features，预测下一根 bar 的收盘方向。",
                "focus": "仅依据给定数据判断，不要假设未提供的信息。"
            },
            "criteria": {
                "buy":  {"what": format!("预计下一根 bar 涨幅超过 {pct}（扣除双边交易成本后仍有正收益）")},
                "sell": {"what": format!("预计下一根 bar 跌幅超过 {pct}")},
                "hold": {"what": "预计涨跌幅不超过上述阈值、方向不明或处于震荡"}
            }
        },

        // ── Group 2: 信号确认（纯归因，不参与 gate）── //
        "trend_confirms": {
            "type": "noul",
            "instructions": "state.features.sma5_vs_sma20 与 ret_1/ret_5 是否方向一致、互相印证？"
        },
        "risk_regime_bad": {
            "type": "noul",
            "instructions": "当前是否处于高波动（vol_20 显著偏高）或成交量异常放大（volume_z 绝对值大）等不利开仓状态？"
        },
        "signal_clarity": {
            "type": "score",
            "instructions": "当前交易信号的清晰度如何？",
            "criteria": [
                "信号混乱，多指标互相矛盾",
                "信号一般，方向不够明确",
                "信号清晰，指标方向一致"
            ]
        },

        // ── Group 3: v2 新增 ── //
        // 3a: 仓位分档（Choice，不做 gate，留给 harness position guard + risk engine）
        "position_size": {
            "type": "choice",
            "instructions": {
                "question": "建议的开仓分档比例？考虑当前波动率、信号清晰度和方向置信度。",
                "focus": "0% = 观望不进；25%/50% = 保守/常规；100% = 充分确信全仓（高风险）。"
            },
            "criteria": {
                "0pct":   {"what": "不建议开仓，风险或不确定性过高"},
                "25pct":  {"what": "小仓位试探性开仓，信号一般"},
                "50pct":  {"what": "常规仓位开仓，信号较好"},
                "100pct": {"what": "全仓开仓，信号强烈且方向高度确定"}
            }
        },

        // 3b: 波动率 regime（Score，供 harness FastPathRouter）
        "volatility_regime": {
            "type": "score",
            "instructions": "当前波动率处于哪个 regime？参考 vol_20 的绝对水平与历史分布。",
            "criteria": [
                "低波动（vol_20 < 0.5%），安全区间",
                "正常波动（0.5% - 2%）",
                "高波动（vol_20 > 2%），谨慎开仓或缩量"
            ]
        },

        // 3c: 回撤风险（Noul，供 harness CircuitBreaker 预检查）
        "drawdown_risk": {
            "type": "noul",
            "instructions": "当前是否面临显著回撤风险？参考：RSI 是否超买/超卖、close_vs_sma20 偏离幅度、近期是否有连续同向 bar。"
        },

        // 3d: 执行紧急度（Choice，供 harness FastPathRouter 路由到不同执行策略）
        "execution_urgency": {
            "type": "choice",
            "instructions": {
                "question": "执行这个决策的时间敏感度如何？",
                "focus": "immediate = 价格稍纵即逝，必须立即执行；within_60s = 信号持续有效窗口 > 60s；cancel = 不值得执行。"
            },
            "criteria": {
                "immediate":  {"what": "价格瞬时机会，必须立即 market 单"},
                "within_60s": {"what": "机会窗口有几秒到几十秒，可用限价或分批"},
                "cancel":     {"what": "时机已过或不值得执行"}
            }
        }
    });

    // 直接把 JSON Object 转成 BTreeMap<String, Value>
    raw.as_object()
        .expect("questions template must be a JSON object")
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_feature_names_match_python() {
        // 8 个特征名，顺序冻结
        assert_eq!(FEATURE_NAMES_V2.len(), 8);
        // v1 前 5 个不变
        assert_eq!(FEATURE_NAMES_V2[0], "ret_1");
        assert_eq!(FEATURE_NAMES_V2[1], "ret_5");
        assert_eq!(FEATURE_NAMES_V2[2], "vol_20");
        assert_eq!(FEATURE_NAMES_V2[3], "sma5_vs_sma20");
        assert_eq!(FEATURE_NAMES_V2[4], "volume_z");
        // v2 新增 3 个
        assert_eq!(FEATURE_NAMES_V2[5], "close_vs_sma20");
        assert_eq!(FEATURE_NAMES_V2[6], "rsi_14");
        assert_eq!(FEATURE_NAMES_V2[7], "volume_ratio");
    }

    #[test]
    fn v2_questions_have_8_keys() {
        let q = build_questions_v2(0.002);
        assert_eq!(q.len(), 8);
        // v1 全部 4 个键存在
        for k in [
            "next_direction",
            "trend_confirms",
            "risk_regime_bad",
            "signal_clarity",
        ] {
            assert!(q.contains_key(k), "missing v1 key: {k}");
        }
        // v2 新增 4 个键存在
        for k in [
            "position_size",
            "volatility_regime",
            "drawdown_risk",
            "execution_urgency",
        ] {
            assert!(q.contains_key(k), "missing v2 key: {k}");
        }
    }

    #[test]
    fn v2_questions_have_correct_types() {
        let q = build_questions_v2(0.002);
        let t = |key: &str| -> &str { q[key]["type"].as_str().unwrap_or("") };
        assert_eq!(t("next_direction"), "choice");
        assert_eq!(t("trend_confirms"), "noul");
        assert_eq!(t("risk_regime_bad"), "noul");
        assert_eq!(t("signal_clarity"), "score");
        assert_eq!(t("position_size"), "choice");
        assert_eq!(t("volatility_regime"), "score");
        assert_eq!(t("drawdown_risk"), "noul");
        assert_eq!(t("execution_urgency"), "choice");
    }
}
