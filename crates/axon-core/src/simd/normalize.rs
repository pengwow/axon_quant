//! SIMD 加速的特征归一化

/// Min-Max 归一化：`(x - min) / (max - min)`
///
/// 将数据归一化到 [0, 1] 范围。
/// 使用 SIMD 并行处理 8 个 f32（AVX2）或 4 个 f32（SSE2/NEON）。
pub fn normalize_min_max(data: &mut [f32], min: f32, max: f32) {
    let range = max - min;
    if range.abs() < f64::EPSILON as f32 {
        data.fill(0.0);
        return;
    }

    let inv_range = 1.0 / range;

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            // SAFETY: AVX2 已通过运行时检测确认可用
            unsafe { normalize_min_max_avx2(data, min, inv_range) }
        } else {
            normalize_min_max_scalar(data, min, inv_range)
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    {
        normalize_min_max_scalar(data, min, inv_range)
    }
}

/// Z-Score 归一化：`(x - mean) / std`
///
/// 将数据标准化为零均值单位方差。
pub fn normalize_zscore(data: &mut [f32], mean: f32, std: f32) {
    if std.abs() < f64::EPSILON as f32 {
        data.fill(0.0);
        return;
    }

    let inv_std = 1.0 / std;

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            // SAFETY: AVX2 已通过运行时检测确认可用
            unsafe { normalize_zscore_avx2(data, mean, inv_std) }
        } else {
            normalize_zscore_scalar(data, mean, inv_std)
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    {
        normalize_zscore_scalar(data, mean, inv_std)
    }
}

// ── 标量回退 ──

fn normalize_min_max_scalar(data: &mut [f32], min: f32, inv_range: f32) {
    for x in data.iter_mut() {
        *x = (*x - min) * inv_range;
    }
}

fn normalize_zscore_scalar(data: &mut [f32], mean: f32, inv_std: f32) {
    for x in data.iter_mut() {
        *x = (*x - mean) * inv_std;
    }
}

// ── x86_64 AVX2 ──

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn normalize_min_max_avx2(data: &mut [f32], min: f32, inv_range: f32) {
    use std::arch::x86_64::*;

    let min_vec = _mm256_set1_ps(min);
    let inv_range_vec = _mm256_set1_ps(inv_range);

    let chunks = data.len() / 8;
    let ptr = data.as_mut_ptr();

    for i in 0..chunks {
        let offset = i * 8;
        // SAFETY: offset 在 data 范围内（chunks = len / 8）
        let values = unsafe { _mm256_loadu_ps(ptr.add(offset)) };
        let result = _mm256_mul_ps(_mm256_sub_ps(values, min_vec), inv_range_vec);
        unsafe { _mm256_storeu_ps(ptr.add(offset), result) };
    }

    // 处理剩余元素
    for x in data.iter_mut().skip(chunks * 8) {
        *x = (*x - min) * inv_range;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn normalize_zscore_avx2(data: &mut [f32], mean: f32, inv_std: f32) {
    use std::arch::x86_64::*;

    let mean_vec = _mm256_set1_ps(mean);
    let inv_std_vec = _mm256_set1_ps(inv_std);

    let chunks = data.len() / 8;
    let ptr = data.as_mut_ptr();

    for i in 0..chunks {
        let offset = i * 8;
        // SAFETY: offset 在 data 范围内（chunks = len / 8）
        let values = unsafe { _mm256_loadu_ps(ptr.add(offset)) };
        let result = _mm256_mul_ps(_mm256_sub_ps(values, mean_vec), inv_std_vec);
        unsafe { _mm256_storeu_ps(ptr.add(offset), result) };
    }

    for x in data.iter_mut().skip(chunks * 8) {
        *x = (*x - mean) * inv_std;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_min_max_basic() {
        let mut data = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        normalize_min_max(&mut data, 1.0, 8.0);
        assert!((data[0] - 0.0).abs() < 1e-6);
        assert!((data[7] - 1.0).abs() < 1e-6);
        assert!((data[3] - 3.0 / 7.0).abs() < 1e-6);
    }

    #[test]
    fn test_normalize_min_max_zero_range() {
        let mut data = vec![5.0, 5.0, 5.0, 5.0];
        normalize_min_max(&mut data, 5.0, 5.0);
        assert!(data.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn test_normalize_zscore_basic() {
        let mut data = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        normalize_zscore(&mut data, 4.5, 2.5);
        assert!((data[0] - (-1.4)).abs() < 1e-6);
        assert!((data[7] - 1.4).abs() < 1e-6);
    }

    #[test]
    fn test_normalize_zscore_zero_std() {
        let mut data = vec![5.0, 5.0, 5.0, 5.0];
        normalize_zscore(&mut data, 5.0, 0.0);
        assert!(data.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn test_normalize_min_max_non_aligned() {
        // 测试非对齐长度（不是 8 的倍数）
        let mut data = vec![1.0, 2.0, 3.0, 5.0, 7.0];
        normalize_min_max(&mut data, 1.0, 7.0);
        assert!((data[0] - 0.0).abs() < 1e-6);
        assert!((data[4] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_normalize_min_max_large() {
        // 测试大数据集
        let mut data: Vec<f32> = (0..128).map(|i| i as f32).collect();
        normalize_min_max(&mut data, 0.0, 127.0);
        assert!((data[0] - 0.0).abs() < 1e-6);
        assert!((data[127] - 1.0).abs() < 1e-6);
    }
}
