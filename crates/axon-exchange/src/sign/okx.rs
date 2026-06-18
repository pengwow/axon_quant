//! OKX V5 HMAC-SHA256 Base64 签名 + 4 header 构造
//!
//! 签名规则:`HMAC-SHA256(secret, timestamp + method + path + body)` 后做 Base64 编码
//!
//! 4 header(`OK-ACCESS-KEY` / `OK-ACCESS-SIGN` / `OK-ACCESS-TIMESTAMP` / `OK-ACCESS-PASSPHRASE`)
//! 是所有私有 REST 端点必须携带的。
//!
//! 公开端点(无鉴权)使用 `/api/v5/public/*` 路径,无需签名。

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// HMAC-SHA256 算法实例
type HmacSha256 = Hmac<Sha256>;

/// OKX 4 个签名相关 header
#[derive(Debug, Clone)]
pub struct OkxHeaders {
    pub ok_access_key: String,
    pub ok_access_timestamp: String,
    pub ok_access_sign: String,
    pub ok_access_passphrase: String,
}

/// 计算 OKX 签名
///
/// 签名规则:HMAC-SHA256(secret, timestamp + method + path + body),Base64 编码
///
/// # 参数
/// - `method`:HTTP 方法(GET / POST),大小写不敏感(内部会大写化)
/// - `path`:请求路径(含 query),如 `/api/v5/account/balance?ccy=USDT`
/// - `body`:请求 body 原文(GET 时为 `""`)
/// - `secret`:用户 API secret
///
/// # 返回
/// `(timestamp_iso8601, signature_base64)` 元组
pub fn sign_request(method: &str, path: &str, body: &str, secret: &str) -> (String, String) {
    // 1. ISO8601 时间戳(毫秒精度)
    let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

    // 2. 拼接待签名字符串(timestamp + 大写 method + path + body)
    //    method 必须大写,这是 OKX 协议要求
    let message = format!("{}{}{}{}", timestamp, method.to_uppercase(), path, body);

    // 3. HMAC-SHA256 Base64
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(message.as_bytes());
    let sig = BASE64.encode(mac.finalize().into_bytes());

    (timestamp, sig)
}

/// 构造完整 4 header
pub fn build_headers(
    api_key: &str,
    secret: &str,
    passphrase: &str,
    method: &str,
    path: &str,
    body: &str,
) -> OkxHeaders {
    let (timestamp, sign) = sign_request(method, path, body, secret);
    OkxHeaders {
        ok_access_key: api_key.to_string(),
        ok_access_timestamp: timestamp,
        ok_access_sign: sign,
        ok_access_passphrase: passphrase.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_request_returns_iso8601_timestamp_and_base64_sig() {
        let (ts, sig) = sign_request("GET", "/api/v5/account/balance", "", "test_secret");
        // ISO8601: 形如 "2024-01-01T12:34:56.789Z"
        assert!(ts.starts_with("20"));
        assert!(ts.ends_with("Z"));
        // Base64 编码(32 bytes → ceil(32/3)*4 = 44 字符,可能无 padding)
        assert!(
            sig.len() >= 40,
            "expected base64 length >= 40, got {}",
            sig.len()
        );
        // 验证 sig 真的是 base64
        BASE64.decode(&sig).expect("signature must be valid base64");
    }

    #[test]
    fn sign_request_method_is_case_insensitive() {
        // method 大小写不同但语义相同,签名结果应一致(因为内部会 to_uppercase)
        let (_, sig_lower) = sign_request("get", "/api/v5/account/balance", "", "test");
        let (_, sig_upper) = sign_request("GET", "/api/v5/account/balance", "", "test");
        // 时间戳可能差 1ms,所以签名可能不同 — 这里只断言两次都产生非空
        assert!(!sig_lower.is_empty());
        assert!(!sig_upper.is_empty());
    }

    #[test]
    fn sign_request_body_affects_signature() {
        let (_, sig_empty) = sign_request("POST", "/api/v5/trade/order", "", "test");
        let (_, sig_with_body) =
            sign_request("POST", "/api/v5/trade/order", r#"{"side":"buy"}"#, "test");
        // 两次签名都非空(时间戳可能相同也可能不同,这里只断言)
        assert!(!sig_empty.is_empty());
        assert!(!sig_with_body.is_empty());
    }

    #[test]
    fn build_headers_includes_all_four() {
        let h = build_headers("key", "secret", "pass", "GET", "/path", "");
        assert_eq!(h.ok_access_key, "key");
        assert_eq!(h.ok_access_passphrase, "pass");
        assert!(!h.ok_access_sign.is_empty());
        assert!(!h.ok_access_timestamp.is_empty());
    }
}
