# Engine Accuracy Benchmark Analysis

> Date: 2026-11-25
> Baseline: axon_quant 0.25.0
> Purpose: benchmark AXON's core trading-engine accuracy discipline against established engineering practice from mature, production-proven engines; produce a gap matrix, borrowable mechanisms, and a phased roadmap.
> Note: the reference is described here as the "benchmark engine" — a long-running, live-verified open-source Rust quant engine. This document focuses on mechanisms, not on naming external projects.

---

## 1. AXON Accuracy Assurance: Current State

### 1.1 Data validation

| Mechanism | Detail | Evidence | Status |
|---|---|---|---|
| Schema validation | Parquet read/write column & type checks | `axon-data/src/sources/parquet.rs:65,119,203` | ✅ |
| OHLC validity | high≥low, open/close in range | `axon-core/src/market/bar.rs:109-127` | ⚠️ `debug_assert!` only (`bar.rs:97-103`) — **inactive in release** |
| Gap detection | per (instrument, interval) bar-start jump → Gap → REST backfill (max 1000) | `axon-worker/src/pump.rs:15,46-58` | ✅ real-time pump only |
| Dedup/ordering | aggregator admits one bar per (instrument, interval, ts) | `axon-kernel/src/aggregator.rs:19-27` | ⚠️ 16-bar bucket window; documented miss on late duplicates; aggregator **assumes sorted input** |
| Historical data cleansing | — | — | ❌ none; bad data can reach matching |

**Thinnest layer of the six.** Schema checks block nothing numeric; no ingest quality gate; the only behavioral guard is disabled in release.

### 1.2 Order lifecycle

7-state machine (`types.rs:65-78`) with an explicit transition matrix; illegal transitions return `OmsError` (never silent), incl. 2 best-effort reverse edges for failed fill application. Idempotency keys on submit reused across live retries (cap `PLACE_ATTEMPTS=2`, `live_sink.rs:55`); venue order ids stored in `order.meta`. Over-fill has dual semantics (`manager.rs:187-233`): live clamps to remaining; replay expands to mirror the venue — annotated as the fix for a prior reconciliation incident. Gap: the manager itself does not reconcile (that lives in `axon-worker/reconcile.rs`); no systematic per-edge test coverage matrix.

### 1.3 Matching consistency

L1 price-time priority (IOC/FOK/limit/market) with L2/L3 enhancement layers; `PartialFillTracker` chains fills; synthetic seeded book. **No latency model** — zero hits for `latency|delay` on the core path, fully synchronous fills, no cancels at M1 (`sink.rs:248-252`). Invariant protection: proptest in 14 crates, 35 `proptest!` blocks (`axon-integration-tests/src/fuzz.rs:8-596`).

### 1.4 Backtest/live alignment

Single-loop trait swap: `Kernel` holds `Box<dyn ExecutionSink>`; BacktestSink / PaperSink / LiveSink assemble the same kernel; paper driver is documented as the twins of the live driver. Determinism: BinaryHeap + global seq FIFO, zero RNG on the core loop, RunManifest fingerprint rejected field-by-field on mismatch (`kernel_loop.rs:359-367`). Gaps: (i) consistency is code-level, **not market-kinetics-level** (latency/queueing/resting orders unmodeled); (ii) paper↔backtest NAV drift tolerance uncalibrated (`axon-kernel/src/lib.rs:63`); (iii) testnet report reconciliation unverified (`lib.rs:62`).

### 1.5 Fault tolerance (strongest layer)

Journal replay recovery: append-only JSONL of external inputs, flush-before-progress, partial-line tolerance, `resume_replay` + mandatory three-way reconciliation. Reconcile gate (`reconcile.rs:295-325`): position/cash break or ghost orders → `Err`, refuses auto-resume. Fail-fast startup; daily-loss kill switch (no new risk, allows exits); risk CircuitBreaker with cooldown; venue-layer rate/circuit limits; WS exponential backoff. Panics: 3976 unwrap/expect overall, ~56% at Python binding edges; core production path minor (lock poisoning), backtest L3 engine not yet audited.

### 1.6 Audit & traceability

journal evensourcing (external inputs only, derived events reproducible; `kernel_loop.rs:1308` byte-equal replay tests) + SHA-256 hash-chain compliance audit (`axon-compliance/src/audit/log.rs:1-67`) — but the compliance chain is **not on the trading path** (mounted via axon-python only). Strategy decisions are not persisted (tracing only, 116 sites); replayable in principle — `lib.rs:66` admits RL observation statistics lack file-level evidence. Artifacts: `*.reconcile_last_ok.json`, shadow-oracle aggregate diffs, RunManifest fingerprints.

### 1.7 Maturity baseline

32 crates / 546 commits / 5.5 months (2026-06→11) / 3738 inline unit + 764 integration + 300 Python e2e tests / 11 blocking CI jobs / 39 changelog releases.

---

## 2. Established Practice from Mature Engines

1. **Isomorphic architecture**: kernel assembles a single execution engine; backtest, sandbox and real venue clients implement one client trait — matching is literally shared. A sandbox adapter wires the backtest matching engine into a live node for paper trading against live data. Venue behaviors (funding, rollover) enter via pluggable simulation modules.
2. **Test pyramid + discipline machine-enforced**: bare `#[test]` banned by pre-commit in favor of table-driven parameterized macros; tens of thousands of parameterized tests; memory-leak test suites; test-data supply chain (small data in-repo, large data on object storage + SHA-256 manifest + `prepare-test-data`).
3. **Enumerated boundary suites**: hundreds of named matching-edge cases — zero volume, indivisible remainders, empty-book replenishment waits (no crossing), FOK cancel consumption-level rollback, fee-model failure leaving no dirty state, settlement-currency mismatch rejection; portfolio prefers returning `None` to fabricating results.
4. **Property/formal placement**: proptest for numeric round-trips, fuzzing at trust boundaries (adapter parsing), miri on nightly; no loom/model-checking/Kani — the formal rung is *aspirational*, honestly documented; replaced by type invariants + pyramid + golden fingerprints + deterministic simulation (an 8-rung ladder documented as policy: "assert on returns/events/state, not log text").
5. **Determinism as a first-class contract**: one seed determines scheduling, timers and randomness; failing-seed replayable; discipline lint-ified via clippy disallowed-methods (raw OS RNG banned, scheduling primitives gated), biased `select!`, ordered containers wherever iteration order matters (each site recorded in docs); nightly seed sweeps attack concurrent schedules; probabilistic fill models seeded for reproducibility; replay merging via sort + k-way heap with documented tie-breaks.
6. **Golden fingerprints**: 4 canonical backtest workloads × precision tiers, full statistics JSON compiled into the test binary and compared per PR with float tolerances — any semantic drift breaks CI. Highest ROI guard against behavioral regressions.
7. **Sim/live convergence, honestly**: no automated backtest-vs-live differential; instead shared matching, a runtime-conformance document expressing each live invariant as "source commit + implementation site + named test", and an explicit principle: shared code does not imply identical outcomes.
8. **Incident traceback**: event sourcing captured at the message-bus layer before every publish/send (nothing bypasses the tap), append-only log with seq+hash covering commands, raw venue reports and correlations; verifier recomputes hashes on startup; snapshot-anchor + tail replay; cache is a projection, not the source of truth. Every fix in the changelog references an upstream issue and credits the reporter; regression tests carry a `Regression:` marker — an years-long incident→test→document closed loop.
9. **Process discipline**: ~a dozen workflows with a blocking PR chain (lint → full tests + test data → multi-platform builds → supply-chain audits), nightly layer for heavyweight checks (miri/fuzz/leaks), tight clippy complexity thresholds with committed allowance files, "bug fixes take precedence over features", one-workflow-at-a-time migrations with mapping docs, formal deprecation machinery for major versions.

---

## 3. Gap Matrix and Borrowable Items

| # | Dimension | AXON today | Mature practice | Gap | Action (by ROI) |
|---|---|---|---|:---:|---|
| 1 | Regression guard | journal replay equivalence only | canonical golden fingerprints per PR | 🔴 | build golden workload fingerprints — half a day, guards everything |
| 2 | Determinism discipline | manual (heap+seq+manifest); HashMap iteration-order crack (multi-instrument order-id/fill-order drift: described `axon-worker/src/shadow.rs:43-46`, root cause `axon-backtest/src/engine.rs:2519,2630,1449`), 16-bar dedup window misses | documented contract + clippy enforcement + nightly seed sweeps | 🔴 | machine-enforce deterministic ordering (sort-on-collect) + regression tests; DST sim deferred |
| 3 | Data quality gate | schema only; debug_assert OHLC; dirty data reaches matching | checked types + data manifests + enumerated boundary tests | 🔴 | ingest quality pipeline with release-grade OHLC checks, gap manifest into RunManifest |
| 4 | Matching fidelity | zero latency model, synchronous | seeded latency & fill models | 🟡 | port seeded Latency/Fill models (RNG only in BacktestSink; seed in manifest) |
| 5 | Backtest/live alignment evidence | code-sharing only; NAV drift uncalibrated (`lib.rs:63`) | shared sandbox matching + conformance documents | 🟡 | assemble the same L1 matcher in PaperSink; drift-calibration CI job |
| 6 | Audit on-path | compliance chain off main path; decisions not persisted | bus-level ordered capture + verifier + cache-as-projection | 🟡 | internalize audit capture into the kernel path |
| 7 | Boundary suites | proptest-dominated, few enumerated cases | hundreds of named boundary tests | 🟡 | table-driven transition-matrix coverage (≥1 test per edge) |
| 8 | Type invariants | f64 OHLC; good OMS matrix | fixed-point checked Price/Quantity | 🟡 | decimal fixed-point for order prices/accounting |
| 9 | Incident closed loop | disciplined changelog, no issue refs/named regressions | issue-linked fixes + `Regression:` tests | 🟢 | zero-cost convention, adopt now |
| 10 | fuzz/miri/deny | cargo-audit only | fuzz parsing surfaces, nightly miri, deny+vet | 🟢 | add to a non-blocking nightly layer |
| 11 | Test-data engineering | scattered fixtures | SHA-256 manifest + prepare command | 🟢 | once datasets grow |
| 12 | Formal verification | none | none (aspirational) | ⚪ | skip |

Do **not** copy: single-thread Rc/RefCell core design (the heap+seq loop suffices; refactors of a working core are unrecoverable), adapter-breadth expansion before kernel stabilization, model checking.

---

## 4. Phased Roadmap

### Phase 0 — stop bleeding, harden the floor (1–2 weeks, P0)

| Item | Action | Acceptance |
|---|---|---|
| O1 iteration-order crack | deterministic same-frame ordering (fix the collection loops at `axon-backtest/src/engine.rs:2519,2630,1449` described by `axon-worker/src/shadow.rs:43-46`, following the existing `margin.rs:288` pattern; then sweep remaining order-id allocation paths) | proptest: double-run same-workload byte-equal event stream + post-fix symbol-rename permutation equivalence; randomized test ordering in CI |
| O2 OHLC gate | replace `debug_assert!` with release-active `Result` validation at ingest/aggregation boundaries | dirty fixtures (high<low, out-of-order ts, duplicates, gaps) rejected in integration tests incl. release build |
| O3 dedup window | SEEN_PER_KEY=16 → watermark pruning or venue seq dedup; interim: missed duplicates emit warning counts | unit test: duplicate bar 17 periods late produces a duplicate/Gap event, not silence |
| O4 golden baseline | fixed dataset + full statistics fingerprint JSON (upgrade `assert_same_run` to a canonical workload matrix) with a fingerprint-update rationale flow | `cargo test -p axon-integration-tests canonical` breaks on any unacknowledged matching/aggregation change |
| O5 lint discipline | workspace lints: deny `todo!/unimplemented!` (test stubs exempt), no raw println on production paths; cognate complexity threshold ~20 | CI clippy passes and blocks future violations |
| P0 logic: all items convert known cracks into machine enforcement; highest hidden cost, lowest repair cost. |
| O6 incident closed loop | changelog fixes reference issue ids; regression tests commented `Regression:` | effective next release, documented in contributing |

### Phase 1 — fidelity & completeness (1–2 months, P1)

- **D1 data quality pipeline**: disorder detection, abnormal price/volume filtering, gap-list hash into RunManifest; SHA-256 test-data manifest + prepare command. Accept: dirty-data fixture matrix green.
- **D2 Latency/Fill models**: static three-stage latency + seeded probabilistic fill (seed lives only in BacktestSink, recorded in RunManifest). Accept: same-seed fill sequences reproducible.
- **D3 NAV drift calibration** (resolves `lib.rs:63`): shadow-baseline paper↔backtest comparison, drift report + tolerance band in CI; testnet reconciliation the same batch (`lib.rs:62`).
- **D4 sandbox alignment**: PaperSink assembles the backtest L1 matcher (live data injected), ending the twin implementation. Accept: differential test of matcher outputs on one replay.
- **D5 state-machine matrix**: table-driven coverage of every legal/illegal edge (partial→cancel, reject-then-late-ack, rollback under concurrent cancel, duplicate idempotency keys, over-fill semantics, four reconcile-difference paths).

### Phase 2 — observability, audit, evolution discipline (3–6 months, P2)

- **A1** on-path audit capture: run-scoped append-only store for commands/reports/risk events/decision signals + startup hash verifier (resolves `lib.rs:65`) — accept: any trading day rebuildable (NAV + order trajectories) from the event store alone.
- **A2** nightly non-blocking layer: miri (core/oms/risk), seeded scheduling perturbation sweeps, fuzzing adapter parsing surfaces.
- **A3** retire the three legacy paths (BacktestRunner/streaming/EventDrivenLoop) with a mapping document and schedule.
- **A4** runtime-conformance document: invariants like report-before-fill, rollback-on-apply-failure, reconcile-break-refuses-resume written as commit+site+test triples, CI-checked.
- **A5** incremental fixed-point: decimal across order prices/accounting (market data may stay f64); negative-quantity subtraction panics.

### Phase 3 — ecosystem & differentiation (6+ months, P3)

Multi-instrument live, cross-venue reconciliation, deprecation machinery, testnet capability-card acceptance, deterministic-simulation DST reassessment.

---

## 5. Evolution Direction

**Diagnosis**: AXON is in the "kernel convergence & trustability" phase — accuracy is already the stated priority (single-loop kernel, replay journal, proptest matrix), but the enforcement hooks (snapshots/golden/differential) of `testing-plan.md` are not yet landed. This is the same lesson the benchmark engine taught itself over years; the windows align.

**Three axes**: (1) a thin, hard kernel — golden fingerprints + machine-enforced determinism + market-kinetics models; (2) trustworthy data supply — quality gates and measurable backtest/live error bands; (3) differentiation in the intelligence stack — RL/HPO/walk-forward/LLM explainability plus train-vs-serve observation-statistics consistency (`lib.rs:65`), territory generic engines do not occupy.

**12-month endgame**: zero known determinism cracks; observable data-quality rejection; NAV drift alarmable; 100% state-machine edge coverage; day-replayable event store; nightly miri/fuzz; legacy gone; file-level evidence for intelligence-layer observation consistency.

**Risks**: every week of dual-path coexistence inflates differential-verification cost — freeze legacy features and schedule A3; a 546-commits-in-5.5-months cadence is regression-accumulation-prone — O4 golden fingerprints are this week's highest-ROI insurance; pinch-points: L3 engine and Python-binding panic surfaces before `unwrap_used`-style tightening.
