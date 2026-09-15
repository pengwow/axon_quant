# OptunaHPO 剪枝链路修复 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 OptunaHPO 的 report(step, value) 真正调 optuna trial.report + should_prune，使 Median/Hyperband/SuccessiveHalving 剪枝生效。

**Architecture:** 把 report 从实例方法改为 `_objective` 内部绑定当前 trial 的闭包，双参 objective_fn（params, report）直接调用闭包触发剪枝；删除自建 `_intermediate` 缓存，结果从 optuna trial.intermediate_values 读。RLHPOSweeper 串行/并行路径同步改造。

**Tech Stack:** Python 3.14, optuna, numpy

**Spec:** docs/superpowers/specs/2026-09-15-hpo-report-pruning-design.md

---

### Task 1: 修复 OptunaHPO 核心（optuna_runner.py）

**Files:**
- Modify: `python/axon_hpo/optuna_runner.py`

- [ ] **Step 1: 删除 `self._intermediate` 初始化（__init__ L83-84）**
- [ ] **Step 2: 删除旧 report 方法（L115-121）**
- [ ] **Step 3: 改 _objective 签名为双参 objective_fn(params, report) 并注入闭包**
- [ ] **Step 4: run() 和 collect_results() 的 intermediate_values 从 optuna trial.intermediate_values 读**

### Task 2: 修复 RLHPOSweeper（hpo_sweeper.py）

**Files:**
- Modify: `python/axon_quant/training/hpo_sweeper.py`

- [ ] **Step 1: __init__ 加 pruner 参数并透传到 _sweep_serial**
- [ ] **Step 2: _sweep_parallel 补传 pruner=pruner.build() + objective_factory 注入 report 闭包**

### Task 3: 版本号 + CHANGELOG

**Files:**
- Modify: `pyproject.toml`, `Cargo.toml`, `CHANGELOG.md`

- [ ] **Step 1: 版本 0.14.4 → 0.14.5（三处同步）**
- [ ] **Step 2: CHANGELOG 新增 0.14.5 条目，标注 Breaking Change**

### Task 4: 测试（重写 + 新增）

**Files:**
- Modify: `tests/test_axon_hpo.py`
- Create/Modify: `tests/test_hpo_sweeper.py`（如不存在）

- [ ] **Step 1: 安装依赖（若缺失）**
- [ ] **Step 2: 重写 test_report_intermediate → test_pruning_effective，断言 PRUNED state**
- [ ] **Step 3: 改所有单参 objective 为双参（含现有单目标/多目标/collect_results 测试）**
- [ ] **Step 4: 跑 pytest 通过**

### Task 5: 文档同步 + fmt + 提交

**Files:**
- Modify: 6 个 docs/*.md

- [ ] **Step 1: 文档中 objective_fn 示例改双参 + report**
- [ ] **Step 2: cargo fmt --all -- --check**
- [ ] **Step 3: git commit + git push origin 0.14.5**
- [ ] **Step 4: 开 PR（base 0.14.4）**
