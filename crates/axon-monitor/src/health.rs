use std::time::Instant;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentHealth {
    pub name: String,
    pub status: HealthStatus,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheck {
    pub status: HealthStatus,
    pub components: Vec<ComponentHealth>,
    pub uptime_secs: u64,
}

pub struct HealthService {
    start_time: Instant,
}

impl HealthService {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
        }
    }

    pub fn check(&self, components: Vec<ComponentHealth>) -> HealthCheck {
        let worst = components
            .iter()
            .map(|c| match c.status {
                HealthStatus::Healthy => 0,
                HealthStatus::Degraded => 1,
                HealthStatus::Unhealthy => 2,
            })
            .max()
            .unwrap_or(0);

        let status = match worst {
            0 => HealthStatus::Healthy,
            1 => HealthStatus::Degraded,
            _ => HealthStatus::Unhealthy,
        };

        HealthCheck {
            status,
            components,
            uptime_secs: self.start_time.elapsed().as_secs(),
        }
    }
}

impl Default for HealthService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_healthy() {
        let service = HealthService::new();
        let check = service.check(vec![ComponentHealth {
            name: "db".into(),
            status: HealthStatus::Healthy,
            message: "ok".into(),
        }]);
        assert_eq!(check.status, HealthStatus::Healthy);
    }

    #[test]
    fn test_degraded() {
        let service = HealthService::new();
        let check = service.check(vec![
            ComponentHealth {
                name: "db".into(),
                status: HealthStatus::Healthy,
                message: "ok".into(),
            },
            ComponentHealth {
                name: "cache".into(),
                status: HealthStatus::Degraded,
                message: "slow".into(),
            },
        ]);
        assert_eq!(check.status, HealthStatus::Degraded);
    }
}
