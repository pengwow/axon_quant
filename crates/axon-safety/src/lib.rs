//! 生产级安全组件
//!
//! | 模块 | 说明 |
//! |------|------|
//! | [`circuit_breaker`] | 熔断器（CLOSED/OPEN/HALF_OPEN 状态机） |
//! | [`audit`] | 审计链（Blake3 哈希链，防篡改） |
//! | [`position`] | 仓位守卫（单品种/总仓位限制） |

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod audit;
pub mod circuit_breaker;
pub mod position;

pub use audit::{AuditChain, AuditEntry};
pub use circuit_breaker::{CircuitBreaker, CircuitBreakerConfig};
pub use position::PositionGuard;
