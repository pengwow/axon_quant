# AXON 全流程全场景测试计划

> 日期：2026-06-14
> 目标：验证所有模块功能真实有效，端到端闭环可用

## 一、测试分层架构

```
┌─────────────────────────────────────────────────────┐
│  L4: 端到端场景测试（Python + Rust 联动）            │
│      回测→训练→优化→验证→注册 全链路                 │
├─────────────────────────────────────────────────────┤
│  L3: 跨模块集成测试（Rust workspace）               │
│      crate 间 trait 对接、数据流转                   │
├─────────────────────────────────────────────────────┤
│  L2: 模块功能测试（单 crate）                        │
│      每个 crate 的核心 API 真实调用                   │
├─────────────────────────────────────────────────────┤
│  L1: 单元测试（函数级）                              │
│      已有 ~776 个，覆盖基本逻辑                       │
├─────────────────────────────────────────────────────┤
│  L0: 编译 + 静态检查                                 │
│      cargo check / clippy / fmt                     │
└─────────────────────────────────────────────────────┘
```

## 二、测试场景矩阵

### 场景 1：回测引擎全流程（axon-backtest + axon-core）

| 步骤 | 操作 | 验证点 |
|------|------|--------|
| 1.1 | 构造 OHLCV 数据（100 根 K 线） | 数据完整性 |
| 1.2 | 创建 BacktestEngine + 配置 | 引擎初始化成功 |
| 1.3 | 注册简单策略（均线交叉） | 策略注册成功 |
| 1.4 | 运行回测 | 无 panic、返回 RunResult |
| 1.5 | 验证结果字段 | total_trades > 0, pnl 有值 |
| 1.6 | 验证订单状态机 | New→Filled 完整流转 |
| 1.7 | 验证费用计算 | fee > 0 |
| 1.8 | 验证市场冲击 | 大单价格偏移 > 0 |

### 场景 2：RL 环境全流程（axon-rl + Python）

| 步骤 | 操作 | 验证点 |
|------|------|--------|
| 2.1 | Python import axon_quant | 模块加载成功 |
| 2.2 | 创建 TradingEnv（传入 market_data） | 实例创建成功 |
| 2.3 | env.reset() | 返回 (obs, info)，obs.shape 正确 |
| 2.4 | env.action_space.sample() | 采样动作合法 |
| 2.5 | env.step(action) | 返回 5 元组 |
| 2.6 | 跑完一个 episode | terminated=True 或 truncated=True |
| 2.7 | 累计 reward 有值 | reward != 0 |
| 2.8 | VecEnv 并行采样 | 多环境同步/异步正确 |

### 场景 3：HPO 超参数优化全流程（axon-hpo）

| 步骤 | 操作 | 验证点 |
|------|------|--------|
| 3.1 | 创建 HPORunner | 实例创建成功 |
| 3.2 | 定义搜索空间 | 参数范围正确 |
| 3.3 | 运行优化（mock 目标函数） | 返回 trials 列表 |
| 3.4 | 验证 Pareto 前沿 | 非支配解正确 |
| 3.5 | 验证超体积计算 | hv > 0 |

### 场景 4：Walk-Forward 验证全流程（axon-walk-forward）

| 步骤 | 操作 | 验证点 |
|------|------|--------|
| 4.1 | 创建 WalkForwardRunner | 实例创建成功 |
| 4.2 | 配置 splits + purge + embargo | 参数正确 |
| 4.3 | 运行前向验证 | 返回 fold 结果 |
| 4.4 | 验证泄漏检测 | 有泄漏时检测到 |
| 4.5 | 验证 embargo | 正确排除重叠索引 |
| 4.6 | 验证 deflated Sharpe | dsr < 原始 sharpe |

### 场景 5：实验追踪 + 模型注册（axon-tracker + axon-registry）

| 步骤 | 操作 | 验证点 |
|------|------|--------|
| 5.1 | 创建 MemoryTracker | run_id 生成 |
| 5.2 | log_param + log_metric | 数据记录成功 |
| 5.3 | get_metrics | 返回正确数据 |
| 5.4 | 创建 LocalStorage + ModelRegistry | 实例创建成功 |
| 5.5 | 注册模型版本 | 版本号递增 |
| 5.6 | 查询模型 | 返回正确元数据 |

### 场景 6：分布式训练（axon-distributed）

| 步骤 | 操作 | 验证点 |
|------|------|--------|
| 6.1 | 创建 DistributedRunner (mock 模式) | 实例创建成功 |
| 6.2 | 序列化指标 | 返回 JSON 字符串 |
| 6.3 | 保存/加载 checkpoint | 文件存在且可读 |

### 场景 7：Python Wheel 打包 + 安装

| 步骤 | 操作 | 验证点 |
|------|------|--------|
| 7.1 | maturin build --release | wheel 文件生成 |
| 7.2 | pip install wheel | 安装成功 |
| 7.3 | import axon_quant | 模块加载 |
| 7.4 | 所有子模块可访问 | 6 个子模块 |
| 7.5 | __version__ 正确 | 版本一致 |

## 三、测试用例生成策略

### 3.1 属性测试（Property-Based Testing）

用 `proptest` 生成随机输入，验证不变量：

```rust
proptest! {
    #[test]
    fn order_quantity_always_positive(qty in 0.01f64..1000.0) {
        let order = Order::spot(1, "BTC", "USDT", Side::Buy, OrderType::Market, Quantity::from_f64(qty), TimeInForce::GTC);
        assert!(order.quantity.as_f64() > 0.0);
    }

    #[test]
    fn matching_engine_never_loses_money(orders in vec(any::<Order>(), 1..100)) {
        let result = engine.run(orders);
        assert!(result.pnl.is_finite());
    }
}
```

### 3.2 快照测试（Snapshot Testing）

对关键输出建立基线，后续变更自动对比：

```rust
#[test]
fn test_run_result_snapshot() {
    let result = run_backtest(test_data());
    assert_json_snapshot!("run_result", result);
}
```

### 3.3 模糊测试（Fuzz Testing）

对解析器、序列化器进行 fuzz：

```rust
#[test]
fn fuzz_json_parse() {
    for input in generate_fuzz_inputs(1000) {
        let _ = serde_json::from_str::<TradingSignal>(&input);
        // 不应 panic
    }
}
```

### 3.4 差分测试（Differential Testing）

Rust 实现 vs Python 参考实现对比：

```python
def test_pareto_front_rust_vs_python():
    trials = generate_random_trials(100)
    rust_result = axon_quant.hpo.py_compute_pareto_front(trials, ["maximize"])
    python_result = naive_pareto(trials)  # Python 参考实现
    assert rust_result == python_result
```

## 四、自动化测试执行

### 4.1 Rust 测试（cargo test）

```bash
# L0: 编译检查
cargo check --workspace
cargo clippy --workspace -- -D warnings

# L1: 单元测试
cargo test --workspace --lib

# L2: 集成测试
cargo test --workspace --test '*'

# L3: 文档测试
cargo test --workspace --doc
```

### 4.2 Python 测试（pytest）

```bash
# 安装 wheel
maturin develop --release

# 运行 Python 测试
pytest tests/python/ -v --tb=short
```

### 4.3 端到端测试脚本

```bash
# 全流程验证
./scripts/e2e_test.sh
```

## 五、自动化修复流程

```
测试失败
  ↓
分析失败类型
  ├─ 编译错误 → 修复 Cargo.toml / 代码
  ├─ 单元测试失败 → 修复实现逻辑
  ├─ 集成测试失败 → 修复模块间接口
  ├─ Python 测试失败 → 修复 PyO3 绑定
  └─ 端到端失败 → 修复数据流/配置
  ↓
运行测试验证修复
  ↓
提交修复
```

## 六、执行计划

| 阶段 | 内容 | 预计耗时 |
|------|------|---------|
| P1 | 创建测试基础设施（fixtures, helpers） | 30min |
| P2 | L2 模块功能测试（7 个场景） | 60min |
| P3 | Python 端到端测试 | 30min |
| P4 | 自动化测试脚本 | 20min |
| P5 | 修复发现的问题 | 按需 |

## 七、成功标准

- [ ] 所有 L0 检查通过（0 warnings, 0 errors）
- [ ] 所有 L1 单元测试通过
- [ ] 所有 L2 模块功能测试通过
- [ ] Python wheel 安装后所有子模块可用
- [ ] 端到端回测→训练→优化闭环跑通
- [ ] 无 panic、无 unwrap、无 unimplemented
