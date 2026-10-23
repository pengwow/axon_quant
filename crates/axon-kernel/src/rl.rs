//! RL 推理 Host：把模型接到统一主回路
//!
//! 对内核而言 RL 与规则策略没有区别：收到收市 bar → 吐 `OrderIntent`。差异只在
//! Host 内部：观测由 [`ObservationSpec`] 组装（与训练侧同一份代码），动作经
//! [`ActionDecoder::decode_target_ratio`] 解码为权益占比 `∈ [-1, 1]`，最终以
//! `SetTarget` 交给 sink 换算数量。
//!
//! # 为何不依赖 pyo3
//!
//! 推理后端用 [`InferenceBackend`] trait 抽象，内核只认识「`obs -> Action`」。
//! V1 的 SB3 / Python 回调由 `axon-worker` 侧包一层实现（设计 6.3），内核保持
//! 纯 Rust、可独立单测。

use axon_core::{Bar, Instrument};
use axon_rl::env::action_decoder::ActionDecoder;
use axon_rl::env::error::EnvResult;
use axon_rl::env::types::MarketBar;
use axon_rl::{Action, ActionSpace, ObservationInput, ObservationSpec, PortfolioState};

use crate::event::BarClosedEvent;
use crate::host::{HostCtx, OrderIntent, StrategyHost};

/// 推理后端
///
/// 同步 trait：每 bar 一次调用，模型推理耗时属于策略计算成本，不为它开异步
/// 分支（设计 4.4 / 7.2 同一立场）。
///
/// `ponytail:` V1 后端是同步 trait，SB3 / SciPy 回调在 `axon-worker` 侧包一层
/// 实现；若将来推理变成异步或需要批量预取，升级路径是新增一个 `infer_batch`
/// 变体，而不是把 `on_event` 改成 async。
pub trait InferenceBackend: Send {
    /// 用观测向量推理出一个动作
    fn infer(&mut self, obs: &[f64]) -> Action;
    /// 模型标识（写入日志 / journal 的产物三元组之一）
    fn model_id(&self) -> &str;
}

/// RL 策略 Host
pub struct RLStrategyHost {
    /// 推理后端（可替换）
    backend: Box<dyn InferenceBackend>,
    /// 观测组装规格（与训练侧共用）
    spec: ObservationSpec,
    /// 本 Host 负责的品种
    symbol: Instrument,
    /// 已收市窗口，最多 `spec.window_size` 根，按时间升序存放
    ///
    /// 对外组装观测时会反转成「最新在前」，与 `TradingEnv` 收集历史的顺序一致。
    history: Vec<MarketBar>,
    /// 动作解码器（动作空间由模型决定）
    decoder: ActionDecoder,
}

impl RLStrategyHost {
    /// 构造 Host
    ///
    /// `action_space` 必须与训练该模型时使用的动作空间一致，否则
    /// `decode_target_ratio` 的裁剪边界会错。
    pub fn new(
        backend: Box<dyn InferenceBackend>,
        spec: ObservationSpec,
        symbol: Instrument,
        action_space: ActionSpace,
    ) -> EnvResult<Self> {
        let decoder = ActionDecoder::from_space(&action_space, &crate::core::symbol_key(&symbol))?;
        Ok(Self {
            backend,
            spec,
            symbol,
            history: Vec::new(),
            decoder,
        })
    }

    /// 替换推理后端（热更新）
    ///
    /// **只在 bar 边界调用**：本方法不能在 `on_event` 执行期间（mid-bar）被触发，
    /// 否则同一根 bar 的观测与动作可能来自两个模型。约定调用方在主回路之外、
    /// 两次 `on_event` 之间完成 `build` → `swap`（设计 6.4 第 3 条）。
    pub fn replace_backend(&mut self, backend: Box<dyn InferenceBackend>) {
        self.backend = backend;
    }

    /// 当前模型标识
    pub fn model_id(&self) -> &str {
        self.backend.model_id()
    }

    /// 当前已收市窗口（按时间升序）
    pub fn history(&self) -> &[MarketBar] {
        &self.history
    }

    /// 观测规格（只读）
    pub fn spec(&self) -> &ObservationSpec {
        &self.spec
    }
}

impl StrategyHost for RLStrategyHost {
    fn name(&self) -> &str {
        self.backend.model_id()
    }

    fn on_event(&mut self, ev: &BarClosedEvent<'_>, ctx: &HostCtx) -> Vec<OrderIntent> {
        // 多 Host 共用主回路时按品种自行过滤
        if ev.instrument != &self.symbol {
            return Vec::new();
        }

        let bar = market_bar_of(ev.bar);

        // 1) 维护已收市窗口：只保留最近 window_size 根
        self.history.push(bar);
        if self.history.len() > self.spec.window_size {
            self.history.remove(0);
        }

        // 2) 组合视图：HostCtx 只有持仓 / 权益 / 盯市价，现金按权益反推
        let position = ctx.positions.get(&self.symbol).copied().unwrap_or(0.0);
        let last_price = ctx
            .last_price
            .get(&self.symbol)
            .copied()
            .filter(|price| *price > 0.0)
            .unwrap_or(bar.close);
        let equity = if ctx.equity > 0.0 {
            ctx.equity
        } else {
            position.abs() * last_price
        };
        let portfolio = PortfolioState {
            position,
            cash: equity - position * last_price,
            portfolio_value: equity,
            last_price,
            ..Default::default()
        };

        // 3) 观测：最新在前的窗口，与 TradingEnv 的口径一致
        let window: Vec<MarketBar> = self.history.iter().rev().copied().collect();
        let input = ObservationInput {
            bar: &bar,
            history: &window,
            portfolio: &portfolio,
        };
        let obs = match self.spec.try_build(&input) {
            Ok(obs) => obs,
            Err(err) => {
                // 坏观测不喂模型：显式 Hold（空 vec = 维持上次目标），由日志暴露
                tracing::warn!(
                    model = self.backend.model_id(),
                    instrument = %self.symbol,
                    error = %err,
                    "rl host: observation build failed, holding"
                );
                return Vec::new();
            }
        };

        // 4) 推理 + 动作解码为权益占比（内核侧唯一解码入口）
        let action = self.backend.infer(&obs);
        vec![OrderIntent::SetTarget {
            instrument: self.symbol.clone(),
            target_ratio: self.decoder.decode_target_ratio(&action),
        }]
    }
}

/// `axon_core::Bar`（纳秒时间戳）→ [`MarketBar`]（毫秒时间戳）
///
/// 单位换算只发生在 bar 边界这一处：`ObservationSpec` 在 `axon-rl`，其时间特征
/// 按毫秒口径计算，与 `TradingEnv` 完全一致。
fn market_bar_of(bar: &Bar) -> MarketBar {
    MarketBar {
        timestamp: (bar.timestamp.nanos.max(0) / 1_000_000) as u64,
        open: bar.open.as_f64(),
        high: bar.high.as_f64(),
        low: bar.low.as_f64(),
        close: bar.close.as_f64(),
        volume: bar.volume.as_f64(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axon_core::types::{SpotInstrument, Symbol};
    use axon_core::{Price, Quantity, Timestamp};

    use super::*;
    use crate::core::{KernelConfig, SeedLiquidity};
    use crate::kernel_loop::Kernel;
    use crate::pump::GridFrame;
    use crate::sink::BacktestSink;
    use axon_rl::observation::types::{FeatureConfig, FeatureSource, NormalizerType};

    const MIN: i64 = 60_000_000_000;

    /// 固定输出一个连续动作的假后端，同时记录每次收到的观测长度
    struct FixedBackend {
        model: String,
        value: f64,
        lens: Arc<Mutex<Vec<usize>>>,
    }

    impl FixedBackend {
        fn new(model: &str, value: f64) -> Self {
            Self {
                model: model.to_string(),
                value,
                lens: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl InferenceBackend for FixedBackend {
        fn infer(&mut self, obs: &[f64]) -> Action {
            self.lens.lock().unwrap().push(obs.len());
            Action::continuous(vec![self.value])
        }

        fn model_id(&self) -> &str {
            &self.model
        }
    }

    fn btc() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        })
    }

    fn eth() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from("ETH"),
            quote: Symbol::from("USDT"),
        })
    }

    fn bar(ts: i64, close: f64) -> Bar {
        Bar {
            timestamp: Timestamp::from_nanos(ts),
            open: Price::from_f64(close),
            high: Price::from_f64(close + 1.0),
            low: Price::from_f64(close - 1.0),
            close: Price::from_f64(close),
            volume: Quantity::from_f64(1_000.0),
        }
    }

    fn spec(window: usize) -> ObservationSpec {
        ObservationSpec::new(
            window,
            vec![FeatureConfig {
                name: "close".into(),
                source: FeatureSource::PriceField("close".into()),
                normalizer: NormalizerType::None,
                clip_range: None,
            }],
        )
        .unwrap()
        .with_version("v1-test")
    }

    fn host(window: usize, value: f64) -> RLStrategyHost {
        RLStrategyHost::new(
            Box::new(FixedBackend::new("fake-model", value)),
            spec(window),
            btc(),
            ActionSpace::Continuous(axon_rl::action::types::ContinuousActionSpace::new(
                -1.0, 1.0,
            )),
        )
        .unwrap()
    }

    fn ctx(equity: f64, position: f64, price: f64) -> HostCtx {
        HostCtx {
            positions: [(btc(), position)].into_iter().collect(),
            equity,
            last_price: [(btc(), price)].into_iter().collect(),
        }
    }

    /// 跑一根 bar 并返回 `SetTarget` 的占比（空 vec 表示 Hold）
    fn run_bar(host: &mut RLStrategyHost, b: Bar, ctx: &HostCtx) -> Option<f64> {
        let null = serde_json::Value::Null;
        let ev = BarClosedEvent {
            instrument: &btc(),
            interval: "1m",
            bar: &b,
            ts: Timestamp::from_nanos(b.timestamp.nanos + MIN),
            state: &null,
        };
        let intents = host.on_event(&ev, ctx);
        match intents.as_slice() {
            [] => None,
            [OrderIntent::SetTarget { target_ratio, .. }] => Some(*target_ratio),
            other => panic!("unexpected intents: {other:?}"),
        }
    }

    #[test]
    fn test_host_ratio_history_and_window_truncation() {
        let mut h = host(2, 0.5);
        let c = ctx(10_000.0, 0.0, 100.0);

        assert_eq!(run_bar(&mut h, bar(0, 100.0), &c), Some(0.5));
        assert_eq!(h.history().len(), 1);
        assert_eq!(run_bar(&mut h, bar(MIN, 101.0), &c), Some(0.5));
        assert_eq!(run_bar(&mut h, bar(2 * MIN, 102.0), &c), Some(0.5));

        // 窗口只保留最近 2 根，且按时间升序
        let closes: Vec<f64> = h.history().iter().map(|b| b.close).collect();
        assert_eq!(closes, vec![101.0, 102.0]);
        assert_eq!(h.model_id(), "fake-model");
    }

    #[test]
    fn test_host_rejects_other_instruments() {
        let mut h = host(2, 0.5);
        let null = serde_json::Value::Null;
        let b = bar(0, 100.0);
        let ev = BarClosedEvent {
            instrument: &eth(),
            interval: "1m",
            bar: &b,
            ts: Timestamp::from_nanos(MIN),
            state: &null,
        };
        assert!(h.on_event(&ev, &ctx(10_000.0, 0.0, 100.0)).is_empty());
        assert!(h.history().is_empty(), "别人的 bar 不进入本 Host 窗口");
    }

    #[test]
    fn test_backend_replacement_takes_effect_at_bar_boundary() {
        let mut h = host(2, 0.5);
        let c = ctx(10_000.0, 0.0, 100.0);
        assert_eq!(run_bar(&mut h, bar(0, 100.0), &c), Some(0.5));
        assert_eq!(run_bar(&mut h, bar(MIN, 100.0), &c), Some(0.5));

        // bar 边界替换后端：下一次 on_event 必须用新模型
        h.replace_backend(Box::new(FixedBackend::new("fake-model-v2", -0.4)));
        assert_eq!(run_bar(&mut h, bar(2 * MIN, 100.0), &c), Some(-0.4));
        assert_eq!(h.model_id(), "fake-model-v2");
    }

    /// 端到端：假后端 + 真实 `Kernel::run_history`，必须产生成交
    #[test]
    fn test_host_runs_inside_kernel_main_loop() {
        let config = KernelConfig {
            initial_cash: 10_000.0,
            expected_instruments: [btc()].into_iter().collect(),
            seed: SeedLiquidity {
                half_spread: 0.0005,
                depth_levels: 5,
                size_per_level: 1_000.0,
            },
            ..KernelConfig::default()
        };
        let mut kernel = Kernel::new(config, Box::new(BacktestSink::new()));
        kernel.add_host(Box::new(host(1, 0.5)));

        let frames: Vec<GridFrame> = (0..3)
            .map(|idx| GridFrame {
                ts: Timestamp::from_nanos(idx as i64 * MIN),
                bars: vec![(btc(), bar(idx as i64 * MIN, 100.0))],
                states: Default::default(),
            })
            .collect();

        let result = kernel.run_history(frames).expect("RL 主回路应跑通");
        assert_eq!(result.bars_processed, 3);
        assert_eq!(result.orders_submitted, 3, "每根 bar 一个 SetTarget");
        assert!(result.run.fills >= 1, "0.5 权益占比必须真的成交");
    }
}
