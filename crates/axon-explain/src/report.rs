//! 决策报告生成器
//!
//! 支持 HTML 和 Markdown 两种格式。
//! - HTML：含 CSS 样式，正负向特征用不同颜色标记
//! - Markdown：简洁的表格 + 列表，适合版本控制

use chrono::{DateTime, Utc};

use crate::types::{ContributionDirection, DecisionReport, Explanation};

/// 报告生成器
pub struct ReportGenerator;

impl ReportGenerator {
    /// 从解释列表生成完整决策报告
    pub fn generate_decision_report(
        report_id: &str,
        explanations: Vec<Explanation>,
        period_start: DateTime<Utc>,
        period_end: DateTime<Utc>,
    ) -> DecisionReport {
        let feature_summary = Self::aggregate_features(&explanations);
        let risk_metrics = Self::aggregate_risk(&explanations);

        let mut report = DecisionReport {
            report_id: report_id.to_string(),
            period_start,
            period_end,
            explanations,
            feature_summary,
            risk_metrics,
            html_content: None,
            markdown_content: None,
        };

        // 预渲染 HTML 和 Markdown
        report.html_content = Some(Self::render_html(&report));
        report.markdown_content = Some(Self::render_markdown(&report));

        report
    }

    /// 渲染 HTML 报告
    pub fn render_html(report: &DecisionReport) -> String {
        let mut parts = vec![
            "<!DOCTYPE html>".to_string(),
            "<html><head><meta charset=\"utf-8\">".to_string(),
            "<title>AXON 决策解释报告</title>".to_string(),
            "<style>".to_string(),
            "  body { font-family: -apple-system, sans-serif; margin: 20px; color: #2c3e50; }".to_string(),
            "  .positive { color: #27ae60; }".to_string(),
            "  .negative { color: #c0392b; }".to_string(),
            "  table { border-collapse: collapse; width: 100%; margin: 12px 0; }".to_string(),
            "  th, td { border: 1px solid #ddd; padding: 8px; text-align: left; }".to_string(),
            "  th { background-color: #34495e; color: white; }".to_string(),
            "  .feature-bar { height: 18px; background: #3498db; display: inline-block; vertical-align: middle; }".to_string(),
            "  .meta { color: #7f8c8d; font-size: 0.9em; }".to_string(),
            "  .decision { border-left: 3px solid #3498db; padding: 8px 12px; margin: 10px 0; background: #f8f9fa; }".to_string(),
            "</style></head><body>".to_string(),
            "<h1>决策解释报告</h1>".to_string(),
            format!("<p class=\"meta\"><strong>报告 ID:</strong> {}</p>", html_escape(&report.report_id)),
            format!("<p class=\"meta\"><strong>报告期间:</strong> {} ~ {}</p>",
                report.period_start.format("%Y-%m-%d %H:%M:%S"),
                report.period_end.format("%Y-%m-%d %H:%M:%S")),
            format!("<p class=\"meta\"><strong>决策数量:</strong> {}</p>", report.explanations.len()),
        ];

        // 特征重要性表格
        parts.push("<h2>特征重要性排名</h2>".to_string());
        parts.push("<table><tr><th>特征</th><th>平均 |SHAP|</th><th>贡献</th></tr>".to_string());
        for (name, avg_shap) in &report.feature_summary.top_features {
            let cls = if *avg_shap > 0.0 {
                "positive"
            } else {
                "negative"
            };
            let bar_width = (avg_shap.abs() * 500.0).min(500.0) as usize;
            parts.push(format!(
                "<tr><td>{}</td><td class=\"{}\">{:.6}</td><td><div class=\"feature-bar\" style=\"width:{}px\"></div></td></tr>",
                html_escape(name), cls, avg_shap, bar_width
            ));
        }
        parts.push("</table>".to_string());

        // 决策明细
        parts.push("<h2>决策明细</h2>".to_string());
        for (i, exp) in report.explanations.iter().take(20).enumerate() {
            parts.push("<div class=\"decision\">".to_string());
            parts.push(format!("<h3>决策 #{}</h3>", i + 1));
            parts.push(format!(
                "<p><strong>摘要:</strong> {}</p>",
                html_escape(&exp.summary)
            ));
            parts.push(format!(
                "<p><strong>置信度:</strong> {:.2}%</p>",
                exp.confidence * 100.0
            ));
            parts.push(format!(
                "<p><strong>动作:</strong> position={:.4} entry={:.2} SL={:.2} TP={:.2} ({})</p>",
                exp.action.position_size,
                exp.action.entry_price,
                exp.action.stop_loss,
                exp.action.take_profit,
                exp.action.order_type
            ));

            // Top features
            if let Some(attr) = exp.action_attributions.first() {
                parts.push("<ul>".to_string());
                for feat in attr
                    .top_positive
                    .iter()
                    .take(3)
                    .chain(attr.top_negative.iter().take(3))
                {
                    let cls = if matches!(feat.direction, ContributionDirection::Positive) {
                        "positive"
                    } else {
                        "negative"
                    };
                    parts.push(format!(
                        "<li class=\"{}\">{}: {:+.6} (当前值: {:.4})</li>",
                        cls,
                        html_escape(&feat.feature_name),
                        feat.shap_value,
                        feat.feature_value
                    ));
                }
                parts.push("</ul>".to_string());
            }

            // 反事实
            for cf in &exp.counterfactuals {
                parts.push(format!(
                    "<p><em>反事实:</em> {}</p>",
                    html_escape(&cf.narrative)
                ));
            }

            parts.push("</div>".to_string());
        }

        parts.push("</body></html>".to_string());
        parts.join("\n")
    }

    /// 渲染 Markdown 报告
    pub fn render_markdown(report: &DecisionReport) -> String {
        let mut lines = vec![
            "# 决策解释报告".to_string(),
            String::new(),
            format!("**报告 ID:** {}", report.report_id),
            format!(
                "**报告期间:** {} ~ {}",
                report.period_start.format("%Y-%m-%d %H:%M:%S"),
                report.period_end.format("%Y-%m-%d %H:%M:%S")
            ),
            format!("**决策数量:** {}", report.explanations.len()),
            String::new(),
            "## 特征重要性排名".to_string(),
            String::new(),
            "| 特征 | 平均 |SHAP| | 方向 |".to_string(),
            "|------|------------|------|".to_string(),
        ];

        for (name, avg_shap) in &report.feature_summary.top_features {
            let direction = if *avg_shap > 0.0 { "正向" } else { "负向" };
            lines.push(format!("| {} | {:+.6} | {} |", name, avg_shap, direction));
        }

        lines.push(String::new());
        lines.push("## 决策明细".to_string());
        for (i, exp) in report.explanations.iter().take(20).enumerate() {
            lines.push(String::new());
            lines.push(format!("### 决策 #{}", i + 1));
            lines.push(format!("- **摘要:** {}", exp.summary));
            lines.push(format!("- **置信度:** {:.2}%", exp.confidence * 100.0));
            lines.push(format!(
                "- **动作:** position={:.4} entry={:.2} SL={:.2} TP={:.2} ({})",
                exp.action.position_size,
                exp.action.entry_price,
                exp.action.stop_loss,
                exp.action.take_profit,
                exp.action.order_type
            ));

            if let Some(attr) = exp.action_attributions.first() {
                for feat in attr.top_positive.iter().take(3) {
                    lines.push(format!(
                        "  - ✅ {}: {:+.6} (当前值: {:.4})",
                        feat.feature_name, feat.shap_value, feat.feature_value
                    ));
                }
                for feat in attr.top_negative.iter().take(3) {
                    lines.push(format!(
                        "  - ❌ {}: {:+.6} (当前值: {:.4})",
                        feat.feature_name, feat.shap_value, feat.feature_value
                    ));
                }
            }

            for cf in &exp.counterfactuals {
                lines.push(format!("  - *反事实:* {}", cf.narrative));
            }
        }

        lines.join("\n")
    }

    /// 聚合所有解释的特征统计
    fn aggregate_features(explanations: &[Explanation]) -> crate::types::FeatureSummary {
        use std::collections::HashMap;
        let mut stats: HashMap<String, Vec<f64>> = HashMap::new();

        for exp in explanations {
            for (name, val) in &exp.feature_importance {
                stats.entry(name.clone()).or_default().push(*val);
            }
        }

        let mut top_features: Vec<(String, f64)> = stats
            .iter()
            .map(|(name, vals)| {
                let avg = vals.iter().sum::<f64>() / vals.len() as f64;
                (name.clone(), avg)
            })
            .collect();
        top_features.sort_by(|a, b| {
            b.1.abs()
                .partial_cmp(&a.1.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        crate::types::FeatureSummary {
            top_features,
            feature_stability: HashMap::new(),
            regime_changes: vec![],
        }
    }

    /// 聚合风险归因指标
    ///
    /// 从一组 Explanation 中聚合：
    /// - `var_contribution`：特征 SHAP 绝对值按 `(1 - confidence)` 加权（不确信的方向放大风险）
    /// - `sharpe_contribution`：特征 SHAP 按 `confidence` 加权（确信的方向贡献 Sharpe）
    /// - `max_drawdown_factors`：累计负向 SHAP 的 top 3 特征名
    ///
    /// 公式：
    /// - `var_contribution[f] = Σ_i |shap_i_f| * (1 - confidence_i)`
    /// - `sharpe_contribution[f] = Σ_i shap_i_f * confidence_i`
    /// - `max_drawdown_factors`：按 `Σ_i min(shap_i_f, 0)` 升序取前 3
    pub fn aggregate_risk(explanations: &[Explanation]) -> crate::types::RiskAttributionMetrics {
        use std::collections::HashMap;

        // 空输入直接返回默认值（保留 PartialEq 测试便利性）
        if explanations.is_empty() {
            return crate::types::RiskAttributionMetrics::default();
        }

        // 原始 SHAP（带符号）按特征聚合：用于 Sharpe / max_drawdown
        let mut signed_sums: HashMap<String, f64> = HashMap::new();
        // VaR 加权（|shap| * (1 - confidence)）
        let mut var_contribution: HashMap<String, f64> = HashMap::new();
        // Sharpe 加权（shap * confidence）
        let mut sharpe_contribution: HashMap<String, f64> = HashMap::new();

        for exp in explanations {
            // 置信度裁剪到 [0, 1]，避免异常值破坏聚合
            let conf = exp.confidence.clamp(0.0, 1.0);
            let inv_conf = 1.0 - conf;

            for (name, &abs_shap) in &exp.feature_importance {
                // 通过符号位反推 SHAP 真实值：Explanation 存的是 |SHAP|，
                // 而 attribution 中才保留符号。这里用 action_attributions 的符号作为权威源。
                let signed = signed_shap_for(exp, name).unwrap_or(abs_shap);
                let actual_signed = if signed.is_nan() { abs_shap } else { signed };

                *signed_sums.entry(name.clone()).or_insert(0.0) += actual_signed;
                *var_contribution.entry(name.clone()).or_insert(0.0) += abs_shap * inv_conf;
                *sharpe_contribution.entry(name.clone()).or_insert(0.0) += actual_signed * conf;
            }
        }

        // max_drawdown_factors：累计负向（取累计和最小的前 3 个特征名）
        let mut negative_factors: Vec<(String, f64)> =
            signed_sums.into_iter().filter(|(_, v)| *v < 0.0).collect();
        // 按累计负向值升序（越负越靠前）
        negative_factors.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let max_drawdown_factors: Vec<String> = negative_factors
            .into_iter()
            .take(3)
            .map(|(name, _)| name)
            .collect();

        crate::types::RiskAttributionMetrics {
            var_contribution,
            sharpe_contribution,
            max_drawdown_factors,
        }
    }
}

/// 从 Explanation 的 action_attributions 中按特征名查找 SHAP 带符号值
///
/// 优先取第一个 ActionAttribution 的 feature_contributions 中匹配 name 的 shap_value；
/// 找不到时返回 `None`，调用方应回退到 |SHAP|（取正）。
fn signed_shap_for(exp: &Explanation, name: &str) -> Option<f64> {
    exp.action_attributions
        .iter()
        .flat_map(|attr| attr.feature_contributions.iter())
        .find(|c| c.feature_name == name)
        .map(|c| c.shap_value)
}

/// HTML 字符转义
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
