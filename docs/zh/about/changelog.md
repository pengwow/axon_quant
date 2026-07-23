# 更新日志

AXON 的完整更新日志见仓库根目录的 [`CHANGELOG.md`](https://github.com/pengwow/axon_quant/blob/main/CHANGELOG.md)。

## 当前版本

- **最新稳定版:** [`v0.8.0`](https://github.com/pengwow/axon_quant/blob/main/CHANGELOG.md#080---2026-07-22) — L3 多资产对账 / EngineRouter / OrderArena / SoA 价位簿 / 性能 gate
- **开发中:** [`v0.9.0`](https://github.com/pengwow/axon_quant/blob/0.9.0/CHANGELOG.md)(分支 `0.9.0`) — RL/HPO 训练生产化(`BacktestEnv` / `MultiLegBacktestEnv` / `OnnxPolicyStrategy` / `RLHPOSweeper` / `L3BookDiff`)

## 历史版本

按 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 格式组织,所有 notable changes 在 CHANGELOG.md 中详细记录:

- **Added**:新功能
- **Changed**:现有功能的变更
- **Deprecated**:即将移除的功能
- **Removed**:已移除的功能
- **Fixed**:Bug 修复
- **Security**:安全相关修复

## 升级指南

每次 major 版本升级,CHANGELOG.md 的 **BREAKING CHANGES** 段会列出所有破坏性变更和迁移步骤。应用方应:

1. 仔细阅读 BREAKING CHANGES
2. 在 staging 环境跑 `cargo test --workspace` + LLM 集成 E2E 测试
3. 按 CHANGELOG 中的迁移步骤调整代码 / 配置
4. 灰度上线,观察 metrics

## 贡献

每次 commit 应在 `CHANGELOG.md` 的 `[Unreleased]` 段加条目(参见 [贡献指南](contributing.md))。
