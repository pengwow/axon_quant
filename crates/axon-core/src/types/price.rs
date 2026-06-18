//! 价格类型
//!
//! 使用 `f64` 内部表示，提供类型安全 + 序列化的 newtype 包装。
//! 后续阶段可替换为 `rust_decimal::Decimal` 以获得精确十进制运算。
//!
//! # 设计决策
//!
//! ## 为什么使用 `f64` 而非 `rust_decimal::Decimal`
//!
//! - **性能**：`f64` 是原生 CPU 类型，回测引擎需要处理百万级 Tick/Bar
//! - **生态兼容**：与主流 Rust 数值生态（`serde_json` / `bincode` / `arrow`）无缝
//! - **精度可接受**：回测业务通常容忍 1e-9 量级误差
//!
//! ## `Eq` / `Ord` / `Hash` 手工实现的原因
//!
//! `f64` **根本性不实现 `Ord`**（仅 `PartialOrd`），因为 `f64` 存在 `NaN` 不可比较问题。
//! 但我们的撮合引擎需要 `BTreeMap<Price, ...>` 作为价格索引，必须有全序关系。
//! 因此采用 **`#[derive(PartialEq, PartialOrd)]` + 手工 `impl Eq` + 手工 `impl Ord`** 的标准模式。
//!
//! ## NaN 安全性
//!
//! - `Price::from_f64` 使用 `is_finite() && v >= 0.0` 双重过滤，确保 `Price` 内部不含 NaN / 负数
//! - `Ord::cmp` 的 `unwrap_or(Ordering::Equal)` 是 NaN 的最后一道防线（理论上不会触发）
//! - `Hash` 使用 `f64::to_bits()` 而非直接 `self.0.hash(state)`，避免 NaN 的哈希不稳定
//!
//! ## 何时应当重构
//!
//! 当以下条件之一满足时，应迁移到 `rust_decimal::Decimal`：
//! - 接入实盘交易，对精度有严格要求
//! - 跨语言/跨平台序列化出现精度漂移
//! - Rust 标准库为 `f64` 添加 `Ord` 实现（当前 RFC 2718 未通过）

use serde::{Deserialize, Serialize};

/// 价格类型（newtype 包装 `f64`）
///
/// 价格必须为非负值，构造时自动约束。
/// `f64` 不实现 `Eq`/`Ord`/`Hash`，因此手工实现并保证与 `PartialEq`/`PartialOrd` 一致。
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Price(f64);

impl Eq for Price {}

// ──────────────────────────────────────────────────────────────────────────────
// 警告抑制：`clippy::derive_ord_xor_partial_ord`
//
// 抑制原因：
//   `f64` 不实现 `Ord`（仅 `PartialOrd`），而我们的撮合引擎需要
//   `BTreeMap<Price, ...>` 的有序索引（见 axon-backtest::matching::engine.rs）。
//   必须保留 `#[derive(PartialOrd)]` + 手工 `impl Ord` 的组合。
//
// 适用场景：
//   所有 `Price` 类型需要的全序关系（`<` / `>` / `BTreeMap` 键 / `BTreeSet` 元素）。
//
// 潜在风险：
//   - 若 `Price::from_f64` 未妥善过滤 NaN，`partial_cmp` 返回 `None` 时会
//     退化为 `Equal`，可能破坏买卖盘优先级。**当前 `from_f64` 使用
//     `is_finite() && v >= 0.0` 双重过滤，已确保 `Price` 不含 NaN**。
//   - `partial_cmp` 的 `unwrap_or(Equal)` 是对 NaN 的最后一道防线。
//
// 未来优化：
//   当迁移到 `rust_decimal::Decimal` 或整数定标（i64 × 1e-8）时，可同时
//   移除 `#[allow]` 与手工 `impl Ord`，直接 derive。
//
// 临时措施追踪：CHANGELOG.md L142
// ──────────────────────────────────────────────────────────────────────────────
#[allow(clippy::derive_ord_xor_partial_ord)]
impl Ord for Price {
    #[inline]
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // 构造时已保证非 NaN，可安全使用 partial_cmp
        // 保留 `unwrap_or` 作为防御性编程，应对未来可能的 NaN 渗入路径
        self.0
            .partial_cmp(&other.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    }
}

impl std::hash::Hash for Price {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // 使用 `to_bits()` 而非 `self.0.hash(state)`：
        //   - 避免 NaN 的 `f64::hash` 行为不稳定（不同 NaN 表示可能产生不同哈希）
        //   - 满足 `Hash + Eq` 一致性要求（Eq 已通过构造时过滤 NaN 实现）
        self.0.to_bits().hash(state);
    }
}

impl Price {
    /// 从 `f64` 构造，自动截断负值为 0
    #[inline]
    pub fn from_f64(v: f64) -> Self {
        Self(if v.is_finite() && v >= 0.0 { v } else { 0.0 })
    }

    /// 转换为 `f64`
    #[inline]
    pub fn as_f64(&self) -> f64 {
        self.0
    }

    /// 消耗 self，返回内部 `f64`（零拷贝）
    #[inline]
    pub fn into_inner(self) -> f64 {
        self.0
    }

    /// 是否为零
    #[inline]
    pub fn is_zero(&self) -> bool {
        self.0 == 0.0
    }
}

impl Default for Price {
    #[inline]
    fn default() -> Self {
        Self(0.0)
    }
}

impl std::fmt::Display for Price {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<f64> for Price {
    #[inline]
    fn from(v: f64) -> Self {
        Self::from_f64(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_price_from_f64_roundtrip() {
        let p = Price::from_f64(100.5);
        assert!((p.as_f64() - 100.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_price_default_is_zero() {
        let p = Price::default();
        assert_eq!(p.as_f64(), 0.0);
    }

    #[test]
    fn test_price_comparison() {
        let a = Price::from_f64(100.0);
        let b = Price::from_f64(200.0);
        assert!(a < b);
        assert!(b > a);
        assert_eq!(a.cmp(&b), std::cmp::Ordering::Less);
    }

    #[test]
    fn test_price_equality() {
        let a = Price::from_f64(100.0);
        let b = Price::from_f64(100.0);
        let c = Price::from_f64(100.1);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn test_price_negative_rejected() {
        // 价格为负时自动归零
        let p = Price::from_f64(-1.0);
        assert!(p.as_f64() >= 0.0);
        assert_eq!(p.as_f64(), 0.0);
    }

    #[test]
    fn test_price_from_impl() {
        let p: Price = 50.25_f64.into();
        assert!((p.as_f64() - 50.25).abs() < f64::EPSILON);
    }

    #[test]
    fn test_price_is_zero() {
        assert!(Price::default().is_zero());
        assert!(!Price::from_f64(1.0).is_zero());
    }

    #[test]
    fn test_price_display() {
        let p = Price::from_f64(123.456);
        assert_eq!(format!("{p}"), "123.456");
    }

    // ─── 边界场景 ──────────────────────────────────────

    /// NaN 输入应归零（构造时过滤）
    #[test]
    fn test_price_nan_clamped_to_zero() {
        let p = Price::from_f64(f64::NAN);
        assert!(p.as_f64().is_finite(), "NaN 透传为非有限值");
        assert_eq!(p.as_f64(), 0.0, "NaN 应归零");
        assert!(p.is_zero());
    }

    /// +∞ / -∞ 输入应归零
    #[test]
    fn test_price_infinity_clamped_to_zero() {
        assert_eq!(Price::from_f64(f64::INFINITY).as_f64(), 0.0);
        assert_eq!(Price::from_f64(f64::NEG_INFINITY).as_f64(), 0.0);
    }

    /// 极小正数（f64::MIN_POSITIVE）应保留
    #[test]
    fn test_price_min_positive_preserved() {
        let p = Price::from_f64(f64::MIN_POSITIVE);
        assert!(p.as_f64() > 0.0, "极小正数应保留非零值");
        assert!(!p.is_zero());
    }

    /// 极大正数（f64::MAX）应保留
    #[test]
    fn test_price_max_value_preserved() {
        let p = Price::from_f64(f64::MAX);
        assert_eq!(p.as_f64(), f64::MAX);
    }

    /// 极小负数（-f64::MIN_POSITIVE）应归零
    #[test]
    fn test_price_small_negative_clamped_to_zero() {
        let p = Price::from_f64(-f64::MIN_POSITIVE);
        assert_eq!(p.as_f64(), 0.0, "负数一律归零");
    }

    /// 零价格应保留为 0
    #[test]
    fn test_price_zero_preserved() {
        let p = Price::from_f64(0.0);
        assert_eq!(p.as_f64(), 0.0);
        assert!(p.is_zero());
    }

    /// Hash 一致性：相同价格哈希相同
    #[test]
    fn test_price_hash_consistency() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let a = Price::from_f64(100.5);
        let b = Price::from_f64(100.5);
        let mut ha = DefaultHasher::new();
        let mut hb = DefaultHasher::new();
        a.hash(&mut ha);
        b.hash(&mut hb);
        assert_eq!(ha.finish(), hb.finish(), "相同价格哈希必须一致");
    }

    /// 边界价格作为 BTreeMap 键可用（依赖 Ord 实现）
    #[test]
    fn test_price_btreemap_key_zero_vs_positive() {
        use std::collections::BTreeMap;
        let mut map: BTreeMap<Price, &'static str> = BTreeMap::new();
        map.insert(Price::from_f64(0.0), "zero");
        map.insert(Price::from_f64(f64::MIN_POSITIVE), "min_positive");
        map.insert(Price::from_f64(f64::MAX), "max");

        assert_eq!(map.len(), 3);
        let keys: Vec<_> = map.keys().map(|p| p.as_f64()).collect();
        assert_eq!(keys, vec![0.0, f64::MIN_POSITIVE, f64::MAX]);
    }
}
