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

use std::sync::Arc;

use axon_core::types::Instrument;
use axon_kernel::event::BarClosedEvent;
use axon_kernel::host::{HostCtx, OrderIntent, StrategyHost};
use axon_strategy::trait_::{BarContext, Strategy};

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
}

impl BarContextSim {
    /// 用策略、负责品种与 runtime 句柄创建桥接 Host
    pub fn new(
        inner: Arc<dyn Strategy>,
        instrument: Instrument,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            name: inner.name().to_string(),
            inner,
            instrument,
            runtime,
            bar_index: 0,
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

        // decide 是 async：内核主回路同步调用，这里 block_on 到调用方 runtime。
        // 失败不 panic：按 Hold 处理（空 vec = 维持上次生效目标），并留下告警。
        let decision = match self.runtime.block_on(self.inner.decide(&bar_ctx)) {
            Ok(decision) => decision,
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
