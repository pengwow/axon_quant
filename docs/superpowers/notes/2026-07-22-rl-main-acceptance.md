# 2026-07-22 0.9.0 Main Acceptance Note

## Acceptance Results

_TBD after running 100K timesteps + HPO sweep + ONNX e2e_

| Metric | Target | Actual | Pass |
|--------|--------|--------|------|
| Training convergence | Sharpe > 1.0 | TBD | TBD |
| HPO gain | +20% | TBD | TBD |
| ONNX e2e PnL error | < 5% | TBD | TBD |
| HPO wall time | <= 3h | TBD | TBD |

## Implementation Status

All 19 plan tasks completed on 0.9.0 branch:
- T1-T4: D1.1 / D1.2 — BacktestEnv + MultiLegBacktestEnv + with_seed builder
- T5-T7: C2.1 — L3BookDiff + subscribe/unsubscribe + PyO3 binding
- T8: C3.1 — BaseStrategy ABC
- T9-T10: D1.3 — CartPole smoke + spot single-leg PPO 50K demo
- T11-T14: D1.4 — export_onnx + MultiLegAction + OnnxPolicyStrategy + e2e
- T15-T17: D1.5 — RLHPOSweeper + parallel HPO + 100 trial wall-time gate
- T18-T19: D1.6 — spot+perp arb demo + user guides + CHANGELOG

Branch: `0.9.0` (cut from 0.8.0 at commit 48e4a1a)
