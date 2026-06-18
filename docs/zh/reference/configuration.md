# 配置参考

AXON 的配置以 TOML 为主,支持环境变量插值和默认值。

## 配置文件路径

按以下优先级查找(高 → 低):

1. CLI 参数 `-c / --config <FILE>`
2. 环境变量 `AXON_CONFIG`
3. 当前目录 `./axon.toml`
4. 用户目录 `~/.config/axon/config.toml`
5. 系统目录 `/etc/axon/config.toml`

## 配置结构

```toml
[core]
log_level = "info"           # trace | debug | info | warn | error
log_format = "text"          # text | json
data_dir = "./data"          # 市场数据根目录
output_dir = "./output"      # 输出根目录

[backtest]
engine = "L1"                # L1 | L2 | L3
impact_model = "almgren_chriss"  # almgren_chriss | linear | square_root
latency_model = "fixed_1ms"  # fixed_1ms | gaussian_5ms
fee_model = "taker_5bps"     # taker_5bps | maker_2bps | custom
slippage_bps = 0.0

[rl]
env_name = "AxonEnv-v0"
total_timesteps = 1_000_000
seed = 42

[hpo]
backend = "optuna"           # optuna | ray
n_trials = 100
storage = "sqlite:///hpo.db" # 可选,默认内存

[walk_forward]
window_size = 252            # 训练窗口(trading days)
step_size = 21               # 步进
embargo_size = 5             # 隔离期
purged = true

[tracker]
backend = "mlflow"           # mlflow | wandb | local | memory
tracking_uri = "http://localhost:5000"

[registry]
backend = "file"             # file | sqlite | postgres
path = "./registry"

[llm]
provider = "openai"          # openai | anthropic | local
model = "gpt-4"
api_key = "${OPENAI_API_KEY}"
api_base = "https://api.openai.com/v1"

[llm.trading]
backend = "mock"             # mock | exchange | oms | backtest
safety_mode = "DryRun"       # DryRun | TwoPhase | Direct
config_path = ""             # 子配置文件路径(可选)

[llm.trading.risk]
max_order_notional = 50000.0
max_daily_orders = 100
max_position_abs = 10.0
allowed_symbols = ["BTC-USDT", "ETH-USDT"]

[llm.trading.gate]
type = "AlwaysOpen"          # AlwaysOpen | RejectionCircuitBreaker | RiskPnLCircuitBreaker
threshold = 5
cooldown_ms = 60000

[llm.trading.metrics]
# 应用方注册到自己的监控后端
callback = "prometheus:http://localhost:9100/metrics"
# 或
snapshot_interval_ms = 10000
```

## 环境变量插值

字符串值支持 `${VAR_NAME}` 语法,从环境变量读取:

```toml
[llm]
api_key = "${OPENAI_API_KEY}"
api_secret = "${OPENAI_API_SECRET}"
```

也支持默认值:`${VAR_NAME:-default_value}`

```toml
[core]
log_level = "${AXON_LOG_LEVEL:-info}"
```

## Profile

支持多 profile(开发 / 测试 / 生产),用 `[profile.<name>]` 段:

```toml
# 默认配置
[llm.trading]
backend = "mock"
safety_mode = "DryRun"

# 测试环境
[profile.test.llm.trading]
backend = "mock"
safety_mode = "Direct"

# 生产环境
[profile.prod.llm.trading]
backend = "exchange"
safety_mode = "Direct"

[profile.prod.llm.trading.risk]
max_order_notional = 100000.0
max_daily_orders = 500
```

通过环境变量 `AXON_PROFILE=test` 激活。

## 验证配置

```bash
# 验证配置文件格式 + 必需字段
axon config validate

# 查看实际生效的配置(profile 展开 + env 插值)
axon config show

# 查看某一项
axon config get llm.trading.backend
```

## 配置迁移

升级 AXON 大版本时,配置可能有 breaking change。运行:

```bash
axon config migrate --from <OLD_VERSION> --to <NEW_VERSION>
```

迁移会在原配置文件同目录生成 `axon.toml.new`,原文件保留为 `axon.toml.bak`。
