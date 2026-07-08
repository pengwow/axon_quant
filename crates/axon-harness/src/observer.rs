//! Harness 可观测性组件
//!
//! 决策日志、性能指标。

use crate::types::Adjudication;

/// 决策记录
#[derive(Debug, Clone)]
pub struct DecisionRecord {
    /// 时间戳 (Unix 秒)
    pub timestamp: u64,
    /// Agent ID
    pub agent: String,
    /// 动作
    pub action: String,
    /// 裁决结果
    pub adjudication: Adjudication,
    /// 延迟 (纳秒)
    pub latency_ns: u64,
}

/// Harness 性能指标
#[derive(Debug, Clone, Default)]
pub struct HarnessMetrics {
    /// 总决策数
    pub total_decisions: u64,
    /// 批准数
    pub approved_count: u64,
    /// 拒绝数
    pub rejected_count: u64,
    /// 熔断数
    pub circuit_break_count: u64,
    /// 平均延迟 (纳秒)
    pub avg_latency_ns: u64,
}

/// Harness 可观测性组件
///
/// 决策日志、性能指标。
pub struct HarnessObserver {
    /// 决策日志
    decisions: Vec<DecisionRecord>,
    /// 性能指标
    metrics: HarnessMetrics,
}

impl HarnessObserver {
    /// 创建观测器
    pub fn new() -> Self {
        Self {
            decisions: Vec::new(),
            metrics: HarnessMetrics::default(),
        }
    }

    /// 记录决策
    pub fn record_decision(&mut self, record: DecisionRecord) {
        self.metrics.total_decisions += 1;
        match &record.adjudication {
            Adjudication::Approved => self.metrics.approved_count += 1,
            Adjudication::Rejected(_) => self.metrics.rejected_count += 1,
            Adjudication::CircuitBreak => self.metrics.circuit_break_count += 1,
            _ => {}
        }

        // 更新平均延迟
        let total_latency =
            self.metrics.avg_latency_ns * (self.metrics.total_decisions - 1) + record.latency_ns;
        self.metrics.avg_latency_ns = total_latency / self.metrics.total_decisions;

        self.decisions.push(record);
    }

    /// 获取指标
    pub fn metrics(&self) -> &HarnessMetrics {
        &self.metrics
    }

    /// 获取最近 N 条决策
    pub fn recent_decisions(&self, n: usize) -> Vec<&DecisionRecord> {
        let start = self.decisions.len().saturating_sub(n);
        self.decisions[start..].iter().collect()
    }

    /// 清空记录
    pub fn clear(&mut self) {
        self.decisions.clear();
        self.metrics = HarnessMetrics::default();
    }
}

impl Default for HarnessObserver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_record() -> DecisionRecord {
        DecisionRecord {
            timestamp: 1000,
            agent: "market".into(),
            action: "analyze".into(),
            adjudication: Adjudication::Approved,
            latency_ns: 100,
        }
    }

    #[test]
    fn test_record_decision() {
        let mut observer = HarnessObserver::new();
        observer.record_decision(test_record());
        assert_eq!(observer.metrics().total_decisions, 1);
        assert_eq!(observer.metrics().approved_count, 1);
    }

    #[test]
    fn test_recent_decisions() {
        let mut observer = HarnessObserver::new();
        for i in 0..10 {
            observer.record_decision(DecisionRecord {
                timestamp: i,
                ..test_record()
            });
        }
        let recent = observer.recent_decisions(3);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].timestamp, 7);
    }

    #[test]
    fn test_clear() {
        let mut observer = HarnessObserver::new();
        observer.record_decision(test_record());
        observer.clear();
        assert_eq!(observer.metrics().total_decisions, 0);
    }
}
