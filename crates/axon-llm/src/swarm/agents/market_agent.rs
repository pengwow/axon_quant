//! MarketAgent - 市场分析 Agent

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::swarm::agent::{AgentId, AgentRole, AgentStatus};
use crate::swarm::agent_runner::{DeclarativeAgentRunner, RunnerOutput};
use crate::swarm::error::SwarmError;
use crate::swarm::market_data::MarketDataSource;
use crate::swarm::message::{AgentMessage, MarketSignal, MessageContent, SignalType};

use axon_core::market::Tick;

/// MarketAgent 配置
pub struct MarketAgentConfig {
    /// 分析的交易对
    pub symbols: Vec<String>,
    /// 信号阈值
    pub signal_threshold: f64,
}

impl Default for MarketAgentConfig {
    fn default() -> Self {
        Self {
            symbols: vec!["BTC-USDT".into()],
            signal_threshold: 0.7,
        }
    }
}

/// MarketAgent - 市场分析 Agent
pub struct MarketAgent {
    id: AgentId,
    status: AgentStatus,
    config: MarketAgentConfig,
    #[allow(dead_code)]
    inbox: mpsc::Receiver<AgentMessage>,
    outbox: mpsc::Sender<AgentMessage>,
    /// 市场数据源(0.3.0 P0 接入);为 `None` 时 MarketAgent 不订阅外部数据
    data_source: Option<Box<dyn MarketDataSource>>,
    /// 价格变动阈值(0~1),超过此值才产 MarketSignal
    price_change_threshold: f64,
    /// 已产 MarketSignal 数
    signals_produced: usize,
}

impl MarketAgent {
    /// 创建新的 MarketAgent(无 data_source 版本)
    pub fn new(
        id: AgentId,
        config: MarketAgentConfig,
        inbox: mpsc::Receiver<AgentMessage>,
        outbox: mpsc::Sender<AgentMessage>,
    ) -> Self {
        Self {
            id,
            status: AgentStatus::Idle,
            config,
            inbox,
            outbox,
            data_source: None,
            price_change_threshold: 0.01,
            signals_produced: 0,
        }
    }

    /// 创建新的 MarketAgent 并接入数据源
    pub fn with_data_source(
        id: AgentId,
        config: MarketAgentConfig,
        inbox: mpsc::Receiver<AgentMessage>,
        outbox: mpsc::Sender<AgentMessage>,
        data_source: Box<dyn MarketDataSource>,
    ) -> Self {
        Self {
            id,
            status: AgentStatus::Idle,
            config,
            inbox,
            outbox,
            data_source: Some(data_source),
            price_change_threshold: 0.01,
            signals_produced: 0,
        }
    }

    /// 设置价格变动阈值(>此值才产非 Hold signal)
    pub fn set_price_change_threshold(&mut self, threshold: f64) {
        self.price_change_threshold = threshold;
    }

    /// 获取已产 MarketSignal 数
    pub fn signals_produced(&self) -> usize {
        self.signals_produced
    }

    /// 获取 Agent ID
    pub fn id(&self) -> &AgentId {
        &self.id
    }

    /// 获取角色
    pub fn role(&self) -> AgentRole {
        AgentRole::Market
    }

    /// 获取状态
    pub fn status(&self) -> AgentStatus {
        self.status
    }

    /// 获取配置的交易对
    pub fn symbols(&self) -> &[String] {
        &self.config.symbols
    }

    /// 处理消息
    pub async fn handle_message(&mut self, msg: AgentMessage) -> Result<(), SwarmError> {
        self.status = AgentStatus::Thinking;

        match msg.content {
            MessageContent::Heartbeat => {
                // 心跳响应
                self.status = AgentStatus::Idle;
            }
            MessageContent::Shutdown => {
                self.status = AgentStatus::Failed;
            }
            _ => {
                // 其他消息类型暂不处理
                self.status = AgentStatus::Idle;
            }
        }

        Ok(())
    }

    /// 生成市场信号（模拟）
    pub fn generate_signal(&self, symbol: &str) -> MarketSignal {
        MarketSignal {
            symbol: symbol.to_string(),
            signal_type: SignalType::Hold,
            confidence: 0.5,
            reasoning: "Insufficient data".into(),
        }
    }

    /// 单 tick → MarketSignal(简单规则:价格相对上一 tick 变化 > threshold 时触发 Buy/Sell)
    ///
    /// `prev_price`:上一个 tick 的价格(首次为 `None` → 产 Hold 信号作为基线)
    /// `symbol`:tick 对应的 symbol(`Tick` 类型本身无 symbol 字段,需由调用方注入)
    /// 返回:`MarketSignal`(已基于价格变动方向 + 阈值决策)
    pub fn tick_to_signal(&self, symbol: &str, tick: &Tick, prev_price: Option<f64>) -> MarketSignal {
        Self::build_signal(symbol, tick, prev_price, self.price_change_threshold)
    }

    /// 纯函数版:基于 `(symbol, tick, prev_price, threshold)` 产 `MarketSignal`
    ///
    /// 与 `tick_to_signal` 等价,但不依赖 `self`,可在借用 `&mut self.data_source`
    /// 的循环里直接调用,避免双重借用。
    pub fn build_signal(
        symbol: &str,
        tick: &Tick,
        prev_price: Option<f64>,
        threshold: f64,
    ) -> MarketSignal {
        let price = tick.price.as_f64();

        let prev = match prev_price {
            None => {
                // 第一个 tick:产 Hold 信号(建立基线)
                return MarketSignal {
                    symbol: symbol.to_string(),
                    signal_type: SignalType::Hold,
                    confidence: 0.5,
                    reasoning: "Baseline tick (no previous price)".into(),
                };
            }
            Some(p) => p,
        };

        if prev <= 0.0 {
            return MarketSignal {
                symbol: symbol.to_string(),
                signal_type: SignalType::Hold,
                confidence: 0.5,
                reasoning: "Invalid previous price (≤ 0)".into(),
            };
        }

        let change = (price - prev) / prev;
        if change > threshold {
            MarketSignal {
                symbol: symbol.to_string(),
                signal_type: SignalType::Buy,
                confidence: change.min(1.0),
                reasoning: format!("Price up {:.2}% (> threshold)", change * 100.0),
            }
        } else if change < -threshold {
            MarketSignal {
                symbol: symbol.to_string(),
                signal_type: SignalType::Sell,
                confidence: (-change).min(1.0),
                reasoning: format!("Price down {:.2}% (< -threshold)", change * 100.0),
            }
        } else {
            MarketSignal {
                symbol: symbol.to_string(),
                signal_type: SignalType::Hold,
                confidence: 0.5,
                reasoning: format!("Price change {:.2}% within ±threshold", change * 100.0),
            }
        }
    }

    /// 从 data_source 拉 tick 跑主循环,产 MarketSignal 经 outbox 发出
    ///
    /// 阻塞直到 data_source 返回 `None`(流结束)。
    /// 返回:产出的 MarketSignal 数量。
    pub async fn run_market_loop(&mut self) -> Result<usize, SwarmError> {
        let data_source = match self.data_source.as_mut() {
            Some(ds) => ds,
            None => {
                return Err(SwarmError::Other(
                    "MarketAgent has no data_source attached".into(),
                ))
            }
        };

        let mut prev_price: Option<f64> = None;
        let mut count = 0;
        // 用 config.symbols 第一个作为本次循环绑定的 symbol;
        // data_source.symbols() 与 config.symbols 在 Mock 场景下保持一致。
        let symbol = self
            .config
            .symbols
            .first()
            .cloned()
            .unwrap_or_else(|| "UNKNOWN".to_string());
        // 把 threshold 提到循环外,避免 `data_source` 的可变借用与
        // `self.tick_to_signal` 的不可变借用冲突。
        let threshold = self.price_change_threshold;
        while let Some(tick) = data_source.next_tick().await {
            self.status = AgentStatus::Thinking;
            let signal = Self::build_signal(&symbol, &tick, prev_price, threshold);
            prev_price = Some(tick.price.as_f64());

            let msg = AgentMessage {
                id: crate::swarm::message::MessageId::new(),
                from: self.id.clone(),
                to: AgentId::from_string("orchestrator"),
                correlation_id: None,
                content: MessageContent::MarketAnalysis(signal),
                timestamp: chrono::Utc::now().timestamp(),
            };
            self.outbox.send(msg).await.map_err(|e| {
                SwarmError::MessageSendFailed(format!("outbox send failed: {e}"))
            })?;
            count += 1;
        }
        self.signals_produced += count;
        self.status = AgentStatus::Idle;
        Ok(count)
    }
}

#[async_trait]
impl DeclarativeAgentRunner for MarketAgent {
    fn id(&self) -> &AgentId {
        &self.id
    }
    fn role(&self) -> AgentRole {
        AgentRole::Market
    }
    fn status(&self) -> AgentStatus {
        self.status
    }
    async fn run_step(&mut self, msg: AgentMessage) -> Result<RunnerOutput, SwarmError> {
        // MarketAgent 的 `handle_message` 是无下游消息的版本(主要处理心跳/关闭)
        self.handle_message(msg).await?;
        Ok(RunnerOutput::None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::agent::AgentId;

    #[test]
    fn test_market_agent_creation() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("market_0");
        let config = MarketAgentConfig::default();
        let agent = MarketAgent::new(id.clone(), config, rx, tx);

        assert_eq!(agent.id(), &id);
        assert_eq!(agent.role(), AgentRole::Market);
        assert_eq!(agent.status(), AgentStatus::Idle);
    }

    #[test]
    fn test_market_agent_symbols() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("market_0");
        let config = MarketAgentConfig {
            symbols: vec!["BTC-USDT".into(), "ETH-USDT".into()],
            signal_threshold: 0.8,
        };
        let agent = MarketAgent::new(id, config, rx, tx);

        assert_eq!(agent.symbols().len(), 2);
        assert!(agent.symbols().contains(&"BTC-USDT".to_string()));
    }

    #[test]
    fn test_market_agent_generate_signal() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("market_0");
        let config = MarketAgentConfig::default();
        let agent = MarketAgent::new(id, config, rx, tx);

        let signal = agent.generate_signal("BTC-USDT");
        assert_eq!(signal.symbol, "BTC-USDT");
        assert!(signal.confidence >= 0.0 && signal.confidence <= 1.0);
    }

    #[tokio::test]
    async fn test_market_agent_handle_heartbeat() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("market_0");
        let config = MarketAgentConfig::default();
        let mut agent = MarketAgent::new(id, config, rx, tx);

        let msg = AgentMessage {
            id: crate::swarm::message::MessageId::new(),
            from: AgentId::from_string("orchestrator"),
            to: AgentId::from_string("market_0"),
            correlation_id: None,
            content: MessageContent::Heartbeat,
            timestamp: 1000,
        };

        agent.handle_message(msg).await.unwrap();
        assert_eq!(agent.status(), AgentStatus::Idle);
    }

    /// `MarketAgent` 实现 `DeclarativeAgentRunner`:
    /// - 走 `run_step` 调内部 `handle_message`
    /// - 状态机会从 `Idle` → `Thinking` → `Idle`
    /// - 产出 `RunnerOutput::None`(无下游消息)
    #[tokio::test]
    async fn test_market_agent_runner_trait_impl() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("market_0");
        let config = MarketAgentConfig::default();
        let mut agent = MarketAgent::new(id, config, rx, tx);

        let msg = AgentMessage {
            id: crate::swarm::message::MessageId::new(),
            from: AgentId::from_string("orchestrator"),
            to: AgentId::from_string("market_0"),
            correlation_id: None,
            content: MessageContent::Heartbeat,
            timestamp: 1000,
        };

        let out = agent.run_step(msg).await.unwrap();
        assert!(matches!(out, RunnerOutput::None));
        assert_eq!(agent.status(), AgentStatus::Idle);

        // 也可作为 `dyn DeclarativeAgentRunner` 使用(object-safe)
        let boxed: Box<dyn DeclarativeAgentRunner> = Box::new(agent);
        assert_eq!(boxed.role(), AgentRole::Market);
    }

    /// `MarketAgent::build_signal` 纯函数:基线 / 上涨 / 下跌 / 持平 四类场景
    #[test]
    fn test_market_agent_build_signal_scenarios() {
        use axon_core::market::Side;
        use axon_core::time::Timestamp;
        use axon_core::types::{Price, Quantity};

        let mk = |price: f64| {
            Tick::new(
                Timestamp::from_nanos(0),
                Price::from_f64(price),
                Quantity::from_f64(1.0),
                Side::Buy,
            )
        };

        // 1. 基线:prev_price = None → Hold
        let s = MarketAgent::build_signal("BTC-USDT", &mk(100.0), None, 0.01);
        assert!(matches!(s.signal_type, SignalType::Hold));
        assert!(s.reasoning.contains("Baseline"));

        // 2. 上涨 +5%(> 1% threshold)→ Buy
        let s = MarketAgent::build_signal("BTC-USDT", &mk(105.0), Some(100.0), 0.01);
        assert!(matches!(s.signal_type, SignalType::Buy));
        assert!((s.confidence - 0.05).abs() < 1e-9);

        // 3. 下跌 -5% → Sell
        let s = MarketAgent::build_signal("BTC-USDT", &mk(95.0), Some(100.0), 0.01);
        assert!(matches!(s.signal_type, SignalType::Sell));

        // 4. 微涨 +0.5% (< 1% threshold)→ Hold
        let s = MarketAgent::build_signal("BTC-USDT", &mk(100.5), Some(100.0), 0.01);
        assert!(matches!(s.signal_type, SignalType::Hold));
    }

    /// `MarketAgent::run_market_loop` 接入 MockSourceAdapter:
    /// - 6 tick 价格序列(100→103→102→100→108→100),threshold=0.01
    /// - 期望 6 条 `MarketAnalysis` 消息(基线 1 + Buy/Sell/Hold 混合)
    #[tokio::test]
    async fn test_market_agent_run_loop_with_mock_source() {
        use crate::swarm::market_data::MockSourceAdapter;

        // 两对 channel:inbox(测试 → agent),outbox(agent → 测试,本测试要消费)
        let (inbox_tx, inbox_rx) = mpsc::channel::<AgentMessage>(8);
        let (outbox_tx, mut outbox_rx) = mpsc::channel::<AgentMessage>(16);

        let id = AgentId::from_string("market_0");
        let config = MarketAgentConfig {
            symbols: vec!["BTC-USDT".into()],
            signal_threshold: 0.7,
        };

        // 价格序列:100 → 103 (+3%) → 102 (-1%) → 100 (-1.96%) → 108 (+8%) → 100 (-7.4%)
        let src = MockSourceAdapter::from_tick_series("BTC-USDT", 6, 1, |i| match i {
            0 => 100.0,
            1 => 103.0,
            2 => 102.0,
            3 => 100.0,
            4 => 108.0,
            _ => 100.0,
        });

        // inbox_rx → agent,outbox_tx → agent,src → data_source
        let mut agent = MarketAgent::with_data_source(id, config, inbox_rx, outbox_tx, Box::new(src));
        agent.set_price_change_threshold(0.01);

        let n = agent.run_market_loop().await.unwrap();
        assert_eq!(n, 6);
        assert_eq!(agent.signals_produced(), 6);
        assert_eq!(agent.status(), AgentStatus::Idle);

        // inbox_tx 提前 drop,避免 send 不必要
        drop(inbox_tx);

        // 收取 6 条消息,验证类型
        let mut buy_count = 0;
        let mut sell_count = 0;
        let mut hold_count = 0;
        for _ in 0..6 {
            let msg = outbox_rx.recv().await.expect("must have msg");
            match msg.content {
                MessageContent::MarketAnalysis(sig) => {
                    match sig.signal_type {
                        SignalType::Buy => buy_count += 1,
                        SignalType::Sell => sell_count += 1,
                        SignalType::Hold => hold_count += 1,
                    }
                }
                _ => panic!("expected MarketAnalysis"),
            }
        }
        // 基线(100)Hold, +3% Buy, -1% Hold, -1.96% Sell, +8% Buy, -7.4% Sell
        assert_eq!(hold_count, 2, "expected 2 holds (baseline + micro)");
        assert_eq!(buy_count, 2, "expected 2 buys (+3% +8%)");
        assert_eq!(sell_count, 2, "expected 2 sells (-1.96% -7.4%)");
    }

    /// `MarketAgent::run_market_loop` 无 data_source 时报 Other 错误
    #[tokio::test]
    async fn test_market_agent_run_loop_without_data_source_fails() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("market_0");
        let config = MarketAgentConfig::default();
        let mut agent = MarketAgent::new(id, config, rx, tx);

        let res = agent.run_market_loop().await;
        assert!(res.is_err());
        assert!(matches!(res.unwrap_err(), SwarmError::Other(_)));
    }
}
