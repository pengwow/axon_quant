"""walk_forward_purging.py — Purge / Embargo / Leakage 检测示例。"""

from __future__ import annotations

import sys
from pathlib import Path

CARGO_MANIFEST = Path(__file__).parent.parent / "crates" / "axon-walk-forward"
sys.path.insert(0, str(CARGO_MANIFEST / "python"))

import numpy as np  # noqa: E402

from axon_walk_forward import purging  # noqa: E402


def main() -> int:
    print("=" * 60)
    print("Purge / Embargo / Leakage 示例")
    print("=" * 60)

    # 1. Purge
    print("\n[1] Purge：移除与测试集标签重叠的训练样本")
    train_idx = np.arange(0, 100)
    test_idx = np.arange(100, 150)
    purged = purging.purge_overlapping_labels(train_idx, test_idx, label_horizon=5)
    print(f"  原始训练集: {len(train_idx)} 个 (0..99)")
    print(f"  测试集起始: {test_idx[0]}")
    print(f"  horizon=5 → 移除索引 >= {test_idx[0] - 5} = 95")
    print(f"  Purge 后:   {len(purged)} 个 (0..94)")
    assert len(purged) == 95
    assert purged[-1] == 94

    # 2. Embargo
    print("\n[2] Embargo：测试集后添加隔离期")
    test_idx = np.arange(100, 150)
    embargoed = purging.embargo_indices(test_idx, embargo_pct=0.1, n_total=200)
    print(f"  测试集: {len(test_idx)} 个 (100..149)")
    print(f"  embargo_pct=0.1 → {len(embargoed)} 个索引")
    print(f"  Embargo 索引: {embargoed.tolist()}")
    # start = test_end + 1 = 150, embargo_size = ceil(50 * 0.1) = 5
    # 实际返回 [150, 151, 152, 153, 154]
    assert len(embargoed) == 5
    assert int(embargoed[0]) == 150

    # 3. Leakage 检测
    print("\n[3] Leakage 检测")

    # 3a. 索引重叠
    train = np.array([0, 1, 2, 3, 4])
    test = np.array([3, 4, 5, 6])
    has, pairs = purging.detect_leakage(train, test, feature_lag=0)
    print(f"  3a. 索引重叠: has_leakage={has}, pairs={pairs}")
    assert has

    # 3b. 无重叠无 lag
    train = np.arange(0, 100)
    test = np.arange(100, 150)
    has, pairs = purging.detect_leakage(train, test, feature_lag=0)
    print(f"  3b. 无重叠无 lag: has_leakage={has}")
    assert not has

    # 3c. 无重叠但 lag 触发泄漏
    train = np.arange(0, 100)
    test = np.arange(102, 110)  # 102 - 99 = 3 <= feature_lag=5
    has, pairs = purging.detect_leakage(train, test, feature_lag=5)
    print(f"  3c. lag 触发: has_leakage={has}, pairs={pairs}")
    assert has

    print("\n=== ALL PASS ===")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
