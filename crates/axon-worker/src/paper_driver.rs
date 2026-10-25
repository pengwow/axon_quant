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
};

use crate::backtest::instrument_key_for;
use crate::exchange_gateway::ExchangeGateway;
use crate::pump::KernelPump;

/// 行情 fan-out 通道容量（与 `live_driver` 同值：tick 节奏下不会触顶）
const FANOUT_CHANNEL_CAPACITY: usize = 4096;

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
        for sym in &symbols {
            let key = instrument_key_for(sym);
            let instrument = config.instrument_mode.instrument(&key);
            expected.insert(instrument.clone());
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

    /// 推进一拍：泵行情（gateway 模式）→ 步进消化（含 feed_bar 已灌事件）
    pub fn tick(&mut self) -> Result<(), String> {
        let kernel = self
            .kernel
            .as_mut()
            .ok_or_else(|| "内核未启动，请先 start".to_string())?;
        // 1. 泵走 fanout 里攒的行情（feed_bar 模式无 pump，事件已直接入队）
        if let Some(pump) = self.pump.as_mut() {
            for event in pump.drain().events {
                kernel.enqueue_external(event, None).map_err(kerr)?;
            }
        }
        // 2. 步进消化（多品种时栅栏未齐则本轮空转，等待后续 bar）
        while kernel.step().map_err(kerr)? {}
        // 3. PaperSink::poll 恒空，但保持与 LiveDriver 相同的 tick 三步节奏
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
}
