# CLI Commands

> Applicable version: axon-cli v0.3.0+
> Installation: `cargo install --path crates/axon-cli --locked`

axon-cli is AXON's command-line entry point, providing subcommands for backtesting, training, optimization, validation, tracking, and more.

## Global Parameters

```bash
axon [OPTIONS] <SUBCOMMAND>

OPTIONS:
    -c, --config <FILE>     Config file path (default: ./axon.toml)
    -v, --verbose           Verbose logging
    -q, --quiet             Quiet mode
    --log-format <FORMAT>   Log format (text|json, default: text)
```

## Subcommand List

| Command | Description |
|---------|-------------|
| `backtest` | Run backtesting simulation |
| `train` | Train RL model |
| `hpo` | Hyperparameter optimization |
| `validate` | Walk-forward validation |
| `track` | Experiment tracking |
| `serve` | Start inference server |
| `exchange` | Exchange operations |

## backtest

Run backtesting simulation with specified configuration.

```bash
axon backtest [OPTIONS]

OPTIONS:
    -c, --config <FILE>       Config file path
    --strategy <STRATEGY>     Strategy name or path
    --data <DATA>             Market data file path
    --start <START>           Start date (YYYY-MM-DD)
    --end <END>               End date (YYYY-MM-DD)
    --output <OUTPUT>         Output directory for results
    --format <FORMAT>         Output format (json|csv|parquet)
    --verbose                 Enable detailed logging
```

Example:

```bash
# Run backtest with default config
axon backtest -c config.toml --strategy momentum --data ./data/btc_1h.parquet

# Run backtest with custom parameters
axon backtest -c config.toml --strategy mean_reversion \
    --start 2024-01-01 --end 2024-06-01 \
    --output ./results/ --format json
```

## train

Train RL model using specified algorithm and environment.

```bash
axon train [OPTIONS]

OPTIONS:
    -c, --config <FILE>       Config file path
    --algorithm <ALG>         RL algorithm (ppo|sac|dqn)
    --env <ENV>               Environment name
    --timesteps <N>           Total training timesteps
    --eval-freq <N>           Evaluation frequency
    --save-freq <N>           Model save frequency
    --output <OUTPUT>         Model output directory
    --verbose                 Enable detailed logging
```

Example:

```bash
# Train PPO agent
axon train -c config.toml --algorithm ppo --timesteps 1000000

# Train with custom evaluation frequency
axon train -c config.toml --algorithm sac \
    --timesteps 500000 --eval-freq 10000
```

## hpo

Run hyperparameter optimization using Optuna.

```bash
axon hpo [OPTIONS]

OPTIONS:
    -c, --config <FILE>       Config file path
    --trials <N>              Number of trials
    --timeout <SECONDS>       Timeout in seconds
    --storage <URL>           Optuna storage URL
    --study-name <NAME>       Study name
    --direction <DIR>         Optimization direction (maximize|minimize)
    --sampler <SAMPLER>       Sampler type (tpe|random|grid)
    --pruner <PRUNER>         Pruner type (median|hyperband|none)
    --output <OUTPUT>         Results output directory
    --verbose                 Enable detailed logging
```

Example:

```bash
# Run HPO with 50 trials
axon hpo -c config.toml --trials 50 --direction maximize

# Run HPO with custom sampler and pruner
axon hpo -c config.toml --trials 100 \
    --sampler tpe --pruner median \
    --timeout 3600
```

## validate

Run walk-forward validation to assess strategy robustness.

```bash
axon validate [OPTIONS]

OPTIONS:
    -c, --config <FILE>       Config file path
    --strategy <STRATEGY>     Strategy name or path
    --data <DATA>             Market data file path
    --train-size <N>          Training window size
    --test-size <N>           Test window size
    --step-size <N>           Rolling step size
    --output <OUTPUT>         Validation results directory
    --verbose                 Enable detailed logging
```

Example:

```bash
# Run walk-forward validation
axon validate -c config.toml --strategy momentum \
    --train-size 1000 --test-size 200 --step-size 100
```

## track

Start experiment tracking server or log metrics.

```bash
axon track [OPTIONS]

OPTIONS:
    -c, --config <FILE>       Config file path
    --backend <BACKEND>       Tracking backend (mlflow|wandb|local)
    --experiment <NAME>       Experiment name
    --run-name <NAME>         Run name
    --log-param <KEY=VALUE>   Log parameter
    --log-metric <KEY=VALUE>  Log metric
    --verbose                 Enable detailed logging
```

Example:

```bash
# Start MLflow tracking server
axon track -c config.toml --backend mlflow --experiment ppo_btc

# Log parameters and metrics
axon track -c config.toml --log-param "lr=0.001" --log-metric "sharpe=1.5"
```

## serve

Start model inference server for production deployment.

```bash
axon serve [OPTIONS]

OPTIONS:
    -c, --config <FILE>       Config file path
    --model <MODEL>           Model file path
    --host <HOST>             Listen host (default: 0.0.0.0)
    --port <PORT>             Listen port (default: 8080)
    --workers <N>             Number of worker threads
    --verbose                 Enable detailed logging
```

Example:

```bash
# Start inference server
axon serve -c config.toml --model ./models/trading_model.onnx \
    --host 0.0.0.0 --port 8080 --workers 4
```

## exchange

Perform exchange operations (query balances, positions, etc.).

```bash
axon exchange [OPTIONS]

OPTIONS:
    -c, --config <FILE>       Config file path
    --exchange <EXCHANGE>     Exchange name (binance|okx)
    --action <ACTION>         Action (balances|positions|ticker)
    --symbol <SYMBOL>         Trading symbol
    --testnet                 Use testnet
    --verbose                 Enable detailed logging
```

Example:

```bash
# Query Binance balances
axon exchange -c config.toml --exchange binance --action balances

# Query OKX positions
axon exchange -c config.toml --exchange okx --action positions
```

## Exit Codes

| Code | Description |
|------|-------------|
| 0 | Success |
| 1 | General error |
| 2 | Configuration error |
| 3 | Network error |
| 4 | Authentication error |
| 5 | Rate limit exceeded |

## Environment Variables

| Variable | Description |
|----------|-------------|
| `AXON_CONFIG` | Default config file path |
| `AXON_LOG_LEVEL` | Log level (trace/debug/info/warn/error) |
| `AXON_LOG_FORMAT` | Log format (text/json) |
| `AXON_API_KEY` | Exchange API key |
| `AXON_API_SECRET` | Exchange API secret |

## Next Steps

- [Configuration Reference](configuration.md) — Detailed configuration options
- [API Reference](api-reference.md) — Python and Rust API documentation
- [Python Bindings](python-bindings.md) — Using AXON from Python
