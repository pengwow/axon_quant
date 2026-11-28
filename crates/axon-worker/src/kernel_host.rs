//! `Strategy` → 内核 `StrategyHost` 的桥接（`BarContextSim`）
//!
//! 内核的 `StrategyHost` 是同步的（每 bar 一次，不做 async 切换），而
//! `axon-strategy::Strategy::decide` 是 async 的。本模块负责把两者接起来：
//! 收市事件 → `BarContext` → `block_on(decide)` → `OrderIntent::SetTarget`。
//!
//! # 为什么不放进 axon-kernel
//!
//! `axon-kernel` 不允许依赖 `axon-strategy`（内核只认 `StrategyHost` 抽象，
//! 依赖方向必须是 strategy/worker → kernel）。桥接层因此放在 `axon-worker`。
//!
//! ponytail: 这里用同步 `block_on` 驱动 async 策略，天花板是「策略数 × 每 bar
//! 一次 runtime 进入」，并且纯 Python 策略还会在内部拿 GIL（QuantCell 的
//! Python 策略适配器）。回测是单线程跑批，这个开销可接受；一旦需要并发或
//! 高 bar 频率 live，升级路径是给 `StrategyHost` 加 async 变体（或预先在
//! 同一 runtime 上批量 await），而不是在这里起线程。

use std::sync::{Arc, Mutex};

use axon_core::types::Instrument;
use axon_kernel::event::BarClosedEvent;
use axon_kernel::host::{HostCtx, OrderIntent, StrategyHost};
use axon_strategy::trait_::{BarContext, Strategy, StrategyError};

/// 用 `BarContext` 驱动 `axon-strategy::Strategy` 的内核 Host
///
/// 一个实例只负责一个品种（按 `instrument` 过滤事件），与
/// `BacktestRunner::run()` 里「每个策略只吃自己 symbol 的 bar」一致。
pub struct BarContextSim {
    inner: Arc<dyn Strategy>,
    /// 本 Host 负责的品种：内核按帧广播所有品种的 `BarClosed`，必须过滤
    instrument: Instrument,
    /// 调用方（`PyKernel` / 测试）持有的 runtime 句柄
    ///
    /// 选择 `Handle` 而不是自带 `Runtime`：runtime 生命周期由调用方统一管理，
    /// 避免每个策略各建一个线程池；`block_on` 只在非 async 调用点进入（内核
    /// 主回路是同步的），不会嵌套 runtime。
    runtime: tokio::runtime::Handle,
    name: String,
    /// 该品种已决策的 bar 数（`BarContext.bar_index` 的递增来源）
    bar_index: u64,
    /// 致命错误登记槽（共享给 run_history 收口）
    ///
    /// 内核 `StrategyHost::on_event` 契约只产出 `Vec<OrderIntent>`，没有错误
    /// 通道，契约性失败（`StrategyError::Fatal`，如 LLM 回放 ReplayMissError）
    /// 只能先记进共享槽，由调用方（`PyKernel::run_history`）在 run 结束后统一
    /// 抛错终止整场回测。`None` 槽（`new` 创建）= 无人收口，保持纯日志行为
    /// （paper/live 语义不变）。
    fatal: Arc<Mutex<Option<String>>>,
}

impl BarContextSim {
    /// 用策略、负责品种与 runtime 句柄创建桥接 Host
    ///
    /// 默认自带一个无观察者的 fatal 槽：paper/live 装配处不收口 Fatal，
    /// 只留下 error 日志（与降级 Hold 的旧行为一致）。
    pub fn new(
        inner: Arc<dyn Strategy>,
        instrument: Instrument,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self::new_with_fatal(inner, instrument, runtime, Arc::new(Mutex::new(None)))
    }

    /// 携带共享 fatal 槽创建（回测路径用：`run_history` 结束后读槽收口抛错）
    pub fn new_with_fatal(
        inner: Arc<dyn Strategy>,
        instrument: Instrument,
        runtime: tokio::runtime::Handle,
        fatal: Arc<Mutex<Option<String>>>,
    ) -> Self {
        Self {
            name: inner.name().to_string(),
            inner,
            instrument,
            runtime,
            bar_index: 0,
            fatal,
        }
    }
}

impl StrategyHost for BarContextSim {
    fn name(&self) -> &str {
        &self.name
    }

    fn on_event(&mut self, ev: &BarClosedEvent<'_>, ctx: &HostCtx) -> Vec<OrderIntent> {
        if ev.instrument != &self.instrument {
            // 内核把本帧所有品种的收市事件都广播给全部 Host；别的品种不归本策略管，
            // 返回空 vec（Hold），不能决策。
            return Vec::new();
        }

        let symbol = format!(
            "{}/{}",
            ev.instrument.base().as_str(),
            ev.instrument.quote().as_str()
        );
        let bar_ctx = BarContext {
            symbol: &symbol,
            bar_index: self.bar_index,
            timestamp_ns: ev.bar.timestamp.nanos as u64,
            bar: ev.bar,
            state: ev.state,
        };
        self.bar_index += 1;

        // 已有致命错误登记：整场回测注定会被 run_history 抛错终止，后续 bar
        // 直接短路（改动前每 bar 仍会打一次失败调用/日志）。ponytail: 短路
        // 依赖「fatal 后 run 必被丢弃」这一上游契约，paper/live 不收口故不受影响。
        if self.fatal.lock().map(|g| g.is_some()).unwrap_or(false) {
            return Vec::new();
        }

        // decide 是 async：内核主回路同步调用，这里 block_on 到调用方 runtime。
        // 逐 bar 异常不 panic：按 Hold 处理（空 vec = 维持上次生效目标）；
        // Fatal（契约性失败）例外——登记进共享槽由 run_history 收口上抛。
        let decision = match self.runtime.block_on(self.inner.decide(&bar_ctx)) {
            Ok(decision) => decision,
            Err(StrategyError::Fatal(msg)) => {
                tracing::error!(
                    host = %self.name,
                    instrument = %ev.instrument,
                    error = %msg,
                    "kernel host: 策略致命失败，登记后终止整场运行"
                );
                if let Ok(mut slot) = self.fatal.lock() {
                    *slot = Some(format!("[{}] {}", self.name, msg));
                }
                return Vec::new();
            }
            Err(e) => {
                tracing::warn!(
                    host = %self.name,
                    instrument = %ev.instrument,
                    error = %e,
                    "kernel host: 策略决策失败，按 Hold 处理"
                );
                return Vec::new();
            }
        };

        // Hold（以及任何无显式目标位的决策）沿用内核的上次生效目标：
        // 空 vec 才是 Hold，`SetTarget { 0.0 }` 会真的平仓，语义必须区分。
        if decision.action == axon_llm::decision::Action::Hold {
            return Vec::new();
        }
        let quantity = decision.target_position.unwrap_or(0.0);

        let Some(price) = ctx.last_price.get(&self.instrument).copied() else {
            tracing::debug!(
                host = %self.name,
                instrument = %ev.instrument,
                "kernel host: 无最新价，跳过目标登记"
            );
            return Vec::new();
        };
        if price <= 0.0 || ctx.equity <= 0.0 {
            tracing::debug!(
                host = %self.name,
                instrument = %ev.instrument,
                price,
                equity = ctx.equity,
                "kernel host: 价格或权益非正，跳过目标登记"
            );
            return Vec::new();
        }

        // `StrategyDecision.target_position` 是**绝对数量**（与 `BacktestRunner`
        // 的语义一致），而内核 `SetTarget` 收的是**权益占比**，换算点固定在
        // `BacktestSink::submit`：`qty = target_ratio * core.equity() / price`。
        // 这里必须用 `ctx.equity` 做逆运算才能回推数量 —— 决策前内核刚把
        // `HostCtx.equity` 回填为 `core.equity()`，submit 时刻两者逐位相同。
        // 浮点极限：来回乘除仍可能差 1 ulp（如 0.3 → 0.29999999999999993），
        // 量化到引擎的 ×1e6 后 ≤1 微单位，parity 测试用微单位容差覆盖。
        // **不要**改成 `initial_cash`：权益随回测漂移后会产生系统性偏差，宿主
        // 与 sink 的换算就不再互逆，parity 立刻失败。
        vec![OrderIntent::SetTarget {
            instrument: self.instrument.clone(),
            target_ratio: quantity * price / ctx.equity,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axon_core::market::bar::Bar;
    use axon_core::time::Timestamp;
    use axon_core::types::{Price, Quantity, SpotInstrument, Symbol};
    use axon_strategy::trait_::StrategyDecision;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 恒错策略：每次 decide 抛指定错误，并累计真实调用次数（验证短路）
    struct FailingStrategy {
        fatal: bool,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Strategy for FailingStrategy {
        fn name(&self) -> &str {
            "fail"
        }
        fn symbol(&self) -> &str {
            "BTC/USDT"
        }
        async fn decide(&self, _: &BarContext<'_>) -> Result<StrategyDecision, StrategyError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(if self.fatal {
                StrategyError::Fatal("LLM 回放未命中（ReplayMissError）: boom".into())
            } else {
                StrategyError::Logic("普通失败".into())
            })
        }
    }

    fn bar_event() -> (Bar, Instrument, serde_json::Value) {
        let bar = Bar {
            timestamp: Timestamp::from_nanos(1_700_000_000_000_000_000),
            open: Price::from_f64(100.0),
            high: Price::from_f64(101.0),
            low: Price::from_f64(99.0),
            close: Price::from_f64(100.0),
            volume: Quantity::from_f64(10_000.0),
        };
        (
            bar,
            Instrument::Spot(SpotInstrument {
                base: Symbol::from("BTC"),
                quote: Symbol::from("USDT"),
            }),
            serde_json::Value::Null,
        )
    }

    fn host_ctx(inst: &Instrument) -> HostCtx {
        HostCtx {
            positions: HashMap::new(),
            equity: 100_000.0,
            last_price: HashMap::from([(inst.clone(), 100.0)]),
        }
    }

    /// `block_on` 依赖 Handle：必须在 runtime 上下文**之外**调用（见 kernel_host_parity 注释）
    fn run_on_event(sim: &mut BarContextSim, ev_calls: usize) {
        let (bar, inst, state) = bar_event();
        let ev = BarClosedEvent {
            instrument: &inst,
            interval: "1m",
            bar: &bar,
            ts: Timestamp::from_nanos(1_700_000_060_000_000_000),
            state: &state,
        };
        let ctx = host_ctx(&inst);
        for _ in 0..ev_calls {
            let intents = sim.on_event(&ev, &ctx);
            assert!(intents.is_empty(), "失败后必须回到 Hold（空 vec）");
        }
    }

    #[test]
    fn fatal_error_is_recorded_and_short_circuits() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let fatal: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let strategy = Arc::new(FailingStrategy {
            fatal: true,
            calls: AtomicUsize::new(0),
        });
        let (_, inst, _) = bar_event();
        let mut sim = BarContextSim::new_with_fatal(
            strategy.clone(),
            inst,
            runtime.handle().clone(),
            fatal.clone(),
        );

        // 两次收市事件：第一次登记 fatal，第二次短路不再调用 decide
        run_on_event(&mut sim, 2);
        assert_eq!(strategy.calls.load(Ordering::SeqCst), 1);
        let recorded = fatal.lock().unwrap().clone();
        assert!(
            recorded
                .as_deref()
                .is_some_and(|m| m.contains("ReplayMissError")),
            "fatal 槽必须记录错误信息，实际 {recorded:?}"
        );
    }

    #[test]
    fn logic_error_keeps_hold_without_fatal() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let fatal: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let (_, inst, _) = bar_event();
        let mut sim = BarContextSim::new_with_fatal(
            Arc::new(FailingStrategy {
                fatal: false,
                calls: AtomicUsize::new(0),
            }),
            inst,
            runtime.handle().clone(),
            fatal.clone(),
        );

        run_on_event(&mut sim, 3);
        assert!(
            fatal.lock().unwrap().is_none(),
            "Logic 失败不得进入 fatal 槽，否则普通容错语义被破坏"
        );
    }
}
