//! SIMD 加速的订单簿深度计算

/// 批量计算订单簿深度（总价值和总数量）
///
/// 使用 SIMD 并行计算价格×数量的累加。
/// 返回 (total_value, total_quantity)。
pub fn sum_depth(prices: &[f64], quantities: &[f64]) -> (f64, f64) {
    assert_eq!(prices.len(), quantities.len());

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            // SAFETY: AVX2 已通过运行时检测确认可用
            unsafe { return sum_depth_avx2(prices, quantities) }
        }
    }

    sum_depth_scalar(prices, quantities)
}

/// 标量回退
fn sum_depth_scalar(prices: &[f64], quantities: &[f64]) -> (f64, f64) {
    let mut total_value = 0.0;
    let mut total_qty = 0.0;

    for (p, q) in prices.iter().zip(quantities.iter()) {
        total_value += p * q;
        total_qty += q;
    }

    (total_value, total_qty)
}

/// x86_64 AVX2 实现
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn sum_depth_avx2(prices: &[f64], quantities: &[f64]) -> (f64, f64) {
    use std::arch::x86_64::*;

    let len = prices.len();
    let chunks = len / 4;

    let p_ptr = prices.as_ptr();
    let q_ptr = quantities.as_ptr();

    let mut sum_value = _mm256_setzero_pd();
    let mut sum_qty = _mm256_setzero_pd();

    for i in 0..chunks {
        let offset = i * 4;
        // SAFETY: offset 在 prices/quantities 范围内（chunks = len / 4）
        let p = unsafe { _mm256_loadu_pd(p_ptr.add(offset)) };
        let q = unsafe { _mm256_loadu_pd(q_ptr.add(offset)) };
        sum_value = _mm256_add_pd(sum_value, _mm256_mul_pd(p, q));
        sum_qty = _mm256_add_pd(sum_qty, q);
    }

    // 水平求和
    // SAFETY: sum_value/sum_qty 是 __m256d，与 [f64; 4] 布局相同
    let value_arr: [f64; 4] = unsafe { std::mem::transmute(sum_value) };
    let qty_arr: [f64; 4] = unsafe { std::mem::transmute(sum_qty) };

    let mut total_value = value_arr.iter().sum::<f64>();
    let mut total_qty = qty_arr.iter().sum::<f64>();

    // 处理剩余元素
    for i in (chunks * 4)..len {
        total_value += prices[i] * quantities[i];
        total_qty += quantities[i];
    }

    (total_value, total_qty)
}

/// 计算买卖价差
#[allow(dead_code)]
pub fn calculate_spread(best_bid: f64, best_ask: f64) -> f64 {
    best_ask - best_bid
}

/// 计算中间价
#[allow(dead_code)]
pub fn calculate_mid_price(best_bid: f64, best_ask: f64) -> f64 {
    (best_bid + best_ask) * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sum_depth_basic() {
        let prices = vec![100.0, 101.0, 102.0, 103.0];
        let quantities = vec![10.0, 20.0, 30.0, 40.0];
        let (value, qty) = sum_depth(&prices, &quantities);
        assert!((value - (1000.0 + 2020.0 + 3060.0 + 4120.0)).abs() < 1e-6);
        assert!((qty - 100.0).abs() < 1e-6);
    }

    #[test]
    fn test_sum_depth_non_aligned() {
        let prices = vec![100.0, 101.0, 102.0];
        let quantities = vec![10.0, 20.0, 30.0];
        let (value, qty) = sum_depth(&prices, &quantities);
        assert!((value - (1000.0 + 2020.0 + 3060.0)).abs() < 1e-6);
        assert!((qty - 60.0).abs() < 1e-6);
    }

    #[test]
    fn test_calculate_spread() {
        assert!((calculate_spread(100.0, 101.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_calculate_mid_price() {
        assert!((calculate_mid_price(100.0, 102.0) - 101.0).abs() < 1e-6);
    }
}
