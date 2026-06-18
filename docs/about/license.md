# 许可证

AXON 项目采用 **Apache License 2.0** 单一许可证。

## Apache License 2.0 完整文本

详见仓库根目录的 [`LICENSE`](https://github.com/pengwow/axon_quant/blob/main/LICENSE) 文件,或参考官方文本:

<https://www.apache.org/licenses/LICENSE-2.0>

## 关键条款摘要

- ✅ **商业使用**:允许
- ✅ **修改**:允许
- ✅ **分发**:允许
- ✅ **专利使用**:允许
- ✅ **私用**:允许
- ⚠️ **署名要求**:必须保留版权声明和许可证文本
- ⚠️ **变更声明**:修改文件必须声明
- ⚠️ **商标**:本协议不授予商标使用权
- ❌ **责任**:作者 / 贡献者**不**对使用本项目产生的任何损失负责
- ❌ **担保**:**无**任何形式的担保

## 第三方依赖许可证

AXON 依赖的所有 crate 列表见 `Cargo.lock`,各依赖的许可证详见:

- 直接依赖:`cargo metadata --format-version 1 | jq '.packages[] | {name, license}'`
- 完整传递依赖:`cargo tree --edges normal | head -100`

主要依赖许可证:

| 依赖 | 许可证 |
|------|--------|
| tokio | MIT |
| crossbeam | MIT / Apache-2.0 |
| arrow / parquet | Apache-2.0 |
| pyo3 | Apache-2.0 / MIT |
| thiserror / anyhow | MIT / Apache-2.0 |
| serde | MIT / Apache-2.0 |
| nalgebra | Apache-2.0 |
| tracing | MIT |
| candle / tch | MIT / Apache-2.0 |
| prometheus | Apache-2.0 |
| reqwest | MIT / Apache-2.0 |

## 历史变更

- 早期(2026-06-11 之前):考虑过 MIT / Apache-2.0 双许可
- 当前(2026-06-11 之后):改为 Apache-2.0 单一许可,详见 [ADR 0003](../adr/0003-license-dual-mit-apache.md)

## 贡献者协议

提交 PR 即视为同意以 Apache-2.0 许可贡献你的代码。
详见 [贡献指南](contributing.md)。
