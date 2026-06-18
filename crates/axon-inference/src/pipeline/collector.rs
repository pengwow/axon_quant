//! 多源观测聚合器
//!
//! `ObservationCollector` 从多个 `ObservationSource` 拉取数据，
//! 将其转换为 `Observation` 后送入 `BatchInferencePipeline` 的输入端。
//!
//! 设计目标：
//! - 错误隔离：单个源失败不影响其他源
//! - 后台轮询：`start()` 返回 `JoinHandle`，主线程可继续其他工作
//! - 优雅关闭：sink 关闭时退出循环

use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::interval;

use crate::error::Observation;

/// 观测数据源抽象
///
/// 实现方负责维护自己的状态（如 WebSocket 订阅、文件游标、内存缓冲区等），
/// 在 `poll()` 被调用时尝试产出最新的 `Observation`。
pub trait ObservationSource: Send + 'static {
    /// 拉取一次观测
    ///
    /// 返回：
    /// - `Ok(Some(obs))`：有新数据
    /// - `Ok(None)`：当前无新数据
    /// - `Err(e)`：拉取错误（Collector 会记录 warn 并继续轮询）
    fn poll(&mut self) -> Result<Option<Observation>, Box<dyn std::error::Error + Send + Sync>>;

    /// 源名称（用于日志与调试）
    fn name(&self) -> &str;
}

/// 多源观测聚合器
///
/// 通过 `add_source` 注册任意数量的源，`start()` 启动后台轮询任务。
/// 任务在 sink 关闭（receiver drop）时自动退出。
pub struct ObservationCollector {
    sink: mpsc::Sender<Observation>,
    poll_interval: Duration,
    sources: Vec<Box<dyn ObservationSource>>,
}

impl ObservationCollector {
    /// 创建聚合器
    ///
    /// - `sink`：`BatchInferencePipeline` 的 `obs_tx` 或其他接收端
    /// - `poll_interval`：每个 tick 之间的间隔（默认 100ms 即可）
    pub fn new(sink: mpsc::Sender<Observation>, poll_interval: Duration) -> Self {
        Self {
            sink,
            poll_interval,
            sources: Vec::new(),
        }
    }

    /// 注册数据源
    pub fn add_source(&mut self, source: Box<dyn ObservationSource>) {
        self.sources.push(source);
    }

    /// 当前注册的源数量
    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    /// 启动后台轮询任务，返回 `JoinHandle` 用于 `await` 或 `abort()`
    pub fn start(mut self) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = interval(self.poll_interval);
            // 第一次 tick 立即触发，后续按 interval 节奏
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                // 若所有源都已轮询一遍，本轮结束
                let mut any_sent = false;
                for source in self.sources.iter_mut() {
                    let name = source.name().to_string();
                    match source.poll() {
                        Ok(Some(obs)) => {
                            // 发送失败：receiver 已 drop，Collector 退出
                            if self.sink.send(obs).await.is_err() {
                                tracing::info!(
                                    "ObservationCollector: sink closed, stopping (last source={})",
                                    name
                                );
                                return;
                            }
                            any_sent = true;
                        }
                        Ok(None) => {
                            // 源暂无数据，继续
                        }
                        Err(e) => {
                            // 错误隔离：单个源失败不影响其他源
                            tracing::warn!(
                                "ObservationCollector: source {} poll error: {}",
                                name,
                                e
                            );
                        }
                    }
                }
                // 避免在无源时疯狂空转（无实际意义，但保持代码清晰）
                let _ = any_sent;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// 测试用简单源：每次 poll 返回预定义序列中的一个 Observation
    struct SequenceSource {
        name: String,
        outputs: Arc<Mutex<Vec<Observation>>>,
        poll_count: Arc<Mutex<usize>>,
    }

    impl SequenceSource {
        fn new(name: &str, outputs: Vec<Observation>) -> Self {
            Self {
                name: name.to_string(),
                outputs: Arc::new(Mutex::new(outputs)),
                poll_count: Arc::new(Mutex::new(0)),
            }
        }
    }

    impl ObservationSource for SequenceSource {
        fn poll(
            &mut self,
        ) -> Result<Option<Observation>, Box<dyn std::error::Error + Send + Sync>> {
            let mut count = self.poll_count.lock().unwrap();
            *count += 1;
            let mut outputs = self.outputs.lock().unwrap();
            Ok(outputs.pop())
        }

        fn name(&self) -> &str {
            &self.name
        }
    }

    /// 测试用总是报错的源
    struct ErrorSource;

    impl ObservationSource for ErrorSource {
        fn poll(
            &mut self,
        ) -> Result<Option<Observation>, Box<dyn std::error::Error + Send + Sync>> {
            Err("simulated source error".into())
        }

        fn name(&self) -> &str {
            "error_source"
        }
    }

    fn make_obs(symbol: &str) -> Observation {
        Observation {
            symbol: symbol.into(),
            timestamp_ns: 1_000_000_000,
            features: vec![0.0; 4],
        }
    }

    #[tokio::test]
    async fn test_collector_aggregates_from_multiple_sources() {
        let (tx, mut rx) = mpsc::channel(16);
        let mut collector = ObservationCollector::new(tx, Duration::from_millis(10));

        // 倒序 pop：每次 poll 返回最右元素
        collector.add_source(Box::new(SequenceSource::new(
            "src_a",
            vec![make_obs("A1"), make_obs("A2")],
        )));
        collector.add_source(Box::new(SequenceSource::new("src_b", vec![make_obs("B1")])));

        assert_eq!(collector.source_count(), 2);
        let handle = collector.start();

        // 等若干 tick 直到收到 3 个 Observation
        let mut received = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        while received.len() < 3 && std::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
                Ok(Some(obs)) => received.push(obs),
                _ => continue,
            }
        }
        handle.abort();

        let symbols: Vec<String> = received.iter().map(|o| o.symbol.clone()).collect();
        // 不关心顺序，只要 3 个都收到
        assert!(symbols.contains(&"A1".to_string()));
        assert!(symbols.contains(&"A2".to_string()));
        assert!(symbols.contains(&"B1".to_string()));
    }

    #[tokio::test]
    async fn test_collector_continues_on_source_error() {
        let (tx, mut rx) = mpsc::channel(16);
        let mut collector = ObservationCollector::new(tx, Duration::from_millis(10));

        // 错误源 + 正常源同时存在
        collector.add_source(Box::new(ErrorSource));
        collector.add_source(Box::new(SequenceSource::new("good", vec![make_obs("G1")])));

        let handle = collector.start();
        let obs = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;
        handle.abort();

        let obs = obs.expect("应能在超时内收到 Observation").unwrap();
        assert_eq!(obs.symbol, "G1");
    }

    #[tokio::test]
    async fn test_collector_stops_when_sink_closed() {
        let (tx, rx) = mpsc::channel(2);
        let mut collector = ObservationCollector::new(tx, Duration::from_millis(5));
        collector.add_source(Box::new(SequenceSource::new(
            "src",
            (0..100).map(|i| make_obs(&format!("S{i}"))).collect(),
        )));

        // 立即 drop receiver，模拟 sink 已关闭
        drop(rx);

        let handle = collector.start();
        // 给 collector 几次 tick 时间感知到 sink 关闭
        tokio::time::sleep(Duration::from_millis(50)).await;
        // 任务应已自动退出（join 不应阻塞过久）
        let res = tokio::time::timeout(Duration::from_millis(200), handle).await;
        assert!(res.is_ok(), "sink 关闭后 collector 应在超时内结束");
    }

    #[tokio::test]
    async fn test_collector_handles_none_observation() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut collector = ObservationCollector::new(tx, Duration::from_millis(10));
        // 空源（pop 后内部 outputs 为空，poll 返回 None）
        collector.add_source(Box::new(SequenceSource::new("empty", vec![])));
        let handle = collector.start();

        // 等待若干 tick 验证 collector 不 panic
        tokio::time::sleep(Duration::from_millis(30)).await;
        handle.abort();
        // sink 仍应可用且无数据
        assert!(rx.try_recv().is_err());
    }
}
