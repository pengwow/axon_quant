//! 审计链
//!
//! 使用 Blake3 哈希（比 SHA-256 快 14x），每条记录包含前一条的哈希，
//! 形成链式结构，防篡改。

use serde::{Deserialize, Serialize};

/// 审计条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// 条目 ID（递增）
    pub entry_id: u64,
    /// 时间戳 (Unix 秒)
    pub timestamp: u64,
    /// 事件类型，如 "trade", "decision", "risk_check"
    pub event_type: String,
    /// 执行操作的 Agent
    pub agent_id: String,
    /// 操作描述
    pub action: String,
    /// details 的 Blake3 哈希（截断 16 字节）
    pub details_hash: [u8; 16],
    /// 前一条的哈希
    pub prev_hash: [u8; 16],
    /// 本条的哈希
    pub hash: [u8; 16],
}

/// 审计链
///
/// 链式结构，每条记录的哈希包含前一条的哈希，支持完整性验证。
pub struct AuditChain {
    entries: Vec<AuditEntry>,
}

impl AuditChain {
    /// 创建空链
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// 记录事件，返回 entry_id
    pub fn record(&mut self, event_type: &str, agent_id: &str, action: &str, details: &str) -> u64 {
        let entry_id = self.entries.len() as u64;
        let timestamp = now_secs();
        let prev_hash = self.entries.last().map(|e| e.hash).unwrap_or([0u8; 16]);

        let details_hash = blake3_hash(details);
        let hash = blake3_chain(
            entry_id, timestamp, event_type, agent_id, action, details, prev_hash,
        );

        self.entries.push(AuditEntry {
            entry_id,
            timestamp,
            event_type: event_type.to_string(),
            agent_id: agent_id.to_string(),
            action: action.to_string(),
            details_hash,
            prev_hash,
            hash,
        });

        entry_id
    }

    /// 验证整条链的完整性
    ///
    /// 注意：由于原始 details 不可恢复，仅验证 prev_hash 链接和 entry_id 连续性。
    pub fn verify_chain(&self) -> bool {
        for (i, entry) in self.entries.iter().enumerate() {
            if i > 0 && entry.prev_hash != self.entries[i - 1].hash {
                return false;
            }
            if entry.entry_id as usize != i {
                return false;
            }
        }
        true
    }

    /// 条目数
    pub fn entry_count(&self) -> u64 {
        self.entries.len() as u64
    }

    /// 最近 N 条
    pub fn recent_entries(&self, n: usize) -> Vec<&AuditEntry> {
        let start = self.entries.len().saturating_sub(n);
        self.entries[start..].iter().collect()
    }

    /// 全部条目（导出用）
    pub fn all_entries(&self) -> &[AuditEntry] {
        &self.entries
    }
}

impl Default for AuditChain {
    fn default() -> Self {
        Self::new()
    }
}

fn blake3_hash(data: &str) -> [u8; 16] {
    let hash = blake3::hash(data.as_bytes());
    let mut result = [0u8; 16];
    result.copy_from_slice(&hash.as_bytes()[..16]);
    result
}

fn blake3_chain(
    entry_id: u64,
    timestamp: u64,
    event_type: &str,
    agent_id: &str,
    action: &str,
    details: &str,
    prev_hash: [u8; 16],
) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&entry_id.to_le_bytes());
    hasher.update(&timestamp.to_le_bytes());
    hasher.update(event_type.as_bytes());
    hasher.update(agent_id.as_bytes());
    hasher.update(action.as_bytes());
    hasher.update(details.as_bytes());
    hasher.update(&prev_hash);
    let hash = hasher.finalize();
    let mut result = [0u8; 16];
    result.copy_from_slice(&hash.as_bytes()[..16]);
    result
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_chain_valid() {
        let chain = AuditChain::new();
        assert!(chain.verify_chain());
        assert_eq!(chain.entry_count(), 0);
    }

    #[test]
    fn test_chain_integrity() {
        let mut chain = AuditChain::new();
        chain.record("trade", "market_agent", "buy BTC", "qty=0.1");
        chain.record("risk_check", "risk_agent", "approve", "score=0.9");
        chain.record("decision", "execution_agent", "execute", "twap");

        assert_eq!(chain.entry_count(), 3);
        assert!(chain.verify_chain());
    }

    #[test]
    fn test_recent_entries() {
        let mut chain = AuditChain::new();
        for i in 0..10 {
            chain.record("trade", "agent", &format!("action_{i}"), "details");
        }
        let recent = chain.recent_entries(3);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].entry_id, 7);
        assert_eq!(recent[2].entry_id, 9);
    }
}
