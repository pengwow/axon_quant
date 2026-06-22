#!/usr/bin/env python3
"""Axon Explain 模块演示 —— 可解释性 AI (XAI) 完整功能展示。

覆盖范围:
  1. KernelSHAP 创建与 SHAP 计算
  2. CounterfactualConfig 配置
  3. ReportGenerator 报告生成
  4. ContributionDirection 枚举
  5. FeatureContribution 结构
  6. ActionSnapshot 结构
  7. Explanation 结构 (通过 KernelSHAP 流程间接展示)
  8. DecisionReport 结构 (通过 ReportGenerator 间接展示)

运行方式:
    source .venv/bin/activate
    python examples/15_explain/explain_demo.py

零外部依赖: 仅需 axon_quant + Python 标准库。
"""

from __future__ import annotations

import sys
from pathlib import Path

_VENV_SITE = Path("/Users/liupeng/workspace/quant/axon/.venv/lib/python3.14/site-packages")
if _VENV_SITE.exists() and str(_VENV_SITE) not in sys.path:
    sys.path.insert(0, str(_VENV_SITE))

RESET = "\033[0m"
BOLD = "\033[1m"
DIM = "\033[2m"
RED = "\033[31m"
GREEN = "\033[32m"
YELLOW = "\033[33m"
CYAN = "\033[36m"

if sys.platform == "win32":
    try:
        import os
        os.system("")
    except Exception:
        pass


def header(title: str, icon: str = "▶") -> None:
    print(f"\n{BOLD}{CYAN}{'═' * 60}{RESET}")
    print(f"{BOLD}{CYAN}  {icon} {title}{RESET}")
    print(f"{BOLD}{CYAN}{'═' * 60}{RESET}")


def step(n: int, text: str) -> None:
    print(f"\n  {BOLD}{YELLOW}[步骤 {n}]{RESET} {text}")


def ok(msg: str) -> None:
    print(f"    {GREEN}✅ {msg}{RESET}")


def info(msg: str) -> None:
    print(f"    {DIM}{msg}{RESET}")


def warn(msg: str) -> None:
    print(f"    {YELLOW}⚠️  {msg}{RESET}")


def fail(msg: str) -> None:
    print(f"    {RED}❌ {msg}{RESET}")


def value(label: str, v: object, width: int = 22) -> None:
    print(f"    {BOLD}{label:<{width}}{RESET} {v}")


def separator() -> None:
    print(f"    {DIM}{'─' * 50}{RESET}")


def demo_kernel_shap() -> None:
    header("KernelSHAP 创建与 SHAP 计算", "🔍")

    from axon_quant.explain import KernelSHAP, ExplainError

    def mock_model(features):
        """模拟交易模型: 加权求和 + 偏置。"""
        weights = [0.3, 0.5, 0.2]
        bias = 1.0
        return [sum(w * f for w, f in zip(weights, features)) + bias]

    step(1, "创建 KernelSHAP 解释器（3 个特征的模拟交易模型）")
    background = [
        [10.0, 50.0, 0.8],
        [12.0, 45.0, 0.6],
        [8.0, 55.0, 0.9],
        [11.0, 48.0, 0.7],
    ]
    shap_explainer = KernelSHAP(mock_model, background, n_samples=50)
    ok(f"KernelSHAP 创建成功: repr={repr(shap_explainer)}")

    step(2, "计算 SHAP 值 —— 观测 [15.0, 60.0, 0.95]")
    observation = [15.0, 60.0, 0.95]
    shap_values = shap_explainer.compute_shap(observation)
    for i, (name, sv) in enumerate(
        zip(["价格", "成交量", "情绪"], shap_values)
    ):
        direction = "+" if sv > 0 else "-" if sv < 0 else "="
        value(f"  特征 {i}: {name}", f"SHAP={sv:+.4f}  ({direction})")
    ok("SHAP 值计算完成 — 每个特征对预测的贡献度")

    step(3, "异常路径演示 —— 空背景数据")
    try:
        KernelSHAP(mock_model, [], 10)
        fail("应该抛出异常")
    except ExplainError:
        ok("空背景数据正确抛出 ExplainError")

    step(4, "异常路径演示 —— 特征维度不匹配")
    try:
        shap_explainer.compute_shap([1.0, 2.0])
        fail("应该抛出异常")
    except ExplainError:
        ok("维度不匹配 (2 vs 3) 正确抛出 ExplainError")

    separator()
    ok("KernelSHAP 完成！支持任意 callable 模型 + 背景数据集\n")


def demo_contribution_direction() -> None:
    header("ContributionDirection 枚举", "🧭")

    from axon_quant.explain import ContributionDirection

    step(1, "枚举变体展示")
    for name in ("Positive", "Negative", "Neutral"):
        member = getattr(ContributionDirection, name)
        value(f"  {name}", f"repr={repr(member)}, str={str(member)}")
    ok("三个变体: Positive / Negative / Neutral")

    step(2, "枚举值比较")
    eq = ContributionDirection.Positive == ContributionDirection.Positive
    neq = ContributionDirection.Positive == ContributionDirection.Negative
    value("  Positive == Positive", eq)
    value("  Positive == Negative", neq)
    ok("枚举支持标准比较操作")

    separator()
    ok("ContributionDirection 完成！\n")


def demo_feature_contribution() -> None:
    header("FeatureContribution 结构", "📊")

    from axon_quant.explain import FeatureContribution, ContributionDirection

    step(1, "创建正向贡献特征")
    fc_pos = FeatureContribution("rsi_oversold", 0.45, 28.5, ContributionDirection.Positive)
    value("  feature_name", fc_pos.feature_name)
    value("  shap_value", f"{fc_pos.shap_value:+.4f}")
    value("  feature_value", fc_pos.feature_value)
    value("  direction", str(fc_pos.direction))
    d = fc_pos.to_dict()
    value("  to_dict()", d)
    ok("正向贡献: RSI 超卖信号推动买入决策")

    step(2, "创建负向贡献特征")
    fc_neg = FeatureContribution("volume_spike", -0.32, 150000.0, ContributionDirection.Negative)
    value("  feature_name", fc_neg.feature_name)
    value("  shap_value", f"{fc_neg.shap_value:+.4f}")
    value("  direction", str(fc_neg.direction))
    ok("负向贡献: 成交量异常放大抑制买入信号")

    step(3, "创建中性贡献特征")
    fc_neu = FeatureContribution("bid_ask_spread", 0.0003, 0.012, ContributionDirection.Neutral)
    value("  shap_value", f"{fc_neu.shap_value:+.6f}")
    value("  direction", str(fc_neu.direction))
    ok("中性贡献: 买卖价差对决策几乎无影响")

    separator()
    ok("FeatureContribution 完成！支持 to_dict() 序列化\n")


def demo_action_snapshot() -> None:
    header("ActionSnapshot 结构", "📸")

    from axon_quant.explain import ActionSnapshot

    step(1, "创建买入动作快照")
    snap_buy = ActionSnapshot(
        position_size=1.5,
        entry_price=50000.0,
        stop_loss=49000.0,
        take_profit=52500.0,
        order_type="limit",
    )
    value("  position_size", snap_buy.position_size)
    value("  entry_price", f"{snap_buy.entry_price:,.2f}")
    value("  stop_loss", f"{snap_buy.stop_loss:,.2f}")
    value("  take_profit", f"{snap_buy.take_profit:,.2f}")
    value("  order_type", snap_buy.order_type)
    d = snap_buy.to_dict()
    value("  to_dict()", d)
    ok("买入快照: 1.5 BTC @ 50,000 (止损 49,000 / 止盈 52,500)")

    step(2, "创建卖出动作快照")
    snap_sell = ActionSnapshot(
        position_size=-2.0,
        entry_price=51200.0,
        stop_loss=52000.0,
        take_profit=49500.0,
        order_type="market",
    )
    value("  position_size", snap_sell.position_size)
    value("  order_type", snap_sell.order_type)
    ok("卖出快照: -2.0 BTC 市价单 (止损 52,000 / 止盈 49,500)")

    separator()
    ok("ActionSnapshot 完成！\n")


def demo_counterfactual_config() -> None:
    header("CounterfactualConfig 配置", "⚙️")

    from axon_quant.explain import CounterfactualConfig

    step(1, "默认配置")
    cfg_default = CounterfactualConfig()
    value("  max_changes", cfg_default.max_changes)
    value("  step_size", cfg_default.step_size)
    value("  confidence_threshold", cfg_default.confidence_threshold)
    ok(f"默认: {repr(cfg_default)}")

    step(2, "自定义配置 —— 静态工厂方法")
    cfg_custom = CounterfactualConfig.with_max_changes(5)
    value("  max_changes (自定义)", cfg_custom.max_changes)
    ok("修改最大修改特征数: 3 → 5")

    cfg_step = CounterfactualConfig.with_step_size(0.3)
    value("  step_size (自定义)", cfg_step.step_size)
    ok("修改步长: 0.5 → 0.3")

    cfg_thresh = CounterfactualConfig.with_confidence_threshold(0.1)
    value("  confidence_threshold (自定义)", cfg_thresh.confidence_threshold)
    ok("修改置信度阈值: 0.05 → 0.1")

    step(3, "异常路径 —— 步长越界")
    cfg_clamp = CounterfactualConfig.with_step_size(1.5)
    value("  输入 1.5 → 实际值", cfg_clamp.step_size)
    ok("步长被 clamp 到 [0.0, 1.0] 范围内")

    separator()
    ok("CounterfactualConfig 完成！\n")


def demo_report_generator() -> None:
    header("ReportGenerator 报告生成", "📝")

    from axon_quant.explain import ReportGenerator

    step(1, "查看 ReportGenerator 类")
    value("  type", type(ReportGenerator).__name__)
    value("  repr", repr(ReportGenerator))
    ok("ReportGenerator 是静态方法类，无需实例化")

    step(2, "静态方法一览")
    methods = ["generate_decision_report", "render_html", "render_markdown"]
    for m in methods:
        has = hasattr(ReportGenerator, m)
        status = "✅" if has else "❌"
        value(f"  {m}", f"{status} 存在" if has else f"{status} 不存在")
    ok("三个静态方法: generate / render_html / render_markdown")

    step(3, "使用说明")
    info("ReportGenerator.generate_decision_report(")
    info("    report_id='RPT-001',")
    info("    explanations=[Explanation, ...],  # 内部类型")
    info("    period_start='2026-01-01T00:00:00Z',")
    info("    period_end='2026-01-31T23:59:59Z',")
    info(") → DecisionReport")
    ok("通过 explain 流程间接获取 Explanation 后调用")

    separator()
    ok("ReportGenerator 完成！\n")


def demo_explanation_and_report() -> None:
    header("Explanation & DecisionReport 结构", "📋")

    from axon_quant.explain import Explanation, DecisionReport

    step(1, "Explanation 类型信息")
    value("  type", type(Explanation).__name__)
    info("Explanation 是内部 Rust 类型，无法直接从 Python 构造")
    info("字段: id, observation_id, action, feature_importance,")
    info("      action_attributions, counterfactuals, summary, confidence")
    ok("Explanation 展示完成")

    step(2, "DecisionReport 类型信息")
    value("  type", type(DecisionReport).__name__)
    info("DecisionReport 由 ReportGenerator.generate_decision_report() 返回")
    info("字段: report_id, period_start, period_end, explanations,")
    info("      feature_summary, risk_metrics, html_content, markdown_content")
    ok("DecisionReport 展示完成")

    step(3, "端到端工作流示意")
    info("1) KernelSHAP → compute_shap(obs) → SHAP 值")
    info("2) 构建 Explanation (FeatureContribution + ActionSnapshot)")
    info("3) CounterfactualConfig → 反事实分析")
    info("4) ReportGenerator.generate_decision_report() → DecisionReport")
    info("5) ReportGenerator.render_html / render_markdown → 输出")
    ok("完整可解释性流程: SHAP → 归因 → 反事实 → 报告")

    separator()
    ok("Explanation & DecisionReport 完成！\n")


def main() -> int:
    print(f"""
{BOLD}{CYAN}╔══════════════════════════════════════════════════════════╗
║                                                          ║
║   {CYAN}AXON Quant{RESET}{CYAN}  —  Explain 可解释性模块演示              ║
║   {DIM}KernelSHAP · 反事实解释 · 决策报告{RESET}{CYAN}                    ║
║                                                          ║
╚══════════════════════════════════════════════════════════╝{RESET}
""")

    demos = [
        ("KernelSHAP", demo_kernel_shap),
        ("ContributionDirection", demo_contribution_direction),
        ("FeatureContribution", demo_feature_contribution),
        ("ActionSnapshot", demo_action_snapshot),
        ("CounterfactualConfig", demo_counterfactual_config),
        ("ReportGenerator", demo_report_generator),
        ("Explanation & DecisionReport", demo_explanation_and_report),
    ]

    for i, (name, func) in enumerate(demos, 1):
        try:
            func()
        except Exception as e:
            fail(f"{name} 执行出错: {e}")
            import traceback
            traceback.print_exc()
            return 1

    print(f"\n  {BOLD}{GREEN}全部 {len(demos)} 个模块演示完成！{RESET}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
