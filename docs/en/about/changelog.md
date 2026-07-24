# Changelog

AXON's complete changelog is maintained in the repository root's [`CHANGELOG.md`](https://github.com/pengwow/axon_quant/blob/main/CHANGELOG.md).

## Current Version

- **Latest stable:** [`v0.9.0`](https://github.com/pengwow/axon_quant/blob/main/CHANGELOG.md#090---2026-07-23) — RL/HPO 训练生产化(`BacktestEnv` / `MultiLegBacktestEnv` / `OnnxPolicyStrategy` / `RLHPOSweeper` / `L3BookDiff` / `MultiLegAction`)
- **Previous:** [`v0.8.0`](https://github.com/pengwow/axon_quant/blob/main/CHANGELOG.md#080---2026-07-22) — L3 多资产对账 / EngineRouter / OrderArena / SoA 价位簿 / 性能 gate

## Versioning

AXON follows [Semantic Versioning](https://semver.org/):

- **MAJOR**: Incompatible API changes
- **MINOR**: Backwards-compatible functionality additions
- **PATCH**: Backwards-compatible bug fixes

## Release History

### v0.1.0 (2024-01-01)

Initial release.

**Features**:
- Core trading engine with L1/L2/L3 matching
- RL environment (Gymnasium compatible)
- LLM agent integration
- HPO with Optuna
- Walk-forward validation
- Model registry
- Exchange adapters (Binance, OKX)
- Risk control system
- Explainability (SHAP, counterfactual)

**Performance**:
- Backtesting throughput: > 1M events/sec
- Matching latency P99: < 1μs
- RL training: > 10K steps/sec

## Upgrade Guide

When upgrading between major versions:

1. Review CHANGELOG for breaking changes
2. Run migration tool: `axon migrate -c config.toml`
3. Update configuration files
4. Run tests: `cargo test --workspace`
5. Deploy to staging first
6. Monitor metrics for anomalies

## Contributing

Each commit should include an entry in `CHANGELOG.md` under the `[Unreleased]` section (see [Contributing Guide](contributing.md)).
