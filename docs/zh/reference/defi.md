# DeFi 链上交易 (Python 绑定)

> 适用版本:AXON v0.3.0+(`axon-defi` 0.3.0 P0 Batch 1-4 完整交付)
> 上游:[axon-defi Rust crate](https://github.com/pengwow/axon_quant/blob/main/crates/axon-defi/) + [axon_quant.defi 顶层 wrapper](https://github.com/pengwow/axon_quant/blob/main/python/axon_quant/defi.py)
> 设计文档:[DeFi 链上交易架构设计](https://github.com/pengwow/axon_quant/blob/main/.axon-internal/specs/2026-06-21-defi-onchain-trading-design.md)
> 完整可运行示例:`examples/17_python_bindings/python_bindings_demo.py` 中的 defi 段

0.3.0 P0 之前 `axon-defi` 整 crate 是"壳":`bridge_tokens` 返回 `format!("0x{:064x}", 67890)` 假 hash,`submit_transaction` 返回 `format!("0x{:064x}", 12345)` 假 hash,`quote_swap` 用 `amount_in * fee_factor` 模拟公式。0.3.0 改造后,**全部走真链 RPC**:用 `alloy-rs` 替代零依赖空实现,所有写路径(approve/transfer/swap/bridge_tokens/submit_transaction)都有真 receipt / 真 bundle hash。

## 目录

- 架构概览
- 支持的链
- 核心组件
- 快速上手
- EVM Provider & Signer
- ERC-20 客户端
- Multicall3 批量查询
- Uniswap V3 接入
- LayerZero V2 跨链桥
- Flashbots MEV 保护
- 错误处理
- 真链接入验证
- 本地 anvil fork 开发
- API 参考

---

## 架构概览

```
┌─────────────────────────────────────────────────────────────────────────┐
│                          axon_quant.defi (Python)                       │
│                                                                         │
│  defi.py 重新导出 18 个核心类 + 3 个工厂函数                            │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                  axon-defi::python (PyO3 绑定层)                        │
│                                                                         │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌────────────┐  │
│  │ evm.rs       │  │ bridge.rs    │  │ mev.rs       │  │ chain/...  │  │
│  │ (Provider/   │  │ (BridgeMgr/  │  │ (MevShare/   │  │ (4 公共    │  │
│  │  Signer/     │  │  estimate/   │  │  submit)     │  │  子模块)   │  │
│  │  ERC20/      │  │  bridge_     │  │              │  │            │  │
│  │  V3Quoter/   │  │  tokens)     │  │              │  │            │  │
│  │  V3Router/   │  │              │  │              │  │            │  │
│  │  Multicall)  │  │              │  │              │  │            │  │
│  └──────────────┘  └──────────────┘  └──────────────┘  └────────────┘  │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                  axon-defi (Rust 真链交互层)                            │
│                                                                         │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌────────────┐  │
│  │ evm/         │  │ dex/         │  │ bridge/      │  │ mev/       │  │
│  │ chain        │  │ v3_quoter    │  │ layerzero    │  │ share      │  │
│  │ provider     │  │ v3_router    │  │ (V2 真链)    │  │ (Flashbots │  │
│  │ signer       │  │ v3_pool      │  │              │  │  真链)     │  │
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
│                     真 EVM 链 / Flashbots Relay                         │
│   Ethereum mainnet · Arbitrum · Optimism · Polygon · relay.flashbots.net│
└─────────────────────────────────────────────────────────────────────────┘
```

---

## 支持的链

| 链 | Chain ID | LayerZero V2 EID | Multicall3 部署 | Uniswap V3 Router |
|----|----------|------------------|------------------|--------------------|
| Ethereum | 1 | 30101 | ✅ | `0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45` |
| Arbitrum | 42161 | 30110 | ✅ | 同上(SwapRouter02 canonical) |
| Optimism | 10 | 30111 | ✅ | 同上 |
| Polygon | 137 | 30109 | ✅ | 同上 |

`Chain.from_chain_id(int)` 支持从 chain ID 反查。

---

## 核心组件

`axon_quant.defi` 顶层一次性暴露 **18 个核心类 + 3 个工厂函数**:

| 类别 | 类 | 说明 |
|------|-----|------|
| **基础类型** | `Chain` | EVM 链枚举(Ethereum / Arbitrum / Optimism / Polygon) |
| | `EvmConfig` | EVM 配置(chain_id / rpc_url / private_key / api_key) |
| | `DefiOrder` | DeFi 订单(token / amount / amount_usd / slippage) |
| | `SwapRoute` | 路由(input/output token / fee tier / amount_out / ticks / gas) |
| | `RiskCheckResult` | 风控检查结果 |
| | `UniswapV3Contracts` | 4 链 Uniswap V3 合约地址集合 |
| **EVM** | `ProviderConfig` | RPC 配置(rpc_url / timeout_ms / max_retries) |
| | `EvmProvider` | 真链 RPC 客户端(chain_id / block_number) |
| | `LocalSigner` | 本地私钥签名器(from_hex / address / next_nonce) |
| **ERC-20 / DEX** | `Erc20Client` | ERC-20 客户端(decimals / symbol / balance_of) |
| | `V3Quoter` | Uniswap V3 报价器(IQuoterV2 真链) |
| | `V3Router` | Uniswap V3 交易路由器(SwapRouter02 真链) |
| | `Multicall` | Multicall3 批量查询(balance_of_batch) |
| **Bridge / MEV** | `BridgeConfig` | LayerZero V2 桥配置(endpoint / supported_chains) |
| | `BridgeManager` | LayerZero V2 桥管理(estimate_fee / bridge_tokens) |
| | `MevShareConfig` | Flashbots MEV 配置(rpc_url / signing_key) |
| | `MevShareClient` | Flashbots MEV 客户端(submit_transaction) |
| **异常** | `DefiError` | DeFi 异常基类(继承 builtin `Exception`) |

---

## 快速上手

```python
from axon_quant.defi import (
    # 基础类型
    Chain, EvmConfig, DefiOrder,
    # EVM
    ProviderConfig, EvmProvider, LocalSigner,
    # ERC-20 / DEX / Multicall
    Erc20Client, V3Quoter, V3Router, Multicall,
    # Bridge / MEV
    BridgeConfig, BridgeManager, MevShareConfig, MevShareClient,
    # 工厂函数
    evm_provider, local_signer, erc20_client,
    # 异常
    DefiError,
)
import asyncio

async def main():
    # 1) 真实 RPC 客户端
    provider = evm_provider(Chain.Ethereum, "https://eth.llamarpc.com")
    cid = await provider.chain_id()           # 1
    bn  = await provider.block_number()       # 20_xxx_xxx

    # 2) 真链 ERC-20 查询:查 USDC 余额
    usdc = erc20_client("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", provider)
    print("USDC decimals:", await usdc.decimals())  # 6(预设元信息)
    bal = await usdc.balance_of("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")  # vitalik

    # 3) 批量查 100 个 holder 余额(Multicall3,1 次 RPC)
    mc = Multicall(provider, Chain.Ethereum)
    holders = ["0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045", "0x..."]
    bals = await mc.balance_of_batch(
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",  # USDC
        holders,
    )

    # 4) Uniswap V3 真链 quote
    quoter = V3Quoter(provider, Chain.Ethereum)
    amount_out = await quoter.quote_exact_input_single(
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",  # USDC
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",  # WETH
        "1000000",  # 1 USDC(6 decimals)
        3000,       # 0.3% fee tier
    )
    print(f"Quote: 1 USDC → {amount_out} wei WETH")

asyncio.run(main())
```

---

## EVM Provider & Signer

### EvmProvider

`EvmProvider` 是真链 RPC 客户端,内部用 `alloy::providers::ProviderBuilder::connect_http` 构造。

```python
from axon_quant.defi import evm_provider, Chain, ProviderConfig

# 工厂函数(最简)
provider = evm_provider(Chain.Ethereum, "https://eth.llamarpc.com")
print(provider.rpc_url)         # "https://eth.llamarpc.com"

# 显式构造(更多控制)
config = ProviderConfig.for_chain(Chain.Arbitrum, "https://arb.public-rpc.com")
provider = EvmProvider(config)
print(provider.rpc_url, config.timeout_ms, config.max_retries)
```

**异步方法**(走真 RPC,需要 `await`):

| 方法 | 返回 | 说明 |
|------|------|------|
| `chain_id()` | `int` | 链 ID(1 / 42161 / 10 / 137) |
| `block_number()` | `int` | 最新区块号 |

### LocalSigner

`LocalSigner` 包装 `alloy::signers::local::PrivateKeySigner`,`AtomicU64` 持 nonce。

```python
from axon_quant.defi import local_signer, Chain

# 工厂函数
signer = local_signer("0x" + "ab" * 32, Chain.Ethereum)
print(signer.address)        # 0x...
n0 = signer.next_nonce       # 0
n1 = signer.next_nonce       # 1
```

| 方法 | 返回 | 说明 |
|------|------|------|
| `from_hex(hex, chain)` | `LocalSigner` | 静态工厂(0x 前缀 + 64 hex chars) |
| `address` | `str` | 签名地址(EIP-55) |
| `next_nonce` | `int` | 原子分配并返回下一个 nonce(可调用多次) |

> **生产提示**:nonce 应通过 `provider.get_transaction_count(addr)` 同步,而非从 0 计数。Rust 端 `LocalSigner::sync_nonce()` 已实现,Python 端暴露在后续版本。

---

## ERC-20 客户端

```python
from axon_quant.defi import erc20_client, evm_provider, Chain

provider = evm_provider(Chain.Ethereum, "https://eth.llamarpc.com")
usdc = erc20_client("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", provider)

# 元信息(USDC / USDT / DAI / WETH 走预设,无需 RPC)
print(usdc.info.symbol)       # "USDC"
print(usdc.info.decimals)     # 6

# 真链 RPC 查询
symbol  = await usdc.symbol()                # 走 RPC(未知 token)
decimals = await usdc.decimals()             # 走 RPC
balance = await usdc.balance_of(holder_addr)  # wei 单位(string)
```

**已知 token 预设**(走预设元信息,无需 RPC):

| Token | 地址 | decimals |
|-------|------|----------|
| USDC | `0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48` | 6 |
| USDT | `0xdAC17F958D2ee523a2206206994597C13D831ec7` | 6 |
| DAI  | `0x6B175474E89094C44Da98b954EedeAC495271d0F` | 18 |
| WETH | `0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2` | 18 |

---

## Multicall3 批量查询

`Multicall3`(`0xcA11bde05977b3631167028862bE2a173976CA11`)是 mds1 在 4 链同地址部署的批量查询合约,1 次 RPC 拿 N 个查询结果,显著降低网络开销。

```python
from axon_quant.defi import Multicall, evm_provider, Chain

provider = evm_provider(Chain.Ethereum, "https://eth.llamarpc.com")
mc = Multicall(provider, Chain.Ethereum)

# 100 个 holder 余额 1 次 RPC
holders = ["0x" + format(i, "040x") for i in range(100)]
bals = await mc.balance_of_batch(
    "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",  # USDC
    holders,
)
# bals: list of 100 strings(wei 单位)
```

**支持的链**:Ethereum / Arbitrum / Optimism / Polygon(4 链全支持)。

---

## Uniswap V3 接入

### V3Quoter — 真链报价

`V3Quoter` 封装 `IQuoterV2`(canonical `0x61fFE014bA17989E743c5F6cB21bF9697530B56e`),走 `eth_call` 拿真实 quote。

```python
from axon_quant.defi import V3Quoter, evm_provider, Chain

provider = evm_provider(Chain.Ethereum, "https://eth.llamarpc.com")
quoter = V3Quoter(provider, Chain.Ethereum)
print(quoter.address)  # 0x61fFE014bA17989E743c5F6cB21bF9697530B56e

# 4 个 fee tier 都可报价
for fee in [100, 500, 3000, 10000]:
    out = await quoter.quote_exact_input_single(
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",  # USDC
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",  # WETH
        "1000000",   # 1 USDC
        fee,         # 0.01% / 0.05% / 0.3% / 1%
    )
    print(f"fee={fee}bps: 1 USDC → {out} wei WETH")
```

### V3Router — 真链 swap

`V3Router` 封装 `SwapRouter02`(canonical `0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45`),走真发交易。Python 端 `swap()` 自 0.6.0 起可用,见 `axon_quant.defi.UniswapV3.swap()`(0.3.0 收口时先通过 `UniswapRouter::swap` Rust API + 工厂 `build_tx` 提供离线构造)。

```python
# 0.3.0 收口起已可读取 router 地址
from axon_quant.defi import V3Router, evm_provider, Chain

provider = evm_provider(Chain.Ethereum, "https://eth.llamarpc.com")
router = V3Router(provider, Chain.Ethereum)
print(router.address)  # 0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45
```

---

## LayerZero V2 跨链桥

`BridgeManager` 封装 LayerZero V2 `EndpointV2`(`0x1a44076050125825900e736c501f859c50fE728c`,4 链同地址 canonical)。

### 构造与查询

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

### estimate_fee — 真链查询 native fee

走 `EndpointV2.quote(MessagingParams, payInLzToken)`:

```python
fee = await mgr.estimate_fee(
    provider,
    {
        "dst_eid": 30110,             # Arbitrum mainnet EID
        "receiver": "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045",
        "message": b"hello",          # 或 0x hex string
        "options": b"",
        "pay_in_lz_token": False,
    },
)
# 返回 native fee 字符串(U256 wei)
```

### bridge_tokens — 真链发跨链

走 `EndpointV2.send(MessagingParams, refund)`,带 `value = native_fee`:

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
# status: True(1) / False(0)
# gas_used: 21_000 ~ 数百万
```

> **前置步骤**:跨链转账的 `message` 字段是 OApp/OFT 协议自定义的 payload(通常由 OFT adapter 的 `encodeOFT.send` 构造)。本模块不内联 ABI 编码,调用方按协议自行序列化。

---

## Flashbots MEV 保护

`MevShareClient` 走 `eth_sendBundle` JSON-RPC 提交到 Flashbots relay(默认 `https://relay.flashbots.net`)。

```python
from axon_quant.defi import MevShareConfig, MevShareClient

# 默认配置(public Flashbots relay)
cfg = MevShareConfig.default()
print(cfg.rpc_url)           # https://relay.flashbots.net
print(cfg.max_wait_secs)      # 60

# 自定义(生产环境)
cfg = MevShareConfig.new(
    "https://relay.flashbots.net",
    "0x" + "ab" * 32,         # 签名私钥(用于 X-Flashbots-Signature)
)

client = MevShareClient(cfg)

# 提交 signed tx(0x 前缀 hex)
signed_tx_hex = "0x02f86c0180..."  # 任意已签名交易
bundle_hash = await client.submit_transaction(signed_tx_hex)
# 返回 "0x..."(Flashbots bundleHash,真值)
```

> **签名要求**:生产环境 `X-Flashbots-Signature` 需要 HMAC 签名(签名私钥 + body)。0.6.0 仍以占位符 + 真实 signing_key 上送为主(开发环境够用),生产级 HMAC 完整支持规划在 0.7.0+(详见 [axon-defi Roadmap](https://github.com/pengwow/axon_quant/issues))。

---

## 错误处理

`DefiError` 是 9 个变体的基类,继承 builtin `Exception`(非 `AxonError`,避免 cargo 循环)。

| 变体 | 触发条件 | 字段 |
|------|----------|------|
| `UnsupportedChain` | 目标链不在 supported_chains | `chain_id: int` |
| `RpcError` | HTTP / RPC 错误 | `url: str, status: u16, body: str` |
| `ChainError` | 链上操作失败(非合约层) | `chain_id: int, reason: str` |
| `TransactionFailed` | tx 状态 0 / receipt 失败 | `tx_hash: str, reason: str` |
| `NoRouteFound` | 无可用交易路径 | `reason: str` |
| `SlippageTooHigh` | 滑点超限 | `expected: f64, actual: f64` |
| `RiskRejected` | 风控拒绝 | `rule: str, reason: str` |
| `BridgeError` | 跨链桥失败 | `direction: str, reason: str` |
| `ContractError` | 合约调用 revert | `address: str, method: str, reason: str` |
| `ConfigError` | 配置错误 | `reason: str` |

```python
from axon_quant.defi import DefiError

try:
    bal = await usdc.balance_of(invalid_addr)
except DefiError as e:
    print(f"DeFi 错误: {e}")
except Exception as e:
    print(f"其他错误: {e}")
```

---

## 真链接入验证

0.3.0 之前:`bridge_tokens` 返回 `format!("0x{:064x}", 67890)`,`submit_transaction` 返回 `format!("0x{:064x}", 12345)`,`quote_swap` 用 `amount_in * fee_factor` 模拟。

0.3.0 改造后:

| 方法 | 0.3.0 前 | 0.3.0 后 |
|------|----------|----------|
| `BridgeManager.bridge_tokens` | `format!("0x{:064x}", 67890)` 假 hash | 真 `TransactionReceipt.transaction_hash` |
| `MevShareClient.submit_transaction` | `format!("0x{:064x}", 12345)` 假 hash | 真 Flashbots `result.bundleHash` |
| `UniswapRouter.quote_swap` | `amount_in * fee_factor` 模拟 | 真 `IQuoterV2.quoteExactInputSingle` 链上 quote |
| `Erc20Client.balance_of` | 0(stub) | 真 `eth_call balanceOf(holder)` |
| `Multicall.balance_of_batch` | 0(stub) | 真 Multicall3 `aggregate3` 批量 |

**单元测试覆盖**:`cargo test -p axon-defi --features evm` 153/153 通过(其中 anvil fork 集成测试需 `anvil --fork https://eth.llamarpc.com --port 8545` 启动时跑通真链交互)。

---

## 本地 anvil fork 开发

真链 RPC 可能限流或缺 USDT 等测试 token,推荐用 `anvil --fork` 做本地开发:

```bash
# 1) 启动 anvil fork mainnet
anvil --fork https://eth.llamarpc.com --port 8545

# 2) 跑 axon-defi 集成测试(自动连本机 anvil)
cargo test -p axon-defi --features evm

# 3) 跑 anvil 集成测试(不会被 skip)
cargo test -p axon-defi --features evm --test bridge_layerzero
cargo test -p axon-defi --features evm --test evm_v3_router
cargo test -p axon-defi --features evm --test evm_erc20_write
```

集成测试模式:探测 `http://127.0.0.1:8545` 是否能 500ms 内响应,否则自动 skip(本地无 anvil 时不报失败)。

---

## API 参考

| 类 / 函数 | 来源 | 说明 |
|-----------|------|------|
| `Chain` | `axon-defi::evm::chain` | 4 链枚举 |
| `EvmConfig` | `axon-defi::python::config` | EVM 配置(0.3.0 旧 API,保留) |
| `DefiOrder` | `axon-defi::python::types` | DeFi 订单 |
| `SwapRoute` | `axon-defi::python::types` | 交易路由 |
| `RiskCheckResult` | `axon-defi::python::types` | 风控结果 |
| `UniswapV3Contracts` | `axon-defi::python::types` | 4 链合约地址 |
| `ProviderConfig` | `axon-defi::python::evm` | 0.3.0 起,RPC 配置 |
| `EvmProvider` | `axon-defi::python::evm` | 0.3.0 起,真链 RPC 客户端 |
| `LocalSigner` | `axon-defi::python::evm` | 0.3.0 起,本地签名器 |
| `Erc20Client` | `axon-defi::python::evm` | 0.3.0 起,真链 read/write |
| `V3Quoter` | `axon-defi::python::evm` | 0.3.0 起,IQuoterV2 |
| `V3Router` | `axon-defi::python::evm` | 0.3.0 起,SwapRouter02 |
| `Multicall` | `axon-defi::python::evm` | 0.3.0 起,Multicall3 |
| `BridgeConfig` | `axon-defi::python::bridge` | 0.3.0 起,LayerZero V2 |
| `BridgeManager` | `axon-defi::python::bridge` | 0.3.0 起,estimate_fee / bridge_tokens |
| `MevShareConfig` | `axon-defi::python::mev` | 0.3.0 起,Flashbots |
| `MevShareClient` | `axon-defi::python::mev` | 0.3.0 起,eth_sendBundle |
| `DefiError` | `axon-defi::python::error` | 9 变体,继承 Exception |
| `evm_provider(chain, url)` | `defi.py` 工厂 | 快速构造 EvmProvider |
| `local_signer(hex, chain)` | `defi.py` 工厂 | 快速构造 LocalSigner |
| `erc20_client(addr, provider)` | `defi.py` 工厂 | 快速构造 Erc20Client |

---

**上一节**:[Python 绑定总览](python-bindings.md)
**下一节**:[API 参考](api.md)
