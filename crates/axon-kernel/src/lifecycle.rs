//! per-bar 编排(设计文档 4.8 的九步)
//!
//! 内核把一**帧**(同一网格点上所有有真实 bar 的品种)交给本模块,按
//! `axon-worker::BacktestRunner::run()` 的顺序执行:
//!
//! ```text
//! 1 pre_bar   set_clock(frame_ts) + 更新 last_price(无 bar 品种前向填充)
//! 2 pre_bar   快照 liquidated_symbols
//! 3 pre_bar   begin_bar_multi(seed + 自动 rebalance + funding 调度 + NAV 采样)
//! 4 decide    逐 BarClosed 逐 Host → OrderIntent
//! 5 allocate  budget 同比例缩减 effective_targets(仅未启用保证金时)
//! 6 execute   目标交 sink 登记 → finish_bar 一次 set_target + rebalance
//! 7 post_bar  push_funding + 排空引擎队列
//! 8 post_bar  check_margin(强平)
//! 9 post_bar  sample_bar_nav(第 3 步内部已完成,不要重复)
//! ```
//!
//! 三条与 `BacktestRunner` 必须一致的语义:Hold 沿用上次生效目标;强平后 Hold
//! 归零;多品种按帧对齐(每帧只决策「本帧有真实 bar」的品种)。

use std::collections::{BTreeMap, HashSet};

use axon_core::Event;

use crate::KernelError;
use crate::core::{KernelConfig, KernelCore, symbol_key};
use crate::event::{BarClosedEvent, close_time};
use crate::host::{HostCtx, OrderIntent, StrategyHost};
use crate::pump::GridFrame;
use crate::sink::ExecutionSink;

/// bar 生命周期状态
#[derive(Debug, Default)]
pub struct BarLifecycle {
    /// 已处理的帧数(每帧 = 一个网格点,不是单个品种的 bar 数)
    pub bars_processed: u64,
    /// 已交给 sink 的意图数(不是成交数)
    orders_submitted: u64,
}

impl BarLifecycle {
    /// 创建初始状态
    pub fn new() -> Self {
        Self::default()
    }

    /// 累计提交给 sink 的意图数
    pub fn orders_submitted(&self) -> u64 {
        self.orders_submitted
    }

    /// 处理一帧收市事件:九步编排 → 逐个提交 sink → 返回产出事件
    ///
    /// `ctx` 会在第 3 步之后按账务核心回填(equity / positions / last_price),
    /// 供 Host 决策使用;第 4 步的每个 `BarClosedEvent` 都会调用**全部** Host,
    /// Host 必须按 `ev.instrument` 自行过滤(固定 symbol 的适配器天然如此)。
    // 六参数是设计文档 4.8 编排签名的直接映射;拆成结构体只会把同一组依赖
    // 再包一层,不降低耦合,故保留长参数列表并在此说明理由。
    #[allow(clippy::too_many_arguments)]
    pub fn on_bar_closed(
        &mut self,
        frame: &GridFrame,
        config: &KernelConfig,
        core: &mut KernelCore,
        ctx: &mut HostCtx,
        hosts: &mut [Box<dyn StrategyHost>],
        sink: &mut dyn ExecutionSink,
    ) -> Result<Vec<Event>, KernelError> {
        self.bars_processed += 1;
        let mut produced = Vec::new();

        // 1) 时钟对齐到网格时间戳(bar 起始时间),并更新前向填充价格
        core.set_clock(frame.ts);
        for (instrument, bar) in &frame.bars {
            core.update_last_price(instrument, bar.close.as_f64());
        }

        // 2) 强平快照必须在 begin_bar_multi 的自动 rebalance 之前取
        core.snapshot_liquidated();

        // 3) seed 流动性 + 自动 rebalance + funding 调度 + sample_bar_nav
        core.begin_bar_multi();

        // HostCtx 回填:决策前让 Host 看到本 bar 的账户视图
        ctx.equity = core.equity();
        ctx.positions = core.positions();
        ctx.last_price = core.last_price_map();

        // 4) 逐 BarClosed 逐 Host 决策(只对本帧有真实 bar 的品种)
        let close_ts = close_time(frame.ts, config.interval.as_str()).unwrap_or(frame.ts);
        let mut decided: HashSet<String> = HashSet::new();
        let null_state = serde_json::Value::Null;
        for (instrument, bar) in &frame.bars {
            let view = BarClosedEvent {
                instrument,
                interval: config.interval.as_str(),
                bar,
                ts: close_ts,
                state: frame.states.get(instrument).unwrap_or(&null_state),
            };
            for host in hosts.iter_mut() {
                for intent in host.on_event(&view, ctx) {
                    if let OrderIntent::SetTarget { instrument, .. } = &intent {
                        decided.insert(symbol_key(instrument));
                    }
                    self.orders_submitted += 1;
                    tracing::debug!(
                        host = host.name(),
                        instrument = %instrument,
                        interval = view.interval,
                        bars_processed = self.bars_processed,
                        "kernel: submitting order intent"
                    );
                    produced.extend(sink.submit(intent, core, ctx)?);
                }
            }
        }

        // Hold:本帧有真实 bar 但没有 Host 给出 SetTarget 的品种。
        // 未命中强平快照时沿用 effective_targets(什么都不做);
        // 命中则清零,避免 Hold 复活刚被强平的仓位。
        for (instrument, _) in &frame.bars {
            if decided.contains(&symbol_key(instrument)) {
                continue;
            }
            core.resolve_hold(instrument);
        }

        // 5) 组合资金约束:总名义敞口 ≤ budget,超出则全部目标同比例缩减。
        // 启用保证金时强制停用(杠杆靠目标数量放大,缩放会把杠杆抵消)。
        if config.margin_specs.is_empty()
            && let Some(budget) = config.capital_budget
        {
            let total_notional: f64 = core
                .effective_targets()
                .iter()
                .filter_map(|(key, quantity)| {
                    core.last_price_by_key(key)
                        .map(|price| quantity.abs() * price)
                })
                .sum();
            if total_notional > budget && total_notional > 0.0 {
                core.scale_effective_targets(budget / total_notional);
            }
        }

        // 6) bar 末统一落地:set_target_position + 一次 rebalance
        produced.extend(sink.finish_bar(core)?);

        // 7) 资金费结算:仅本帧有真实 bar 的品种,用 grid ts 查表
        let mut funding_pushed = false;
        for (instrument, _) in &frame.bars {
            let key = symbol_key(instrument);
            let Some((rate, mark)) =
                lookup_funding(&config.funding_by_symbol, &key, frame.ts.nanos)
            else {
                continue;
            };
            // mark 缺数据时用该品种最近 close 兜底(对齐 `BacktestRunner`)
            let fallback = core.last_price(instrument).unwrap_or(0.0);
            let mark_price = if mark > 0.0 { mark } else { fallback };
            core.push_funding(instrument.clone(), rate, mark_price, frame.ts);
            funding_pushed = true;
        }
        if funding_pushed {
            core.drain();
        }

        // 8) 保证金检查与强平(放在资金费之后,权益口径才完整)
        if !config.margin_specs.is_empty() {
            core.check_margin();
        }

        // 9) sample_bar_nav 已在第 3 步的 `begin_bar_multi` 内部完成,不重复调用
        Ok(produced)
    }
}

/// 资金费查表
///
/// 历史数据链路的表以交易所原始 symbol(如 `BTCUSDT`)为 key,而内核内部
/// 统一用 `"BTC/USDT"`,这里按三种写法依次尝试,避免两侧归一化口径不一致
/// 导致资金费静默漏结算。
fn lookup_funding(
    table: &BTreeMap<String, BTreeMap<i64, (f64, f64)>>,
    key: &str,
    ts_ns: i64,
) -> Option<(f64, f64)> {
    let candidates = [key.to_string(), key.replace('/', ""), key.replace('/', "-")];
    candidates
        .iter()
        .find_map(|candidate| {
            table
                .get(candidate)
                .and_then(|schedule| schedule.get(&ts_ns))
        })
        .copied()
}

#[cfg(test)]
mod tests {
    use axon_core::types::{SpotInstrument, Symbol};
    use axon_core::{Bar, Instrument, Price, Quantity, Timestamp};

    use super::*;
    use crate::core::SeedLiquidity;
    use crate::sink::BacktestSink;

    const MIN: i64 = 60_000_000_000;

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
            high: Price::from_f64(close),
            low: Price::from_f64(close),
            close: Price::from_f64(close),
            volume: Quantity::from_f64(1_000.0),
        }
    }

    /// 每个事件都给出 `ratio` 目标的 Host
    struct TargetHost {
        ratio: f64,
        silent_from: u64,
        calls: u64,
    }

    impl StrategyHost for TargetHost {
        fn name(&self) -> &str {
            "target"
        }

        fn on_event(&mut self, ev: &BarClosedEvent<'_>, _ctx: &HostCtx) -> Vec<OrderIntent> {
            self.calls += 1;
            if self.calls >= self.silent_from {
                return Vec::new(); // Hold
            }
            vec![OrderIntent::SetTarget {
                instrument: ev.instrument.clone(),
                target_ratio: self.ratio,
            }]
        }
    }

    fn run_frame(
        lifecycle: &mut BarLifecycle,
        frame: &GridFrame,
        config: &KernelConfig,
        core: &mut KernelCore,
        hosts: &mut [Box<dyn StrategyHost>],
        sink: &mut dyn ExecutionSink,
    ) {
        let mut ctx = HostCtx::default();
        lifecycle
            .on_bar_closed(frame, config, core, &mut ctx, hosts, sink)
            .expect("帧编排不应失败");
    }

    fn config(initial_cash: f64) -> KernelConfig {
        KernelConfig {
            initial_cash,
            seed: SeedLiquidity {
                half_spread: 0.0005,
                depth_levels: 5,
                size_per_level: 1_000.0,
            },
            ..KernelConfig::default()
        }
    }

    #[test]
    fn test_hold_keeps_previous_effective_target() {
        let cfg = config(10_000.0);
        let mut core = KernelCore::new(&cfg);
        let mut lifecycle = BarLifecycle::new();
        let mut sink = BacktestSink::new();
        // 第一帧决策 1.0，第二帧起 Hold
        let mut hosts: Vec<Box<dyn StrategyHost>> = vec![Box::new(TargetHost {
            ratio: 1.0,
            silent_from: 2,
            calls: 0,
        })];

        run_frame(
            &mut lifecycle,
            &GridFrame {
                ts: Timestamp::from_nanos(0),
                bars: vec![(btc(), bar(0, 100.0))],
                states: Default::default(),
            },
            &cfg,
            &mut core,
            &mut hosts,
            &mut sink,
        );
        let key = symbol_key(&btc());
        let after_first = core.effective_targets()[&key];
        assert!(
            (after_first - 100.0).abs() < 1e-9,
            "1.0 × 10000 / 100 = 100"
        );

        run_frame(
            &mut lifecycle,
            &GridFrame {
                ts: Timestamp::from_nanos(MIN),
                bars: vec![(btc(), bar(MIN, 100.0))],
                states: Default::default(),
            },
            &cfg,
            &mut core,
            &mut hosts,
            &mut sink,
        );
        assert_eq!(
            core.effective_targets()[&key],
            after_first,
            "Hold 必须沿用上次生效目标"
        );
    }

    #[test]
    fn test_hold_after_liquidation_resolves_to_zero() {
        let mut core = KernelCore::new(&KernelConfig::default());
        core.set_effective_target(&btc(), 10.0);
        core.force_liquidated_for_test([symbol_key(&btc())].into_iter().collect());

        core.resolve_hold(&btc());
        assert_eq!(
            core.effective_targets()[&symbol_key(&btc())],
            0.0,
            "强平后 Hold 必须归零"
        );
        // 未命中强平的品种保持原目标
        core.set_effective_target(&eth(), 3.0);
        core.resolve_hold(&eth());
        assert_eq!(core.effective_targets()[&symbol_key(&eth())], 3.0);
    }

    #[test]
    fn test_capital_budget_scales_all_targets() {
        let mut cfg = config(10_000.0);
        cfg.capital_budget = Some(10_000.0);
        let mut core = KernelCore::new(&cfg);
        let mut lifecycle = BarLifecycle::new();
        let mut sink = BacktestSink::new();
        let mut hosts: Vec<Box<dyn StrategyHost>> = vec![Box::new(TargetHost {
            ratio: 1.0,
            silent_from: u64::MAX,
            calls: 0,
        })];

        run_frame(
            &mut lifecycle,
            &GridFrame {
                ts: Timestamp::from_nanos(0),
                bars: vec![(btc(), bar(0, 100.0)), (eth(), bar(0, 100.0))],
                states: Default::default(),
            },
            &cfg,
            &mut core,
            &mut hosts,
            &mut sink,
        );

        // 每品种 100 数量 × 100 价 = 20000 名义,超 budget 10000 → 各缩到 50
        assert!((core.effective_targets()[&symbol_key(&btc())] - 50.0).abs() < 1e-9);
        assert!((core.effective_targets()[&symbol_key(&eth())] - 50.0).abs() < 1e-9);
    }
}
