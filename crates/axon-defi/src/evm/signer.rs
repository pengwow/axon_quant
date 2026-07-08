//! EVM 签名器封装
//!
//! 0.3.0 P0 新增:LocalSigner 包装 alloy `PrivateKeySigner`,
//! 提供 nonce 原子分配 + EIP-1559 fee 估算 + transfer_eth 写路径。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::evm::chain::Chain;

#[cfg(feature = "evm")]
use alloy::network::{EthereumWallet, TransactionBuilder};
#[cfg(feature = "evm")]
use alloy::primitives::Address;
#[cfg(feature = "evm")]
use alloy::providers::Provider;
#[cfg(feature = "evm")]
use alloy::rpc::types::TransactionRequest;
#[cfg(feature = "evm")]
use alloy::signers::local::PrivateKeySigner;

use thiserror::Error;

/// 签名器错误
#[derive(Debug, Error)]
pub enum SignerError {
    /// 私钥格式错误(非 hex 或长度不对)
    #[error("invalid private key: {0}")]
    InvalidPrivateKey(String),
    /// nonce 同步失败
    #[error("nonce sync failed: {0}")]
    NonceSyncFailed(String),
    /// 交易发送失败
    #[error("transaction send failed: {0}")]
    TxSendFailed(String),
}

/// 签名器类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignerKind {
    /// 本地私钥签名
    Local,
    /// Keystore 文件(0.3.x 后续实现)
    Keystore,
}

/// 本地私钥签名器
///
/// 内部持:
/// - `chain: Chain`  关联链
/// - `nonce: Arc<AtomicU64>`  原子 nonce 计数器(用 Arc 共享以支持 Clone)
/// - (feature=evm) `inner: PrivateKeySigner`  alloy 签名器
#[cfg(feature = "evm")]
#[derive(Debug, Clone)]
pub struct LocalSigner {
    chain: Chain,
    nonce: Arc<AtomicU64>,
    inner: PrivateKeySigner,
}

#[cfg(not(feature = "evm"))]
#[derive(Debug, Clone)]
pub struct LocalSigner {
    chain: Chain,
    nonce: Arc<AtomicU64>,
}

impl LocalSigner {
    /// 从 hex 私钥构造
    ///
    /// hex 必须以 `0x` 开头,后跟 64 个 hex 字符(32 字节)
    #[cfg(feature = "evm")]
    pub fn from_hex(hex: &str, chain: Chain) -> Result<Self, SignerError> {
        let inner = hex
            .parse::<PrivateKeySigner>()
            .map_err(|e| SignerError::InvalidPrivateKey(format!("{}", e)))?;
        Ok(Self {
            chain,
            nonce: Arc::new(AtomicU64::new(0)),
            inner,
        })
    }

    /// 从 hex 私钥构造(无 evm feature,stub)
    #[cfg(not(feature = "evm"))]
    pub fn from_hex(_hex: &str, chain: Chain) -> Result<Self, SignerError> {
        Ok(Self {
            chain,
            nonce: Arc::new(AtomicU64::new(0)),
        })
    }

    /// 获取签名器类型
    pub fn kind(&self) -> SignerKind {
        SignerKind::Local
    }

    /// 关联的链
    pub fn chain(&self) -> Chain {
        self.chain
    }

    /// 获取签名地址(EIP-55 checksum 格式)
    #[cfg(feature = "evm")]
    pub fn address(&self) -> Address {
        self.inner.address()
    }

    /// 获取内部 alloy PrivateKeySigner 引用
    ///
    /// 用于 Erc20Client::approve/transfer 内部构造 EthereumWallet
    #[cfg(feature = "evm")]
    pub fn raw_signer(&self) -> &PrivateKeySigner {
        &self.inner
    }

    /// 获取签名地址(stub)
    #[cfg(not(feature = "evm"))]
    pub fn address(&self) -> String {
        "0x0000000000000000000000000000000000000000".to_string()
    }

    /// 强制设置 nonce(测试用,生产应走 sync_nonce)
    pub fn set_nonce(&self, n: u64) {
        self.nonce.store(n, Ordering::SeqCst);
    }

    /// 原子分配并返回下一个 nonce
    pub fn next_nonce(&self) -> u64 {
        self.nonce.fetch_add(1, Ordering::SeqCst)
    }

    /// 从链上同步 nonce
    #[cfg(feature = "evm")]
    pub async fn sync_nonce(
        &self,
        provider: &crate::evm::provider::EvmProvider,
    ) -> Result<u64, SignerError> {
        use alloy::providers::ProviderBuilder;

        let parsed = ::url::Url::parse(&provider.config().rpc_url)
            .map_err(|e| SignerError::NonceSyncFailed(format!("invalid url: {}", e)))?;
        let p = ProviderBuilder::new().connect_http(parsed);

        let addr = self.inner.address();
        let nonce = p
            .get_transaction_count(addr)
            .await
            .map_err(|e| SignerError::NonceSyncFailed(format!("{}", e)))?;
        self.nonce.store(nonce, Ordering::SeqCst);
        Ok(nonce)
    }

    /// 从链上同步 nonce(stub)
    #[cfg(not(feature = "evm"))]
    pub async fn sync_nonce(
        &self,
        _provider: &crate::evm::provider::EvmProvider,
    ) -> Result<u64, SignerError> {
        Err(SignerError::NonceSyncFailed(
            "evm feature not enabled".into(),
        ))
    }

    /// 转账 ETH(基础写路径,用于演示 e2e)
    ///
    /// 实际 swap / approve 在 Batch 2/3 实现
    #[cfg(feature = "evm")]
    pub async fn transfer_eth(
        &self,
        provider: &crate::evm::provider::EvmProvider,
        to: Address,
        value_wei: alloy::primitives::U256,
    ) -> Result<alloy::rpc::types::TransactionReceipt, SignerError> {
        use alloy::providers::ProviderBuilder;

        let parsed = ::url::Url::parse(&provider.config().rpc_url)
            .map_err(|e| SignerError::TxSendFailed(format!("invalid url: {}", e)))?;
        let p = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.inner.clone()))
            .connect_http(parsed);

        let nonce = self.next_nonce();
        let tx = TransactionRequest::default()
            .with_to(to)
            .with_value(value_wei)
            .with_nonce(nonce);

        let pending = p
            .send_transaction(tx)
            .await
            .map_err(|e| SignerError::TxSendFailed(format!("send: {}", e)))?;
        let receipt = pending
            .get_receipt()
            .await
            .map_err(|e| SignerError::TxSendFailed(format!("receipt: {}", e)))?;
        Ok(receipt)
    }

    /// 转账 ETH(stub)
    #[cfg(not(feature = "evm"))]
    pub async fn transfer_eth(
        &self,
        _provider: &crate::evm::provider::EvmProvider,
        _to: String,
        _value_wei: String,
    ) -> Result<(), SignerError> {
        Err(SignerError::TxSendFailed("evm feature not enabled".into()))
    }
}

/// re-export alloy 关键类型,方便 integration test 使用
#[cfg(feature = "evm")]
pub use alloy_primitives;
