//! EVM Multicall3 批量查询
//!
//! 0.3.0 P0 Batch 2 / T1.5:封装 Multicall3 合约(`0xca11bde05977b3631167028862be2a173976ca11`),
//! 将 N 次 RPC 查询合并为 1 次,降低网络开销。
//!
//! 设计要点:
//! - `Multicall3::CANONICAL_ADDRESS` 固定(mudeb 部署在 mainnet/几乎所有 EVM 链同一地址)
//! - `aggregate3(Call3[])` 走真链,失败 call 隔离(`allowFailure`)
//! - `balance_of_batch(token, holders)` / `decimals_batch(tokens)` 便捷封装
//!
//! 注意:仅当 `evm` feature 启用时才引入 alloy,默认 feature 下结构体仍可构造(无 RPC 能力)。

use serde::{Deserialize, Serialize};

use crate::error::DefiError;
use crate::evm::chain::Chain;
use crate::evm::provider::EvmProvider;

#[cfg(feature = "evm")]
use alloy::network::TransactionBuilder;
#[cfg(feature = "evm")]
use alloy::primitives::{Address, U256};
#[cfg(feature = "evm")]
use alloy::providers::Provider;
#[cfg(feature = "evm")]
use alloy::rpc::types::TransactionRequest;
#[cfg(feature = "evm")]
use alloy::sol_types::SolValue;

/// Multicall3 合约(Mudeb 部署,几乎所有 EVM 链同一地址)
pub struct Multicall3;

impl Multicall3 {
    /// 通用 Multicall3 部署地址
    ///
    /// 在 Ethereum / Arbitrum / Optimism / Polygon 等绝大多数 EVM 链上同一地址:
    /// `0xcA11bde05977b3631167028862bE2a173976CA11`
    pub const CANONICAL_ADDRESS: &'static str = "0xcA11bde05977b3631167028862bE2a173976CA11";

    /// 该链是否部署了 Multicall3
    ///
    /// 已知 4 条主链全部支持
    pub fn is_deployed_on(chain: Chain) -> bool {
        matches!(
            chain,
            Chain::Ethereum | Chain::Arbitrum | Chain::Optimism | Chain::Polygon
        )
    }
}

/// Multicall3.Call3 结构
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Call3 {
    /// 目标合约地址
    pub target: String,
    /// 是否允许失败(失败时仍继续后续 call,返回 success=false)
    pub allow_failure: bool,
    /// ABI 编码后的调用数据
    pub call_data: Vec<u8>,
}

impl Call3 {
    /// 新建允许失败的 call
    pub fn new(target: &str, call_data: &str) -> Self {
        Self {
            target: target.to_lowercase(),
            allow_failure: true,
            call_data: hex_decode(call_data),
        }
    }

    /// 新建 strict call(失败立即终止)
    pub fn strict(target: &str, call_data: &str) -> Self {
        Self {
            target: target.to_lowercase(),
            allow_failure: false,
            call_data: hex_decode(call_data),
        }
    }
}

/// hex 字符串 → bytes("0x" 前缀可选)
///
/// 独立实现,不依赖 alloy,保证 `cargo build --no-default-features` 也能编译。
fn hex_decode(s: &str) -> Vec<u8> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() % 2 != 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for i in (0..bytes.len()).step_by(2) {
        let hi = hex_nibble(bytes[i]);
        let lo = hex_nibble(bytes[i + 1]);
        match (hi, lo) {
            (Some(h), Some(l)) => out.push((h << 4) | l),
            _ => return Vec::new(),
        }
    }
    out
}

/// bytes → hex 字符串("0x" 前缀)
fn hex_encode(b: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(2 + b.len() * 2);
    s.push_str("0x");
    for byte in b {
        s.push(HEX[(byte >> 4) as usize] as char);
        s.push(HEX[(byte & 0x0f) as usize] as char);
    }
    s
}

/// 单个十六进制字符 → 0..=15
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// aggregate3 单条结果
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallResult {
    /// 是否成功
    pub success: bool,
    /// ABI 解码后的 return data
    pub return_data: Vec<u8>,
}

/// Multicall 客户端
#[derive(Debug, Clone)]
pub struct Multicall {
    provider: EvmProvider,
    chain: Chain,
}

impl Multicall {
    /// 构造 Multicall
    pub fn new(provider: EvmProvider, chain: Chain) -> Self {
        Self { provider, chain }
    }

    /// 关联链
    pub fn chain(&self) -> Chain {
        self.chain
    }

    /// Multicall3 合约地址
    pub fn address(&self) -> String {
        Multicall3::CANONICAL_ADDRESS.to_lowercase()
    }

    /// 走 aggregate3 合约批量查询
    ///
    /// 返回每个 call 的结果(顺序与输入一致)
    #[cfg(feature = "evm")]
    pub async fn aggregate3(&self, calls: Vec<Call3>) -> Result<Vec<CallResult>, DefiError> {
        use alloy::sol;
        sol! {
            #[sol(rpc)]
            interface IMulticall3Wrapper {
                struct Call {
                    address target;
                    bool allowFailure;
                    bytes callData;
                }
                struct Result {
                    bool success;
                    bytes returnData;
                }
                function aggregate3(Call[] calls) external payable returns (Result[] returnData);
            }
        }
        use alloy::primitives::Address;

        // 构造 call 数组
        let mc_addr: Address = Multicall3::CANONICAL_ADDRESS
            .parse()
            .map_err(|e| DefiError::ConfigError(format!("invalid multicall3 addr: {}", e)))?;
        let parsed_url = ::url::Url::parse(&self.provider.config().rpc_url)
            .map_err(|e| DefiError::ConfigError(format!("invalid url: {}", e)))?;
        let p = alloy::providers::ProviderBuilder::new().connect_http(parsed_url);

        let mc_calls: Vec<IMulticall3Wrapper::Call> = {
            let mut acc = Vec::with_capacity(calls.len());
            for c in &calls {
                let target: Address = c.target.parse().map_err(|e| {
                    DefiError::ConfigError(format!("invalid target {}: {}", c.target, e))
                })?;
                acc.push(IMulticall3Wrapper::Call {
                    target,
                    allowFailure: c.allow_failure,
                    callData: c.call_data.clone().into(),
                });
            }
            acc
        };

        let call = IMulticall3Wrapper::aggregate3Call { calls: mc_calls };
        let input = call.abi_encode();
        let tx = TransactionRequest::default()
            .with_to(mc_addr)
            .with_input(input);
        let output: alloy::primitives::Bytes =
            p.call(tx).await.map_err(|e| DefiError::RpcError {
                url: self.provider.config().rpc_url.clone(),
                status: 0,
                body: DefiError::truncated_body(&format!("{}", e)),
            })?;

        // 解码 results 数组
        // aggregate3 返回 Result[],所以 Return = Vec<Result>
        use alloy::sol_types::SolCall;
        type Aggregate3Return = <IMulticall3Wrapper::aggregate3Call as SolCall>::Return;
        let results: Aggregate3Return =
            SolValue::abi_decode(&output).map_err(|e| DefiError::ContractError {
                address: Multicall3::CANONICAL_ADDRESS.to_string(),
                method: "aggregate3".into(),
                reason: format!("decode: {}", e),
            })?;

        Ok(results
            .into_iter()
            .map(|r| CallResult {
                success: r.success,
                return_data: r.returnData.to_vec(),
            })
            .collect())
    }

    /// aggregate3 stub
    #[cfg(not(feature = "evm"))]
    pub async fn aggregate3(&self, _calls: Vec<Call3>) -> Result<Vec<CallResult>, DefiError> {
        Err(DefiError::ConfigError("evm feature not enabled".into()))
    }

    /// ERC-20 balanceOf 批量查询
    ///
    /// 1 次 RPC 拿 N 个 holder 的余额(对比 N 次单查)
    #[cfg(feature = "evm")]
    pub async fn balance_of_batch(
        &self,
        token: &str,
        holders: &[Address],
    ) -> Result<Vec<U256>, DefiError> {
        use alloy::sol;
        sol! {
            interface IERC20Batch {
                function balanceOf(address account) external view returns (uint256);
            }
        }

        // balanceOf selector = 0x70a08231
        let selector = alloy::primitives::hex::decode("70a08231").unwrap();
        let mut calls = Vec::with_capacity(holders.len());
        for holder in holders {
            let mut data = selector.clone();
            data.extend_from_slice(&[0u8; 12]); // address padding
            data.extend_from_slice(holder.as_slice());
            calls.push(Call3::strict(token, &hex_encode(&data)));
        }

        let results = self.aggregate3(calls).await?;
        let mut balances = Vec::with_capacity(results.len());
        for r in results {
            if !r.success {
                // strict call 失败 → 整体失败
                return Err(DefiError::ContractError {
                    address: token.to_string(),
                    method: "balanceOf".into(),
                    reason: "multicall strict call reverted".into(),
                });
            }
            // balanceOf 返回 uint256,32 字节大端
            if r.return_data.len() < 32 {
                return Err(DefiError::ContractError {
                    address: token.to_string(),
                    method: "balanceOf".into(),
                    reason: format!("output too short: {} bytes", r.return_data.len()),
                });
            }
            let v = U256::from_be_slice(&r.return_data[..32]);
            balances.push(v);
        }
        Ok(balances)
    }

    /// balance_of_batch stub(evm feature 关闭时)
    #[cfg(not(feature = "evm"))]
    pub async fn balance_of_batch(
        &self,
        _token: &str,
        _holders: &[String],
    ) -> Result<Vec<String>, DefiError> {
        Err(DefiError::ConfigError("evm feature not enabled".into()))
    }

    /// ERC-20 decimals 批量查询
    #[cfg(feature = "evm")]
    pub async fn decimals_batch(&self, tokens: &[&str]) -> Result<Vec<u8>, DefiError> {
        // decimals() selector = 0x313ce567
        let selector = alloy::primitives::hex::decode("313ce567").unwrap();
        let mut calls = Vec::with_capacity(tokens.len());
        for token in tokens {
            calls.push(Call3::strict(token, &hex_encode(&selector)));
        }
        let results = self.aggregate3(calls).await?;
        let mut decimals = Vec::with_capacity(results.len());
        for r in results {
            if !r.success || r.return_data.len() < 32 {
                return Err(DefiError::ContractError {
                    address: "batch".into(),
                    method: "decimals".into(),
                    reason: "multicall decimals revert".into(),
                });
            }
            // ABI uint8 padded to 32 bytes, value in last byte
            decimals.push(r.return_data[31]);
        }
        Ok(decimals)
    }

    /// decimals_batch stub(evm feature 关闭时)
    #[cfg(not(feature = "evm"))]
    pub async fn decimals_batch(&self, _tokens: &[&str]) -> Result<Vec<u8>, DefiError> {
        Err(DefiError::ConfigError("evm feature not enabled".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_address_lowercase() {
        assert_eq!(
            Multicall3::CANONICAL_ADDRESS.to_lowercase(),
            "0xca11bde05977b3631167028862be2a173976ca11"
        );
    }

    #[test]
    fn call3_new_default_allows_failure() {
        let c = Call3::new("0xAbC", "0x70a08231");
        assert!(c.allow_failure);
        assert_eq!(c.target, "0xabc");
        assert_eq!(c.call_data, vec![0x70, 0xa0, 0x82, 0x31]);
    }

    #[test]
    fn call3_strict_disallows_failure() {
        let c = Call3::strict("0xAbC", "0x70a08231");
        assert!(!c.allow_failure);
    }
}
