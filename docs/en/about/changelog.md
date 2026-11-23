# Changelog

AXON's complete changelog is maintained in the repository root's [`CHANGELOG.md`](https://github.com/pengwow/axon_quant/blob/main/CHANGELOG.md).

## Current Version

- **Latest stable:** [`v0.25.0`](https://github.com/pengwow/axon_quant/blob/main/CHANGELOG.md#0250---2026-11-10) — OMS naming semantics unified (BREAKING) + RL kernel-backtest/evaluation facilities moved down + registry `unpromote` + MSRV raised to 1.100.0
- **Previous:** [`v0.24.1`](https://github.com/pengwow/axon_quant/blob/main/CHANGELOG.md#0241---2026-11-04) — Cascade decisions retain raw S1/S2 outputs (`s1_raw`/`s2_raw`/`s2_thinking`)
- **Earlier:** [`v0.24.0`](https://github.com/pengwow/axon_quant/blob/main/CHANGELOG.md#0240---2026-11-03) — Kernel progress callback + `axon-llm` nested-runtime fix
- **Earlier:** [`v0.23.0`](https://github.com/pengwow/axon_quant/blob/main/CHANGELOG.md#0230---2026-10-30) — Runtime risk hot-reload + live position adoption + resume reconciliation false-positive fixes

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
