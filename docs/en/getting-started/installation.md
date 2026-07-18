# Installation

> **Quick start**: [`examples/01_getting_started/00_all_in_one.py`](https://github.com/pengwow/axon_quant/blob/main/examples/01_getting_started/00_all_in_one.py)
> One command, covers all 6 stages with built-in offline data. No extra setup needed.

> Applicable version: AXON v0.6.0+

This document describes how to install AXON.

## 1. Prerequisites

### 1.1 Required

| Tool | Version | Description |
|------|---------|-------------|
| **Rust** | 1.97.0+ | Enforced by `rust-toolchain.toml` |
| **Git** | 2.30+ | Clone source code |

### 1.2 Optional

| Tool | Purpose |
|------|---------|
| **Python 3.12+** | PyO3 bindings (axon-rl, axon-hpo, axon-walk-forward, axon-distributed, axon-llm) |
| **CUDA Toolkit** | GPU acceleration (axon-inference feature = `cuda`) |
| **Docker** | Containerized deployment |

## 2. Install Rust Toolchain

If using `rustup`, simply enter the repository root to trigger automatic installation via `rust-toolchain.toml`:

```bash
cd axon_quant
rustup show  # Automatically downloads 1.97.0
```

## 3. Clone and Build

```bash
git clone https://github.com/pengwow/axon_quant.git
cd axon_quant

# Debug build
cargo build --workspace

# Release build (LTO, 1 codegen-unit)
cargo build --workspace --release
```

## 4. Install CLI

```bash
cargo install --path crates/axon-cli --locked
```

Verify:

```bash
axon --version
```

## 5. Python Bindings (Optional)

```bash
# Create virtual environment
python -m venv .venv
source .venv/bin/activate  # Linux/macOS
# .venv\Scripts\activate   # Windows

# Compile and install
make python-install

# Verify
python -c "import axon_quant; print(axon_quant.__version__)"
```

## 6. Verify Installation

```bash
# Run all unit tests
cargo test --workspace

# Run lint + format check
make verify
```

## Next Steps

- [Quick Start](quickstart.md) — 5 minutes to run your first backtest
- [Architecture Overview](../user-guide/architecture.md) — Understand system components
