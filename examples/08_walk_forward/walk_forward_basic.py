"""walk_forward_basic.py — Walk-Forward 基本用法。

使用 `axon_quant.walk_forward` 模块。

运行方式：
    cd axon
    .venv/bin/python examples/08_walk_forward/walk_forward_basic.py
"""

from __future__ import annotations

import random

import axon_quant  # noqa: E402
py_detect_leakage = axon_quant.walk_forward.py_detect_leakage
py_embargo_indices = axon_quant.walk_forward.py_embargo_indices
py_deflated_sharpe = axon_quant.walk_forward.py_deflated_sharpe
py_purge_overlapping_labels = axon_quant.walk_forward.py_purge_overlapping_labels


def main() -> int:
    print("=" * 60)
    print("Walk-Forward 基本用法示例")
    print("=" * 60)

    # 1. 泄漏检测
    print("\n[1] 泄漏检测")
    train_idx = list(range(0, 80))
    test_idx = list(range(80, 100))
    has_leak, pairs = py_detect_leakage(train_idx, test_idx, 0)
    print(f"  无重叠: has_leakage={has_leak}, pairs={len(pairs)}")
    assert not has_leak

    train_idx = list(range(0, 90))
    test_idx = list(range(80, 100))
    has_leak, pairs = py_detect_leakage(train_idx, test_idx, 0)
    print(f"  有重叠: has_leakage={has_leak}, pairs={len(pairs)}")
    assert has_leak

    # 2. Embargo
    print("\n[2] Embargo")
    test_idx = list(range(100, 150))
    embargoed = py_embargo_indices(test_idx, 0.1, 200)
    print(f"  embargo 索引数: {len(embargoed)}")
    assert len(embargoed) > 0

    # 3. Purge
    print("\n[3] Purge")
    train_idx = list(range(100))
    test_idx = list(range(100, 150))
    purged = py_purge_overlapping_labels(train_idx, test_idx, 5)
    print(f"  purge 后训练集大小: {len(purged)}（原始 100）")
    assert len(purged) < 100

    # 4. Deflated Sharpe
    print("\n[4] Deflated Sharpe")
    observed_sharpe = 2.0
    dsr = py_deflated_sharpe(observed_sharpe, 100, 0.5)
    print(f"  observed_sharpe={observed_sharpe}, deflated_sharpe={dsr:.4f}")
    assert dsr <= observed_sharpe

    # 5. 更多 trial 的惩罚
    print("\n[5] 更多 trial 的惩罚")
    dsr_50 = py_deflated_sharpe(2.0, 50, 0.5)
    dsr_200 = py_deflated_sharpe(2.0, 200, 0.5)
    print(f"  50 trials: {dsr_50:.4f}")
    print(f"  200 trials: {dsr_200:.4f}")
    assert dsr_200 <= dsr_50

    print("\n=== ALL PASS ===")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
