# Configuration Reference

AXON uses TOML for configuration, with environment variable interpolation and default values.

## Configuration File Paths

Searched in the following priority order (high → low):

1. CLI argument `-c / --config <FILE>`
2. Environment variable `AXON_CONFIG`
3. Current directory `./axon.toml`
4. User directory `~/.config/axon/config.toml`
5. System directory `/etc/axon/config.toml`

## Configuration Structure

```toml
[core]
log_level = "info"           # trace | debug | info | warn | error
log_format = "text"          # text | json

[trading]
backend = "exchange"         # mock | exchange | oms | backtest
symbol = "BTCUSDT"
initial_capital = 100000.0

[trading.exchange]
exchange_id = "binance"      # binance | okx
api_key = "${AXON_API_KEY}"  # Environment variable interpolation
api_secret = "${AXON_API_SECRET}"
testnet = true
rest_base_url = "https://testnet.binance.vision"
ws_url = "wss://testnet.binance.vision/ws"

[risk]
max_order_notional = 50000.0
max_daily_orders = 100
max_position_abs = 10.0
allowed_symbols = ["BTC-USDT", "ETH-USDT"]

[safety]
mode = "TwoPhase"            # DryRun | TwoPhase | Direct

[gate]
type = "RejectionCircuitBreaker"  # AlwaysOpenGate | RejectionCircuitBreaker | RiskPnLCircuitBreaker
threshold = 5
cooldown_ms = 60000

[hpo]
study_name = "ppo_optimization"
direction = "maximize"
n_trials = 50
n_jobs = 1

[walk_forward]
train_size = 1000
test_size = 200
step_size = 100
window_type = "rolling"      # rolling | expanding

[inference]
model_path = "models/trading_model.onnx"
backend = "onnx"             # onnx | candle | tch
device = "cpu"               # cpu | cuda:0
fp16 = false
num_threads = 4

[metrics]
callback = "prometheus:http://localhost:9100/metrics"
```

## Environment Variable Interpolation

AXON supports `${VAR_NAME}` syntax for environment variable interpolation:

```toml
[trading.exchange]
api_key = "${AXON_API_KEY}"
api_secret = "${AXON_API_SECRET}"
```

Environment variables are resolved at runtime. If a variable is not set, the configuration will fail to load unless a default is provided.

## Profiles

AXON supports configuration profiles for different environments:

```bash
# Use production profile
axon backtest -c config.toml --profile production

# Use development profile
axon backtest -c config.toml --profile development
```

Profiles are defined in the configuration file:

```toml
[profiles.production]
trading.testnet = false
risk.max_order_notional = 100000.0

[profiles.development]
trading.testnet = true
risk.max_order_notional = 10000.0
```

## Validation

AXON validates configuration at startup. You can validate without running:

```bash
axon validate-config -c config.toml
```

## Migration

When upgrading between versions, configuration schema may change. AXON provides migration tools:

```bash
# Check for migration needs
axon migrate --check -c config.toml

# Run migration
axon migrate -c config.toml
```

## Next Steps

- [CLI Commands](cli.md) — Command-line interface reference
- [API Reference](api-reference.md) — Python and Rust API documentation
- [Risk & Safety](../user-guide/llm-trading/risk-safety.md) — Risk control configuration
