# FAQ

> **Related examples**: [`examples/`](../../../examples/) contains complete runnable examples for all features.
> Start with [`examples/01_getting_started/00_all_in_one.py`](../../../examples/01_getting_started/00_all_in_one.py).

This document collects common questions and answers for the AXON quantitative trading framework, organized by category for quick reference.

---

## 1. Environment Setup

### Q1: What are the minimum system requirements for AXON?

**A:** AXON framework minimum requirements:

- **Rust version**: 1.96.0 or higher (check with `rustc --version`)
- **Python version**: 3.9 or higher (if using Python bindings)
- **Operating System**: Linux (Ubuntu 22.04+ recommended), macOS 13+, Windows 11+
- **Memory**: Minimum 8GB, 16GB recommended (for training large models)
- **Disk**: Minimum 10GB available space (including dependencies and model files)

For GPU inference:
- **NVIDIA GPU**: CUDA 11.8+ / cuDNN 8.6+
- **GPU Memory**: Minimum 4GB, 8GB+ recommended

### Q2: How do I install AXON?

**A:** Install from source using Cargo:

```bash
git clone https://github.com/pengwow/axon_quant.git
cd axon_quant

# Build all crates
cargo build --workspace

# Run tests
cargo test --workspace

# Install CLI
cargo install --path crates/axon-cli --locked
```

For Python bindings:

```bash
# Create virtual environment
python -m venv .venv
source .venv/bin/activate

# Install with maturin
pip install maturin
maturin develop --release

# Verify
python -c "import axon_quant; print(axon_quant.__version__)"
```

### Q3: How do I set up the development environment?

**A:** Recommended development setup:

```bash
# Install Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install Python (pyenv recommended)
pyenv install 3.14.6
pyenv virtualenv 3.14.6 axon_dev
pyenv local axon_dev

# Install development dependencies
pip install -e ".[dev]"

# Run full verification
make verify
```

---

## 2. Compilation Issues

### Q4: I get "Rust edition 2024 is unstable" error

**A:** AXON requires Rust 1.96.0+. Update your toolchain:

```bash
rustup update stable
rustc --version  # Should show 1.96.0+
```

### Q5: ONNX Runtime linking fails on Linux

**A:** Install required system libraries:

```bash
# Ubuntu/Debian
sudo apt-get update
sudo apt-get install -y libgomp1 libssl-dev pkg-config

# CentOS/RHEL
sudo yum install -y libgomp openssl-devel
```

### Q6: macOS "library not found for -lpython3.X" error

**A:** Set the Python environment variable:

```bash
export PYO3_PYTHON=$(which python3.14)
maturin develop --release
```

---

## 3. Usage Issues

### Q7: How do I run a backtest?

**A:** Use the CLI or Python API:

```bash
# CLI
axon backtest -c config.toml --strategy momentum --data ./data/btc_1h.parquet

# Python
import axon_quant as aq

env = aq.make_env(
    market_data=aq.MarketData.from_arrays(prices, volumes),
    matching_engine="L1",
)
result = env.run()
print(f"Sharpe: {result.sharpe_ratio:.2f}")
```

### Q8: How do I configure risk control?

**A:** Risk control is configured in the config file:

```toml
[risk]
max_order_notional = 50000.0
max_daily_orders = 100
max_position_abs = 10.0
allowed_symbols = ["BTC-USDT", "ETH-USDT"]

[safety]
mode = "TwoPhase"  # DryRun | TwoPhase | Direct

[gate]
type = "RejectionCircuitBreaker"
threshold = 5
cooldown_ms = 60000
```

### Q9: How do I integrate with exchanges?

**A:** Use the ExchangeAdapter:

```python
import axon_quant.exchange as exchange

adapter = exchange.BinanceAdapter(
    api_key="your-key",
    api_secret="your-secret",
    testnet=True,
)

await adapter.connect()
await adapter.subscribe(["BTCUSDT"])

# Place order
order_id = await adapter.place_order(
    symbol="BTCUSDT",
    side="BUY",
    quantity=0.001,
    order_type="MARKET",
)
```

---

## 4. Training Issues

### Q10: Training is too slow

**A:** Try these optimizations:

1. Use VecEnv for parallel environments
2. Reduce observation window size
3. Use mixed precision (FP16) for inference
4. Enable CPU affinity for compute-bound tasks

```python
# Parallel training
envs = aq.make_vec_env(env_fn, n_envs=8)

# Reduce observation window
config = {"observation_window": 10}  # Instead of 50
```

### Q11: How do I save and load models?

**A:** Use the ModelRegistry:

```python
import axon_quant.registry as registry

# Save model
storage = registry.LocalStorage.new(base_dir="./models")
reg = registry.ModelRegistry.new(storage)

reg.register(
    name="my_model",
    artifact_path="./model.onnx",
    metadata={"sharpe": "1.5"},
)

# Load model
model = reg.get("my_model", version="latest")
```

---

## 5. Deployment Issues

### Q12: How do I deploy to production?

**A:** Follow these steps:

1. Build release binary: `cargo build --release`
2. Configure production settings in `config.toml`
3. Set up monitoring (see [Operations Runbook](../user-guide/llm-trading/operations-runbook.md))
4. Deploy with systemd or Docker

```bash
# Systemd service
sudo cp target/release/axon /usr/local/bin/
sudo systemctl start axon-trading

# Docker
docker build -t axon-trading .
docker run -d -p 8080:8080 axon-trading
```

### Q13: How do I monitor the trading system?

**A:** Use the built-in metrics system:

```python
import axon_quant.metrics as metrics

# Enable Prometheus metrics
metrics.start_http_server(9100)

# Metrics are automatically collected:
# - trading_orders_total
# - trading_risk_rejections_total
# - trading_tool_execute_duration_seconds
```

---

## 6. Contributing

### Q14: How do I contribute to AXON?

**A:** See the [Contributing Guide](contributing.md):

1. Fork the repository
2. Create a feature branch
3. Write tests (TDD recommended)
4. Submit a pull request

### Q15: Where do I report bugs?

**A:** Report bugs via GitHub Issues:

https://github.com/pengwow/axon_quant/issues

Include:
- Steps to reproduce
- Expected behavior
- Actual behavior
- Environment details (OS, Rust version, Python version)

---

## Next Steps

- [Installation](../getting-started/installation.md) — Detailed installation guide
- [Quick Start](../getting-started/quickstart.md) — Get started in 5 minutes
- [Architecture Overview](../user-guide/architecture.md) — Understand the system
