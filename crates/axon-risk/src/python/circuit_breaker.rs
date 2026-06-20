//! Python 端 `CircuitBreaker` —— 单日 PnL 触发的熔断控制(Stage 3 Task 4)。
//!
//! # 暴露的符号
//!
//! - `CircuitBreaker` — 熔断器(独立可单独使用,不依赖 `DefaultRiskEngine`)
//!
//! # Rust API 对齐
//!
//! Rust 端 `crate::circuit_breaker::CircuitBreaker` 仅 4 个方法,本文件
//! 1:1 暴露:
//!
//! - `new(daily_loss_limit: f64, cooldown: Duration)` → Python `CircuitBreaker(daily_loss_limit, cooldown_seconds)`
//! - `is_active(&self) -> bool` → 读 getter `is_active`(避免与方法名同名冲突)
//! - `check_and_trigger(&self, daily_pnl: f64)` → 同步方法
//! - `reset(&self)` → 同步方法
//!
//! # 设计决策
//!
//! - **`Duration` 转 `u64` 秒**:Rust 端 `cooldown: Duration` 在 Python 端
//!   用 `cooldown_seconds: u64` 暴露,降低跨语言时间类型转换复杂度
//!   (与 `RiskConfig.circuit_breaker_cooldown_secs` 保持一致)。
//!
//! - **构造参数本地缓存**:`daily_loss_limit` 和 `cooldown_seconds` 是构造
//!   时传入的不可变配置,Python 端不直接读 Rust 端私有字段(避免改 Rust
//!   端 API 加 getter),而是在 `PyCircuitBreaker` 上缓存构造参数,
//!   通过 getter 暴露。运行时状态(active/activated_at)仍由 Rust 端持有。
//!
//! - **`is_active` 同时是属性与方法**:Rust 方法名 `is_active`,Python
//!   端用 `#[getter]` 暴露属性 `is_active`,便于 `cb.is_active` 这种
//!   属性风格访问(参照 `RiskMetrics.leverage` 等)。`cb.is_active()`
//!   调用也兼容(属性无括号访问 + 方法带括号访问是 PyO3 的双重支持)。
//!
//! - **独立于 `DefaultRiskEngine`**:虽然 `DefaultRiskEngine` 内部组合了
//!   `CircuitBreaker`,但 Python 端**单独**暴露 `CircuitBreaker` 类,
//!   便于用户做轻量级熔断控制(不启风控全套)。

use std::time::Duration;

use pyo3::prelude::*;

use crate::circuit_breaker::CircuitBreaker as RustBreaker;

// ═══════════════════════════════════════════════════════════════════════════
// PyCircuitBreaker
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端 `CircuitBreaker` —— 独立可用的熔断器。
///
/// 注:本类不实现 `Clone`(`RustBreaker` 内部用 `AtomicBool` + `AtomicI64`,
/// `Clone` 语义不明确,且 `&self` 方法不冲突所有权),所以**不**用
/// `from_py_object`。
#[pyclass(name = "CircuitBreaker", skip_from_py_object)]
pub struct PyCircuitBreaker {
    /// Rust 端 `CircuitBreaker`(持有 active flag + activated_at + 私有 limit/cooldown)
    inner: RustBreaker,
    /// 构造时缓存的日内亏损阈值(Rust 端字段私有不暴露 getter,Python 端不可直接读)
    daily_loss_limit: f64,
    /// 构造时缓存的冷却时长(秒,Rust 端 `Duration` 转 `u64` 暴露)
    cooldown_seconds: u64,
}

#[pymethods]
impl PyCircuitBreaker {
    /// 构造熔断器
    ///
    /// Args:
    /// - `daily_loss_limit` (`float`):日内亏损阈值(正值,如 `10000.0`)
    /// - `cooldown_seconds` (`int`):触发后冷却秒数(冷却期内拒绝新订单)
    ///
    /// 示例:
    /// ```text
    /// cb = CircuitBreaker(daily_loss_limit=10000.0, cooldown_seconds=3600)
    /// ```
    #[new]
    fn new(daily_loss_limit: f64, cooldown_seconds: u64) -> Self {
        Self {
            inner: RustBreaker::new(daily_loss_limit, Duration::from_secs(cooldown_seconds)),
            daily_loss_limit,
            cooldown_seconds,
        }
    }

    /// 检查并按需触发熔断
    ///
    /// 触发条件:`daily_pnl <= -daily_loss_limit` 且当前未激活。
    /// 触发后 `is_active` 在冷却期内返回 `True`,冷却期过后自动恢复。
    ///
    /// Args:
    /// - `daily_pnl` (`float`):当日累计 PnL(负值表示亏损)
    fn check_and_trigger(&self, daily_pnl: f64) {
        self.inner.check_and_trigger(daily_pnl);
    }

    /// 强制重置熔断器(管理员操作)
    ///
    /// 重置后 `is_active` 立即返回 `False`,可重新接受订单。
    fn reset(&self) {
        self.inner.reset();
    }

    /// 是否处于激活状态(冷却期内)
    #[getter]
    fn is_active(&self) -> bool {
        self.inner.is_active()
    }

    /// 日内亏损阈值(正值,构造时设置,运行期不变)
    #[getter]
    fn daily_loss_limit(&self) -> f64 {
        self.daily_loss_limit
    }

    /// 冷却时长(秒,构造时设置,运行期不变)
    #[getter]
    fn cooldown_seconds(&self) -> u64 {
        self.cooldown_seconds
    }

    fn __repr__(&self) -> String {
        format!(
            "CircuitBreaker(active={}, daily_loss_limit={}, cooldown_seconds={})",
            self.is_active(),
            self.daily_loss_limit,
            self.cooldown_seconds
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 注册
// ═══════════════════════════════════════════════════════════════════════════

/// 在 `_native.risk` 下注册 `CircuitBreaker`。
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyCircuitBreaker>()
}

// ═══════════════════════════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// 初始状态非激活
    #[test]
    fn initially_inactive() {
        let cb = PyCircuitBreaker::new(10_000.0, 60);
        assert!(!cb.is_active());
    }

    /// 触发条件:`daily_pnl <= -daily_loss_limit`
    #[test]
    fn triggers_on_loss_limit() {
        let cb = PyCircuitBreaker::new(10_000.0, 60);
        cb.check_and_trigger(-10_000.0);
        assert!(cb.is_active());
    }

    /// 触发条件边界:`daily_pnl > -daily_loss_limit` 不触发
    #[test]
    fn does_not_trigger_above_limit() {
        let cb = PyCircuitBreaker::new(10_000.0, 60);
        cb.check_and_trigger(-9_999.0);
        assert!(!cb.is_active());
    }

    /// 正向 PnL 不触发
    #[test]
    fn positive_pnl_does_not_trigger() {
        let cb = PyCircuitBreaker::new(10_000.0, 60);
        cb.check_and_trigger(5_000.0);
        assert!(!cb.is_active());
    }

    /// `reset` 强制重置
    #[test]
    fn reset_clears_active() {
        let cb = PyCircuitBreaker::new(10_000.0, 60);
        cb.check_and_trigger(-10_000.0);
        assert!(cb.is_active());
        cb.reset();
        assert!(!cb.is_active());
    }

    /// 已激活时再次 `check_and_trigger` 不会重置 `activated_at`(`is_active` 仍 True)
    #[test]
    fn repeat_check_and_trigger_keeps_active() {
        let cb = PyCircuitBreaker::new(10_000.0, 60);
        cb.check_and_trigger(-10_000.0);
        cb.check_and_trigger(-15_000.0); // 二次调用,不应影响已激活状态
        assert!(cb.is_active());
    }

    /// 构造参数 getter 正确回读
    #[test]
    fn constructor_params_cached_for_getters() {
        let cb = PyCircuitBreaker::new(5_000.0, 120);
        assert_eq!(cb.daily_loss_limit(), 5_000.0);
        assert_eq!(cb.cooldown_seconds(), 120);
    }

    /// `__repr__` 包含 `CircuitBreaker(` 前缀与关键字段
    #[test]
    fn repr_contains_class_name() {
        let cb = PyCircuitBreaker::new(10_000.0, 60);
        let s = cb.__repr__();
        assert!(s.starts_with("CircuitBreaker("), "got: {s}");
        // 注:Rust `bool` 默认 `Display` 是小写 `false`/`true`,与 Python 风格不同。
        // 这里断言小写形态(实际格式),后续如需 Python 风格可改用 `{:?}`(等价输出)。
        assert!(s.contains("active=false"), "got: {s}");
        assert!(s.contains("daily_loss_limit=10000"), "got: {s}");
        assert!(s.contains("cooldown_seconds=60"), "got: {s}");
    }

    /// `register` 函数签名稳定(编译期断言)
    #[test]
    fn register_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register;
    }
}
