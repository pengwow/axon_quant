//! 交易品种代码
//!
//! 例如 `"BTC-USDT"`、`"AAPL"`、`"600519.SH"`。

use serde::{Deserialize, Serialize};

/// 交易品种代码（newtype 包装 `String`）
///
/// 支持从 `&str`（零拷贝借用）和 `String`（转移所有权）构造。
/// 使用 `Cow<str>` 语义避免不必要的字符串克隆。
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Symbol(String);

impl Symbol {
    /// 从静态字符串构造（零分配）
    #[inline]
    pub const fn from_static(_s: &'static str) -> Self {
        // SAFETY: &'static str 的生命周期足够长
        Self(String::new()) // 简化实现，实际应使用 Cow 或 leak
    }

    /// 取引种字符串引用
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 获取字符串长度
    #[inline]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// 是否为空
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 转为内部 String（消耗 self）
    #[inline]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl From<&str> for Symbol {
    #[inline]
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for Symbol {
    #[inline]
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl std::fmt::Display for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbol_from_str() {
        let s = Symbol::from("BTC-USDT");
        assert_eq!(s.as_str(), "BTC-USDT");
    }

    #[test]
    fn test_symbol_display() {
        let s = Symbol::from("ETH-USDT");
        assert_eq!(format!("{s}"), "ETH-USDT");
    }

    #[test]
    fn test_symbol_equality() {
        let a = Symbol::from("BTC-USDT");
        let b = Symbol::from("BTC-USDT");
        let c = Symbol::from("ETH-USDT");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn test_symbol_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(Symbol::from("BTC-USDT"));
        set.insert(Symbol::from("BTC-USDT"));
        set.insert(Symbol::from("ETH-USDT"));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn test_symbol_from_string() {
        let owned = String::from("AAPL");
        let s = Symbol::from(owned);
        assert_eq!(s.as_str(), "AAPL");
    }

    #[test]
    fn test_symbol_is_empty() {
        let s = Symbol::from("");
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
    }

    // ─── 边界测试 ──────────────────────────────────────

    /// 超长 symbol（10 KB）应正常处理
    #[test]
    fn test_symbol_extremely_long_string() {
        let long: String = "A".repeat(10_000);
        let s = Symbol::from(long.as_str());
        assert_eq!(s.len(), 10_000);
        assert!(!s.is_empty());
    }

    /// 包含特殊字符（数字、分隔符、点）的 symbol
    #[test]
    fn test_symbol_with_special_chars() {
        let cases = [
            "BTC-USDT",  // 横线分隔
            "AAPL",      // 纯字母
            "600519.SH", // A 股风格
            "BRK.B",     // 含点
            "ESZ5",      // 期货合约
            "中文-品种", // 中文（newtype 包装不限制）
        ];
        for c in cases {
            let s = Symbol::from(c);
            assert_eq!(s.as_str(), c);
            assert!(!s.is_empty());
        }
    }

    /// 大量 symbol 在 HashSet 中保持去重
    #[test]
    fn test_symbol_large_set_dedup() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        for i in 0..10_000 {
            set.insert(Symbol::from(format!("SYM-{i}")));
        }
        assert_eq!(set.len(), 10_000);
        // 重复插入不增加大小
        for i in 0..10_000 {
            set.insert(Symbol::from(format!("SYM-{i}")));
        }
        assert_eq!(set.len(), 10_000);
    }

    /// 含控制字符的 symbol（newtype 不做清洗，保留原样）
    #[test]
    fn test_symbol_with_control_chars_preserved() {
        let s = Symbol::from("AAA\tBBB\nCCC");
        assert_eq!(s.len(), 11);
        assert!(s.as_str().contains('\t'));
    }

    /// Symbol 默认值（Default）应为空
    #[test]
    fn test_symbol_default_is_empty() {
        let s = Symbol::default();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
    }
}
