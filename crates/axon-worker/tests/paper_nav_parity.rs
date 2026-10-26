//! paper NAV ↔ run_history NAV 逐点 parity（SubTask 10.5-A 验收门）
//!
//! 同 config（fee=0、`half_spread=0.05` 的种子流动性撮合）、同一 bar 序列、
//! 同一决策脚本，跑两条路径：
//!
//! - **A 路**：`PaperDriver` feed_bar 模式（`PaperSink` 虚拟撮合，收市事件流式入队）
//! - **B 路**：`Kernel::run_history`（`BacktestSink`，网格一次性展平成帧）
//!
//! 两条路共用同一套虚拟撮合引擎（`SeedLiquidity` 由 `KernelConfig.seed` 统一驱动
//! `engine.with_seed_liquidity`），因此 NAV 曲线必须逐点一致——这不是近似相等，
//! 而是编排等价性的验收门：paper 与回测漂移必须落在滑点带内（此处配置下带内
//! 即逐位重合）。
//!
//! # NAV 采样点对齐原理（为什么 6 根 bar、为什么价格恒定）
//!
//! `BarLifecycle::on_bar_closed` 的 NAV 采样在每帧第 3 步 `begin_bar_multi`
//! （pre_bar）打点，时刻 = 引擎 clock = **网格时间戳（bar 起始时刻）**，
//! 数值 = 处理完上一帧后的现金 + 持仓 × **当前帧 close 的 mark 价**。
//! A 路 feed_bar 发出的收市事件在 `KernelLoop` 流式 dispatch 时同样回退
//! `frame_ts = close_ts − interval`，两路 frame_ts 口径一致，于是：
//!
//! ```text
//! B 路 curve[i]（时刻 START+i·1m）  ↔  A 路第 i−1 次 tick 后 snapshot
//! curve[0]（initial_cash）          ↔  A 路 start 后、未 tick 的 snapshot
//! ```
//!
//! 但 mark 时点天然错一拍：A 路 snapshot 只能在 tick 边界取，mark 停在
//! 「已处理完的最后一帧」的 close；B 路 curve[i] 的 mark 已切到第 i 帧 close。
//! 两路引擎状态（现金/持仓）本身逐位一致，差异纯属估值时点——因此数据用
//! **恒定价格 100.0**：mark 价与新旧行情无关，错拍归零，采样点严格逐点对齐。
//! 成交多样性不靠价格波动，靠 target 序列（开仓 → 全平 → 调仓 → 全平 → 建仓
//! → Hold）驱动。末帧（第 6 根）走 Hold，只为把「处理完第 5 帧」的权益送进
//! curve[5]，并与 `run.final_nav` 一起对上 A 路第 5 次 tick 后的 snapshot。

use std::collections::BTreeMap;

use axon_core::market::{Bar, Side};
use axon_core::{Price, Quantity, Timestamp};
use axon_kernel::event::BarClosedEvent;
use axon_kernel::{
    BacktestSink, HostCtx, InstrumentMode, Kernel, KernelConfig, OrderIntent, SeedLiquidity,
    StrategyHost, build_frames,
};
use axon_worker::PaperDriver;

const START: i64 = 1_700_000_000_000_000_000;
const ONE_MIN: i64 = 60_000_000_000;

/// f64 相对误差（与 kernel_parity 同款规则；绝对值为 0 时退化为绝对误差）
const REL_TOL: f64 = 1e-9;
const ABS_TOL: f64 = 1e-12;

fn f64_close(a: f64, b: f64) -> bool {
    if a == b {
        return true;
    }
    let diff = (a - b).abs();
    if diff <= ABS_TOL {
        return true;
    }
    diff <= REL_TOL * a.abs().max(b.abs())
}

/// 无摩擦配置（两路共用）：fee=0、half_spread=0.05。
/// half_spread 必须为正——引擎把 `<=0` 视为不种盘口；成交价 = mid ± 0.05
/// 让所有锚点可手算。
fn config() -> KernelConfig {
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

/// 按帧下标的决策脚本：`Some(ratio)` = SetTarget（权益占比），`None` = Hold
/// （不吐单，沿用上次生效目标）。A/B 两路共用同一实例逻辑，保证判据一致。
struct ScriptHost {
    policy: Vec<Option<f64>>,
    idx: usize,
}

impl StrategyHost for ScriptHost {
    fn name(&self) -> &str {
        "script"
    }
    fn on_event(&mut self, ev: &BarClosedEvent<'_>, _ctx: &HostCtx) -> Vec<OrderIntent> {
        let ratio = self.policy[self.idx.min(self.policy.len() - 1)];
        self.idx += 1;
        match ratio {
            Some(r) => vec![OrderIntent::SetTarget {
                instrument: ev.instrument.clone(),
                target_ratio: r,
            }],
            None => Vec::new(),
        }
    }
}

/// OHLC 同价且全序列同价的 bar——mark 时点错拍归零（见模块文档），价格序列
/// 只作为成交价与估值的常数基底，成交多样性由 target 序列驱动。
fn flat_bar(start_ns: i64, price: f64) -> Bar {
    Bar {
        timestamp: Timestamp::from_nanos(start_ns),
        open: Price::from_f64(price),
        high: Price::from_f64(price),
        low: Price::from_f64(price),
        close: Price::from_f64(price),
        volume: Quantity::from_f64(10.0),
    }
}

#[test]
fn paper_nav_parallels_run_history() {
    // 恒定价格：消除 B 路 curve[i]（mark = 第 i 帧 close）与 A 路 tick 后
    // snapshot（mark = 第 i−1 帧 close）的估值时点差（见模块文档）
    let prices = [100.0f64; 6];
    let policy = vec![Some(0.5), Some(0.0), Some(0.4), Some(0.0), Some(0.3), None];

    // ── A 路：PaperDriver（feed_bar + tick，PaperSink）──
    let mut driver = PaperDriver::new(None, config(), vec!["BTCUSDT".into()]).expect("构造成功");
    driver.connect().expect("feed_bar 模式 connect 直接 Ok");
    driver
        .start(vec![Box::new(ScriptHost {
            policy: policy.clone(),
            idx: 0,
        })])
        .expect("start 成功");
    // 起点采样：内核已建、零交易，equity = initial_cash，时刻记 START
    // （对应 B 路 curve[0]，该点是 begin_bar_multi 的 first-frame 语义）
    let mut paper_curve: Vec<(Timestamp, f64)> = vec![(
        Timestamp::from_nanos(START),
        driver.snapshot().expect("起始 snapshot 成功").equity,
    )];
    for (i, price) in prices.iter().enumerate() {
        driver
            .feed_bar(
                "BTCUSDT",
                START + i as i64 * ONE_MIN,
                *price,
                *price,
                *price,
                *price,
                10.0,
            )
            .expect("灌 bar 应成功");
        driver.tick().expect("tick 应成功");
        let snap = driver.snapshot().expect("snapshot 成功");
        paper_curve.push((
            Timestamp::from_nanos(START + (i as i64 + 1) * ONE_MIN),
            snap.equity,
        ));
    }

    // ── B 路：Kernel + BacktestSink（run_history）──
    let bars: Vec<Option<Bar>> = prices
        .iter()
        .enumerate()
        .map(|(i, price)| Some(flat_bar(START + i as i64 * ONE_MIN, *price)))
        .collect();
    let bars_by_symbol = BTreeMap::from([("BTC/USDT".to_string(), bars)]);
    let frames =
        build_frames(&bars_by_symbol, &InstrumentMode::Spot, None).expect("网格应能展平成帧");
    let mut kernel = Kernel::new(config(), Box::new(BacktestSink::new()));
    kernel.add_host(Box::new(ScriptHost { policy, idx: 0 }));
    let result = kernel.run_history(frames).expect("内核回测应跑通");
    let curve = &result.run.bar_nav_curve;

    // ── 断言 1：NAV 曲线逐点一致（时刻 + 数值）。paper_curve 共 7 点
    //    （起点 + 6 次 tick），前 6 点与 curve 逐点对齐；第 7 点走 final_nav。──
    assert_eq!(result.bars_processed, 6, "6 帧必须全部处理");
    assert_eq!(curve.len(), 6, "每帧一个 pre_bar 采样点");
    assert_eq!(paper_curve.len(), 7, "起点 + 每 tick 一点");
    for (i, ((bt, bv), (pt, pv))) in curve.iter().zip(paper_curve.iter()).enumerate() {
        assert_eq!(
            bt.nanos, pt.nanos,
            "curve[{i}] 时刻不一致：B={} A={}",
            bt.nanos, pt.nanos
        );
        assert!(f64_close(*bv, *pv), "curve[{i}] NAV 不一致：B={bv} A={pv}");
    }
    // 第 7 点：A 路第 5 次 tick（Hold 帧）后的权益 == B 路末态 final_nav
    assert!(
        f64_close(result.run.final_nav, paper_curve[6].1),
        "末态权益不一致：B final_nav={} A snapshot={}",
        result.run.final_nav,
        paper_curve[6].1
    );

    // ── 断言 2：人工锚点——首帧（建仓 50 @ 100.05）后权益
    //    = 10000 − half_spread×qty = 10000 − 0.05×50 = 9997.5 ──
    let (t1, v1) = paper_curve[1];
    assert_eq!(t1.nanos, START + ONE_MIN);
    assert!(f64_close(v1, 9_997.5), "首帧后权益 = 10000 − 0.05×50: {v1}");

    // ── 断言 3：逐笔成交价 = 本帧 close ± half_spread（滑点带精确不变量）、
    //    数量与 target 换算一致。数量手算（fee=0、决策 equity 见注释）：
    //    ①0.5×10000/100=50 ②全平 50 ③0.4×9995/100=39.98 ④全平 39.98
    //    ⑤0.3×9991.002/100=29.973006 ──
    let fills = &result.run.fills_detail;
    assert_eq!(fills.len(), 5, "target 变动 5 次 = 5 笔成交");
    let expected: [(Side, f64, f64); 5] = [
        (Side::Buy, 100.0, 50.0),
        (Side::Sell, 100.0, 50.0),
        (Side::Buy, 100.0, 39.98),
        (Side::Sell, 100.0, 39.98),
        (Side::Buy, 100.0, 29.973006),
    ];
    for (i, (fill, (side, close, qty))) in fills.iter().zip(expected.iter()).enumerate() {
        assert_eq!(fill.taker_side, *side, "fills[{i}] 方向");
        assert!(
            f64_close(fill.quantity.as_f64(), *qty),
            "fills[{i}] 数量：{} vs 预期 {qty}",
            fill.quantity.as_f64()
        );
        let expected_price = match side {
            Side::Buy => close + 0.05,
            Side::Sell => close - 0.05,
        };
        assert!(
            f64_close(fill.price.as_f64(), expected_price),
            "fills[{i}] 成交价必须落在 mid±half_spread：{} vs {expected_price}",
            fill.price.as_f64()
        );
    }

    // ── 断言 4：末态持仓一致（0.3 建仓后保留到末帧 Hold）──
    let end_pos = driver.snapshot().expect("末态 snapshot 成功").positions;
    assert_eq!(end_pos.len(), 1, "末态应持有 BTC");
    let b_qty = result
        .run
        .positions
        .values()
        .next()
        .copied()
        .expect("内核末态应有一笔持仓");
    let (_, p_qty) = end_pos.iter().next().expect("上面断言保证非空");
    assert!(
        f64_close(*p_qty, b_qty),
        "末态持仓：paper={p_qty} kernel={b_qty}"
    );
}
