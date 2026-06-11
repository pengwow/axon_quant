"""AXON 超参数优化 Python 辅助库。

设计原则：
- **可选依赖**：`optuna` 与 `numpy` 都设为可选，模块导入时不强制要求；
  调用对应功能时再 `import`。
- **零外部数据依赖**：库本身不依赖任何数据源，只负责 HPO 流程编排。
- **多目标支持**：原生支持 Optuna 的多目标 API（`directions`）与 Pareto 前沿分析。
- **可序列化**：所有结果结构可转为 JSON / Parquet，方便后续分析。

模块组织：
- `types`：搜索空间 / 剪枝器 / Trial 结果的数据类
- `optuna_runner`：Optuna study 主循环封装
- `search_space`：常见 RL 搜索空间预设
- `pruning`：自定义剪枝策略
- `multi_objective`：Pareto 前沿与超体积计算
"""

from __future__ import annotations

__version__ = "0.0.1"

__all__ = ["__version__"]
