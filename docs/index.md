# AXON

> **面向量化交易与强化学习的高性能开源框架**
> Rust 核心 + Python 接口,从回测到生产的全链路统一框架

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/pengwow/axon_quant/blob/main/LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.96.0%2B-orange.svg)](https://github.com/pengwow/axon_quant)
[![CI](https://img.shields.io/badge/CI-validation-blue)](https://github.com/pengwow/axon_quant/actions)

## 核心特性

- **AI 原生** —— 内置 Gymnasium 兼容 RL 环境,直接挂 Stable-Baselines3 / Ray RLlib
- **Rust 高性能** —— 纳秒级时间戳、确定性撮合、零成本抽象
- **全链路统一** —— 回测、训练、优化、验证、追踪、注册共用一套数据结构
- **LLM 交易** —— 4 个核心 tool(下单/查持仓/撤单/改单)+ 4 种后端(Mock / Exchange / OMS / Backtest)+ 3 道风控(SafetyMode / RiskLimits / RiskGate)
- **模块化** —— 21 个 crate 独立可编译,feature flag 按需启用
- **100% 开源** —— Apache-2.0 许可,无企业版、无功能阉割

## 项目状态

| 阶段 | 内容 | 状态 |
|------|------|------|
| M1 | 回测引擎(L1/L2/L3 撮合 + 冲击 / 延迟 / 费用) | ✅ 完成 |
| M2 | RL 环境(Gymnasium + VecEnv + 6 示例) | ✅ 完成 |
| M3 | 训练管线(HPO + Walk-forward + Tracker + Registry + Distributed) | ✅ 完成 |
| M4 | 生产就绪(交易所 + 风控 + OMS + 监控) | 🟢 进行中 |
| M5 | AI 高级功能(LLM + 集成 + 可解释性) | ✅ 完成 |

## 快速跳转

- 📘 [快速开始](getting-started/installation.md) —— 5 分钟跑通第一个回测
- 🏗️ [架构总览](user-guide/architecture.md) —— 系统组件 + 数据流
- 🤖 [LLM 交易架构](user-guide/llm-trading/overview.md) —— 4 个 tool + 4 种后端 + 3 道风控
- 📚 [API 文档](reference/api.md) —— 完整 Rust API(链接 docs.rs)
- 🛠️ [CLI 命令](reference/cli.md) —— axon-cli 用法
- 📋 [架构决策](adr/index.md) —— 历史 ADR

## 社区

- [GitHub 仓库](https://github.com/pengwow/axon_quant)
- [Issue 跟踪](https://github.com/pengwow/axon_quant/issues)
- [更新日志](about/changelog.md)

## 许可证

Apache-2.0,详见 [LICENSE](https://github.com/pengwow/axon_quant/blob/main/LICENSE)。
