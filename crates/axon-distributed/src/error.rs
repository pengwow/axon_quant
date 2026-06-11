//! 统一错误类型

use thiserror::Error;

/// 分布式训练错误
#[derive(Debug, Error)]
pub enum DistributedError {
    /// 配置错误
    #[error("config error: {0}")]
    Config(String),

    /// 校验错误
    #[error("validation error: {0}")]
    Validation(String),

    /// TOML 解析错误
    #[error("toml parse error: {0}")]
    Toml(String),

    /// IO 错误
    #[error("io error: {0}")]
    Io(String),

    /// 序列化错误
    #[error("serialization error: {0}")]
    Serialization(String),

    /// 集群错误
    #[error("cluster error: {0}")]
    Cluster(String),

    /// 算法错误
    #[error("algorithm error: {0}")]
    Algorithm(String),

    /// Checkpoint 错误
    #[error("checkpoint error: {0}")]
    Checkpoint(String),

    /// 参数服务器错误
    #[error("param server error: {0}")]
    ParamServer(String),
}

/// 分布式训练 Result 类型别名
pub type DistributedResult<T> = Result<T, DistributedError>;
