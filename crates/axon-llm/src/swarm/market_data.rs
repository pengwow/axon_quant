//! Market data source 抽象
//!
//! `MarketAgent` 通过此 trait 拉取市场 tick 流(timestamp + price + qty + side),
//! 把"价格变动 → MarketSignal"决策逻辑与具体数据源解耦。
//!
//! ## 设计
//!
//! - **trait 优先**:不绑死具体数据源(WS / CSV / Mock),任何实现此 trait 的
//!   数据源都能喂给 `MarketAgent`
//! - **pull 模型**:`async fn next_tick()` 由 agent 主循环主动拉取,易于限流 / 暂停
//! - **`Send` 约束**:允许 `tokio::spawn` 跑 agent loop
//!
//! ## 当前内置实现
//!
//! - [`MockSourceAdapter`] —— 包装 `axon_data::sources::MockSource`(测试 / 演示用)
//!
//! ## 未来扩展
//!
//! - `WsMarketDataSource`(从 `axon_data::ws` 接收 Binance/OKX 实时 tick)
//! - `CsvMarketDataSource`(回放历史数据)

use async_trait::async_trait;

use axon_core::market::{Side, Tick};
use axon_core::time::Timestamp;
use axon_core::types::{Price, Quantity};

/// 市场数据源 trait
///
/// 实现者持有 1 个或多个 symbol 的 tick 流,逐个 yield 给消费者。
/// `next_tick()` 在无更多数据时返回 `None`,消费者据此结束循环。
///
/// `Send + Sync` 约束允许 `Box<dyn MarketDataSource>` 在 `SwarmOrchestrator` 中
/// 通过 `Arc<dyn DeclarativeAgentRunner>(后者要求 Send+Sync)` 共享。
#[async_trait]
pub trait MarketDataSource: Send + Sync {
    /// 数据源名称(用于日志 / 监控)
    fn name(&self) -> &str;

    /// 拉取下一个 tick
    ///
    /// 返回 `None` 表示流结束(测试结束 / WS 断开)。
    async fn next_tick(&mut self) -> Option<Tick>;

    /// 支持的交易对列表(用于 MarketAgent 过滤不感兴趣的 symbol)
    fn symbols(&self) -> Vec<String>;
}

// ═══════════════════════════════════════════════════════════════════════════
// MockSourceAdapter — 包装 axon_data::sources::MockSource 的本地 tick 列表
// ═══════════════════════════════════════════════════════════════════════════

/// Mock 数据源适配器 — 内部保存 `Vec<Tick>`,逐个 yield 给消费者
///
/// 直接构造(不依赖 `axon_data` 的具体类型),方便单元测试。
/// 如果需要从 `MockSource` 构造,可用 [`MockSourceAdapter::from_ticks`] 把
/// `MockSource::with_tick_series` 生成的 tick 列表转过来。
pub struct MockSourceAdapter {
    name: String,
    ticks: Vec<Tick>,
    /// 下一个要 yield 的 tick 索引
    cursor: usize,
}

impl MockSourceAdapter {
    /// 构造一个 mock 数据源(从 tick 列表)
    pub fn from_ticks(name: impl Into<String>, ticks: Vec<Tick>) -> Self {
        Self {
            name: name.into(),
            ticks,
            cursor: 0,
        }
    }

    /// 构造一个时间序列 mock(简化 API,等价于 `MockSource::with_tick_series`)
    pub fn from_tick_series<F>(
        name: impl Into<String>,
        count: usize,
        nanos_per_step: i64,
        price_fn: F,
    ) -> Self
    where
        F: Fn(usize) -> f64,
    {
        let mut ticks = Vec::with_capacity(count);
        for i in 0..count {
            ticks.push(Tick::new(
                Timestamp::from_nanos(i as i64 * nanos_per_step),
                Price::from_f64(price_fn(i)),
                Quantity::from_f64(1.0),
                Side::Buy,
            ));
        }
        Self {
            name: name.into(),
            ticks,
            cursor: 0,
        }
    }

    /// 剩余 tick 数(测试可观察)
    pub fn remaining(&self) -> usize {
        self.ticks.len().saturating_sub(self.cursor)
    }

    /// 已 yield 的 tick 数
    pub fn consumed(&self) -> usize {
        self.cursor
    }
}

#[async_trait]
impl MarketDataSource for MockSourceAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    async fn next_tick(&mut self) -> Option<Tick> {
        if self.cursor < self.ticks.len() {
            let tick = self.ticks[self.cursor].clone();
            self.cursor += 1;
            Some(tick)
        } else {
            None
        }
    }

    fn symbols(&self) -> Vec<String> {
        // mock 数据源:返回 1 个 symbol(name 即 symbol)
        vec![self.name.clone()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 空 mock 立即返回 None
    #[tokio::test]
    async fn empty_mock_returns_none_immediately() {
        let mut src = MockSourceAdapter::from_tick_series("btc", 0, 1, |_| 0.0);
        assert_eq!(src.name(), "btc");
        assert_eq!(src.remaining(), 0);
        assert!(src.next_tick().await.is_none());
    }

    /// 3 tick 序列 + cursor 推进 + remaining 递减
    #[tokio::test]
    async fn mock_yields_ticks_in_order() {
        let mut src = MockSourceAdapter::from_tick_series("btc", 3, 100, |i| 100.0 + i as f64);
        assert_eq!(src.remaining(), 3);
        let t1 = src.next_tick().await.unwrap();
        assert_eq!(t1.timestamp.nanos, 0);
        assert!((t1.price.as_f64() - 100.0).abs() < 1e-9);
        let t2 = src.next_tick().await.unwrap();
        assert_eq!(t2.timestamp.nanos, 100);
        let t3 = src.next_tick().await.unwrap();
        assert_eq!(t3.timestamp.nanos, 200);
        assert_eq!(src.remaining(), 0);
        assert_eq!(src.consumed(), 3);
        // 4th 应返回 None
        assert!(src.next_tick().await.is_none());
    }

    /// symbols() 返回数据源名
    #[tokio::test]
    async fn mock_symbols_returns_name() {
        let src = MockSourceAdapter::from_tick_series("eth-usdt", 5, 1, |_| 0.0);
        let symbols = src.symbols();
        assert_eq!(symbols, vec!["eth-usdt".to_string()]);
    }

    /// Trait 必须 object-safe(`Box<dyn MarketDataSource>` 可用)
    #[test]
    fn trait_is_object_safe() {
        let _: Box<dyn MarketDataSource> =
            Box::new(MockSourceAdapter::from_tick_series("x", 1, 1, |_| 0.0));
    }
}
