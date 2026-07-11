//! 统一数据源接口
//!
//! ## 两个内置实现
//!
//! - [`ExchangeStreamSource`]:模拟交易所 WebSocket 接入(测试用 `try_push` 同步推入)
//! - [`ReplayStreamSource`]:从内存 `Vec<Tick>` 顺序回放(给回测 / 单元测试用)
//!
//! ## 设计要点
//!
//! - `next_event` 为 `async fn`,但**当前两个实现都是同步**的(无真正的 WS / 文件 I/O),
//!   后续替换为真 WS 时改 `async` 实现即可,调用方 API 不变
//! - `ExchangeStreamSource::try_push` 用 `Mutex<VecDeque>` 缓冲,
//!   满足 `StreamDataSource: Send + Sync`(PyO3 绑定需要)
//! - `ReplayStreamSource::with_ticks(...)` 接受预构造的 `Vec<Tick>`,
//!   不引入 csv 依赖(CSV 解析留 0.4.0 后续)

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;

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

/// 市场数据事件(流式)
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

/// 交易所 WebSocket 数据源(包装 axon-exchange)
///
/// 当前实现:内部用 `Mutex<VecDeque<MarketDataEvent>>` 缓冲,
/// 通过 `try_push` 同步推入,`next_event` 异步弹出。
/// **测试用**:真正的 WS 接入留给 0.4.0 后续,届时把 `try_push` 替换为
/// `tokio::sync::mpsc::Receiver` 即可,API 兼容。
pub struct ExchangeStreamSource {
    name: String,
    connected: bool,
    buffer: Mutex<VecDeque<MarketDataEvent>>,
}

impl ExchangeStreamSource {
    /// 创建新的交易所数据源
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            connected: false,
            buffer: Mutex::new(VecDeque::new()),
        }
    }

    /// 同步推入事件(给测试 / 外部 WS 适配层用)
    ///
    /// 线程安全:内部用 `Mutex` 保护,可在多线程间调用。
    /// 与 `next_event` 并发安全(buffer 持有顺序保证 FIFO)。
    pub fn try_push(&self, event: MarketDataEvent) {
        let mut buf = self.buffer.lock().expect("buffer mutex");
        buf.push_back(event);
    }

    /// 当前缓冲中待消费事件数
    pub fn buffered(&self) -> usize {
        self.buffer.lock().expect("buffer mutex").len()
    }
}

#[async_trait]
impl StreamDataSource for ExchangeStreamSource {
    async fn subscribe(&mut self, _symbols: &[Symbol]) -> Result<(), StreamError> {
        // 当前无真 WS 连接;直接标 connected
        // 真正的 WS 接入留给 0.4.0 后续
        self.connected = true;
        Ok(())
    }

    async fn next_event(&mut self) -> Option<MarketDataEvent> {
        self.buffer.lock().expect("buffer mutex").pop_front()
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// 文件回放数据源(用于测试)
///
/// 当前实现:从 `with_ticks(symbol, ticks)` 注入的 `Vec<Tick>` 顺序回放。
/// `path` 字段保留,后续 `from_csv` 实装后从 `path` 读 CSV。
pub struct ReplayStreamSource {
    name: String,
    path: PathBuf,
    connected: bool,
    ticks: Vec<MarketDataEvent>,
    cursor: usize,
}

impl ReplayStreamSource {
    /// 创建新的回放数据源
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self {
            name: format!("replay:{}", path.display()),
            path,
            connected: false,
            ticks: Vec::new(),
            cursor: 0,
        }
    }

    /// 注入 tick 序列(从 CSV / 数组构造,后续可加 `from_csv`)
    ///
    /// # 参数
    ///
    /// - `symbol`:所有 tick 归属的 symbol
    /// - `ticks`:按时间顺序排列的 tick 序列
    pub fn with_ticks(mut self, symbol: Symbol, ticks: Vec<Tick>) -> Self {
        self.ticks = ticks
            .into_iter()
            .map(|tick| MarketDataEvent::Tick {
                symbol: symbol.clone(),
                tick,
            })
            .collect();
        self
    }

    /// 剩余 tick 数(未消费的)
    pub fn remaining(&self) -> usize {
        self.ticks.len().saturating_sub(self.cursor)
    }

    /// 已消费 tick 数
    pub fn consumed(&self) -> usize {
        self.cursor
    }
}

#[async_trait]
impl StreamDataSource for ReplayStreamSource {
    async fn subscribe(&mut self, _symbols: &[Symbol]) -> Result<(), StreamError> {
        // 校验:ticks 为空且 path 不存在 → 报 FileNotFound
        // 允许"有 ticks 但 path 不存在"通过(测试场景,不在意真实文件)
        if self.ticks.is_empty() && !self.path.exists() {
            return Err(StreamError::FileNotFound(self.path.display().to_string()));
        }
        self.connected = true;
        Ok(())
    }

    async fn next_event(&mut self) -> Option<MarketDataEvent> {
        if self.cursor >= self.ticks.len() {
            return None;
        }
        let ev = self.ticks[self.cursor].clone();
        self.cursor += 1;
        Some(ev)
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::market::{Side, Tick};
    use axon_core::time::Timestamp;
    use axon_core::types::{Price, Quantity};

    fn sym() -> Symbol {
        Symbol::from("BTC-USDT")
    }

    fn tick(price: f64) -> Tick {
        Tick::new(
            Timestamp::from_nanos(1_000),
            Price::from_f64(price),
            Quantity::from_f64(1.0),
            Side::Buy,
        )
    }

    #[tokio::test]
    async fn exchange_source_try_push_and_next_event_roundtrip() {
        let mut src = ExchangeStreamSource::new("test");
        let _ = src.subscribe(&[sym()]).await;
        assert!(src.is_connected());
        assert_eq!(src.buffered(), 0);

        // 推入 3 个 tick
        for p in [100.0, 101.0, 102.0] {
            src.try_push(MarketDataEvent::Tick {
                symbol: sym(),
                tick: tick(p),
            });
        }
        assert_eq!(src.buffered(), 3);

        // 顺序弹出
        let e1 = src.next_event().await.expect("e1");
        let e2 = src.next_event().await.expect("e2");
        let e3 = src.next_event().await.expect("e3");
        let e4 = src.next_event().await;
        assert!(e4.is_none(), "第 4 次应返回 None");

        // 验证价格顺序
        if let (
            MarketDataEvent::Tick { tick: t1, .. },
            MarketDataEvent::Tick { tick: t2, .. },
            MarketDataEvent::Tick { tick: t3, .. },
        ) = (e1, e2, e3)
        {
            assert_eq!(t1.price.as_f64(), 100.0);
            assert_eq!(t2.price.as_f64(), 101.0);
            assert_eq!(t3.price.as_f64(), 102.0);
        } else {
            panic!("应为 Tick 事件");
        }
    }

    #[tokio::test]
    async fn replay_source_emits_ticks_in_fifo_order() {
        let src = ReplayStreamSource::new("/tmp/nonexistent.csv")
            .with_ticks(sym(), vec![tick(100.0), tick(101.0), tick(102.0)]);
        let mut src = src;
        let _ = src.subscribe(&[sym()]).await;
        assert!(src.is_connected());
        assert_eq!(src.remaining(), 3);

        let e1 = src.next_event().await.expect("e1");
        assert_eq!(src.remaining(), 2);
        if let MarketDataEvent::Tick { tick: t, .. } = e1 {
            assert_eq!(t.price.as_f64(), 100.0);
        } else {
            panic!("Tick 期望");
        }
    }

    #[tokio::test]
    async fn replay_source_drains_to_none_after_last_tick() {
        let mut src = ReplayStreamSource::new("/tmp/nonexistent.csv")
            .with_ticks(sym(), vec![tick(100.0)]);
        let _ = src.subscribe(&[sym()]).await;
        let _ = src.next_event().await.expect("first");
        let none = src.next_event().await;
        assert!(none.is_none());
        assert_eq!(src.remaining(), 0);
        assert_eq!(src.consumed(), 1);
    }

    #[tokio::test]
    async fn replay_source_subscribe_fails_when_no_ticks_and_no_path() {
        // 既无 ticks 又无 path → FileNotFound
        let mut src = ReplayStreamSource::new("/tmp/this_should_not_exist_12345.csv");
        let result = src.subscribe(&[sym()]).await;
        assert!(matches!(result, Err(StreamError::FileNotFound(_))));
    }

    #[tokio::test]
    async fn replay_source_subscribe_succeeds_with_ticks_even_if_path_missing() {
        // 有 ticks 但 path 不存在 → 仍能 subscribe(测试场景,不在意真实文件)
        let mut src = ReplayStreamSource::new("/tmp/this_should_not_exist_67890.csv")
            .with_ticks(sym(), vec![tick(100.0)]);
        let result = src.subscribe(&[sym()]).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn exchange_source_name() {
        let src = ExchangeStreamSource::new("binance-spot");
        assert_eq!(src.name(), "binance-spot");
    }

    #[tokio::test]
    async fn replay_source_name_includes_path() {
        let src = ReplayStreamSource::new("/tmp/test.csv");
        assert_eq!(src.name(), "replay:/tmp/test.csv");
    }
}
