//! EVM 链定义

use serde::{Deserialize, Serialize};

/// 支持的 EVM 链
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Chain {
    /// Ethereum 主网
    Ethereum,
    /// Arbitrum One
    Arbitrum,
    /// Optimism
    Optimism,
    /// Polygon
    Polygon,
}

impl Chain {
    /// 从链 ID 创建 Chain
    pub fn from_chain_id(chain_id: u64) -> Result<Self, crate::error::DefiError> {
        match chain_id {
            1 => Ok(Self::Ethereum),
            42161 => Ok(Self::Arbitrum),
            10 => Ok(Self::Optimism),
            137 => Ok(Self::Polygon),
            _ => Err(crate::error::DefiError::UnsupportedChain(chain_id)),
        }
    }

    /// 获取链 ID
    pub fn chain_id(&self) -> u64 {
        match self {
            Self::Ethereum => 1,
            Self::Arbitrum => 42161,
            Self::Optimism => 10,
            Self::Polygon => 137,
        }
    }

    /// 获取链名称
    pub fn name(&self) -> &'static str {
        match self {
            Self::Ethereum => "Ethereum",
            Self::Arbitrum => "Arbitrum",
            Self::Optimism => "Optimism",
            Self::Polygon => "Polygon",
        }
    }

    /// 获取 LayerZero 链 ID
    pub fn lz_chain_id(&self) -> u16 {
        match self {
            Self::Ethereum => 101,
            Self::Arbitrum => 110,
            Self::Optimism => 111,
            Self::Polygon => 109,
        }
    }
}

impl std::fmt::Display for Chain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chain_from_chain_id() {
        assert_eq!(Chain::from_chain_id(1).unwrap(), Chain::Ethereum);
        assert_eq!(Chain::from_chain_id(42161).unwrap(), Chain::Arbitrum);
        assert_eq!(Chain::from_chain_id(10).unwrap(), Chain::Optimism);
        assert_eq!(Chain::from_chain_id(137).unwrap(), Chain::Polygon);
    }

    #[test]
    fn test_chain_from_chain_id_unsupported() {
        assert!(Chain::from_chain_id(999).is_err());
    }

    #[test]
    fn test_chain_chain_id() {
        assert_eq!(Chain::Ethereum.chain_id(), 1);
        assert_eq!(Chain::Arbitrum.chain_id(), 42161);
        assert_eq!(Chain::Optimism.chain_id(), 10);
        assert_eq!(Chain::Polygon.chain_id(), 137);
    }

    #[test]
    fn test_chain_name() {
        assert_eq!(Chain::Ethereum.name(), "Ethereum");
        assert_eq!(Chain::Arbitrum.name(), "Arbitrum");
        assert_eq!(Chain::Optimism.name(), "Optimism");
        assert_eq!(Chain::Polygon.name(), "Polygon");
    }

    #[test]
    fn test_chain_lz_chain_id() {
        assert_eq!(Chain::Ethereum.lz_chain_id(), 101);
        assert_eq!(Chain::Arbitrum.lz_chain_id(), 110);
        assert_eq!(Chain::Optimism.lz_chain_id(), 111);
        assert_eq!(Chain::Polygon.lz_chain_id(), 109);
    }

    #[test]
    fn test_chain_display() {
        assert_eq!(Chain::Ethereum.to_string(), "Ethereum");
        assert_eq!(Chain::Arbitrum.to_string(), "Arbitrum");
    }

    #[test]
    fn test_chain_equality() {
        assert_eq!(Chain::Ethereum, Chain::Ethereum);
        assert_ne!(Chain::Ethereum, Chain::Arbitrum);
    }

    #[test]
    fn test_chain_serialization() {
        let chain = Chain::Ethereum;
        let json = serde_json::to_string(&chain).unwrap();
        let restored: Chain = serde_json::from_str(&json).unwrap();
        assert_eq!(chain, restored);
    }
}
