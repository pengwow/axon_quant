//! TDD 第六轮：ReportGenerator
//!
//! 关键测试用例（来自设计）：
//! - HTML 输出包含 DOCTYPE / 报告期间 / 特征表格 / 决策明细
//! - Markdown 输出包含标题 / 表格 / 决策列表
//! - HTML 包含正负向特征的颜色分类

use axon_explain::report::ReportGenerator;
use axon_explain::types::{
    ActionAttribution, ActionSnapshot, ContributionDirection, DecisionReport, Explanation,
    FeatureContribution, FeatureSummary, RiskAttributionMetrics,
};
use chrono::Utc;
use std::collections::HashMap;

// ─── 辅助：构造测试报告 ──────────────────────────────────

fn make_explanation(summary: &str, confidence: f64) -> Explanation {
    let mut feat_importance = HashMap::new();
    feat_importance.insert("rsi_14".to_string(), 0.15);
    feat_importance.insert("volume".to_string(), 0.08);

    let contribs = vec![
        FeatureContribution {
            feature_name: "rsi_14".to_string(),
            shap_value: 0.15,
            feature_value: 62.5,
            direction: ContributionDirection::Positive,
        },
        FeatureContribution {
            feature_name: "volume".to_string(),
            shap_value: -0.08,
            feature_value: 1000.0,
            direction: ContributionDirection::Negative,
        },
    ];
    let attr = ActionAttribution::from_contributions(
        "position_size".to_string(),
        1.5,
        1.0,
        contribs.clone(),
    );

    Explanation {
        id: "exp_1".to_string(),
        observation_id: "obs_1".to_string(),
        action: ActionSnapshot {
            position_size: 1.0,
            entry_price: 50_000.0,
            stop_loss: 49_000.0,
            take_profit: 52_000.0,
            order_type: "limit".to_string(),
        },
        feature_importance: feat_importance,
        action_attributions: vec![attr],
        attention_weights: None,
        counterfactuals: vec![],
        summary: summary.to_string(),
        confidence,
        generated_at: Utc::now(),
    }
}

fn make_report() -> DecisionReport {
    DecisionReport {
        report_id: "rpt_001".to_string(),
        period_start: Utc::now(),
        period_end: Utc::now(),
        explanations: vec![
            make_explanation("RSI 处于超买区", 0.85),
            make_explanation("交易量异常放大", 0.7),
        ],
        feature_summary: FeatureSummary {
            top_features: vec![("rsi_14".to_string(), 0.15), ("volume".to_string(), 0.08)],
            feature_stability: HashMap::new(),
            regime_changes: vec![],
        },
        risk_metrics: RiskAttributionMetrics::default(),
        html_content: None,
        markdown_content: None,
    }
}

// ─── HTML 报告 ──────────────────────────────────────────

#[test]
fn test_html_report_contains_doctype_and_heading() {
    let report = make_report();
    let html = ReportGenerator::render_html(&report);
    assert!(
        html.contains("<!DOCTYPE html>"),
        "HTML 报告必须包含 DOCTYPE"
    );
    assert!(html.contains("<html>"), "HTML 报告必须包含 <html> 标签");
    assert!(html.contains("决策解释报告"), "HTML 报告必须包含中文标题");
}

#[test]
fn test_html_report_contains_feature_table() {
    let report = make_report();
    let html = ReportGenerator::render_html(&report);
    assert!(html.contains("<table>"), "HTML 报告必须包含特征表格");
    assert!(html.contains("rsi_14"), "HTML 报告必须列出特征 rsi_14");
    assert!(html.contains("volume"), "HTML 报告必须列出特征 volume");
}

#[test]
fn test_html_report_marks_positive_and_negative_classes() {
    let report = make_report();
    let html = ReportGenerator::render_html(&report);
    // 正负向特征应标记 CSS class
    assert!(html.contains("positive"), "正向特征应使用 positive 样式");
    assert!(html.contains("negative"), "负向特征应使用 negative 样式");
}

#[test]
fn test_html_report_includes_decision_details() {
    let report = make_report();
    let html = ReportGenerator::render_html(&report);
    assert!(html.contains("决策"), "HTML 报告应包含决策明细");
    assert!(html.contains("RSI 处于超买区"), "HTML 报告应包含解释摘要");
    assert!(html.contains("85.00"), "HTML 报告应包含置信度 85.00%");
}

#[test]
fn test_html_report_escapes_special_chars() {
    let mut report = make_report();
    // 在 summary 中注入特殊字符
    let mut expl = make_explanation("包含 <script> 标签 & 特殊字符", 0.5);
    expl.action = ActionSnapshot {
        position_size: 1.0,
        entry_price: 0.0,
        stop_loss: 0.0,
        take_profit: 0.0,
        order_type: "limit".into(),
    };
    report.explanations = vec![expl];
    let html = ReportGenerator::render_html(&report);
    // 不应包含未转义的 <script>
    assert!(!html.contains("<script>"), "HTML 报告必须转义 <script>");
    assert!(
        html.contains("&lt;script&gt;"),
        "HTML 报告应转义为 &lt;script&gt;"
    );
}

// ─── Markdown 报告 ──────────────────────────────────────

#[test]
fn test_markdown_report_contains_heading() {
    let report = make_report();
    let md = ReportGenerator::render_markdown(&report);
    assert!(
        md.contains("# 决策解释报告"),
        "Markdown 报告必须包含一级标题"
    );
}

#[test]
fn test_markdown_report_contains_feature_table() {
    let report = make_report();
    let md = ReportGenerator::render_markdown(&report);
    assert!(md.contains("| 特征 |"), "Markdown 报告必须包含特征表头");
    assert!(md.contains("rsi_14"), "Markdown 报告必须列出 rsi_14");
}

#[test]
fn test_markdown_report_includes_decision_section() {
    let report = make_report();
    let md = ReportGenerator::render_markdown(&report);
    assert!(
        md.contains("## 决策明细"),
        "Markdown 报告必须包含决策明细节"
    );
    assert!(md.contains("### 决策 #1"), "Markdown 报告应包含分项决策");
    assert!(md.contains("置信度"), "Markdown 报告应包含置信度字段");
}

/// 关键测试（来自设计）：报告生成 < 1s（基线性能）
#[test]
fn test_report_generation_completes_quickly() {
    let report = make_report();
    let start = std::time::Instant::now();
    let _ = ReportGenerator::render_html(&report);
    let _ = ReportGenerator::render_markdown(&report);
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_secs() < 1,
        "报告生成耗时 {} 超过 1s",
        elapsed.as_secs_f64()
    );
}

// ─── generate_decision_report ─────────────────────────────

#[test]
fn test_generate_decision_report_populates_html_and_markdown() {
    let explanations = vec![
        make_explanation("决策 1", 0.8),
        make_explanation("决策 2", 0.7),
    ];
    let report =
        ReportGenerator::generate_decision_report("rpt_test", explanations, Utc::now(), Utc::now());
    assert_eq!(report.report_id, "rpt_test");
    assert!(report.html_content.is_some());
    assert!(report.markdown_content.is_some());
    assert_eq!(report.explanations.len(), 2);
    let html = report.html_content.unwrap();
    assert!(html.contains("决策 1"));
}

// ─── aggregate_risk ────────────────────────────────────

#[test]
fn test_aggregate_risk_empty_returns_default() {
    let metrics = ReportGenerator::aggregate_risk(&[]);
    assert!(metrics.var_contribution.is_empty());
    assert!(metrics.sharpe_contribution.is_empty());
    assert!(metrics.max_drawdown_factors.is_empty());
}

#[test]
fn test_aggregate_risk_computes_var_and_sharpe_contributions() {
    // exp1: rsi_14 shap=+0.15, confidence=0.85 -> var 0.0225, sharpe 0.1275
    // exp2: rsi_14 shap=+0.15, confidence=0.70 -> var 0.045,  sharpe 0.105
    // 累计：rsi_14 var=0.0675, sharpe=0.2325
    // 累计：volume var=0.024, sharpe=-0.04（两个 exp 中 shap=-0.08 * 0.5 = -0.04/each）
    let explanations = vec![make_explanation("e1", 0.85), make_explanation("e2", 0.70)];
    let metrics = ReportGenerator::aggregate_risk(&explanations);
    let rsi_var = metrics
        .var_contribution
        .get("rsi_14")
        .copied()
        .unwrap_or_default();
    let rsi_sharpe = metrics
        .sharpe_contribution
        .get("rsi_14")
        .copied()
        .unwrap_or_default();
    // 容差 1e-6
    assert!(
        (rsi_var - (0.15 * 0.15 + 0.15 * 0.30)).abs() < 1e-6,
        "rsi var = {rsi_var}"
    );
    assert!(
        (rsi_sharpe - (0.15 * 0.85 + 0.15 * 0.70)).abs() < 1e-6,
        "rsi sharpe = {rsi_sharpe}"
    );
    // volume SHAP 在 attribution 中为 -0.08（带符号）
    let vol_sharpe = metrics
        .sharpe_contribution
        .get("volume")
        .copied()
        .unwrap_or_default();
    assert!((vol_sharpe - (-0.08 * 0.85 + -0.08 * 0.70)).abs() < 1e-6);
}

#[test]
fn test_aggregate_risk_max_drawdown_factors_picks_top_negative() {
    // 两个 explanation 中 volume shap 都是 -0.08（负向），应为 max_drawdown_factors 第一名
    let explanations = vec![make_explanation("e1", 0.5), make_explanation("e2", 0.5)];
    let metrics = ReportGenerator::aggregate_risk(&explanations);
    assert!(!metrics.max_drawdown_factors.is_empty());
    assert_eq!(metrics.max_drawdown_factors[0], "volume");
}

#[test]
fn test_aggregate_risk_clamps_confidence_to_unit_interval() {
    // 异常 confidence=1.5 应被 clamp 到 1.0，1-1=0 => var 贡献为 0，sharpe 贡献 = shap
    let mut e = make_explanation("extreme", 1.5);
    e.confidence = 1.5;
    let metrics = ReportGenerator::aggregate_risk(&[e]);
    let rsi_var = metrics
        .var_contribution
        .get("rsi_14")
        .copied()
        .unwrap_or_default();
    let rsi_sharpe = metrics
        .sharpe_contribution
        .get("rsi_14")
        .copied()
        .unwrap_or_default();
    assert!(
        rsi_var.abs() < 1e-9,
        "clamped confidence=1.0 应使 var 贡献 = 0"
    );
    assert!(
        (rsi_sharpe - 0.15).abs() < 1e-6,
        "sharpe 贡献 = shap * 1.0 = 0.15"
    );
}
