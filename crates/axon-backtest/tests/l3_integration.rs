//! 端到端测试:L3 多资产撮合引擎的 E2E 语义(P1-1)
//!
//! ## 测试目标
//!
//! v1 落地的 e2e_*.rs 全部用 `L1MatchingEngine`(`matching::MatchingEngine` trait
//! 唯一直接实现者)。L3 `MultiAssetMatchingEngine` 是**多资产路由 + 暗池 +
//! 批量拍卖**的高阶撮合,但源码未实现 `MatchingEngine` trait,不能直接被
//! `BacktestEngine::run` 消费。
//!
//! 本测试套件**部分通过 BacktestEngine + L3Adapter**(连续模式) + **部分直接
//! 调 L3 API**(`step()` 模式,适配批量/暗池语义)验证:
//!
//! 1. **多资产路由隔离**:L3 把订单路由到正确 symbol 的内部 L2
//! 2. **批量拍卖**:Auction 模式暂存 + `run_auction` 出清算价
//! 3. **暗池撮合**:DarkPool 模式扫暗池簿
//! 4. **跨资产套利检测**:`detect_arbitrage` 在盘口偏离时返回 `ArbitrageOpportunity`
//! 5. **快照/恢复**:`snapshot + restore` 后资产/配置保留(订单簿不恢复,这是已知限制)
//!
//! ## 设计要点
//!
//! - **直接 L3 API** 验证 L3 自身语义(批量/暗池/套利 与 BacktestEngine 事件队列
//!   模型不兼容,不适合走 BacktestEngine 主循环)
//! - **L3Adapter 桥接**(测试 6)用 thin wrapper 验证 BacktestEngine 持有 L3 的
//!   Continuous 模式行为(只是"插得进",不保证所有 L3 语义生效)
//! - **手算对账**:清算价/暗池匹配价/套利 deviation 都可手算
//!
//! 运行:`cargo test -p axon-backtest --test l3_integration`

use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig};
use axon_backtest::matching::l3::{BatchMode, CrossPair, DarkOrder, MultiAssetMatchingEngine};
use axon_backtest::matching::{MatchingEngine, OrderBookLevel, SubmitResult};
use axon_core::event::{EventBuilder, OrderAction};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{Price, Quantity, Symbol};

// ── 共享 helper ──────────────────────────────────────────────────────

fn btc() -> Symbol {
    Symbol::from("BTC/USDT")
}
fn eth() -> Symbol {
    Symbol::from("ETH/USDT")
}

fn make_limit(id: u64, symbol: Symbol, side: Side, price: f64, qty: f64) -> Order {
    Order::new(
        id,
        symbol,
        side,
        OrderType::Limit {
            price: Price::from_f64(price),
        },
        Quantity::from_f64(qty),
        TimeInForce::GTC,
    )
}

fn make_market(id: u64, symbol: Symbol, side: Side, qty: f64) -> Order {
    Order::new(
        id,
        symbol,
        side,
        OrderType::Market,
        Quantity::from_f64(qty),
        TimeInForce::IOC,
    )
}

// ── L3Adapter:让 L3 接入 MatchingEngine trait(Continuous 模式) ─────

/// `MultiAssetMatchingEngine` → `MatchingEngine` trait 适配器
///
/// 限制:
/// - 仅 Continuous 模式语义;Auction 模式 orders 暂存,不在 BacktestEngine 主循环生效
/// - `best_bid` / `best_ask` / `depth` 取**任一注册 symbol** 的最优价(简单聚合)
/// - `seed_liquidity` / `clear_book` 在 L3 上语义有限,做 no-op
///
/// **关键实现细节**:L3 未暴露 enumerate registered symbols 的 API,本 adapter
/// 自行跟踪已注册 symbol 列表(测试层语义)。
struct L3Adapter {
    inner: MultiAssetMatchingEngine,
    /// 测试层跟踪:已注册 symbols(用于 best_bid/best_ask/depth 聚合)
    registered: Vec<Symbol>,
}

impl L3Adapter {
    fn new() -> Self {
        Self {
            inner: MultiAssetMatchingEngine::new(),
            registered: Vec::new(),
        }
    }

    fn register(&mut self, symbol: Symbol) {
        self.inner.register_asset(symbol.clone());
        if !self.registered.contains(&symbol) {
            self.registered.push(symbol);
        }
    }
}

impl Default for L3Adapter {
    fn default() -> Self {
        Self::new()
    }
}

impl MatchingEngine for L3Adapter {
    fn submit(&mut self, order: Order) -> SubmitResult {
        // L3.submit 在 Continuous 模式路由到内部 L2 → 返回 fills
        // Auction / DarkPool 模式可能返回空 fills(订单暂存)
        let active_before = self.active_order_count();
        let fills_res = self.inner.submit(order);
        let active_after = self.active_order_count();
        match fills_res {
            Ok(fills) if !fills.is_empty() => SubmitResult::filled(fills),
            _ if active_after > active_before => {
                // 挂入内部 L2 簿但未成交(Continuous limit 单)
                SubmitResult::empty(Quantity::default())
            }
            _ => SubmitResult::empty(Quantity::default()),
        }
    }

    fn cancel(&mut self, _order_id: u64) -> bool {
        // L3 未实现统一 cancel API(需逐 symbol 调 inner engine_mut)
        false
    }

    fn best_bid(&self) -> Option<Price> {
        self.registered
            .iter()
            .filter_map(|s| self.inner.engine(s).and_then(|e| e.best_bid()))
            .next()
    }

    fn best_ask(&self) -> Option<Price> {
        self.registered
            .iter()
            .filter_map(|s| self.inner.engine(s).and_then(|e| e.best_ask()))
            .next()
    }

    fn spread(&self) -> Option<Price> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        Some(Price::from_f64(ask.as_f64() - bid.as_f64()))
    }

    fn depth(&self, levels: usize) -> (Vec<OrderBookLevel>, Vec<OrderBookLevel>) {
        let mut bids = Vec::new();
        let mut asks = Vec::new();
        for symbol in &self.registered {
            if let Some(engine) = self.inner.engine(symbol) {
                let (b, a) = engine.depth(levels);
                bids.extend(b);
                asks.extend(a);
            }
        }
        (bids, asks)
    }

    fn active_order_count(&self) -> usize {
        self.registered
            .iter()
            .filter_map(|s| self.inner.engine(s).map(|e| e.active_order_count()))
            .sum()
    }

    fn clear_book(&mut self) {
        // L2MatchingEngine 未实现 `clear_book` 方法,且 L3Adapter 接口本身
        // 对此方法的语义有限(参见模块顶部 `L3Adapter` 文档说明),
        // 显式 no-op,避免编译错误。若未来 BacktestEngine 需强依赖此方法,
        // 应改用 L2 的 cancel-all 或新建 `MatchingEngine::clear_book` 默认实现。
    }
}

// ── 测试 1:多资产路由隔离 ───────────────────────────────────────────

/// 注册 BTC + ETH,submit BTC sell + ETH sell
/// 验证:两 symbol 内部 L2 簿各自挂单,互不影响
#[test]
fn l3_routes_to_correct_asset_in_continuous_mode() {
    let mut m = MultiAssetMatchingEngine::new();
    m.register_asset(btc());
    m.register_asset(eth());

    // BTC 卖单 @ 50000
    let btc_sell_fills = m
        .submit(make_limit(1, btc(), Side::Sell, 50_000.0, 1.0))
        .expect("submit btc sell");
    assert!(btc_sell_fills.is_empty(), "无对手方,挂簿不成交");

    // ETH 卖单 @ 3000
    let eth_sell_fills = m
        .submit(make_limit(2, eth(), Side::Sell, 3_000.0, 1.0))
        .expect("submit eth sell");
    assert!(eth_sell_fills.is_empty(), "无对手方,挂簿不成交");

    // BTC 内部 L2 有 best_ask,ETH 也有
    let btc_engine = m.engine(&btc()).expect("btc engine");
    assert_eq!(btc_engine.best_ask(), Some(Price::from_f64(50_000.0)));
    assert_eq!(btc_engine.best_bid(), None);

    let eth_engine = m.engine(&eth()).expect("eth engine");
    assert_eq!(eth_engine.best_ask(), Some(Price::from_f64(3_000.0)));
    assert_eq!(eth_engine.best_bid(), None);

    // BTC buy @ 50000 → 吃 BTC sell
    let btc_buy_fills = m
        .submit(make_limit(3, btc(), Side::Buy, 50_000.0, 1.0))
        .expect("submit btc buy");
    assert_eq!(btc_buy_fills.len(), 1, "BTC 撮合 1 笔 fill");
    assert_eq!(
        btc_buy_fills[0].price,
        Price::from_f64(50_000.0),
        "BTC fill @ 50_000"
    );

    // ETH 簿仍只有 ETH sell,没被 BTC buy 撮合
    let eth_engine = m.engine(&eth()).expect("eth engine");
    assert_eq!(eth_engine.best_ask(), Some(Price::from_f64(3_000.0)));
}

// ── 测试 2:批量拍卖清算价 ──────────────────────────────────────────

/// Auction 模式:4 笔订单暂存 + run_auction → 出清算价 + 成交
#[test]
fn l3_batch_auction_clears_at_uniform_price() {
    let mut m = MultiAssetMatchingEngine::new();
    m.register_asset(eth());
    m.set_batch_mode(BatchMode::Auction);

    // 累积 4 笔:2 买 2 卖
    m.submit(make_limit(1, eth(), Side::Buy, 2_990.0, 5.0))
        .expect("ok");
    m.submit(make_limit(2, eth(), Side::Buy, 3_000.0, 3.0))
        .expect("ok");
    m.submit(make_limit(3, eth(), Side::Sell, 3_010.0, 4.0))
        .expect("ok");
    m.submit(make_limit(4, eth(), Side::Sell, 3_020.0, 5.0))
        .expect("ok");

    // 4 笔 submit 都不应成交(Auction 模式)
    let result = m.run_auction(&eth()).expect("auction");
    assert!(result.has_trades(), "应有成交");
    assert!(!result.fills.is_empty(), "至少 1 笔 fill");

    // 清算价应介于 2990-3020 之间
    let cp = result.clearing_price.as_f64();
    assert!(
        (2_990.0..=3_020.0).contains(&cp),
        "清算价 {cp} 应在 [2990, 3020]"
    );
    assert!(result.clearing_volume.as_f64() > 0.0, "成交量 > 0");
}

// ── 测试 3:暗池撮合 ────────────────────────────────────────────────

/// DarkPool 模式:2 个暗池订单(buy + sell @ 同价)→ 1 笔 fill
#[test]
fn l3_dark_pool_matches_existing_dark_order() {
    let mut m = MultiAssetMatchingEngine::new();
    m.register_asset(btc());
    m.set_batch_mode(BatchMode::DarkPool);

    // 1) 暗池 sell
    let sell = make_limit(1, btc(), Side::Sell, 50_000.0, 3.0);
    let sell_fills = m
        .submit_dark_order(DarkOrder {
            visible_quantity: Quantity::from_f64(1.0),
            hidden_quantity: Quantity::from_f64(3.0),
            order: sell,
        })
        .expect("ok");
    assert!(sell_fills.is_empty(), "首个暗池 sell 暂存,无成交");

    // 2) 暗池 buy 同价 → 撮合
    let buy = make_limit(2, btc(), Side::Buy, 50_000.0, 3.0);
    let buy_fills = m
        .submit_dark_order(DarkOrder {
            visible_quantity: Quantity::from_f64(1.0),
            hidden_quantity: Quantity::from_f64(3.0),
            order: buy,
        })
        .expect("ok");

    assert_eq!(buy_fills.len(), 1, "暗池撮合 1 笔");
    assert_eq!(buy_fills[0].quantity, Quantity::from_f64(3.0));
    assert_eq!(m.stats().total_dark_fills, 1, "累计 1 笔暗池 fill");
}

// ── 测试 4:跨资产套利检测 ──────────────────────────────────────────

/// 注册 BTC/ETH pair(比例 16)+ 双方盘口 → 套利信号
#[test]
fn l3_detect_arbitrage_with_deviating_prices() {
    let mut m = MultiAssetMatchingEngine::new();
    m.register_cross_pair(CrossPair::new(btc(), eth(), 16.0, Quantity::from_f64(1.0)))
        .expect("ok");

    // BTC bid=50000, ask=50100 → mid 50050
    m.submit(make_limit(0, btc(), Side::Buy, 50_000.0, 1.0))
        .expect("ok");
    m.submit(make_limit(1, btc(), Side::Sell, 50_100.0, 1.0))
        .expect("ok");
    // ETH bid=3000, ask=3020 → mid 3010
    m.submit(make_limit(2, eth(), Side::Buy, 3_000.0, 1.0))
        .expect("ok");
    m.submit(make_limit(3, eth(), Side::Sell, 3_020.0, 1.0))
        .expect("ok");

    let ops = m.detect_arbitrage();
    assert_eq!(ops.len(), 1, "1 个 pair");
    let op = &ops[0];
    assert!(op.implied_ratio.is_some(), "implied_ratio 应有值");
    let ir = op.implied_ratio.unwrap();
    // implied = btc_mid / eth_mid = 50050 / 3010 ≈ 16.628
    assert!(ir > 16.0, "implied ratio {ir} 应 > 16.0");
    assert!(op.deviation > 0.0, "deviation > 0");
    assert!(op.estimated_profit > 0.0, "estimated_profit > 0");
}

// ── 测试 5:快照/恢复一致性 ─────────────────────────────────────────

/// 完整 setup BTC + ETH + CrossPair + Auction 模式 → snapshot → 新 engine restore
/// 验证:asset_count / cross_pair_count / batch_mode 都恢复
/// (订单簿不恢复是已知限制,见 restore() 注释)
#[test]
fn l3_snapshot_restore_consistency() {
    let mut m = MultiAssetMatchingEngine::new();
    m.register_asset(btc());
    m.register_asset(eth());
    m.register_cross_pair(CrossPair::new(btc(), eth(), 16.0, Quantity::from_f64(1.0)))
        .expect("ok");
    m.set_batch_mode(BatchMode::Auction);

    let snap = m.snapshot();
    assert_eq!(snap.batch_mode, BatchMode::Auction);
    assert_eq!(snap.engines.len(), 2);
    assert_eq!(snap.cross_pairs.len(), 1);

    // 恢复到新 engine
    let mut restored = MultiAssetMatchingEngine::new();
    restored.restore(snap).expect("ok");

    assert_eq!(restored.asset_count(), 2, "2 个 asset 恢复");
    assert_eq!(restored.cross_pair_count(), 1, "1 个 pair 恢复");
    assert_eq!(restored.batch_mode(), BatchMode::Auction, "batch_mode 恢复");
}

// ── 测试 6:L3Adapter 接入 BacktestEngine(连续模式) ────────────────

/// L3Adapter(持 L3 in Continuous mode)接入 BacktestEngine,
/// 验证:BacktestEngine 能跑出有效 fill + PnL
///
/// 限制:L3 自身的 Auction/DarkPool 语义不会通过 BacktestEngine 触发(暂存)。
/// 本测试只验证「插得进、连续模式可成交」,不验证 L3 全部特性。
#[test]
fn l3_adapter_works_in_backtest_engine_continuous_mode() {
    let mut adapter = L3Adapter::new();
    adapter.register(btc());

    let cfg = BacktestEngineConfig {
        clock: SimulatedClock::new(Timestamp::from_nanos(0)),
        matching_engine: Box::new(adapter),
        impact_model: None,
        initial_cash: 100_000.0,
        fee_config: FeeConfig::default(),
        force_liquidate: false,
    };

    // 事件流:对手 sell + 策略 buy market
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(make_limit(1, btc(), Side::Sell, 100.0, 1.0)),
    ));
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        2,
        OrderAction::Submitted(make_market(2, btc(), Side::Buy, 1.0)),
    ));

    let mut engine = BacktestEngine::new(cfg, q);
    let result = engine.run();

    // BacktestEngine 通过 L3Adapter.submit() → L3.submit() → 内部 L2 撮合
    // 期望:1 笔 fill @ 100,买 1 卖 1 抵消,NAV 不变(扣手续费)
    assert_eq!(result.fills, 1, "L3Adapter 撮合 1 笔");
    assert_eq!(result.orders_accepted, 2, "2 单都被接受");
    assert!(
        (result.total_pnl - (-0.1)).abs() < 1e-6,
        "PnL = -fee = -0.1,got {}",
        result.total_pnl
    );
}
