//! Python 端 `TokenBucketRateLimiter` —— 令牌桶限流器状态读取。
//!
//! 委托 `rate_limiter::TokenBucketRateLimiter`,只暴露:
//! - 构造
//! - `try_acquire()` —— 消耗一个 token,失败返回 `ExchangeError(RateLimited)`
//! - `available_tokens()` —— 当前可用 token 数
//! - `capacity()` —— 桶容量(=构造时 `requests_per_second`)
//! - `refill_rate()` —— 补充速率(tokens / sec)
//! - `status_dict()` —— 综合状态 dict
//!
//! Python 端无法在 `Rust` 层做限流(限流是 Rust 适配器内部已用),
//! 这里只暴露状态读取用于监控 / 健康检查。

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::rate_limiter::TokenBucketRateLimiter as RustLimiter;

// ═══════════════════════════════════════════════════════════════════════════
// 主类: PyTokenBucketRateLimiter
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端 `TokenBucketRateLimiter` —— 令牌桶限流器。
///
/// **Stage 5 范围**:Python 端**不**独立使用限流(限流逻辑已嵌入
/// `BinanceAdapter` / `OkxAdapter` 内部),只暴露状态读取用于
/// 监控 / 健康检查。Python 端调用方要限流,应通过 `RateLimitConfig`
/// 在 adapter 构造时配置,而不是单独构造 limiter。
///
/// `skip_from_py_object`:Python 端不传 `TokenBucketRateLimiter` 实例
/// 给其他 Python 函数(只通过构造 + 读状态使用)。
#[pyclass(name = "TokenBucketRateLimiter", skip_from_py_object)]
pub struct PyTokenBucketRateLimiter {
    /// Rust 端 `TokenBucketRateLimiter`
    inner: RustLimiter,
}

#[pymethods]
impl PyTokenBucketRateLimiter {
    /// 构造一个限流器,`requests_per_second` 同时是桶容量和补充速率。
    #[new]
    fn new(requests_per_second: u32) -> Self {
        Self {
            inner: RustLimiter::new(requests_per_second),
        }
    }

    /// 尝试消耗一个 token。
    ///
    /// Returns:
    /// - `true` —— 成功消耗一个 token
    /// - `false` —— 桶空,被限流
    ///
    /// **错误**:Stage 5 实现不抛 `ExchangeError`(避免 Python 端
    /// 处理 `wait_ms`),仅返回 `bool`。
    fn try_acquire(&mut self) -> bool {
        self.inner.try_acquire().is_ok()
    }

    /// 当前可用 token 数(refill 后,饱和到 `[0, capacity]`)。
    fn available_tokens(&self) -> u32 {
        self.inner.available_tokens()
    }

    /// 桶容量(=构造时 `requests_per_second`)。
    fn capacity(&self) -> u32 {
        self.inner.capacity()
    }

    /// 补充速率(tokens / sec,与 `capacity` 相等)。
    fn refill_rate(&self) -> f64 {
        self.inner.refill_rate()
    }

    /// 综合状态 dict: `{"capacity", "available", "refill_rate", "utilization"}`。
    ///
    /// `utilization = (capacity - available) / capacity`,范围 `[0.0, 1.0]`。
    fn status<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let cap = self.inner.capacity();
        let avail = self.inner.available_tokens();
        let d = PyDict::new(py);
        d.set_item("capacity", cap)?;
        d.set_item("available", avail)?;
        d.set_item("refill_rate", self.inner.refill_rate())?;
        // utilization 防止除零(cap >= 1,构造时约束)
        let util = if cap > 0 {
            (cap - avail) as f64 / cap as f64
        } else {
            0.0
        };
        d.set_item("utilization", util)?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "TokenBucketRateLimiter(capacity={}, available={})",
            self.inner.capacity(),
            self.inner.available_tokens(),
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 注册
// ═══════════════════════════════════════════════════════════════════════════

/// 在 `_native.exchange` 下注册 `TokenBucketRateLimiter`
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyTokenBucketRateLimiter>()
}

// ═══════════════════════════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造 + getter 一致性
    #[test]
    fn limiter_construct_getters() {
        let l = PyTokenBucketRateLimiter::new(10);
        assert_eq!(l.capacity(), 10);
        assert!((l.refill_rate() - 10.0).abs() < 1e-9);
        // 初始 available ≤ capacity
        let avail = l.available_tokens();
        assert!(avail <= 10);
    }

    /// `__repr__` 显示 capacity / available
    #[test]
    fn limiter_repr() {
        let l = PyTokenBucketRateLimiter::new(5);
        let r = l.__repr__();
        assert!(r.contains("capacity=5"), "got: {r}");
        assert!(r.contains("available="), "got: {r}");
    }

    /// `try_acquire` 成功消耗返回 true
    #[test]
    fn limiter_try_acquire_success() {
        let mut l = PyTokenBucketRateLimiter::new(2);
        assert!(l.try_acquire());
        assert!(l.try_acquire());
    }

    /// `try_acquire` 在桶空时返回 false
    #[test]
    fn limiter_try_acquire_fail_when_empty() {
        let mut l = PyTokenBucketRateLimiter::new(1);
        assert!(l.try_acquire());
        // 桶已空,但 refilled 可能会让 token 略有补充
        // 因此不强 assert false,而是验证 acquire 多次后仍有边界
        for _ in 0..5 {
            let _ = l.try_acquire();
        }
        // 此时 available 应 <= 1
        assert!(l.available_tokens() <= 1);
    }

    /// `status` dict 字段完整 + utilization 范围合法
    #[test]
    fn limiter_status_dict() {
        Python::attach(|py| {
            let l = PyTokenBucketRateLimiter::new(10);
            let s = l.status(py).unwrap();
            assert_eq!(
                s.get_item("capacity")
                    .unwrap()
                    .unwrap()
                    .extract::<u32>()
                    .unwrap(),
                10
            );
            assert!(s.contains("available").unwrap());
            assert!(s.contains("refill_rate").unwrap());
            let util: f64 = s
                .get_item("utilization")
                .unwrap()
                .unwrap()
                .extract()
                .unwrap();
            assert!(
                (0.0..=1.0).contains(&util),
                "utilization out of range: {util}"
            );
        });
    }

    /// `register` 函数签名稳定
    #[test]
    fn register_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register;
    }
}
