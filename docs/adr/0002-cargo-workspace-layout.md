# ADR-0002: Cargo Workspace 布局与命名

- 状态：已接受
- 日期：2026-06-10
- 决策者：AXON 架构组

## 背景

AXON 由多个功能模块组成（核心、回测、RL、交易所、风控等）。
需要在 Cargo workspace 中组织 crate，定义依赖方向与命名规范。

## 决策

### 目录布局

所有 crate 位于 `crates/` 子目录下，统一命名为 `axon-<功能>`：

```
axon/
├── Cargo.toml              # workspace 根
├── crates/
│   ├── axon-core/          # 共享类型（无 axon-* 依赖）
│   ├── axon-backtest/      # 依赖 axon-core
│   ├── axon-cli/           # 依赖 axon-backtest，binary 入口
│   └── ...
```

### 依赖方向规则

1. `axon-core` **不依赖**任何其他 axon-* crate
2. 业务逻辑 crate 依赖数据层
3. 接口层 crate（`axon-pyo3`、`axon-cli`）依赖业务逻辑
4. **禁止**循环依赖与跨层反向依赖

### 公共依赖管理

- 所有共享依赖在 workspace 根 `[workspace.dependencies]` 声明
- 子 crate 通过 `{ workspace = true }` 引用，确保版本统一
- 子 crate 自己的 `Cargo.toml` 仅声明包元信息与独有依赖

### Feature 组合

- workspace 根不定义 `[features]`
- Feature 组合在 `axon-cli`（顶层 binary crate）声明
- Phase 0 阶段仅暴露 `backtest` / `metrics` 等最小 feature

## 后果

### 正面

- 所有 crate 共享同一依赖版本，避免冲突
- 依赖关系单向且清晰，编译时易于排查
- Feature 组合集中在 CLI 层，使用方一目了然

### 负面

- 大量 crate 共享同一 `Cargo.lock`，锁文件较大
- 新增 crate 时需手动更新 `members` 列表（可借助 `cargo workspace` 工具）

## 参考

- AXON 项目总览见[首页](../index.md)
- 系统架构见[架构总览](../user-guide/architecture.md) — Feature 详细定义
