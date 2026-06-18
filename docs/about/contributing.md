# 贡献指南

欢迎参与 AXON 项目的开发!本文档描述贡献流程和约定。

## 行为准则

- 友善、包容、专业的沟通
- 优先使用 issue 讨论设计,避免直接提大 PR
- 代码质量优先,不接受"先合并再优化"的 PR

## 提交流程

1. **Issue 先行**:任何非平凡改动先开 issue 讨论设计
2. **Fork & Branch**:从 `main` 拉 feature branch(`feat/xxx` / `fix/xxx` / `docs/xxx`)
3. **TDD**:新功能先写测试(参见 [测试计划](testing-plan.md))
4. **Lint & Test**:本地跑 `make verify`,全部通过
5. **PR 描述**:关联 issue + 改动说明 + 测试截图 / benchmark 结果
6. **Code Review**:至少 1 名维护者 approve 才合并
7. **Squash Merge**:使用 squash merge 保持 main 干净

## 开发约定

### 提交信息格式

```text
<type>(<scope>): <subject>

<body>

<footer>
```

**type**:`feat` / `fix` / `docs` / `test` / `refactor` / `perf` / `chore`

**scope**:crate 名(`axon-llm` / `axon-backtest` / `axon-cli` / `workspace` / `docs`)

**subject**:50 字以内,中文或英文,小写开头,无句号

**body**:72 字换行,说明 **为什么** 改,而不是 **改了什么**

**footer**:`BREAKING CHANGE:` / `Closes #123` / `Refs #456`

### 代码风格

- **Rust**:`cargo fmt --all` + `cargo clippy --workspace --all-targets -- -D warnings`
- **Python**:`ruff check` + `ruff format`(`axon_quant/__init__.py` 等)
- **Markdown**:`markdownlint docs/`
- 注释率 ≥ 50%(复杂逻辑必须注释)

### 测试要求

- **新功能** 必须有单元测试 + 至少 1 个集成测试
- **Bug 修复** 必须先写复现测试,确认测试 fail,再修复,确认测试 pass
- **Breaking change** 必须有 contract test(见 [测试计划](testing-plan.md))
- Property-based 测试用 `proptest`,覆盖核心 impact / engine 模块
- 性能敏感代码有 benchmark + 内存影响分析

## Crate 责任分工

| Crate | 责任 |
|-------|------|
| `axon-core` | 基础类型 / 时间 / 市场数据 / 订单 / 事件 / 队列 / portfolio |
| `axon-backtest` | L1/L2/L3 撮合 |
| `axon-rl` | Gymnasium env + VecEnv + PyO3 |
| `axon-llm` | LLM agent + 交易 tool |
| `axon-hpo` | Optuna 集成 + NSGA-II |
| `axon-walk-forward` | Walk-forward 验证 |
| `axon-distributed` | Ray actor + 参数服务器 |
| `axon-tracker` | MLflow / WandB 追踪后端 |
| `axon-registry` | 模型注册表 + SemVer |
| `axon-data` | Arrow IPC + mmap cache |
| `axon-cli` | CLI 入口 |

## 文档贡献

- **API 改动**:同步更新 `docs/reference/api.md` 和 Rust doc comments
- **新 feature**:在 `docs/user-guide/` 对应模块下补文档
- **重大决策**:写 ADR(`docs/adr/0000-xxx.md`)
- **CHANGELOG**:每次 commit 在 `[Unreleased]` 段加条目

## 发布流程

1. 维护者 bump version(`Cargo.toml` workspace + 各 crate)
2. 更新 `CHANGELOG.md` 把 `[Unreleased]` → `[X.Y.Z] - YYYY-MM-DD`
3. 打 tag:`git tag -a vX.Y.Z -m "Release vX.Y.Z"`
4. CI 自动 build + publish(需配置 `CARGO_REGISTRY_TOKEN` / `PYPI_API_TOKEN` secret)
5. 推 GitHub Release,附 CHANGELOG 摘录

## 联系方式

- GitHub Issues:https://github.com/pengwow/axon_quant/issues
- 内部 Slack / Discord(见 GitHub README)
- Email:见 GitHub profile

## 许可证

贡献的代码默认采用 Apache-2.0 许可(详见 [许可证](license.md))。
