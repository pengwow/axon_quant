"""axon_quant.defi 顶层 Python API —— thin wrapper 模式(0.3.0 P0 Batch 4)。

约定:
- 核心实现走 ``axon_quant._native.defi``(PyO3 绑定)
- 本模块负责:
  * 重新导出 DeFi 链上交易全套类型(EvmProvider / LocalSigner /
    Erc20Client / V3Quoter / V3Router / Multicall / BridgeManager /
    MevShareClient / Chain / EvmConfig / DefiOrder / SwapRoute /
    RiskCheckResult / UniswapV3Contracts)
  * 工厂函数 ``evm_provider()`` / ``local_signer()`` /
    ``erc20_client()`` 减少样板代码
  * 异常 ``DefiError`` 的解释(继承 builtin ``PyException`` 而非
    ``AxonError``,避免 ``axon-defi`` 反向依赖 ``axon-python`` 造成
    cargo 循环)

核心组件:
- Provider:``EvmProvider`` —— ``chain_id()`` / ``block_number()``
- 签名器:``LocalSigner`` —— ``from_hex()`` / ``address`` / ``next_nonce()``
- ERC-20:``Erc20Client`` —— ``decimals()`` / ``symbol()`` / ``balance_of()``
- V3 Quoter:``V3Quoter`` —— ``quote_exact_input_single()``
- V3 Router:``V3Router`` —— Uniswap V3 SwapRouter02 地址
- Multicall:``Multicall`` —— ``balance_of_batch()`` 一次 RPC 拿 N 余额
- Bridge:``BridgeManager`` —— LayerZero V2 ``is_supported()`` / ``config``
- MEV:``MevShareClient`` —— ``submit_transaction()`` Flashbots ``eth_sendBundle``
- 类型:``Chain`` / ``EvmConfig`` / ``DefiOrder`` / ``SwapRoute`` /
  ``RiskCheckResult`` / ``UniswapV3Contracts`` / ``ProviderConfig``
- 异常:``DefiError`` —— 继承 builtin ``PyException``

设计要点:
- 0.3.0 P0 三个核心写路径(``erc20.approve`` / ``erc20.transfer`` /
  ``v3_router.swap``)都走真链 RPC,见 ``axon-defi::evm`` /
  ``axon-defi::dex::v3_router`` 实现
- 全部 7 个子模块的类**扁平暴露**到 ``_native.defi``(即
  ``_native.defi.EvmProvider`` 而非 ``_native.defi.evm.EvmProvider``)

用法::

    from axon_quant.defi import (
        Chain, EvmConfig, DefiOrder,
        evm_provider, local_signer, erc20_client,
        V3Quoter, Multicall, BridgeManager, MevShareClient,
        DefiError,
    )

    # 1) 真链 RPC:Ethereum mainnet
    provider = evm_provider(Chain.Ethereum, "https://eth.llamarpc.com")
    cid = await provider.chain_id()  # 1

    # 2) 查 USDC 余额
    usdc = erc20_client(
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",  # USDC mainnet
        provider,
    )
    bal = await usdc.balance_of("0x0000000000000000000000000000000000000000")

    # 3) 批量查 holder 余额(Multicall3,1 次 RPC)
    mc = Multicall(provider, Chain.Ethereum)
    bals = await mc.balance_of_batch(usdc_addr, [h1, h2, h3])

    # 4) Uniswap V3 quote
    quoter = V3Quoter(provider, Chain.Ethereum)
    amount_out = await quoter.quote_exact_input_single(
        usdc_addr, weth_addr, "1000000", 3000,  # 1 USDC, fee=0.3%
    )

    # 5) LayerZero bridge
    bridge = BridgeManager(BridgeConfig.default())
    assert bridge.is_supported(Chain.Arbitrum)

    # 6) Flashbots MEV bundle
    mev = MevShareClient(MevShareConfig.default())
    bh = await mev.submit_transaction(signed_tx_hex)
"""
from __future__ import annotations

from typing import Optional

# 重新导出原生符号(0.3.0 P0 Batch 4 全量)
# 注意:`_native` 是 cdylib 单文件扩展(不是 Python package 目录),
# 所以 `from axon_quant._native.defi import ...` 这种 dot 路径不可用;
# 改用 `from axon_quant._native import defi` 先把子模块对象取出来,
# 再用属性访问取出类(与 `oms.py` / `risk.py` 保持一致)。
from axon_quant._native import defi as _native_defi_module  # noqa: E402

# ── 基础类型 ──────────────────────────────────────────────────────────
Chain = _native_defi_module.Chain
EvmConfig = _native_defi_module.EvmConfig
DefiOrder = _native_defi_module.DefiOrder
SwapRoute = _native_defi_module.SwapRoute
RiskCheckResult = _native_defi_module.RiskCheckResult
UniswapV3Contracts = _native_defi_module.UniswapV3Contracts

# ── EVM Provider / Signer ─────────────────────────────────────────────
ProviderConfig = _native_defi_module.ProviderConfig
EvmProvider = _native_defi_module.EvmProvider
LocalSigner = _native_defi_module.LocalSigner

# ── ERC-20 / Multicall / V3 ──────────────────────────────────────────
Erc20Client = _native_defi_module.Erc20Client
V3Quoter = _native_defi_module.V3Quoter
V3Router = _native_defi_module.V3Router
Multicall = _native_defi_module.Multicall

# ── Bridge / MEV ──────────────────────────────────────────────────────
BridgeConfig = _native_defi_module.BridgeConfig
BridgeManager = _native_defi_module.BridgeManager
MevShareConfig = _native_defi_module.MevShareConfig
MevShareClient = _native_defi_module.MevShareClient

# ── 异常:DefiError 继承 builtin PyException(避免 cargo 循环) ─────────
# 这里不继承 AxonError(Stage 1 实战发现 cargo 循环不可行)。
# Python 端可走 `except Exception` 统一捕获。
DefiError = _native_defi_module.DefiError


# ═══════════════════════════════════════════════════════════════════════
# 工厂函数(减少样板)
# ═══════════════════════════════════════════════════════════════════════


def evm_provider(chain: Chain, rpc_url: str) -> EvmProvider:
    """按链 + RPC URL 快速构造 ``EvmProvider``。

    等价于 ``EvmProvider(ProviderConfig.for_chain(chain, rpc_url))``。

    Args:
        chain: 目标 EVM 链(``Chain.Ethereum`` / ``Arbitrum`` /
            ``Optimism`` / ``Polygon``)
        rpc_url: HTTP RPC 端点(支持 Infura / Alchemy / LlamaRPC /
            本地 anvil fork)

    Returns:
        ``EvmProvider`` 实例,可直接 ``await`` 真链 RPC 调用

    Example::

        provider = evm_provider(Chain.Ethereum, "https://eth.llamarpc.com")
        cid = await provider.chain_id()  # 1
    """
    return EvmProvider(ProviderConfig.for_chain(chain, rpc_url))


def local_signer(hex_private_key: str, chain: Chain) -> LocalSigner:
    """从 hex 私钥构造 ``LocalSigner``。

    Args:
        hex_private_key: 0x 前缀 + 64 hex chars(32 字节)
        chain: 目标 EVM 链(用于 chain_id 关联)

    Returns:
        ``LocalSigner`` 实例,可读 ``.address`` 拿到签名地址

    Raises:
        DefiError: 私钥长度不合法 / 解析失败

    Example::

        signer = local_signer("0x" + "ab" * 32, Chain.Ethereum)
        print(signer.address)  # 0x...
    """
    return LocalSigner.from_hex(hex_private_key, chain)


def erc20_client(
    token_address: str,
    provider: EvmProvider,
) -> Erc20Client:
    """构造 ERC-20 客户端(USDC / USDT / DAI / WETH 自动用预设元信息)。

    Args:
        token_address: token 合约地址(可不校验大小写)
        provider: 已构造好的 ``EvmProvider``

    Returns:
        ``Erc20Client`` 实例

    Example::

        usdc = erc20_client(
            "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",  # USDC mainnet
            provider,
        )
        bal = await usdc.balance_of(holder_addr)
    """
    return Erc20Client(token_address, provider)


__all__ = [
    # 基础类型
    "Chain",
    "EvmConfig",
    "DefiOrder",
    "SwapRoute",
    "RiskCheckResult",
    "UniswapV3Contracts",
    # EVM Provider / Signer
    "ProviderConfig",
    "EvmProvider",
    "LocalSigner",
    # ERC-20 / Multicall / V3
    "Erc20Client",
    "V3Quoter",
    "V3Router",
    "Multicall",
    # Bridge / MEV
    "BridgeConfig",
    "BridgeManager",
    "MevShareConfig",
    "MevShareClient",
    # 异常
    "DefiError",
    # 工厂函数
    "evm_provider",
    "local_signer",
    "erc20_client",
]
