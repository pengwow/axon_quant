use thiserror::Error;

#[derive(Debug, Error)]
pub enum MonitorError {
    #[error("metric not found: {0}")]
    MetricNotFound(String),

    #[error("duplicate metric registration: {0}")]
    DuplicateRegistration(String),
}
