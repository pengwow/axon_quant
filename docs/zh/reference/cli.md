# CLI 命令

> 适用版本:axon-cli v0.2.0+
> 安装:`cargo install --path crates/axon-cli --locked`

axon-cli 是 AXON 的命令行入口,提供回测、训练、优化、验证、追踪等子命令。

## 全局参数

```bash
axon [OPTIONS] <SUBCOMMAND>

OPTIONS:
    -c, --config <FILE>     配置文件路径(默认:./axon.toml)
    -v, --verbose           详细日志
    -q, --quiet             静默模式
    --log-format <FORMAT>   日志格式(text|json,默认:text)
```

## 子命令列表

### `axon backtest`

运行回测。

```bash
axon backtest \
  --data <DATA_FILE> \
  --strategy <STRATEGY_FILE> \
  --engine <L1|L2|L3> \
  --start <YYYY-MM-DD> \
  --end <YYYY-MM-DD> \
  --output <OUTPUT_DIR>
```

参数:

| 参数 | 必需 | 说明 |
|------|------|------|
| `--data` | ✅ | 市场数据文件(Parquet / Arrow IPC) |
| `--strategy` | ✅ | 策略文件(Rust 源文件 / 编译后的 .so) |
| `--engine` | ❌ | 撮合引擎(`L1` / `L2` / `L3`,默认 `L1`) |
| `--start` / `--end` | ❌ | 回测时间范围 |
| `--output` | ❌ | 输出目录(默认 `./output/backtest/`) |

### `axon train`

启动 RL 训练。

```bash
axon train \
  --env <ENV_NAME> \
  --algo <PPO|SAC|DQN|A2C> \
  --total-timesteps <N> \
  --output <OUTPUT_DIR>
```

参数:

| 参数 | 必需 | 说明 |
|------|------|------|
| `--env` | ✅ | 环境名(注册到 Gymnasium) |
| `--algo` | ✅ | 算法(`PPO` / `SAC` / `DQN` / `A2C`) |
| `--total-timesteps` | ✅ | 总训练步数 |
| `--output` | ❌ | 输出目录 |

### `axon hpo`

启动超参优化(基于 Optuna)。

```bash
axon hpo \
  --study <STUDY_NAME> \
  --n-trials <N> \
  --storage <STORAGE_URL>
```

### `axon walk-forward`

启动 walk-forward 验证。

```bash
axon walk-forward \
  --data <DATA_FILE> \
  --strategy <STRATEGY_FILE> \
  --window <N> \
  --step <M> \
  --embargo <K>
```

### `axon registry`

管理模型注册表。

```bash
axon registry list
axon registry push <MODEL_DIR>
axon registry pull <VERSION>
axon registry promote <VERSION> --to <STAGE>
```

### `axon tracker`

启动 MLflow / WandB 追踪后端。

```bash
axon tracker serve --backend <mlflow|wandb> --port <PORT>
```

### `axon llm-trading`

启动 LLM 交易 agent(Stage A~K 交付)。

```bash
axon llm-trading \
  --backend <mock|exchange|oms|backtest> \
  --config <CONFIG_FILE> \
  --llm <openai|anthropic|local>
```

### `axon explain`

生成模型可解释性报告(SHAP / 反事实)。

```bash
axon explain \
  --model <MODEL_DIR> \
  --data <DATA_FILE> \
  --output <REPORT_DIR>
```

## 退出码

| 退出码 | 含义 |
|--------|------|
| 0 | 成功 |
| 1 | 一般错误 |
| 2 | 参数错误 |
| 3 | 数据错误(找不到 / 损坏) |
| 4 | 运行时错误(回测异常 / 训练失败) |
| 5 | 配置错误 |

## 环境变量

| 变量 | 说明 |
|------|------|
| `RUST_LOG` | tracing 日志级别(`info` / `debug` / `trace`) |
| `AXON_CONFIG` | 默认配置文件路径 |
| `PYO3_PYTHON` | Python 解释器路径(仅 Python 绑定需要) |
| `AXON_EXCHANGE_API_KEY` | 交易所 API key(可选,也可走配置文件) |
| `AXON_EXCHANGE_API_SECRET` | 交易所 API secret |
