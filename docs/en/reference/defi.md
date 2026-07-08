# DeFi On-Chain Trading (Python Bindings)

> Applies to:AXON v0.3.0+ (`axon-defi` 0.3.0 P0 Batches 1-4 fully delivered)
> Upstream:[axon-defi Rust crate](https://github.com/pengwow/axon_quant/blob/main/crates/axon-defi/) + [axon_quant.defi top-level wrapper](https://github.com/pengwow/axon_quant/blob/main/python/axon_quant/defi.py)
> Design doc:[DeFi On-Chain Trading Architecture](https://github.com/pengwow/axon_quant/blob/main/.axon-internal/specs/2026-06-21-defi-onchain-trading-design.md)
> Runnable example:`examples/17_python_bindings/python_bindings_demo.py` (DeFi section)

Before 0.3.0 P0, the entire `axon-defi` crate was a "shell":`bridge_tokens` returned `format!("0x{:064x}", 67890)` (fake hash), `submit_transaction` returned `format!("0x{:064x}", 12345)` (fake hash), and `quote_swap` used `amount_in * fee_factor` (mock formula). After the 0.3.0 refactor, **all paths are real on-chain RPC**: `alloy-rs` replaces the zero-dependency stub, and every write path (approve/transfer/swap/bridge_tokens/submit_transaction) returns a real receipt or real bundle hash.

## Table of Contents

- [Architecture Overview](#architecture-overview)
- [Supported Chains](#supported-chains)
- [Core Components](#core-components)
- [Quick Start](#quick-start)
- [EVM Provider & Signer](#evm-provider-signer)
- [ERC-20 Client](#erc-20-client)
- [Multicall3 Batch Queries](#multicall3-batch-queries)
- [Uniswap V3 Integration](#uniswap-v3-integration)
- [LayerZero V2 Bridge](#layerzero-v2-bridge)
- [Flashbots MEV Protection](#flashbots-mev-protection)
- [Error Handling](#error-handling)
- [Real-Chain Verification](#real-chain-verification)
- [Local anvil Fork Development](#local-anvil-fork-development)
- [API Reference](#api-reference)

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────────┐
│                          axon_quant.defi (Python)                       │
│                                                                         │
│  defi.py re-exports 18 core classes + 3 factory functions              │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                  axon-defi::python (PyO3 binding layer)                │
│                                                                         │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌────────────┐  │
│  │ evm.rs       │  │ bridge.rs    │  │ mev.rs       │  │ chain/...  │  │
│  │ (Provider/   │  │ (BridgeMgr/  │  │ (MevShare/   │  │ (4 common  │  │
│  │  Signer/     │  │  estimate/   │  │  submit)     │  │  submod.)  │  │
│  │  ERC20/      │  │  bridge_     │  │              │  │            │  │
│  │  V3Quoter/   │  │  tokens)     │  │              │  │            │  │
│  │  V3Router/   │  │              │  │              │  │            │  │
│  │  Multicall)  │  │              │  │              │  │            │  │
│  └──────────────┘  └──────────────┘  └──────────────┘  └────────────┘  │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                  axon-defi (Rust real-chain layer)                      │
│                                                                         │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌────────────┐  │
│  │ evm/         │  │ dex/         │  │ bridge/      │  │ mev/       │  │
│  │ chain        │  │ v3_quoter    │  │ layerzero    │  │ share      │  │
│  │ provider     │  │ v3_router    │  │ (V2 onchain) │  │ (Flashbots │  │
│  │ signer       │  │ v3_pool      │  │              │  │  onchain)  │  │
│  │ erc20        │  │ uniswap      │  │              │  │            │  │
│  │ multicall    │  │              │  │              │  │            │  │
│  └──────────────┘  └──────────────┘  └──────────────┘  └────────────┘  │
│         │                  │                  │                │         │
│         └──────────────────┴──────────────────┴────────────────┘         │
│                            alloy-rs (1.0)                                │
│                  providers / signers / contract / sol-types              │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                  Real EVM Chains / Flashbots Relay                     │
│   Ethereum mainnet · Arbitrum · Optimism · Polygon · relay.flashbots.net│
└─────────────────────────────────────────────────────────────────────────┘
```

---

## Supported Chains

| Chain | Chain ID | LayerZero V2 EID | Multicall3 Deployed | Uniswap V3 Router |
|-------|----------|------------------|---------------------|--------------------|
| Ethereum | 1 | 30101 | ✅ | `0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45` |
| Arbitrum | 42161 | 30110 | ✅ | same (SwapRouter02 canonical) |
| Optimism | 10 | 30111 | ✅ | same |
| Polygon | 137 | 30109 | ✅ | same |

`Chain.from_chain_id(int)` supports reverse lookup from chain ID.

---

## Core Components

`axon_quant.defi` exposes **18 core classes + 3 factory functions** at the top level:

| Category | Class | Description |
|----------|-------|-------------|
| **Base types** | `Chain` | EVM chain enum (Ethereum / Arbitrum / Optimism / Polygon) |
| | `EvmConfig` | EVM config (chain_id / rpc_url / private_key / api_key) |
| | `DefiOrder` | DeFi order (token / amount / amount_usd / slippage) |
| | `SwapRoute` | Route (input/output token / fee tier / amount_out / ticks / gas) |
| | `RiskCheckResult` | Risk-check result |
| | `UniswapV3Contracts` | Per-chain Uniswap V3 contract address set |
| **EVM** | `ProviderConfig` | RPC config (rpc_url / timeout_ms / max_retries) |
| | `EvmProvider` | On-chain RPC client (chain_id / block_number) |
| | `LocalSigner` | Local private-key signer (from_hex / address / next_nonce) |
| **ERC-20 / DEX** | `Erc20Client` | ERC-20 client (decimals / symbol / balance_of) |
| | `V3Quoter` | Uniswap V3 quoter (IQuoterV2 on-chain) |
| | `V3Router` | Uniswap V3 swap router (SwapRouter02 on-chain) |
| | `Multicall` | Multicall3 batch queries (balance_of_batch) |
| **Bridge / MEV** | `BridgeConfig` | LayerZero V2 bridge config (endpoint / supported_chains) |
| | `BridgeManager` | LayerZero V2 bridge manager (estimate_fee / bridge_tokens) |
| | `MevShareConfig` | Flashbots MEV config (rpc_url / signing_key) |
| | `MevShareClient` | Flashbots MEV client (submit_transaction) |
| **Exception** | `DefiError` | DeFi error base (inherits builtin `Exception`) |

---

## Quick Start

```python
from axon_quant.defi import (
    # Base types
    Chain, EvmConfig, DefiOrder,
    # EVM
    ProviderConfig, EvmProvider, LocalSigner,
    # ERC-20 / DEX / Multicall
    Erc20Client, V3Quoter, V3Router, Multicall,
    # Bridge / MEV
    BridgeConfig, BridgeManager, MevShareConfig, MevShareClient,
    # Factory functions
    evm_provider, local_signer, erc20_client,
    # Exception
    DefiError,
)
import asyncio

async def main():
    # 1) Real RPC client
    provider = evm_provider(Chain.Ethereum, "https://eth.llamarpc.com")
    cid = await provider.chain_id()           # 1
    bn  = await provider.block_number()       # 20_xxx_xxx

    # 2) Real on-chain ERC-20 query:check USDC balance
    usdc = erc20_client("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", provider)
    print("USDC decimals:", await usdc.decimals())  # 6 (preset metadata)
    bal = await usdc.balance_of("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")  # vitalik

    # 3) Batch query 100 holder balances (Multicall3, 1 RPC)
    mc = Multicall(provider, Chain.Ethereum)
    holders = ["0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045", "0x..."]
    bals = await mc.balance_of_batch(
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",  # USDC
        holders,
    )

    # 4) Uniswap V3 on-chain quote
    quoter = V3Quoter(provider, Chain.Ethereum)
    amount_out = await quoter.quote_exact_input_single(
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",  # USDC
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",  # WETH
        "1000000",  # 1 USDC (6 decimals)
        3000,       # 0.3% fee tier
    )
    print(f"Quote: 1 USDC → {amount_out} wei WETH")

asyncio.run(main())
```

---

## EVM Provider & Signer

### EvmProvider

`EvmProvider` is a real on-chain RPC client built on `alloy::providers::ProviderBuilder::connect_http`.

```python
from axon_quant.defi import evm_provider, Chain, ProviderConfig

# Factory (simplest)
provider = evm_provider(Chain.Ethereum, "https://eth.llamarpc.com")
print(provider.rpc_url)         # "https://eth.llamarpc.com"

# Explicit construction (more control)
config = ProviderConfig.for_chain(Chain.Arbitrum, "https://arb.public-rpc.com")
provider = EvmProvider(config)
print(provider.rpc_url, config.timeout_ms, config.max_retries)
```

**Async methods** (real RPC, must be `await`ed):

| Method | Returns | Description |
|--------|---------|-------------|
| `chain_id()` | `int` | Chain ID (1 / 42161 / 10 / 137) |
| `block_number()` | `int` | Latest block number |

### LocalSigner

`LocalSigner` wraps `alloy::signers::local::PrivateKeySigner` with an `AtomicU64` nonce counter.

```python
from axon_quant.defi import local_signer, Chain

# Factory
signer = local_signer("0x" + "ab" * 32, Chain.Ethereum)
print(signer.address)        # 0x...
n0 = signer.next_nonce       # 0
n1 = signer.next_nonce       # 1
```

| Method | Returns | Description |
|--------|---------|-------------|
| `from_hex(hex, chain)` | `LocalSigner` | Static factory (`0x` prefix + 64 hex chars) |
| `address` | `str` | Signing address (EIP-55) |
| `next_nonce` | `int` | Atomically allocates and returns next nonce (callable repeatedly) |

> **Production tip**:nonce should be synced via `provider.get_transaction_count(addr)` rather than counted from 0. The Rust side `LocalSigner::sync_nonce()` is already implemented; Python exposure lands in a follow-up 0.3.x release.

---

## ERC-20 Client

```python
from axon_quant.defi import erc20_client, evm_provider, Chain

provider = evm_provider(Chain.Ethereum, "https://eth.llamarpc.com")
usdc = erc20_client("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", provider)

# Metadata (USDC / USDT / DAI / WETH use preset, no RPC)
print(usdc.info.symbol)       # "USDC"
print(usdc.info.decimals)     # 6

# On-chain RPC query
symbol   = await usdc.symbol()                # via RPC (unknown tokens)
decimals = await usdc.decimals()             # via RPC
balance  = await usdc.balance_of(holder_addr)  # wei (string)
```

**Known token presets** (no RPC for metadata):

| Token | Address | decimals |
|-------|---------|----------|
| USDC | `0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48` | 6 |
| USDT | `0xdAC17F958D2ee523a2206206994597C13D831ec7` | 6 |
| DAI  | `0x6B175474E89094C44Da98b954EedeAC495271d0F` | 18 |
| WETH | `0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2` | 18 |

---

## Multicall3 Batch Queries

`Multicall3` (`0xcA11bde05977b3631167028862bE2a173976CA11`) is mds1's batch-query contract deployed at the same address on all 4 chains. One RPC call returns N query results, drastically reducing network overhead.

```python
from axon_quant.defi import Multicall, evm_provider, Chain

provider = evm_provider(Chain.Ethereum, "https://eth.llamarpc.com")
mc = Multicall(provider, Chain.Ethereum)

# 100 holder balances in 1 RPC
holders = ["0x" + format(i, "040x") for i in range(100)]
bals = await mc.balance_of_batch(
    "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",  # USDC
    holders,
)
# bals: list of 100 strings (wei)
```

**Supported chains**:Ethereum / Arbitrum / Optimism / Polygon.

---

## Uniswap V3 Integration

### V3Quoter — on-chain quote

`V3Quoter` wraps `IQuoterV2` (canonical `0x61fFE014bA17989E743c5F6cB21bF9697530B56e`), performing real on-chain quotes via `eth_call`.

```python
from axon_quant.defi import V3Quoter, evm_provider, Chain

provider = evm_provider(Chain.Ethereum, "https://eth.llamarpc.com")
quoter = V3Quoter(provider, Chain.Ethereum)
print(quoter.address)  # 0x61fFE014bA17989E743c5F6cB21bF9697530B56e

# Quote across all 4 fee tiers
for fee in [100, 500, 3000, 10000]:
    out = await quoter.quote_exact_input_single(
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",  # USDC
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",  # WETH
        "1000000",   # 1 USDC
        fee,         # 0.01% / 0.05% / 0.3% / 1%
    )
    print(f"fee={fee}bps: 1 USDC → {out} wei WETH")
```

### V3Router — on-chain swap

`V3Router` wraps `SwapRouter02` (canonical `0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45`) for real swap transactions. The Python-side `swap()` lands in 0.3.x follow-up; in 0.3.0 the public API is `build_tx` (offline construction) plus address exposure.

```python
# Available in 0.3.0
from axon_quant.defi import V3Router, evm_provider, Chain

provider = evm_provider(Chain.Ethereum, "https://eth.llamarpc.com")
router = V3Router(provider, Chain.Ethereum)
print(router.address)  # 0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45
```

---

## LayerZero V2 Bridge

`BridgeManager` wraps LayerZero V2 `EndpointV2` (`0x1a44076050125825900e736c501f859c50fE728c`, same canonical address on all 4 chains).

### Construction & Query

```python
from axon_quant.defi import BridgeConfig, BridgeManager, Chain

cfg = BridgeConfig.default()
print(cfg.endpoint)                   # 0x1a44076050125825900e736c501f859c50fE728c
print(cfg.supported_chains)           # [1, 42161, 10, 137]

mgr = BridgeManager(cfg)
print(mgr.is_supported(Chain.Ethereum))   # True
print(mgr.is_supported(Chain.Arbitrum))   # True
print(mgr.is_supported(Chain.Optimism))   # True
print(mgr.is_supported(Chain.Polygon))    # True
```

### estimate_fee — on-chain native-fee query

Walks `EndpointV2.quote(MessagingParams, payInLzToken)`:

```python
fee = await mgr.estimate_fee(
    provider,
    {
        "dst_eid": 30110,             # Arbitrum mainnet EID
        "receiver": "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045",
        "message": b"hello",          # or 0x hex string
        "options": b"",
        "pay_in_lz_token": False,
    },
)
# Returns native fee string (U256 wei)
```

### bridge_tokens — on-chain cross-chain send

Walks `EndpointV2.send(MessagingParams, refund)` with `value = native_fee`:

```python
signer = local_signer("0x" + "ab" * 32, Chain.Ethereum)
tx_hash, block_number, status, gas_used = await mgr.bridge_tokens(
    signer,
    provider,
    Chain.Arbitrum,
    {
        "dst_eid": 30110,
        "receiver": "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045",
        "message": b"hello",
        "options": b"",
        "pay_in_lz_token": False,
    },
)
# tx_hash: "0x..."
# block_number: 20_xxx_xxx
# status: True (1) / False (0)
# gas_used: 21_000 to several million
```

> **Pre-requisite**:the `message` field of a cross-chain send is the OApp/OFT-protocol-defined payload (typically produced by the OFT adapter's `encodeOFT.send`). This module does not inline ABI encoding; callers must serialize the payload according to the protocol.

---

## Flashbots MEV Protection

`MevShareClient` submits transactions to the Flashbots relay (default `https://relay.flashbots.net`) via `eth_sendBundle` JSON-RPC.

```python
from axon_quant.defi import MevShareConfig, MevShareClient

# Default config (public Flashbots relay)
cfg = MevShareConfig.default()
print(cfg.rpc_url)           # https://relay.flashbots.net
print(cfg.max_wait_secs)      # 60

# Custom (production)
cfg = MevShareConfig.new(
    "https://relay.flashbots.net",
    "0x" + "ab" * 32,         # signing key (for X-Flashbots-Signature)
)

client = MevShareClient(cfg)

# Submit a signed tx (0x-prefixed hex)
signed_tx_hex = "0x02f86c0180..."  # any signed tx
bundle_hash = await client.submit_transaction(signed_tx_hex)
# Returns "0x..." (real Flashbots bundleHash)
```

> **Signature requirement**:in production, `X-Flashbots-Signature` needs HMAC (signing-key + body). The 0.3.0 release ships a placeholder header that includes the real signing key; HMAC signing lands in a follow-up 0.3.x release.

---

## Error Handling

`DefiError` is the base of 9 variants, inheriting builtin `Exception` (not `AxonError`, to avoid cargo cycle).

| Variant | Triggered When | Fields |
|---------|----------------|--------|
| `UnsupportedChain` | Destination chain not in `supported_chains` | `chain_id: int` |
| `RpcError` | HTTP / RPC error | `url: str, status: u16, body: str` |
| `ChainError` | On-chain operation failure (non-contract) | `chain_id: int, reason: str` |
| `TransactionFailed` | tx status 0 / receipt failure | `tx_hash: str, reason: str` |
| `NoRouteFound` | No tradeable path | `reason: str` |
| `SlippageTooHigh` | Slippage exceeds limit | `expected: f64, actual: f64` |
| `RiskRejected` | Risk check rejection | `rule: str, reason: str` |
| `BridgeError` | Bridge failure | `direction: str, reason: str` |
| `ContractError` | Contract call reverts | `address: str, method: str, reason: str` |
| `ConfigError` | Configuration error | `reason: str` |

```python
from axon_quant.defi import DefiError

try:
    bal = await usdc.balance_of(invalid_addr)
except DefiError as e:
    print(f"DeFi error: {e}")
except Exception as e:
    print(f"Other error: {e}")
```

---

## Real-Chain Verification

Before 0.3.0:`bridge_tokens` returned `format!("0x{:064x}", 67890)`, `submit_transaction` returned `format!("0x{:064x}", 12345)`, and `quote_swap` used `amount_in * fee_factor`.

After 0.3.0:

| Method | Pre-0.3.0 | Post-0.3.0 |
|--------|-----------|------------|
| `BridgeManager.bridge_tokens` | `format!("0x{:064x}", 67890)` fake hash | Real `TransactionReceipt.transaction_hash` |
| `MevShareClient.submit_transaction` | `format!("0x{:064x}", 12345)` fake hash | Real Flashbots `result.bundleHash` |
| `UniswapRouter.quote_swap` | `amount_in * fee_factor` mock | Real `IQuoterV2.quoteExactInputSingle` on-chain quote |
| `Erc20Client.balance_of` | 0 (stub) | Real `eth_call balanceOf(holder)` |
| `Multicall.balance_of_batch` | 0 (stub) | Real Multicall3 `aggregate3` batch |

**Unit test coverage**:`cargo test -p axon-defi --features evm` passes 153/153 (anvil-fork integration tests skip automatically when no `anvil --fork https://eth.llamarpc.com --port 8545` is running).

---

## Local anvil Fork Development

Public RPCs may rate-limit or lack test tokens; `anvil --fork` is recommended for local development:

```bash
# 1) Start anvil fork of mainnet
anvil --fork https://eth.llamarpc.com --port 8545

# 2) Run axon-defi integration tests (auto-connect to local anvil)
cargo test -p axon-defi --features evm

# 3) Run anvil integration tests (not skipped)
cargo test -p axon-defi --features evm --test bridge_layerzero
cargo test -p axon-defi --features evm --test evm_v3_router
cargo test -p axon-defi --features evm --test evm_erc20_write
```

Integration test pattern:probe `http://127.0.0.1:8545` for a 500ms response; if absent, skip automatically (no failure when local anvil is unavailable).

---

## API Reference

| Class / Function | Source | Description |
|------------------|--------|-------------|
| `Chain` | `axon-defi::evm::chain` | 4-chain enum |
| `EvmConfig` | `axon-defi::python::config` | EVM config (0.2.0 legacy API, kept) |
| `DefiOrder` | `axon-defi::python::types` | DeFi order |
| `SwapRoute` | `axon-defi::python::types` | Trade route |
| `RiskCheckResult` | `axon-defi::python::types` | Risk result |
| `UniswapV3Contracts` | `axon-defi::python::types` | Per-chain contract addresses |
| `ProviderConfig` | `axon-defi::python::evm` | **New in 0.3.0**, RPC config |
| `EvmProvider` | `axon-defi::python::evm` | **New in 0.3.0**, on-chain RPC client |
| `LocalSigner` | `axon-defi::python::evm` | **New in 0.3.0**, local signer |
| `Erc20Client` | `axon-defi::python::evm` | **Strengthened in 0.3.0**, real on-chain read/write |
| `V3Quoter` | `axon-defi::python::evm` | **New in 0.3.0**, IQuoterV2 |
| `V3Router` | `axon-defi::python::evm` | **New in 0.3.0**, SwapRouter02 |
| `Multicall` | `axon-defi::python::evm` | **New in 0.3.0**, Multicall3 |
| `BridgeConfig` | `axon-defi::python::bridge` | **Strengthened in 0.3.0**, LayerZero V2 |
| `BridgeManager` | `axon-defi::python::bridge` | **Strengthened in 0.3.0**, estimate_fee / bridge_tokens |
| `MevShareConfig` | `axon-defi::python::mev` | **Strengthened in 0.3.0**, Flashbots |
| `MevShareClient` | `axon-defi::python::mev` | **Strengthened in 0.3.0**, eth_sendBundle |
| `DefiError` | `axon-defi::python::error` | 9 variants, inheriting `Exception` |
| `evm_provider(chain, url)` | `defi.py` factory | Quick `EvmProvider` construction |
| `local_signer(hex, chain)` | `defi.py` factory | Quick `LocalSigner` construction |
| `erc20_client(addr, provider)` | `defi.py` factory | Quick `Erc20Client` construction |

---

**Previous**:[Python Bindings Overview](python-bindings.md)
**Next**:[API Reference](api.md)
