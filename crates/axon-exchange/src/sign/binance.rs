//! Binance HMAC-SHA256 hex 签名
//!
//! 签名规则:`HMAC-SHA256(secret, query_string)` 后做 hex 编码
//!
//! 例:`?symbol=BTCUSDT&leverage=20&timestamp=1234567890` → 64 字符 hex 串
//!
//! 适用端点:`/api/v3/*` 现货 + `/fapi/v1/*`、`/fapi/v2/*` USDⓈ-M 合约 +
//! `/dapi/v1/*` COIN-M 合约。所有这些端点都使用相同的签名规则(共享 `api_secret`)。

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// HMAC-SHA256 算法实例(由 `Hmac` newtype 包装)
type HmacSha256 = Hmac<Sha256>;

/// 计算 Binance 签名
///
/// 签名规则:HMAC-SHA256(secret, query_string),hex 编码
///
/// # 参数
/// - `query`:已经按字母序拼接好的 query string(不含 `signature=`)
/// - `secret`:用户 API secret
///
/// # 返回
/// 64 字符 hex 字符串(SHA-256 = 32 bytes = 64 hex chars)
pub fn sign_query(query: &str, secret: &str) -> String {
    // HMAC 接受任意长度 key,`new_from_slice` 不会因为 key 长度失败
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(query.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// 构造带签名的完整 query(自动加 timestamp)
pub fn signed_query(query: &str, secret: &str, timestamp_ms: i64) -> String {
    let full = if query.is_empty() {
        format!("timestamp={timestamp_ms}")
    } else {
        format!("{query}&timestamp={timestamp_ms}")
    };
    let sig = sign_query(&full, secret);
    format!("{full}&signature={sig}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_query_returns_64_hex_chars() {
        // 已知向量:secret = "test_secret", query = "symbol=BTCUSDT&leverage=20"
        let sig = sign_query("symbol=BTCUSDT&leverage=20", "test_secret");
        // SHA-256 = 32 bytes = 64 hex chars
        assert_eq!(sig.len(), 64);
        assert!(sig.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn sign_query_is_deterministic() {
        // 相同输入应产生相同签名
        let sig1 = sign_query("symbol=BTCUSDT&leverage=20", "test");
        let sig2 = sign_query("symbol=BTCUSDT&leverage=20", "test");
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn sign_query_changes_with_input() {
        // 输入变化应导致签名变化
        let sig1 = sign_query("symbol=BTCUSDT&leverage=20", "test");
        let sig2 = sign_query("symbol=BTCUSDT&leverage=10", "test");
        assert_ne!(sig1, sig2);
    }

    #[test]
    fn sign_query_changes_with_secret() {
        // secret 变化应导致签名变化
        let sig1 = sign_query("symbol=BTCUSDT", "secret_a");
        let sig2 = sign_query("symbol=BTCUSDT", "secret_b");
        assert_ne!(sig1, sig2);
    }

    #[test]
    fn signed_query_includes_timestamp_and_signature() {
        let q = signed_query(
            "symbol=BTCUSDT&leverage=20",
            "test_secret",
            1_700_000_000_000,
        );
        assert!(q.contains("timestamp=1700000000000"));
        assert!(q.contains("signature="));
        assert!(q.contains("symbol=BTCUSDT"));
        assert!(q.contains("leverage=20"));
    }

    #[test]
    fn signed_query_handles_empty_query() {
        let q = signed_query("", "test_secret", 1_700_000_000_000);
        assert_eq!(
            q,
            "timestamp=1700000000000&signature=".to_string()
                + &sign_query("timestamp=1700000000000", "test_secret")
        );
    }
}
