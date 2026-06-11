"""walk_forward_basic.py — Walk-Forward 基本用法。

生成合成收益率序列（随机游走），分别用 Expanding / Rolling 窗口分割，
打印每个 fold 的 OOS 收益与汇总指标。
"""

from __future__ import annotations

import random
import sys
from pathlib import Path

# 让 Python 找到 axon_walk_forward 包
CARGO_MANIFEST = Path(__file__).parent.parent / "crates" / "axon-walk-forward"
sys.path.insert(0, str(CARGO_MANIFEST / "python"))

from axon_walk_forward import types, splitter, evaluation  # noqa: E402


def main() -> int:
    print("=" * 60)
    print("Walk-Forward 基本用法示例")
    print("=" * 60)

    # 1. 合成数据：1000 个交易日
    random.seed(42)
    n_samples = 1000
    returns = [random.gauss(0.0005, 0.02) for _ in range(n_samples)]
    print(f"\n生成 {n_samples} 个交易日合成收益率 (μ=0.05%, σ=2%)")

    # 2. Expanding 窗口：5 年 train + 1 季度 test
    print("\n[1] Expanding 窗口")
    cfg_exp = types.WalkForwardConfig.expanding(
        train_size=200, test_size=50, step_size=50
    )
    cfg_exp.validate()
    folds = splitter.TimeSeriesSplitter(cfg_exp).split(n_samples)
    print(f"  生成 {len(folds)} 个 fold")
    for f in folds[:3]:
        print(f"  fold {f.fold_id}: train [{f.train_start},{f.train_end}) "
              f"test [{f.test_start},{f.test_end})")

    # 计算 OOS 收益（每个 fold）
    fold_results = []
    for f in folds:
        test_ret = sum(returns[f.test_start:f.test_end])
        train_ret = sum(returns[f.train_start:f.train_end])
        # 简化 Sharpe：test 收益 / sqrt(test_size) / σ
        test_slice = returns[f.test_start:f.test_end]
        sharpe = (
            (sum(test_slice) / len(test_slice))
            / (sum((r - sum(test_slice) / len(test_slice)) ** 2 for r in test_slice) / len(test_slice)) ** 0.5
            * (252 ** 0.5)
            if len(test_slice) > 1 else 0.0
        )
        # 简化最大回撤：累计收益最大跌幅
        cum = 0.0
        peak = 0.0
        max_dd = 0.0
        for r in test_slice:
            cum += r
            if cum > peak:
                peak = cum
            dd = peak - cum
            if dd > max_dd:
                max_dd = dd
        overfit = train_ret / test_ret if abs(test_ret) > 1e-9 else float("inf")
        fold_results.append(
            types.FoldResult(
                fold_id=f.fold_id,
                train_return=train_ret,
                validation_return=0.0,
                test_return=test_ret,
                test_sharpe=sharpe,
                test_max_drawdown=-max_dd,
                overfit_ratio=overfit,
            )
        )

    agg, stab = evaluation.aggregate_folds(fold_results)
    print(f"\n  === 汇总指标 ===")
    print(f"  Mean OOS Return:   {agg.mean_oos_return:.4f}")
    print(f"  Std OOS Return:    {agg.std_oos_return:.4f}")
    print(f"  Mean OOS Sharpe:   {agg.mean_oos_sharpe:.4f}")
    print(f"  Median OOS Return: {agg.median_oos_return:.4f}")
    print(f"  Pct Profitable:    {agg.pct_profitable_folds:.2%}")
    print(f"  Worst Fold:        {agg.worst_fold_return:.4f}")
    print(f"  Best Fold:         {agg.best_fold_return:.4f}")
    print(f"\n  === 稳定性指标 ===")
    print(f"  Sharpe of Sharpe:  {stab.sharpe_of_sharpe:.4f}")
    print(f"  Return Autocorr:   {stab.return_autocorrelation:.4f}")
    print(f"  Deflated Sharpe:   {stab.deflated_sharpe:.4f}")
    print(f"  Probability Loss:  {stab.probability_of_loss:.4f}")

    # 3. Rolling 窗口对比
    print("\n[2] Rolling 窗口")
    cfg_roll = types.WalkForwardConfig.rolling(
        train_size=200, test_size=50, step_size=50
    )
    folds_roll = splitter.TimeSeriesSplitter(cfg_roll).split(n_samples)
    print(f"  生成 {len(folds_roll)} 个 fold")
    for f in folds_roll[:3]:
        print(f"  fold {f.fold_id}: train [{f.train_start},{f.train_end}) "
              f"test [{f.test_start},{f.test_end})")

    # 4. Purge gap 演示
    print("\n[3] Purge gap 演示")
    cfg_purge = types.WalkForwardConfig.expanding(
        train_size=200, test_size=50, step_size=50
    )
    cfg_purge.purge_gap = 5
    folds_purge = splitter.TimeSeriesSplitter(cfg_purge).split(n_samples)
    print(f"  生成 {len(folds_purge)} 个 fold（purge_gap=5）")
    for f in folds_purge[:3]:
        gap = f.test_start - f.validation_end if f.val_size > 0 else 0
        print(f"  fold {f.fold_id}: val [{f.validation_start},{f.validation_end}) "
              f"gap={gap} test [{f.test_start},{f.test_end})")

    print("\n=== ALL PASS ===")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
