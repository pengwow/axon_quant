# 更新日志

AXON 的完整更新日志见仓库根目录的 [`CHANGELOG.md`](https://github.com/pengwow/axon_quant/blob/main/CHANGELOG.md)。

## 当前版本

- **最新稳定版:** [`v0.22.1`](https://github.com/pengwow/axon_quant/blob/main/CHANGELOG.md#0221---2026-10-26) — CI wheel 发布矩阵补齐(py3.13/3.14)与重试链修复
- **上一版:** [`v0.22.0`](https://github.com/pengwow/axon_quant/blob/main/CHANGELOG.md#0220---2026-10-26) — 统一交易内核(`axon-kernel`/`axon-worker`)+ testnet 合约实盘链 + 崩溃恢复
- **更早:** [`v0.21.0`](https://github.com/pengwow/axon_quant/blob/main/CHANGELOG.md#0210---2026-10-22) — 保证金交易引擎(逐仓)+ fill 关联 + 存款曲线

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
