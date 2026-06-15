"""walk_forward_purging.py — Purge / Embargo / Leakage 检测示例。

使用 `axon_quant.walk_forward` 模块。

运行方式：
    cd axon
    .venv/bin/python examples/08_walk_forward/walk_forward_purging.py
"""

from __future__ import annotations

import axon_quant  # noqa: E402
py_detect_leakage = axon_quant.walk_forward.py_detect_leakage
py_embargo_indices = axon_quant.walk_forward.py_embargo_indices
py_purge_overlapping_labels = axon_quant.walk_forward.py_purge_overlapping_labels


def main() -> int:
    print("=" * 60)
    print("Purge / Embargo / Leakage 示例")
    print("=" * 60)

    # 1. Purge
    print("\n[1] Purge：移除与测试集标签重叠的训练样本")
    train_idx = list(range(0, 100))
    test_idx = list(range(100, 150))
    purged = py_purge_overlapping_labels(train_idx, test_idx, 5)
    print(f"  原始训练集: {len(train_idx)} 个 (0..99)")
    print(f"  测试集起始: {test_idx[0]}")
    print(f"  horizon=5 → 移除索引 >= {test_idx[0] - 5} = 95")
    print(f"  Purge 后:   {len(purged)} 个 (0..94)")
    assert len(purged) == 95

    # 2. Embargo
    print("\n[2] Embargo：测试集后添加隔离期")
    test_idx = list(range(100, 150))
    embargoed = py_embargo_indices(test_idx, 0.1, 200)
    print(f"  测试集: {len(test_idx)} 个 (100..149)")
    print(f"  embargo_pct=0.1 → {len(embargoed)} 个索引")
    print(f"  Embargo 索引: {embargoed}")
    assert len(embargoed) == 5
    assert embargoed[0] == 150

    # 3. Leakage 检测
    print("\n[3] Leakage 检测")

    # 3a. 索引重叠
    train = list(range(5))
    test = [3, 4, 5, 6]
    has, pairs = py_detect_leakage(train, test, 0)
    print(f"  3a. 索引重叠: has_leakage={has}, pairs={len(pairs)}")
    assert has

    # 3b. 无重叠无 lag
    train = list(range(100))
    test = list(range(100, 150))
    has, pairs = py_detect_leakage(train, test, 0)
    print(f"  3b. 无重叠无 lag: has_leakage={has}")
    assert not has

    # 3c. 无重叠但 lag 触发泄漏
    train = list(range(100))
    test = list(range(102, 110))
    has, pairs = py_detect_leakage(train, test, 5)
    print(f"  3c. lag 触发: has_leakage={has}, pairs={len(pairs)}")
    assert has

    print("\n=== ALL PASS ===")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
