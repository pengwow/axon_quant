# Fix: OptunaHPO 剪枝链路修复

- **版本**: 0.14.5
- **分支**: `fix/hpo-report-pruning`（基于 `0.14.4`）
- **作者**: pengwow
- **状态**: 已设计，待实施

## 问题陈述

`python/axon_hpo/optuna_runner.py` 的 `OptunaHPO.report(trial_id, step, value)` 方法只是把中间值缓存进 `self._intermediate` dict，从未调用 optuna 原生 `trial.report()` / `trial.should_prune()`，导致：

1. **Median / Hyperband / SuccessiveHalving 剪枝实际不生效** — 每个 trial 完整跑完，Optuna pruner 的中间值存储永远为空
2. **用户无法在 objective 内部拿到当前 trial** — `objective_fn(params)` 单参签名，用户想剪枝必须自己维护 trial_id（测试里甚至写死 `trial_number = 0`）
3. **RLHPOSweeper 并行路径也不生效** — `_sweep_parallel` 创建 study 时根本没传 pruner

测试 `test_report_intermediate` 形同虚设：只断言 trial 数量，从未验证中间值是否真被 Optuna 接收。

## 修复目标

- 用户在 objective 中能报告中间值并让 Optuna pruner 真正触发剪枝
- 剪枝后的 trial state 为 `pruned`，中间值保留在结果里
- 串行 / 并行 / RLHPOSweeper 三条路径剪枝语义一致

## 设计

### 1. `objective_fn` 签名改为双参（Breaking Change）

```python
def objective_fn(
    params: dict[str, Any],
    report: Callable[[int, float], None],
) -> list[float]:
    for step in range(n_epochs):
        value = train_one_epoch(params, step)
        report(step, value)   # 调 optuna trial.report + should_prune，命中直接抛 TrialPruned
    return [final_value]
```

**不做旧单参签名兼容探测**。理由：
- `inspect.signature` 对 `functools.partial`、装饰器、`*args` 会误判
- 单参 objective 本来就无法报告中间值，剪枝必然不生效 — "兼容"等于保留 bug
- 下游 QuantCell 当前未用 OptunaHPO，实际破坏面为零
- CHANGELOG 显式标注 Breaking Change 指引迁移

### 2. `report(step, value)` 闭包 + 直接抛 TrialPruned

在 `OptunaHPO._objective` 内部构造闭包：

```python
def report(step: int, value: float) -> None:
    trial.report(value, step)               # optuna 参数序: value 在前
    if trial.should_prune():
        raise optuna.TrialPruned(f"pruned at step {step}")
```

设计决策：
- **闭包**而非实例方法 — 天然绑定当前 trial，无 race condition，`n_jobs > 1` 多进程安全
- **直接抛 TrialPruned** 而非返回 bool — 符合 Optuna 官方惯例，用户代码最少；当前 `_objective` 已有 `except TrialPruned: raise` 透传
- **删除旧 `report(trial_id, step, value)` 方法** 及其唯一调用场景

### 3. 中间值存储从自建缓存迁移到 optuna

删除 `self._intermediate: dict[int, list[tuple[int, float]]]`。optuna trial 自带 `intermediate_values`（`dict[int, float]`），结果收集时读它：

```python
intermediate = [
    (step, trial.intermediate_values[step])
    for step in sorted(trial.intermediate_values)
]
```

消除一份重复状态源，避免 `self._intermediate` 和 optuna 内部存储分叉。

### 4. RLHPOSweeper 一并修复

- `RLHPOSweeper.__init__` 新增 `pruner: PrunerConfig | None = None`
- `_sweep_serial`：透传 pruner 给 OptunaHPO
- `_sweep_parallel`：
  - `create_study` 补传 `pruner=pruner.build()`（修复目前缺失）
  - objective factory 注入同签名 `report` 闭包
- `sweep(objective_fn)` 全链路双参透传

### 5. 剪枝边界行为

| 场景 | 行为 |
|------|------|
| 单目标 + MedianPruner | `report(step, value)` 用 `should_prune()` 判断主目标方向，optuna 内部处理 minimize/maximize 适配 |
| 多目标 + MedianPruner | MedianPruner 会把 `trial.values[0]` 当参考目标方向（optuna 默认），用户在多目标场景应报告"主目标"的中间值；Hyperband/SuccessiveHalving 同理 |
| 用户 objective 内裸 `except Exception` | 会吞掉 TrialPruned 信号 — 文档明确提醒"勿捕获裸 Exception，或至少放行 TrialPruned" |
| 用户不调用 report | 剪枝器没有中间值可用，optuna 会在 `pruner.n_warmup_steps` 后静默放弃剪枝（optuna 默认行为）；trial 完整跑完，等价于无剪枝 |

### 6. 不在本次范围

- **不新增自定义 pruner** — `pruning.py` 的 `adaptive_median_prune` 保持原样，Optuna 内置剪枝器够用
- **不改 Rust 侧 `py_compute_*`** — 数值工具链路与剪枝无关
- **不处理 `axon_distributed` cdylib 残留** — 独立技术债，下轮清理
- **不处理 sdist 中 crates/*/python 副本** — 已确认为相同副本，wheel 只打根 `python/`，不影响功能

## 测试策略

### 新增 / 重写

| 测试 | 内容 |
|------|------|
| `TestOptunaHPO.test_pruning_effective` | 确定性剪枝 E2E：MedianPruner + `n_startup_trials=2`，前 2 个 trial 报告高值，后续报告递减低值 → **断言存在 `state == "pruned"` 的 trial**（验收核心） |
| `TestOptunaHPO.test_pruned_trial_intermediate` | 剪枝被触发的 trial，断言 `TrialResult.intermediate_values` 非空但 `values == []` |
| `TestOptunaHPO.test_no_report_no_prune` | objective 不调用 report → 所有 trial `state == "complete"` |
| `TestOptunaHPO.test_inject_report_signature` | 双参签名 objective 能被正确调用、report 闭包可用 |
| `TestRLHPOSweeper.test_parallel_pruner` | n_jobs=2 + sqlite + PrunerConfig，冒烟验证并行路径 pruner 生效 |

### 保留不变

- 现有 `test_single_objective` / `test_multi_objective` / `test_collect_results` — objective 改造为双参 report 签名，其余断言不变
- 现有测试文件里不涉及 report 的用例原样保留

### 删除

- 旧 `test_report_intermediate` — 形同虚设（写死 trial_number = 0，只断言 trial 数量），完全重写为 `test_pruning_effective`

## 文档变更

- `docs/en/user-guide/strategy-development.md` — objective_fn 示例改双参 + report
- `docs/zh/user-guide/strategy-development.md` — 同上（中文）
- `docs/en/user-guide/modules.md` — Python 主用法示例改双参
- `docs/zh/user-guide/modules.md` — 同上（中文）
- `docs/en/reference/python-bindings.md` — OptunaHPO API 签名更新 + report 用法
- `docs/en/user-guide/llm-trading/oader.md` — objective_fn 示例更新
- `docs/zh/user-guide/llm-trading/oader.md` — 同上（中文）

所有文档同步注明：
- Breaking Change: objective_fn 从单参改为双参
- 多目标场景 report 报告单个主目标的中间值
- objective 内勿裸 except Exception，或至少放行 TrialPruned

## 变更文件清单

| 文件 | 类型 | 变更 |
|------|------|------|
| `python/axon_hpo/optuna_runner.py` | 改 | 核心修复：report 闭包 + 剪枝 + 删自建缓存 + 旧方法移除 |
| `python/axon_quant/training/hpo_sweeper.py` | 改 | pruner 透传 + 并行路径补传 pruner + 双参签名 |
| `tests/test_axon_hpo.py` | 改 | 重写 report 测试 + 新增剪枝 E2E 断言 |
| `tests/test_hpo_sweeper.py` | 改 | 并行路径 pruner 冒烟 |
| `pyproject.toml` | 改 | 版本 0.14.4 → 0.14.5 |
| `Cargo.toml` | 改 | workspace.package 版本 0.14.4 → 0.14.5 |
| `CHANGELOG.md` | 改 | 0.14.5 条目 + Breaking Change 标注 |
| 6 个文档 md | 改 | objective_fn 签名更新 + report 用法示例 |

## 实施计划（概览，详细由 writing-plans 生成）

1. 新分支 `fix/hpo-report-pruning` 基于 `0.14.4`，版本号 bump 到 0.14.5
2. 改 `optuna_runner.py`：删 `_intermediate` 字典、改 `_objective` 注入 report 闭包、从 trial 读 intermediate_values
3. 改 `hpo_sweeper.py`：pruner 参数贯穿 + 并行路径 create_study 补 pruner
4. 重写 / 新增测试，先跑 `pytest tests/test_axon_hpo.py -v` 剪枝 E2E 看到 PRUNED state 才通过
5. `cargo fmt --all -- --check` + `cargo check --workspace` 回归（Rust 侧无改动，理论必过）
6. 更新文档
7. 提交 → 推送 → 开 PR（base `0.14.4`）
