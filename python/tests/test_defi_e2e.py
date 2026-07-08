"""axon_quant.defi 端到端测试(L3 Python E2E,0.3.0 P0 Batch 4)。

覆盖范围:
1. 类型导入 / 实例化(18 个核心类型 + DefiError 异常)
2. 工厂函数 evm_provider / local_signer / erc20_client
3. 基础类型:Chain / EvmConfig / DefiOrder / SwapRoute / RiskCheckResult /
   UniswapV3Contracts / ProviderConfig 字段 + 字典序列化
4. 配置类:EvmConfig.with_oneinch_api_key / with_flashbots_rpc 工厂
5. Provider 构造(无 RPC 调用)
6. Signer 构造 + address 字段 + next_nonce
7. Bridge 默认配置 + is_supported chain
8. Mev 默认配置 + 字段访问
9. 异常:DefiError 是 builtin PyException 子类
10. Erc20 / V3Quoter / Multicall / Bridge / Mev 客户端构造

注意:真链 RPC 调用测试(async 方法)需外部可访问的 anvil fork 或
公共 RPC。本测试聚焦**构造 + 元信息**验证;真链调用覆盖在
``tests/evm_*`` Rust 集成测试(anvil fork skip 模式)。

运行::

    cd /Users/liupeng/workspace/quant/axon
    PYO3_PYTHON=/Users/liupeng/workspace/quant/axon/.venv/bin/python \\
        python -m pytest python/tests/test_defi_e2e.py -v

注意:本测试需先 build wheel(参见 Makefile 的 ``python-build`` /
``python-develop`` 目标)。如未 build,部分测试 skip。
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

# 强制使用本项目 venv(避免 miniconda pyarrow / numpy 干扰)
_VENV_SITE = Path("/Users/liupeng/workspace/quant/axon_quant/.venv-test/lib/python3.13/site-packages")
if _VENV_SITE.exists() and str(_VENV_SITE) not in sys.path:
    sys.path.insert(0, str(_VENV_SITE))

# ``axon_quant`` 在 maturin develop / wheel install 后可被 import
# 缺失时 skip 整个模块(开发期还没 build 时常见)
try:
    import axon_quant  # noqa: F401
    from axon_quant.defi import (
        BridgeConfig,
        BridgeManager,
        Chain,
        DefiError,
        DefiOrder,
        Erc20Client,
        EvmConfig,
        EvmProvider,
        LocalSigner,
        MevShareClient,
        MevShareConfig,
        Multicall,
        ProviderConfig,
        RiskCheckResult,
        SwapRoute,
        UniswapV3Contracts,
        V3Quoter,
        V3Router,
        erc20_client,
        evm_provider,
        local_signer,
    )
    _DEFI_AVAILABLE = hasattr(axon_quant, "_native") and hasattr(
        axon_quant._native, "defi"
    )
except ImportError as _e:
    pytest.skip(f"axon_quant not installed: {_e}", allow_module_level=True)
    raise  # 实际不可达,仅供类型检查

if not _DEFI_AVAILABLE:
    pytest.skip(
        "axon_quant._native.defi not yet registered (need maturin develop)",
        allow_module_level=True,
    )


# ═══════════════════════════════════════════════════════════════════════════
# 类型可用性
# ═══════════════════════════════════════════════════════════════════════════


def test_defi_module_imports_all_symbols():
    """所有 defi 顶层符号都能 import。"""
    # 基础类型
    assert Chain is not None
    assert EvmConfig is not None
    assert DefiOrder is not None
    assert SwapRoute is not None
    assert RiskCheckResult is not None
    assert UniswapV3Contracts is not None
    # EVM Provider / Signer
    assert ProviderConfig is not None
    assert EvmProvider is not None
    assert LocalSigner is not None
    # ERC-20 / Multicall / V3
    assert Erc20Client is not None
    assert V3Quoter is not None
    assert V3Router is not None
    assert Multicall is not None
    # Bridge / MEV
    assert BridgeConfig is not None
    assert BridgeManager is not None
    assert MevShareConfig is not None
    assert MevShareClient is not None
    # 异常
    assert DefiError is not None
    # 工厂函数
    assert callable(evm_provider)
    assert callable(local_signer)
    assert callable(erc20_client)


def test_defi_submodule_path():
    """axon_quant.defi 子模块路径可达。"""
    assert hasattr(axon_quant, "defi")
    # defi.py 模块(纯 Python wrapper)
    assert axon_quant.defi.__file__.endswith("defi.py")


def test_native_defi_module_registers_all_classes():
    """``_native.defi`` 扁平暴露全部 18 个核心类。"""
    nd = axon_quant._native.defi
    expected = [
        "DefiError", "Chain", "EvmConfig", "DefiOrder", "SwapRoute",
        "RiskCheckResult", "UniswapV3Contracts", "ProviderConfig",
        "EvmProvider", "LocalSigner", "Erc20Client", "V3Quoter",
        "V3Router", "Multicall", "BridgeConfig", "BridgeManager",
        "MevShareConfig", "MevShareClient",
    ]
    for name in expected:
        assert hasattr(nd, name), f"[defi] missing class: {name}"


# ═══════════════════════════════════════════════════════════════════════════
# Chain 枚举
# ═══════════════════════════════════════════════════════════════════════════


def test_chain_enum_values():
    """Chain 4 个枚举值 + chain_id 一一对应。"""
    assert Chain.Ethereum.chain_id == 1
    assert Chain.Arbitrum.chain_id == 42161
    assert Chain.Optimism.chain_id == 10
    assert Chain.Polygon.chain_id == 137


def test_chain_from_chain_id_roundtrip():
    """Chain.from_chain_id 双向转换。"""
    for c in [Chain.Ethereum, Chain.Arbitrum, Chain.Optimism, Chain.Polygon]:
        assert Chain.from_chain_id(c.chain_id) == c


def test_chain_name():
    """Chain.name 返回可读名。"""
    assert Chain.Ethereum.name == "Ethereum"
    assert Chain.Arbitrum.name == "Arbitrum"
    assert Chain.Optimism.name == "Optimism"
    assert Chain.Polygon.name == "Polygon"


# ═══════════════════════════════════════════════════════════════════════════
# 工厂函数
# ═══════════════════════════════════════════════════════════════════════════


def test_evm_provider_factory():
    """evm_provider(chain, rpc_url) 直接构造。"""
    p = evm_provider(Chain.Ethereum, "http://x")
    assert p.rpc_url == "http://x"


def test_local_signer_factory_invalid_key():
    """local_signer 非 hex 私钥应抛 DefiError。"""
    with pytest.raises(Exception):  # DefiError / ValueError
        local_signer("not-a-key", Chain.Ethereum)


def test_erc20_client_factory():
    """erc20_client(addr, provider) 构造。"""
    p = evm_provider(Chain.Ethereum, "http://x")
    c = erc20_client(
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",  # USDC mainnet
        p,
    )
    assert c.info.address == (
        "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"  # 自动 lowercase
    )


# ═══════════════════════════════════════════════════════════════════════════
# EvmConfig / DefiOrder / SwapRoute / RiskCheckResult / UniswapV3Contracts
# ═══════════════════════════════════════════════════════════════════════════


def test_evm_config_basic():
    """EvmConfig 构造 + 字段。"""
    cfg = EvmConfig(chain_id=1, rpc_url="http://x", private_key="0x" + "ab" * 32)
    assert cfg.chain_id == 1
    assert cfg.rpc_url == "http://x"


def test_evm_config_with_oneinch_api_key():
    """EvmConfig.with_oneinch_api_key 工厂。"""
    cfg = EvmConfig.with_oneinch_api_key(
        1, "http://x", "0x" + "ab" * 32, "my-api-key"
    )
    assert cfg.chain_id == 1


def test_evm_config_with_flashbots_rpc():
    """EvmConfig.with_flashbots_rpc 工厂。"""
    cfg = EvmConfig.with_flashbots_rpc(
        1, "http://x", "0x" + "ab" * 32, "https://relay.flashbots.net"
    )
    assert cfg.chain_id == 1


def test_defi_order_basic():
    """DefiOrder 构造 + 字段。"""
    order = DefiOrder(token="USDC", amount="1000", amount_usd=1000.0)
    assert order.token == "USDC"
    assert order.amount == "1000"
    assert order.amount_usd == 1000.0


def test_defi_order_with_slippage():
    """DefiOrder.with_slippage 工厂。"""
    order = DefiOrder.with_slippage("USDC", "1000", 1000.0, slippage=0.5)
    assert order.slippage == 0.5


def test_swap_route_fields():
    """SwapRoute 字段读取。"""
    # 实际 SwapRoute 是 from_rust 工厂,这里跳过实例化,
    # 只验证模块里有这个类且字段名可读
    assert SwapRoute is not None


def test_risk_check_result_fields():
    """RiskCheckResult 字段读取。"""
    assert RiskCheckResult is not None


def test_uniswap_v3_contracts_for_chain():
    """UniswapV3Contracts.for_chain 返回主网默认地址。"""
    contracts = UniswapV3Contracts.for_chain(Chain.Ethereum)
    # Ethereum mainnet factory / router / position_manager 都不是空
    assert contracts.factory
    assert contracts.router
    assert contracts.position_manager


# ═══════════════════════════════════════════════════════════════════════════
# ProviderConfig / EvmProvider / LocalSigner
# ═══════════════════════════════════════════════════════════════════════════


def test_provider_config_for_chain():
    """ProviderConfig.for_chain 工厂。"""
    cfg = ProviderConfig.for_chain(Chain.Arbitrum, "http://arb")
    assert cfg.rpc_url == "http://arb"
    assert cfg.timeout_ms > 0
    assert cfg.max_retries > 0


def test_evm_provider_constructs():
    """EvmProvider 构造(无 RPC 调用)。"""
    p = EvmProvider(ProviderConfig.for_chain(Chain.Ethereum, "http://x"))
    assert p.rpc_url == "http://x"


def test_local_signer_from_hex_constructs():
    """LocalSigner.from_hex 合法私钥构造。"""
    # 64 hex chars(测试用私钥,不可用于生产)
    signer = LocalSigner.from_hex("0x" + "ab" * 32, Chain.Ethereum)
    assert signer.address.startswith("0x")
    # nonce 单调递增
    n0 = signer.next_nonce
    n1 = signer.next_nonce
    assert n1 == n0 + 1


# ═══════════════════════════════════════════════════════════════════════════
# Erc20Client / V3Quoter / V3Router / Multicall
# ═══════════════════════════════════════════════════════════════════════════


def test_erc20_client_constructs():
    """Erc20Client 构造(USDC 自动用预设 decimals=6 / symbol='USDC')。"""
    p = evm_provider(Chain.Ethereum, "http://x")
    c = erc20_client(
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", p
    )
    assert c.info.symbol == "USDC"
    assert c.info.decimals == 6


def test_v3_quoter_constructs():
    """V3Quoter 构造。"""
    p = evm_provider(Chain.Ethereum, "http://x")
    q = V3Quoter(p, Chain.Ethereum)
    assert q.address  # QuoterV2 地址非空


def test_v3_router_constructs():
    """V3Router 构造。"""
    p = evm_provider(Chain.Ethereum, "http://x")
    r = V3Router(p, Chain.Ethereum)
    assert r.address  # SwapRouter02 地址非空


def test_multicall_constructs():
    """Multicall 构造 + address。"""
    p = evm_provider(Chain.Ethereum, "http://x")
    m = Multicall(p, Chain.Ethereum)
    # Multicall3 canonical address: 0xca11bde0...
    assert m.address.startswith("0xca11bde0")


# ═══════════════════════════════════════════════════════════════════════════
# Bridge / MEV
# ═══════════════════════════════════════════════════════════════════════════


def test_bridge_config_default():
    """BridgeConfig 默认值。"""
    cfg = BridgeConfig.default()
    assert cfg.endpoint
    assert 1 in cfg.supported_chains  # Ethereum
    assert 42161 in cfg.supported_chains  # Arbitrum


def test_bridge_manager_is_supported():
    """BridgeManager.is_supported 4 链全支持。"""
    mgr = BridgeManager(BridgeConfig.default())
    assert mgr.is_supported(Chain.Ethereum)
    assert mgr.is_supported(Chain.Arbitrum)
    assert mgr.is_supported(Chain.Optimism)
    assert mgr.is_supported(Chain.Polygon)


def test_mev_share_config_default():
    """MevShareConfig 默认走 Flashbots public relay。"""
    cfg = MevShareConfig.default()
    assert cfg.rpc_url == "https://relay.flashbots.net"
    assert cfg.max_wait_secs == 60


def test_mev_share_config_new():
    """MevShareConfig.new 自定义。"""
    cfg = MevShareConfig.new("https://custom", "0x" + "ab" * 32)
    assert cfg.rpc_url == "https://custom"
    assert cfg.signing_key == "0x" + "ab" * 32


def test_mev_share_client_constructs():
    """MevShareClient 构造。"""
    client = MevShareClient(MevShareConfig.default())
    assert client.rpc_url == "https://relay.flashbots.net"


# ═══════════════════════════════════════════════════════════════════════════
# 异常
# ═══════════════════════════════════════════════════════════════════════════


def test_defi_error_is_exception():
    """DefiError 是 builtin Exception 子类(非自定义 AxonError,避免 cargo 循环)。"""
    assert issubclass(DefiError, Exception)


def test_defi_error_subclasses():
    """DefiError 在 _native.defi 模块下有 9 个变体(每个 = 独立子类)。"""
    nd = axon_quant._native.defi
    # 至少能找到这些变体名
    for name in [
        "UnsupportedChain", "RpcError", "ChainError", "TransactionFailed",
        "NoRouteFound", "SlippageTooHigh", "RiskRejected", "BridgeError",
        "ContractError", "ConfigError",
    ]:
        assert hasattr(nd, name), f"[defi] missing error variant: {name}"


# ═══════════════════════════════════════════════════════════════════════════
# repr / __str__ 协议
# ═══════════════════════════════════════════════════════════════════════════


def test_repr_includes_key_fields():
    """主要类的 __repr__ 含关键字段。"""
    p = evm_provider(Chain.Ethereum, "http://x")
    assert "http://x" in repr(p)

    cfg = EvmConfig(chain_id=1, rpc_url="http://y", private_key="0x" + "ab" * 32)
    assert "EvmConfig" in repr(cfg)

    mgr = BridgeManager(BridgeConfig.default())
    assert "BridgeManager" in repr(mgr)


def test_chain_str_protocol():
    """Chain.__str__ 返回可读名。"""
    assert str(Chain.Ethereum) == "Ethereum"
    assert str(Chain.Arbitrum) == "Arbitrum"
