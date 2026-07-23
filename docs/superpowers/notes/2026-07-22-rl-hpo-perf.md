# 2026-07-22 RL HPO Perf Note

**Test:** `tests/bench/bench_hpo_wall_time.py` — 100 trial 8-CPU HPO sweep

**Hardware:** _TBD on actual run_(8-CPU 16GB RAM 预期)

**Result:** _TBD on actual run_

**Acceptance:** <= 3h (10800s)

**How to run:**
```bash
uv run pytest tests/bench/bench_hpo_wall_time.py -v --slow
```

**Note:** 默认 CI 跳过 `@pytest.mark.slow` 测试,只在手动 perf 验证时跑。
完整 100 trial × 50K PPO 训练约 1-1.5h(8-CPU 并发),在 3h gate 内有余量。
