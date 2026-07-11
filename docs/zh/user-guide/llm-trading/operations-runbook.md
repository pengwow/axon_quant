# LLM 交易运维手册

> 适用版本:axon-llm v0.3.0+
> 读者:运维 / SRE / on-call 工程师
> 前置阅读:[overview.md](overview.md) [risk-safety.md](risk-safety.md) [metrics-alerting.md](metrics-alerting.md)

本文档提供 LLM 交易系统的部署、升级、故障排查、回滚的完整 runbook。所有命令假定 cargo workspace 根目录。

axon-llm 是 lib,无独立 server 进程。本文档假设应用方用 Rust(或经 PyO3 调用的 Python)实现 `bin/llm-trading-agent` 主程序,本 runbook 只覆盖 axon-llm 相关部分,主程序细节由各团队自定。

## 1. 部署

### 1.1 系统要求

- **Rust**:1.97.0+(`rust-toolchain.toml` 强制)
- **Python**:3.14.6(默认,pyenv 管理;仅 Python 绑定需要)
- **CPU**:至少 4 cores
- **内存**:开发 4GB / 生产 8GB+
- **网络**:对接交易所需允许出站 HTTPS(443) + WSS(9443)

### 1.2 部署步骤

```bash
# 1. 拉取最新代码
git clone https://github.com/pengwow/axon_quant.git /opt/axon_quant
cd /opt/axon_quant

# 2. 选择 release tag
git checkout v0.3.0  # 或 main / develop

# 3. Release 构建(LTO,1 codegen-unit)
cargo build --workspace --release

# 4. (可选)安装 Python 绑定
make python-install

# 5. 启动应用(由各团队自定,这里只是示例)
RUST_LOG=info /opt/axon_quant/target/release/llm-trading-agent \
  --config /etc/axon_quant/config.toml
```

### 1.3 配置示例(`/etc/axon_quant/config.toml`)

```toml
[trading]
backend = "exchange"  # mock | exchange | oms | backtest
api_key = "${AXON_API_KEY}"  # 从环境变量读取
api_secret = "${AXON_API_SECRET}"
testnet = true  # 测试网

[safety]
mode = "TwoPhase"  # DryRun | TwoPhase | Direct

[risk]
max_order_notional = 50000.0
max_daily_orders = 100
max_position_abs = 10.0
allowed_symbols = ["BTC-USDT", "ETH-USDT"]

[gate]
type = "RejectionCircuitBreaker"
threshold = 5
cooldown_ms = 60000

[metrics]
# 应用方注册到自己的监控后端
callback = "prometheus:http://localhost:9100/metrics"
```

## 2. 升级

### 2.1 滚动升级步骤

axon-llm 是 lib,升级影响的是 `target/release/llm-trading-agent`。标准滚动升级:

```bash
# 1. 拉取新代码
cd /opt/axon_quant
git fetch --tags
git checkout v0.3.0  # 假设的新版本

# 2. 编译新二进制(不中断旧服务)
cargo build --workspace --release

# 3. 健康检查新二进制
./target/release/llm-trading-agent --version
./target/release/llm-trading-agent --self-check

# 4. 逐实例替换(systemd / k8s / supervisor)
systemctl restart llm-trading-agent.service
# 或
kubectl rollout restart deployment/llm-trading-agent

# 5. 观察新实例的 metrics / 日志
journalctl -u llm-trading-agent -f
# 或
kubectl logs -f deployment/llm-trading-agent
```

### 2.2 数据库 / 状态兼容

axon-llm **无服务端持久化状态**(所有状态在进程内 / 在 backend 中),升级前注意:

- `RiskLimits::DailyCounter`:进程内,重启归零。如果应用方需要持久化,需在外部存储镜像(Redis / DB)
- `RejectionCircuitBreaker`:进程内,重启重置
- `TradingMetrics`:进程内,重启清零
- `TwoPhase::PendingOrder`:进程内,重启 token 失效(影响见 [风控与安全](risk-safety.md) §1.2)

**重要**:无状态升级是安全的,无需特殊迁移脚本。

### 2.3 Breaking Change 检查

升级前必须阅读 CHANGELOG 的 **BREAKING CHANGES** 段,典型破坏性变更:

- `TradingBackend` trait 新增必选方法(虽然设计上都是默认实现 + 尽量保持向后兼容)
- `RiskLimits` 新增必填字段
- `Tool` trait 签名变更

**应用方适配流程**:在 staging 环境先跑 E2E 测试套件 `cargo test -p axon-llm --test llm_trading_*_e2e`,通过后再升级生产。

## 3. 故障排查

### 3.1 LLM agent 下单全部失败

**症状**:`trading_risk_rejections_total` 持续增长,`trading_orders_total{status="success"}` = 0。

**排查步骤**:

1. **检查 SafetyMode**:
   ```bash
   journalctl -u llm-trading-agent | grep -i "DryRun\|TwoPhase\|Direct"
   ```
   如果是 DryRun / TwoPhase,确认 SafetyMode 配置正确。

2. **检查 RiskLimits**:
   ```bash
   journalctl -u llm-trading-agent | grep "RiskLimits"
   ```
   看到 `"order notional X exceeds limit Y"` 等消息,说明 LLM 决策超出风控规则。
   行动:调宽 `max_order_notional` / `max_daily_orders` / `max_position_abs`,或约束 LLM prompt。

3. **检查 RiskGate**:
   ```bash
   journalctl -u llm-trading-agent | grep "RiskGate"
   ```
   看到 `"circuit breaker open"`,说明连续拒绝次数达阈值,闸门开。
   行动:等待 cooldown(默认 60s),或手动调 `gate.reset()`(应用方实现)。

4. **检查后端**:
   ```bash
   journalctl -u llm-trading-agent | grep "BackendError"
   ```
   看到网络错误 / 拒绝 / 超时,继续 §3.2。

### 3.2 后端调用失败

**症状**:`trading_backend_errors_total` 持续增长。

**排查步骤**:

1. **确认后端类型**:
   - `exchange`:检查交易所 API status(https://www.binance.com/en/support/announcement)
   - `oms`:检查 axon-oms 服务状态 / 端口
   - `backtest`:无网络问题,检查 `MarketData` 数据完整性
   - `mock`:无网络问题,检查 mock state

2. **Exchange 后端**:
   ```bash
   # 测试连通性
   curl -v https://api.binance.com/api/v3/ping

   # 测试签名
   PYO3_PYTHON=.venv/bin/python python -c "
   import axon_quant
   b = axon_quant.ExchangeTradingBackend(
       api_key='xxx', api_secret='yyy', testnet=True, exchange='binance'
   )
   print(b.get_balances())
   "
   ```

3. **OMS 后端**:
   ```bash
   # 检查 OMS 进程
   systemctl status axon-oms
   # 检查端口
   ss -lnt | grep 8080
   # 健康检查
   curl http://localhost:8080/healthz
   ```

4. **Backtest 后端**:
   - 检查 `MarketData` 是否有有效数据
   - 检查时间范围是否覆盖策略需要的区间

### 3.3 性能下降

**症状**:`trading_tool_execute_duration_seconds` P99 > 1s。

**排查步骤**:

1. **确认是 LLM 调用慢还是后端调用慢**:
   - 拆解:`trading_tool_execute_duration_seconds` 包含 LLM 决策时间 + 风控 + 后端
   - 在应用方代码中加额外的 histogram:`llm_decision_duration_seconds`、`risk_check_duration_seconds`、`backend_call_duration_seconds`

2. **后端慢**:
   - Exchange:检查交易所 API 延迟
   - OMS:检查 OMS 服务健康
   - Backtest:检查 `L1MatchingEngine` 撮合性能(`cargo bench -p axon-backtest`)

3. **风控开销**:
   - 单纯 `RiskLimits::check` 耗时 < 1μs,不应成为瓶颈
   - `RejectionCircuitBreaker` 耗时 < 100ns,不应成为瓶颈

### 3.4 进程崩溃

**症状**:`llm-trading-agent` 进程退出,systemd / k8s 持续重启。

**排查步骤**:

1. **查看 panic 信息**:
   ```bash
   journalctl -u llm-trading-agent -n 200 | grep -A 20 "panic\|FATAL"
   ```

2. **常见 panic 原因**:
   - `unwrap()` 在 `MockTradingBackend` 未初始化的字段上:检查 `setup()` 是否调用
   - `tokio::Runtime` 嵌套:检查 `block_on` 是否在已运行 runtime 的线程中调用
   - 序列化错误:检查 `serde_json` 反序列化参数

3. **core dump**:
   ```bash
   ulimit -c unlimited
   echo "/tmp/core.%e.%p" > /proc/sys/kernel/core_pattern
   # 重启进程,触发 panic,获取 core
   gdb /opt/axon_quant/target/release/llm-trading-agent /tmp/core.llm-trading-agent.1234
   ```

## 4. 回滚

### 4.1 快速回滚

```bash
cd /opt/axon_quant

# 1. 切回旧 tag
git checkout v0.3.0

# 2. 重新编译
cargo build --workspace --release

# 3. 重启服务
systemctl restart llm-trading-agent

# 4. 观察旧版本 metrics
journalctl -u llm-trading-agent -f
```

### 4.2 灰度回滚(k8s)

```bash
# 回滚到上一个 deployment 版本
kubectl rollout undo deployment/llm-trading-agent

# 暂停回滚(如果回滚后仍有问题)
kubectl rollout pause deployment/llm-trading-agent
```

## 5. 日常运维清单

### 5.1 每日

- [ ] 检查 `trading_orders_total` 趋势是否正常
- [ ] 检查 `trading_risk_rejections_total` 是否突增
- [ ] 检查 `trading_backend_errors_total` 是否突增
- [ ] 检查 `trading_daily_orders_count` 是否接近 `max_daily_orders`
- [ ] 检查 LLM agent 日志是否有异常(panic / unwrap / 超时)

### 5.2 每周

- [ ] 审查本周 LLM 决策样本(从 `RiskLimits` 拒绝样本中抽样)
- [ ] 检查 axon-llm 是否有新版本发布(CHANGELOG)
- [ ] 检查交易所 API 是否有 breaking change 公告
- [ ] 检查 `RejectionCircuitBreaker` 触发频率,评估是否需要调阈值

### 5.3 每月

- [ ] 跑回归测试套件 `cargo test --workspace`
- [ ] 跑 LLM 集成 E2E 测试 `cargo test -p axon-llm --test llm_trading_*_e2e`
- [ ] 检查 axon-llm release notes,规划升级
- [ ] 复盘本月 PnL,评估 RiskLimits 阈值是否合理

## 6. 紧急联系

| 角色 | 联系方式 |
|------|---------|
| axon-llm 维护者 | (GitHub Issues / 团队 Slack) |
| 交易所对接 | (各交易所官方支持) |
| OMS 维护者 | (内部团队) |
| LLM provider 故障 | OpenAI / Anthropic / 其他官方 status page |

## 下一步

- [架构总览](architecture.md) —— 系统组件
- [风控与安全](risk-safety.md) —— 三道防线
- [指标与告警](metrics-alerting.md) —— 监控数据
