//! 纸面驱动：统一内核 × 纯内存虚拟撮合的接线层
//!
//! [`LiveDriver`] 的孪生：行情侧同构（gateway 模式用 [`KernelPump`] 泵实时 WS
//! 行情），执行侧换成 `PaperSink`（虚拟撮合，不下真实单），无 journal / resume——
//! paper 账面纯内存，重启即重置（对齐 QuantCell paper 语义）。
//!
//! # 两种行情通道
//!
//! - `new(adapter = Some)`：接交易所 adapter（testnet 实测用），`connect` 走
//!   WS 握手 + 订阅，`tick` 泵实时行情；
//! - `new(adapter = None)`：无网关，上层用 [`PaperDriver::feed_bar`] 直接构造
//!   收市 bar 事件灌入内核（pytest / 自动化验证用）。品种全集写进
//!   `KernelConfig::expected_instruments`——正好落在栅栏「手工灌事件的全局
//!   兜底」的设计用途上。
//!
//! # 与 [`LiveDriver`] 的差异
//!
//! | 环节 | LiveDriver | PaperDriver |
//! |------|-----------|-------------|
//! | sink | `LiveSink`（真实下单） | `PaperSink`（内存撮合，`poll` 恒空） |
//! | journal / resume | 必挂 / 守卫对账 | 无（重启即重置） |
//! | gateway 归属 | start 时 move 进 sink | 常驻 self（sink 不下单，直连无意义） |

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::mpsc;

use axon_core::{
    Bar, Event, Instrument, MarketDataEvent, MarketDataPayload, Price, Quantity, Timestamp,
};
use axon_exchange::ExchangeAdapter;
use axon_exchange::types::Symbol as ExchangeSymbol;
use axon_kernel::{
    Kernel, KernelConfig, KernelError, PaperSink, RunManifest, StrategyHost, close_time,
    interval_to_nanos,
};

use crate::backtest::instrument_key_for;
use crate::exchange_gateway::ExchangeGateway;
use crate::live_sink::LiveOrderGateway;
use crate::pump::{Gap, KernelPump};

/// 行情 fan-out 通道容量（与 `live_driver` 同值：tick 节奏下不会触顶）
const FANOUT_CHANNEL_CAPACITY: usize = 4096;

/// 单次缺口补灌的最大拉取根数（`fetch_ohlcv` 无 since 参数、只能取最近 N 根）
const MAX_GAP_FETCH: usize = 1000;

/// paper 账面快照（全部虚拟，无需交易所对账）
#[derive(Debug, Clone, Serialize)]
pub struct PaperSnapshot {
    /// 当前权益（NAV：现金 + 持仓按最近价盯市）
    pub equity: f64,
    /// 非零持仓（key = `"BASE/QUOTE"`，值 = 净持仓数量）
    pub positions: BTreeMap<String, f64>,
}

/// 统一内核的纸面驱动器
///
/// gateway 可选：`Some` 泵实时行情，`None` 走 [`Self::feed_bar`] 手工灌 bar。
pub struct PaperDriver {
    /// 内核配置（含栅栏品种全集兜底）
    config: KernelConfig,
    /// 品种口径表（key = `"BASE/QUOTE"`），`feed_bar` 归一化后回指
    instruments: BTreeMap<String, Instrument>,
    /// instrument → 交易所 symbol 反查表，缺口补灌按 `gap.instrument` 找 fetch 入参
    by_instrument: Vec<(Instrument, ExchangeSymbol)>,
    /// 订阅品种（gateway 模式 connect 用）
    symbols: Vec<ExchangeSymbol>,
    /// 交易所网关；PaperSink 不下单，无需 move 进 sink，常驻 self 即可
    gateway: Option<ExchangeGateway>,
    /// 行情泵（仅 gateway 模式存在）
    pump: Option<KernelPump>,
    /// 统一内核；`tick` / `feed_bar` / `snapshot` 要求已启动
    kernel: Option<Kernel>,
    /// `feed_bar` 事件的递增序列号
    next_seq: u64,
}

impl PaperDriver {
    /// 构造驱动器：解析品种口径，gateway 模式接好行情 fan-out
    ///
    /// adapter 传 `None` 进入 feed_bar 驱动模式（`connect` 直接 Ok）。
    pub fn new(
        adapter: Option<Arc<tokio::sync::Mutex<Box<dyn ExchangeAdapter>>>>,
        mut config: KernelConfig,
        symbols: Vec<String>,
    ) -> Result<Self, String> {
        if symbols.is_empty() {
            return Err("至少需要一个交易品种".to_string());
        }
        // 品种口径表 + 栅栏全局兜底（feed_bar 手工灌事件时依赖它放行步进）
        let mut instruments = BTreeMap::new();
        let mut expected = HashSet::new();
        let mut exchange_symbols = Vec::with_capacity(symbols.len());
        let mut by_instrument = Vec::with_capacity(symbols.len());
        for sym in &symbols {
            let key = instrument_key_for(sym);
            let instrument = config.instrument_mode.instrument(&key);
            expected.insert(instrument.clone());
            by_instrument.push((instrument.clone(), ExchangeSymbol(sym.clone())));
            instruments.insert(key, instrument);
            exchange_symbols.push(ExchangeSymbol(sym.clone()));
        }
        config.expected_instruments = expected;

        let (gateway, pump) = match adapter {
            Some(adapter) => {
                // fan-out 接收端交给 pump，发送端交给网关（与 LiveDriver 同构）
                let (fanout_tx, fanout_rx) = mpsc::channel(FANOUT_CHANNEL_CAPACITY);
                let gateway = ExchangeGateway::shared(adapter)?.with_market_data_fanout(fanout_tx);
                let pump = KernelPump::with_instrument_mode(fanout_rx, config.instrument_mode);
                (Some(gateway), Some(pump))
            }
            None => (None, None),
        };
        Ok(Self {
            config,
            instruments,
            by_instrument,
            symbols: exchange_symbols,
            gateway,
            pump,
            kernel: None,
            next_seq: 0,
        })
    }

    /// 连接交易所并订阅品种（gateway 模式；失败时网关保留可重试）
    pub fn connect(&mut self) -> Result<(), String> {
        match self.gateway.as_mut() {
            Some(gateway) => gateway
                .connect_sync()
                .and_then(|()| gateway.subscribe(&self.symbols)),
            // feed_bar 模式无外部资源
            None => Ok(()),
        }
    }

    /// 起跑：建内核（PaperSink 虚拟撮合）、注册策略宿主
    pub fn start(&mut self, hosts: Vec<Box<dyn StrategyHost>>) -> Result<(), String> {
        if self.kernel.is_some() {
            return Err("内核已启动，不可重复 start".to_string());
        }
        if hosts.is_empty() {
            return Err("至少需要一个策略宿主".to_string());
        }
        let mut kernel = Kernel::new(self.config.clone(), Box::new(PaperSink::new()));
        kernel.set_manifest(RunManifest::default());
        for host in hosts {
            kernel.add_host(host);
        }
        self.kernel = Some(kernel);
        Ok(())
    }

    /// 手工灌一根收市 bar（feed_bar 驱动模式）
    ///
    /// 事件时间戳 = 收市时刻（`bar.timestamp + interval`，防前视）；
    /// `symbol` 必须是 `new` 时声明过的品种，未知品种直接报错（避免
    /// 与栅栏品种全集脱节）。
    #[allow(clippy::too_many_arguments)]
    pub fn feed_bar(
        &mut self,
        symbol: &str,
        start_ts_nanos: i64,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        volume: f64,
    ) -> Result<(), String> {
        let kernel = self
            .kernel
            .as_mut()
            .ok_or_else(|| "内核未启动，请先 start".to_string())?;
        let key = instrument_key_for(symbol);
        let instrument = self
            .instruments
            .get(&key)
            .cloned()
            .ok_or_else(|| format!("品种 {symbol} 未在 new 时声明，无法灌 bar"))?;
        let bar_ts = Timestamp::from_nanos(start_ts_nanos);
        let close_ts = close_time(bar_ts, &self.config.interval)
            .ok_or_else(|| format!("无效的 K 线周期: {}", self.config.interval))?;
        let event = Event::MarketData(MarketDataEvent::new(
            self.next_seq,
            close_ts,
            MarketDataPayload::BarClosed {
                instrument,
                interval: self.config.interval.clone(),
                bar: Bar {
                    timestamp: bar_ts,
                    open: Price::from_f64(open),
                    high: Price::from_f64(high),
                    low: Price::from_f64(low),
                    close: Price::from_f64(close),
                    volume: Quantity::from_f64(volume),
                },
            },
        ));
        self.next_seq += 1;
        kernel.enqueue_external(event, None).map_err(kerr)
    }

    /// 推进一拍：排空交易所回报 → 泵行情 → 断线补齐 → 步进消化
    pub fn tick(&mut self) -> Result<(), String> {
        let kernel = self
            .kernel
            .as_mut()
            .ok_or_else(|| "内核未启动，请先 start".to_string())?;
        // 1. 排空交易所回报；poll_updates 同时负责把行情搬进 fan-out 通道
        //    （行情 → pump 只从这里驱动，不排它 gateway 模式行情就断流）
        if let Some(gateway) = self.gateway.as_mut() {
            gateway.poll_updates()?;
        }
        // 2. 泵走 fanout 里攒的行情（feed_bar 模式无 pump，事件已直接入队）
        if let Some(pump) = self.pump.as_mut() {
            for event in pump.drain().events {
                kernel.enqueue_external(event, None).map_err(kerr)?;
            }
        }
        // 3. 断线补齐：缺口快照 → 反查品种 → fetch 历史 bar → 灌回同一条管道
        if let (Some(pump), Some(gateway)) = (self.pump.as_mut(), self.gateway.as_ref()) {
            for gap in pump.gaps().to_vec() {
                backfill_gap(pump, gateway, kernel, &self.by_instrument, &gap)?;
            }
        }
        // 4. 步进消化（补灌事件按收市时刻入队，EventQueue 天然把它们插到正确
        //    历史位置；多品种时栅栏未齐则本轮空转，等待后续 bar）
        while kernel.step().map_err(kerr)? {}
        // 5. PaperSink::poll 恒空，保持与 LiveDriver 相同的 tick 收尾节奏
        kernel.poll_external().map_err(kerr)
    }

    /// 运行中账面快照（虚拟撮合，可直接读内核账务）
    pub fn snapshot(&self) -> Result<PaperSnapshot, String> {
        let kernel = self
            .kernel
            .as_ref()
            .ok_or_else(|| "内核未启动，请先 start".to_string())?;
        Ok(PaperSnapshot {
            equity: kernel.equity_snapshot(),
            positions: kernel.positions_snapshot(),
        })
    }

    /// 收尾：paper 账纯内存、无外部挂起资源，幂等 no-op
    pub fn shutdown(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// `KernelError` → `String` 的统一映射（与 `live_driver` 同款）
fn kerr(e: KernelError) -> String {
    e.to_string()
}

/// 把一个缺口的最新历史 bar 拉回来灌进内核（`tick` 第 3 步的落点）
///
/// 自由函数而非 `PaperDriver` 方法：调用点同时借出 kernel / pump / gateway /
/// by_instrument 四个字段，self 方法会立刻撞借用检查。品种不在反查表里只告警
/// 跳过——缺口理论不该来自未声明品种，兜底不炸 tick。
fn backfill_gap(
    pump: &mut KernelPump,
    gateway: &ExchangeGateway,
    kernel: &mut Kernel,
    by_instrument: &[(Instrument, ExchangeSymbol)],
    gap: &Gap,
) -> Result<(), String> {
    let Some((_, symbol)) = by_instrument
        .iter()
        .find(|(instrument, _)| *instrument == gap.instrument)
    else {
        tracing::warn!(instrument = ?gap.instrument, "kernel paper: 缺口品种不在声明表里，跳过补灌");
        return Ok(());
    };
    let Some(interval_ns) = interval_to_nanos(&gap.interval) else {
        return Ok(());
    };
    // ponytail: naive heuristic — fetch_ohlcv 只能拉「最近 N 根」（无 since 参数），
    // limit 按缺失根数 +2 根余量；缺口跨度超 MAX_GAP_FETCH 时只覆盖最近一段，
    // 剩余缺口保留待后续 tick 继续补。升级路径 = fetch_ohlcv 加 since 按区间拉
    let missing = (((gap.end.nanos - gap.start.nanos) / interval_ns).clamp(1, MAX_GAP_FETCH as i64)
        as usize)
        + 2;
    let bars = gateway.fetch_ohlcv(symbol, &gap.interval, missing)?;
    if bars.is_empty() {
        return Ok(());
    }
    let batch = pump.replay_bars(symbol, &gap.interval, &bars);
    for event in batch.events {
        kernel.enqueue_external(event, None).map_err(kerr)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use axon_core::types::SwapSettle;
    use axon_kernel::{BarClosedEvent, HostCtx, InstrumentMode, OrderIntent, SeedLiquidity};

    const START: i64 = 1_700_000_000_000_000_000;
    const ONE_MIN: i64 = 60_000_000_000;

    /// 近零摩擦配置：fee=0；half_spread 必须为正——引擎把 `<=0` 视为不种
    /// 盘口（matching/engine.rs seed_liquidity 守卫），盘口为空时市价单零成交，
    /// paper 账面会恒空仓。取 0.05 让成交价（mid±0.05）断言数字整洁
    fn paper_config() -> KernelConfig {
        KernelConfig {
            initial_cash: 10_000.0,
            fee_rate: 0.0,
            interval: "1m".into(),
            seed: SeedLiquidity {
                half_spread: 0.05,
                ..SeedLiquidity::default()
            },
            ..KernelConfig::default()
        }
    }

    /// 按预设比例序列逐 bar 下 `SetTarget` 的测试宿主（蓝本：live_driver 的 CycleHost）
    struct RatioHost {
        ratios: Vec<f64>,
        idx: usize,
    }

    impl RatioHost {
        fn new(ratios: Vec<f64>) -> Self {
            Self { ratios, idx: 0 }
        }
    }

    impl StrategyHost for RatioHost {
        fn name(&self) -> &str {
            "ratio"
        }

        fn on_event(&mut self, ev: &BarClosedEvent<'_>, _ctx: &HostCtx) -> Vec<OrderIntent> {
            let ratio = self.ratios[self.idx.min(self.ratios.len() - 1)];
            self.idx += 1;
            vec![OrderIntent::SetTarget {
                instrument: ev.instrument.clone(),
                target_ratio: ratio,
            }]
        }
    }

    /// 灌一根 OHLC 相同的平价 bar 便捷函数
    fn feed_flat(driver: &mut PaperDriver, symbol: &str, idx: i64, price: f64) {
        driver
            .feed_bar(
                symbol,
                START + idx * ONE_MIN,
                price,
                price,
                price,
                price,
                10.0,
            )
            .expect("灌 bar 应成功");
    }

    /// T1：feed_bar 全链路——建仓 → 盯市 → 清仓，虚拟撮合亏盈全部落在内核账务
    #[test]
    fn feed_bar_roundtrip_matches_paper_ledger() {
        let mut driver =
            PaperDriver::new(None, paper_config(), vec!["BTCUSDT".into()]).expect("构造成功");
        driver.connect().expect("feed_bar 模式 connect 直接 Ok");
        driver
            .start(vec![Box::new(RatioHost::new(vec![0.3, 0.0]))])
            .expect("start 成功");

        // bar0 close=100：SetTarget 0.3 → 目标 30 股；买单价 = mid+0.05 = 100.05，
        // 现金 = 10000 − 30×100.05 = 6998.5；盯市 equity = 现金 + 30×100 = 9998.5
        feed_flat(&mut driver, "BTCUSDT", 0, 100.0);
        driver.tick().expect("tick 应成功");
        let snap = driver.snapshot().expect("snapshot 成功");
        let qty = snap.positions.get("BTC/USDT").copied().unwrap_or(0.0);
        assert!((qty - 30.0).abs() < 1e-6, "目标仓位 30 股: {qty}");
        assert!(
            (snap.equity - 9_998.5).abs() < 1e-6,
            "建仓后权益 =现金 6998.5 + 持仓盯市 3000: {}",
            snap.equity
        );

        // bar1 close=110：SetTarget 0.0 → 清仓卖单吃 bid = 110−0.05 = 109.95，
        // 现金 = 6998.5 + 30×109.95 = 10297.0
        feed_flat(&mut driver, "BTCUSDT", 1, 110.0);
        driver.tick().expect("tick 应成功");
        let snap = driver.snapshot().expect("snapshot 成功");
        assert!(
            snap.positions.is_empty(),
            "清仓后持仓应为空: {:?}",
            snap.positions
        );
        assert!(
            (snap.equity - 10_297.0).abs() < 1e-6,
            "清仓后权益 = 全部落袋(含 0.05 价差成本): {}",
            snap.equity
        );
    }

    /// 只交易 BTC 的宿主（T2 多品种栅栏用）
    struct BtcOnlyHost;

    impl StrategyHost for BtcOnlyHost {
        fn name(&self) -> &str {
            "btc_only"
        }

        fn on_event(&mut self, ev: &BarClosedEvent<'_>, _ctx: &HostCtx) -> Vec<OrderIntent> {
            if ev.instrument.base().as_str() == "BTC" {
                vec![OrderIntent::SetTarget {
                    instrument: ev.instrument.clone(),
                    target_ratio: 0.3,
                }]
            } else {
                vec![]
            }
        }
    }

    /// T2：多品种栅栏——品种全集有 BTC/ETH 时，单灌一根 bar 不触发决策，
    /// 两根都到齐才放行步进（`expected_instruments` 兜底口径）。
    #[test]
    fn multi_symbol_barrier_gates_decisions() {
        let mut driver = PaperDriver::new(
            None,
            paper_config(),
            vec!["BTCUSDT".into(), "ETHUSDT".into()],
        )
        .expect("构造成功");
        driver.connect().expect("feed_bar 模式 connect 直接 Ok");
        driver
            .start(vec![Box::new(BtcOnlyHost)])
            .expect("start 成功");

        // 只灌 BTC：全局栅栏等 ETH，step 不推进，不应产生持仓
        feed_flat(&mut driver, "BTCUSDT", 0, 100.0);
        driver.tick().expect("栅栏等待不算失败");
        let snap = driver.snapshot().expect("snapshot 成功");
        assert!(
            snap.positions.is_empty(),
            "栅栏未放行前不应有决策: {:?}",
            snap.positions
        );

        // ETH 到齐：栅栏放行，BTC 建仓 30 股
        feed_flat(&mut driver, "ETHUSDT", 0, 50.0);
        driver.tick().expect("tick 应成功");
        let snap = driver.snapshot().expect("snapshot 成功");
        let btc_qty = snap.positions.get("BTC/USDT").copied().unwrap_or(0.0);
        assert!(
            (btc_qty - 30.0).abs() < 1e-6,
            "BTC 目标仓位 30 股: {btc_qty}"
        );
        assert!(
            !snap.positions.contains_key("ETH/USDT"),
            "ETH 无决策不应有仓: {:?}",
            snap.positions
        );
    }

    /// T3：Swap 模式下品种口径表应产出 Swap instrument（永续 paper 的账务前提）
    #[test]
    fn swap_mode_resolves_swap_instrument() {
        let config = KernelConfig {
            instrument_mode: InstrumentMode::Swap {
                settle: SwapSettle::UsdMargin,
                contract_size: 1.0,
            },
            ..paper_config()
        };
        let driver = PaperDriver::new(None, config, vec!["BTCUSDT".into()]).expect("构造成功");
        let instrument = driver
            .instruments
            .get("BTC/USDT")
            .expect("品种表按 BASE/QUOTE 建");
        assert!(
            matches!(instrument, Instrument::Swap(_)),
            "Swap 模式必须解析为 Swap instrument: {instrument:?}"
        );
    }

    /// T4：断线重连补齐——WS 漏推中间一根 bar，tick 的缺口补灌从 REST 拉回重放
    /// （决策次数不变）；重推同一根 bar 被去重。账面语义与 T1 同源：
    /// equity = 10000 − half_spread × Σ|调仓量|
    #[test]
    fn gateway_gap_backfill_replays_missing_bars() {
        use axon_exchange::types::{ExchangeId, Kline, WsMessage};
        use rust_decimal::Decimal;
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // 按到达序计数 + 记录见过的 bar 起点：决策 4 次（不因补灌翻倍）、
        // 补灌 bar 插到正确历史位置
        #[derive(Default)]
        struct CountHost {
            calls: Arc<AtomicUsize>,
            seen: Arc<Mutex<Vec<i64>>>,
        }

        impl StrategyHost for CountHost {
            fn name(&self) -> &str {
                "count"
            }

            fn on_event(&mut self, ev: &BarClosedEvent<'_>, _ctx: &HostCtx) -> Vec<OrderIntent> {
                let idx = self.calls.fetch_add(1, Ordering::SeqCst);
                if let Ok(mut seen) = self.seen.lock() {
                    seen.push(ev.bar.timestamp.nanos);
                }
                // 首根建仓 0.3、随后全平：固定数字让滑点损耗可手算
                let ratios = [0.3, 0.0, 0.0, 0.0];
                vec![OrderIntent::SetTarget {
                    instrument: ev.instrument.clone(),
                    target_ratio: ratios[idx.min(ratios.len() - 1)],
                }]
            }
        }

        // 收市 bar 形状的 WS kline（OHLC 同价 100）
        fn ws_kline(start_ns: i64) -> WsMessage {
            WsMessage::Kline(Kline {
                symbol: ExchangeSymbol("BTCUSDT".to_string()),
                interval: "1m".to_string(),
                open: Decimal::from(100),
                high: Decimal::from(100),
                low: Decimal::from(100),
                close: Decimal::from(100),
                volume: Decimal::from(10),
                timestamp: chrono::DateTime::from_timestamp_nanos(start_ns),
                is_closed: true,
            })
        }

        let (adapter, handle) = crate::test_support::mock_exchange(ExchangeId::Binance);
        let mut driver = PaperDriver::new(Some(adapter), paper_config(), vec!["BTCUSDT".into()])
            .expect("构造成功");
        driver.connect().expect("mock connect 成功");
        let host = CountHost::default();
        let calls = Arc::clone(&host.calls);
        let seen = Arc::clone(&host.seen);
        driver.start(vec![Box::new(host)]).expect("start 成功");

        // WS 推 t0/t1/t3（漏 t2），REST 历史里备好 t2
        for offset in [0, ONE_MIN, 3 * ONE_MIN] {
            handle.push_message(ws_kline(START + offset));
        }
        handle.state().ohlcv = vec![Bar {
            timestamp: Timestamp::from_nanos(START + 2 * ONE_MIN),
            open: Price::from_f64(100.0),
            high: Price::from_f64(100.0),
            low: Price::from_f64(100.0),
            close: Price::from_f64(100.0),
            volume: Quantity::from_f64(10.0),
        }];
        driver.tick().expect("tick 应成功");

        assert_eq!(calls.load(Ordering::SeqCst), 4, "4 根 bar 各决策一次");
        let mut seen_sorted = seen.lock().unwrap().clone();
        seen_sorted.sort();
        seen_sorted.dedup();
        assert_eq!(
            seen_sorted,
            vec![
                START,
                START + ONE_MIN,
                START + 2 * ONE_MIN,
                START + 3 * ONE_MIN
            ],
            "补灌的 t2 必须插到正确历史位置: {seen_sorted:?}"
        );

        // 全平后账面 = 10000 − 0.05 × (30 + 30)
        let snap = driver.snapshot().expect("snapshot 成功");
        assert!(
            snap.positions.is_empty(),
            "全平后持仓应为空: {:?}",
            snap.positions
        );
        assert!(
            (snap.equity - 9_997.0).abs() < 1e-6,
            "权益 = 10000 − 3.0 价差损耗: {}",
            snap.equity
        );

        // 重推 t2（模拟交易所乱序补发）：必须被去重，不再触发决策 / 补灌
        handle.push_message(ws_kline(START + 2 * ONE_MIN));
        driver.tick().expect("tick 应成功");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            4,
            "重复 bar 不得触发第二次决策"
        );
    }
}
