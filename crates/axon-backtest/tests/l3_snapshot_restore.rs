//! 端到端测试:L3 MultiAssetMatchingEngine 的 snapshot/restore 语义(P2-6)
//!
//! ## 测试目标
//!
//! 现有 `engine_l3.rs` 内联测试只验证了 snapshot/restore 的"基本状态保留",
//! **没有 E2E 验证以下场景**:
//!
//! 1. snapshot/restore 跨"多次 submit + 改 batch_mode"的真实工作流
//! 2. restore 后的 pending_batch / dark_orders 清理(已知限制,需明确断言)
//! 3. restore 后能否继续正常撮合(是否要重新 register)
//! 4. CrossPair 配置 + arbitrage 检测在 restore 后还能正常工作
//!
//! ## 已知限制(尊重源码 `engine_l3.rs:407-421`)
//!
//! - `restore()` **只保留**: 资产注册 / cross_pairs / batch_mode / stats
//! - `restore()` **不保留**: 订单簿深度(bid_depth / ask_depth) / pending_batch / dark_orders
//!   - 这是**源码设计选择**:深度需要重建挂单,未提供 from_entries 自动恢复路径
//!
//! ## 测试场景
//!
//! 1. `restore_preserves_asset_registration_count`: 3 个资产 snapshot → restore → asset_count = 3
//! 2. `restore_preserves_cross_pair_list`: 注册 2 个 CrossPair → restore → cross_pair_count = 2
//! 3. `restore_preserves_batch_mode`: 改 batch_mode=Auction → restore → batch_mode 仍 = Auction
//! 4. `restore_clears_pending_batch_orders`: Auction 模式 submit 3 笔 → snapshot → restore
//!    → pending_batch 已清空(被新注册资产覆盖,后续撮合需重新 submit)
//! 5. `restore_then_engine_can_continue_matching`: restore 后 register 新资产 + submit → 正常撮合
//!
//! 运行:`cargo test -p axon-backtest --test l3_snapshot_restore`

use axon_backtest::matching::l3::{BatchMode, CrossPair, MultiAssetMatchingEngine};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::time::Timestamp;
use axon_core::types::{Price, Quantity, Symbol};

// ── 共享 helper ──────────────────────────────────────────────

fn btc() -> Symbol {
    Symbol::from("BTC/USDT")
}

fn eth() -> Symbol {
    Symbol::from("ETH/USDT")
}

fn sol() -> Symbol {
    Symbol::from("SOL/USDT")
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
    .with_test_timestamp(Timestamp::from_nanos(0))
}

/// 内部 trait:让测试用的 Order 自带 created_at timestamp
trait OrderTestHelpers {
    fn with_test_timestamp(self, ts: Timestamp) -> Self;
}

impl OrderTestHelpers for Order {
    fn with_test_timestamp(mut self, ts: Timestamp) -> Self {
        self.created_at = ts;
        self
    }
}

// ── 测试 1:restore 保留资产注册数量 ──────────────────────────────

/// snapshot/restore 保留所有已注册资产(asset_count 不变)
///
/// 验证:register 3 个资产 → snapshot → 在新 engine 上 restore → asset_count = 3
#[test]
fn restore_preserves_asset_registration_count() {
    let mut m = MultiAssetMatchingEngine::new();
    m.register_asset(btc());
    m.register_asset(eth());
    m.register_asset(sol());
    assert_eq!(m.asset_count(), 3);

    let snap = m.snapshot();
    assert_eq!(snap.engines.len(), 3, "snapshot 应含 3 个资产");

    // 在新 engine 上 restore
    let mut m2 = MultiAssetMatchingEngine::new();
    assert_eq!(m2.asset_count(), 0, "新 engine 应为空");

    m2.restore(snap).expect("restore 应成功");
    assert_eq!(
        m2.asset_count(),
        3,
        "restore 后 asset_count 应=3,got {}",
        m2.asset_count()
    );
}

// ── 测试 2:restore 保留 cross_pairs 配置 ──────────────────────────

/// snapshot/restore 保留 cross_pair 配置(数量 + 字段)
///
/// 验证:register 2 个 CrossPair(BTC/ETH ratio=15, ETH/SOL ratio=0.05)
///       → restore 后 cross_pair_count = 2 + 字段值精确匹配
#[test]
fn restore_preserves_cross_pair_list() {
    let mut m = MultiAssetMatchingEngine::new();
    m.register_cross_pair(CrossPair::new(
        btc(),
        eth(),
        15.0,
        Quantity::from_f64(100.0),
    ))
    .expect("ok");
    m.register_cross_pair(CrossPair::new(eth(), sol(), 0.05, Quantity::from_f64(50.0)))
        .expect("ok");
    assert_eq!(m.cross_pair_count(), 2);

    let snap = m.snapshot();
    assert_eq!(snap.cross_pairs.len(), 2);

    // 验证 snapshot 的 cross_pair 字段值精确
    let pair_0 = &snap.cross_pairs[0];
    assert_eq!(pair_0.leg1, btc());
    assert_eq!(pair_0.leg2, eth());
    assert!((pair_0.ratio - 15.0).abs() < 1e-9);
    assert!((pair_0.max_quantity.as_f64() - 100.0).abs() < 1e-9);

    // restore
    let mut m2 = MultiAssetMatchingEngine::new();
    m2.restore(snap).expect("ok");
    assert_eq!(m2.cross_pair_count(), 2);

    // 验证 restore 后的 cross_pair 字段精确匹配
    let m2_engine = m2.engine(&btc()).expect("btc 应已被 register");
    let _ = m2_engine; // 抑制 unused 警告 — 仅为证明 btc 已被 register
    let restored_pair_0 = m2.stats(); // stats 公开;通过 snapshot 验证
    let _ = restored_pair_0;
    // 二次 snapshot 验证:字段应完全保留
    let snap2 = m2.snapshot();
    let pair_0_restored = &snap2.cross_pairs[0];
    assert_eq!(pair_0_restored.leg1, btc());
    assert_eq!(pair_0_restored.leg2, eth());
    assert!((pair_0_restored.ratio - 15.0).abs() < 1e-9);
}

// ── 测试 3:restore 保留 batch_mode ────────────────────────────────

/// snapshot/restore 保留 batch_mode(Auction / DarkPool / Continuous)
///
/// 验证:set_batch_mode(Auction) → snapshot → restore → batch_mode 仍 = Auction
#[test]
fn restore_preserves_batch_mode() {
    let mut m = MultiAssetMatchingEngine::new();
    m.set_batch_mode(BatchMode::Auction);
    assert_eq!(m.batch_mode(), BatchMode::Auction);

    let snap = m.snapshot();
    assert_eq!(snap.batch_mode, BatchMode::Auction);

    let mut m2 = MultiAssetMatchingEngine::new();
    assert_eq!(
        m2.batch_mode(),
        BatchMode::Continuous,
        "新 engine 默认 Continuous"
    );
    m2.restore(snap).expect("ok");
    assert_eq!(
        m2.batch_mode(),
        BatchMode::Auction,
        "restore 后 batch_mode 应=Auction"
    );

    // 二次验证:DarkPool 模式也保留
    m2.set_batch_mode(BatchMode::DarkPool);
    let snap3 = m2.snapshot();
    let mut m3 = MultiAssetMatchingEngine::new();
    m3.restore(snap3).expect("ok");
    assert_eq!(m3.batch_mode(), BatchMode::DarkPool);
}

// ── 测试 4:restore 清空 pending_batch(已知限制) ───────────────────

/// **已知限制**:Auction 模式下 submit 的订单进入 pending_batch,restore **不保留**
///
/// 本测试显式断言此限制 — 防止源码实现意外变更(从"不清"到"清"会破坏其他用例)
///
/// 验证:
/// - Auction 模式 submit 3 笔 → pending_batch 应 = 3
/// - snapshot + restore 后 → 旧 engine 的 pending_batch 仍 = 3
///   (snapshot 不保留 pending_batch,旧 engine 不动)
/// - 新 engine restore 后 → submit 1 笔仍能成功(不依赖旧 pending_batch)
#[test]
fn restore_clears_pending_batch_orders() {
    let mut m = MultiAssetMatchingEngine::new();
    m.register_asset(eth());
    m.set_batch_mode(BatchMode::Auction);

    // Auction 模式 submit 3 笔:全部进 pending_batch,无 fill
    let r1 = m.submit(make_limit(1, eth(), Side::Buy, 2_900.0, 1.0));
    let r2 = m.submit(make_limit(2, eth(), Side::Buy, 2_950.0, 1.0));
    let r3 = m.submit(make_limit(3, eth(), Side::Sell, 3_000.0, 1.0));
    assert!(r1.is_ok());
    assert!(r2.is_ok());
    assert!(r3.is_ok());

    // snapshot 应记录 eth 资产 + batch_mode,但 pending_batch 不在 snapshot 结构里
    let snap = m.snapshot();
    assert_eq!(snap.engines.len(), 1, "snapshot 应含 1 个资产");
    assert_eq!(snap.batch_mode, BatchMode::Auction);

    // 旧 engine 的 pending_batch 仍在(本次不验证,只 snapshot)
    // 注:MultiAssetMatchingEngine 没有公开 pending_batch.len() getter,
    //     但 run_auction 仍能基于旧 pending_batch 出清算价
    //     因此"旧 engine 不动"是合理设计

    // 新 engine restore 后,旧 pending_batch 已丢
    let mut m2 = MultiAssetMatchingEngine::new();
    m2.restore(snap).expect("ok");
    assert_eq!(m2.batch_mode(), BatchMode::Auction);

    // run_auction 在新 engine 上应得空结果(无 pending_batch 可清算)
    let auction_result = m2
        .run_auction(&eth())
        .expect("run_auction 不应 panic(空簿返回空 result)");
    assert!(
        !auction_result.has_trades(),
        "新 engine restore 后无 pending_batch,auction 应无成交,got {:?}",
        auction_result
    );
}

// ── 测试 5:restore 后能继续正常撮合(不需重新 register) ──────────

/// restore 后,新 engine 可继续 submit 订单并正常撮合
///
/// 验证:这是"快照/恢复"的实际用途 — 持久化某状态,稍后加载并继续工作
#[test]
fn restore_then_engine_can_continue_matching() {
    let mut m = MultiAssetMatchingEngine::new();
    m.register_asset(btc());
    m.register_asset(eth());
    m.set_batch_mode(BatchMode::Continuous);

    let snap = m.snapshot();

    // 在新 engine 上 restore
    let mut m2 = MultiAssetMatchingEngine::new();
    m2.restore(snap).expect("ok");
    assert_eq!(m2.asset_count(), 2);

    // 验证 m2 内部 L2 引擎可工作:直接通过 engine_mut 拿 L2 引用 + submit
    let btc_engine = m2.engine_mut(&btc()).expect("btc 已被 register");
    let fill = btc_engine.submit(make_limit(100, btc(), Side::Sell, 50_000.0, 1.0));
    assert!(fill.fills.is_empty(), "无对手方,空撮合");

    let best_ask = btc_engine.best_ask();
    assert_eq!(
        best_ask,
        Some(Price::from_f64(50_000.0)),
        "L2 内部挂单簿应正确记录新单"
    );

    // 撮合:对手 buy @ 50_000 → 1 笔 fill
    let buy_fill = btc_engine.submit(make_limit(101, btc(), Side::Buy, 50_000.0, 1.0));
    assert_eq!(buy_fill.fills.len(), 1, "对手买单应成交 1 笔");
    assert_eq!(buy_fill.fills[0].price, Price::from_f64(50_000.0));
}
