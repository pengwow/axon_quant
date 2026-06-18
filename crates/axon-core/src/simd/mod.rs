//! # SIMD 加速模块
//!
//! 提供运行时检测和 SIMD 优化的数值计算函数。
//!
//! ## 支持的平台
//!
//! | 平台 | SIMD 级别 | 向量宽度 |
//! |------|----------|---------|
//! | x86_64 (AVX2) | `SimdLevel::Avx2` | 256-bit (8×f32, 4×f64) |
//! | x86_64 (SSE2) | `SimdLevel::Sse2` | 128-bit (4×f32, 2×f64) |
//! | aarch64 (NEON) | `SimdLevel::Neon` | 128-bit (4×f32, 2×f64) |
//! | 其他 | `SimdLevel::Scalar` | 标量回退 |
//!
//! ## 使用示例
//!
//! ```rust
//! use axon_core::simd::{detect_simd_level, normalize_min_max, normalize_zscore};
//!
//! let level = detect_simd_level();
//! println!("SIMD level: {:?}", level);
//!
//! let mut data = vec![1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
//! normalize_min_max(&mut data, 1.0, 8.0);
//! ```

mod normalize;
mod orderbook;
mod var;

pub use normalize::{normalize_min_max, normalize_zscore};
pub use orderbook::sum_depth;
pub use var::partial_sort_var;

/// SIMD 级别
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimdLevel {
    /// 无 SIMD（标量回退）
    Scalar,
    /// x86_64 SSE2（128-bit）
    Sse2,
    /// x86_64 AVX2（256-bit）
    Avx2,
    /// aarch64 NEON（128-bit）
    Neon,
}

/// 检测当前平台的 SIMD 支持级别
pub fn detect_simd_level() -> SimdLevel {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return SimdLevel::Avx2;
        }
        SimdLevel::Sse2 // x86_64 baseline
    }

    #[cfg(target_arch = "aarch64")]
    {
        SimdLevel::Neon
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        SimdLevel::Scalar
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_simd_level() {
        let level = detect_simd_level();
        // 在 x86_64 上至少是 Sse2
        #[cfg(target_arch = "x86_64")]
        assert!(level == SimdLevel::Sse2 || level == SimdLevel::Avx2);

        // 在 aarch64 上是 Neon
        #[cfg(target_arch = "aarch64")]
        assert_eq!(level, SimdLevel::Neon);
    }
}
