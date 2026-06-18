//! 统一数据源接口

use std::path::PathBuf;

use async_trait::async_trait;
use thiserror::Error;

use axon_core::market::Tick;
use axon_core::types::Symbol;

/// 流式数据源错误
#[derive(Debug, Error)]
pub enum StreamError {
    /// 连接失败
    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    /// 订阅失败
    #[error("subscription failed: {0}")]
    SubscriptionFailed(String),

    /// 数据源断开
    #[error("data source disconnected")]
    Disconnected,

    /// 文件未找到
    #[error("file not found: {0}")]
    FileNotFound(String),

    /// 解析错误
    #[error("parse error: {0}")]
    ParseError(String),
}

/// 市场数据事件（流式）
#[derive(Debug, Clone)]
pub enum MarketDataEvent {
    /// 逐笔成交
    Tick {
        /// 交易品种
        symbol: Symbol,
        /// 成交数据
        tick: Tick,
    },
    /// 心跳
    Heartbeat,
    /// 数据源断开
    Disconnected,
}

/// 统一数据源 trait
#[async_trait]
pub trait StreamDataSource: Send + Sync {
    /// 订阅行情
    async fn subscribe(&mut self, symbols: &[Symbol]) -> Result<(), StreamError>;

    /// 接收下一个行情事件
    async fn next_event(&mut self) -> Option<MarketDataEvent>;

    /// 数据源是否已连接
    fn is_connected(&self) -> bool;

    /// 数据源名称
    fn name(&self) -> &str;
}

/// 交易所 WebSocket 数据源（包装 axon-exchange）
pub struct ExchangeStreamSource {
    name: String,
    connected: bool,
}

impl ExchangeStreamSource {
    /// 创建新的交易所数据源
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            connected: false,
        }
    }
}

#[async_trait]
impl StreamDataSource for ExchangeStreamSource {
    async fn subscribe(&mut self, _symbols: &[Symbol]) -> Result<(), StreamError> {
        // TODO: 实现交易所 WebSocket 订阅
        self.connected = true;
        Ok(())
    }

    async fn next_event(&mut self) -> Option<MarketDataEvent> {
        // TODO: 实现从 WebSocket 接收事件
        None
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// 文件回放数据源（用于测试）
pub struct ReplayStreamSource {
    name: String,
    path: PathBuf,
    connected: bool,
}

impl ReplayStreamSource {
    /// 创建新的回放数据源
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self {
            name: format!("replay:{}", path.display()),
            path,
            connected: false,
        }
    }
}

#[async_trait]
impl StreamDataSource for ReplayStreamSource {
    async fn subscribe(&mut self, _symbols: &[Symbol]) -> Result<(), StreamError> {
        if !self.path.exists() {
            return Err(StreamError::FileNotFound(self.path.display().to_string()));
        }
        self.connected = true;
        Ok(())
    }

    async fn next_event(&mut self) -> Option<MarketDataEvent> {
        // TODO: 实现从文件回放事件
        None
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    fn name(&self) -> &str {
        &self.name
    }
}
