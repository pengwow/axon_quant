//! 仓位守卫
//!
//! 检查新增仓位是否符合单品种和总仓位限制。

use std::collections::HashMap;

/// 仓位守卫
pub struct PositionGuard {
    /// 单品种最大仓位百分比，默认 20%
    max_position_pct: f64,
    /// 总仓位上限百分比，默认 100%
    max_total_utilization_pct: f64,
}

impl PositionGuard {
    /// 创建仓位守卫
    pub fn new(max_position_pct: f64, max_total_utilization_pct: f64) -> Self {
        Self {
            max_position_pct,
            max_total_utilization_pct,
        }
    }

    /// 检查新增仓位是否合规
    ///
    /// - `symbol`: 品种
    /// - `delta_pct`: 新增仓位百分比
    /// - `existing_positions`: 现有仓位（品种 → 仓位百分比）
    pub fn check_position(
        &self,
        symbol: &str,
        delta_pct: f64,
        existing_positions: &HashMap<String, f64>,
    ) -> bool {
        let current = existing_positions.get(symbol).copied().unwrap_or(0.0);
        // 检查单品种限制
        if current + delta_pct > self.max_position_pct {
            return false;
        }
        // 检查总仓位限制
        let total: f64 = existing_positions.values().sum();
        if total + delta_pct > self.max_total_utilization_pct {
            return false;
        }
        true
    }

    /// 当前允许的最大新增仓位
    pub fn max_allowed(&self, existing_positions: &HashMap<String, f64>) -> f64 {
        let total: f64 = existing_positions.values().sum();
        (self.max_total_utilization_pct - total).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_within_limits() {
        let guard = PositionGuard::new(20.0, 100.0);
        let mut positions = HashMap::new();
        positions.insert("BTC".to_string(), 10.0);
        assert!(guard.check_position("BTC", 5.0, &positions));
        assert!(!guard.check_position("BTC", 15.0, &positions)); // 10+15=25 > 20
    }

    #[test]
    fn test_exceeds_total_limit() {
        let guard = PositionGuard::new(50.0, 80.0);
        let mut positions = HashMap::new();
        positions.insert("BTC".to_string(), 40.0);
        positions.insert("ETH".to_string(), 30.0);
        assert!(!guard.check_position("SOL", 15.0, &positions)); // 70+15=85 > 80
    }

    #[test]
    fn test_max_allowed() {
        let guard = PositionGuard::new(20.0, 100.0);
        let mut positions = HashMap::new();
        positions.insert("BTC".to_string(), 30.0);
        positions.insert("ETH".to_string(), 40.0);
        assert!((guard.max_allowed(&positions) - 30.0).abs() < f64::EPSILON);
    }
}
