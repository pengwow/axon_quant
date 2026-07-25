//! `BacktestTradingBackend`:把 `axon_backtest::L1MatchingEngine` 适配为
//! `TradingBackend`,供 LLM 工具在回测撮合引擎上下单。
//!
//! 详见 `docs/superpowers/specs/2026-06-17-axon-llm-backtest-adapter-design.md`
//! + `docs/superpowers/plans/2026-06-17-axon-llm-backtest-adapter.md`。
//!
//! # 关键设计
//!
//! - 内部状态持 `Arc<RwLock<BacktestInner>>`,L1MatchingEngine 同步撮合,
//!   write 锁内不跨 await(直接调 matching.submit / portfolio.apply_fill)。
//! - 自维护 `PortfolioState`(不复用 OMS Portfolio),简单 f64 cash + HashMap
//!   positions,基于 MatchFill.taker_side 调整。
//! - `place_order` 同步返回 ack,status 取自 `SubmitResult.is_filled` /
//!   `is_partially_filled` / 默认("Submitted")。
//! - 假设单 symbol + 单 base currency(USDT);多 symbol 由多个实例拼合。
//! - `OrderId` 内部从 `AtomicU64` 序列分配,字符串化为 `"bt-{n}"`(与 OMS Uuid / Exchange 区分)。

#![cfg(feature = "trading-backtest")]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio::sync::RwLock;

use axon_backtest::matching::{L1MatchingEngine, MatchFill, MatchingEngine};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::types::{Price, Quantity, Symbol};

use crate::trading::backend::{TradingBackend, TradingError};
use crate::trading::book_snapshot_tool::{OrderBookLevel, OrderBookSnapshot};
use crate::trading::types::{
    BalanceSnapshot, CurrencyBalance, OrderAck, OrderKind, OrderSide, OrderStatus, PlaceOrderArgs,
    PositionSnapshot,
};

// ==================== 辅助:now_ms ====================

/// 当前 unix epoch 毫秒(`i64`,OrderAck / BalanceSnapshot 的 `as_of_ms` 字段需要负值兼容)
fn now_ms_i64() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ==================== PortfolioState ====================

/// 内部 portfolio 状态(回测专用,简单 f64 精度)
///
/// 与 `axon_oms::Portfolio` 不同:
/// - 不使用 `rust_decimal`,f64 即可
/// - 单 base currency("USDT")
/// - `positions[symbol] = (qty, avg_price)`,avg_price 是加权平均
/// - 不应用 OMS 风控规则(回测场景通常不强制 cash 充足)
#[derive(Debug, Clone)]
pub(crate) struct PortfolioState {
    /// 现金(单 base currency "USDT")
    pub(crate) cash: f64,
    /// 持仓:`symbol -> (qty, avg_price)`
    pub(crate) positions: HashMap<Symbol, (f64, f64)>,
}

impl PortfolioState {
    /// 构造初始 portfolio
    pub(crate) fn new(initial_cash: f64) -> Self {
        Self {
            cash: initial_cash,
            positions: HashMap::new(),
        }
    }

    /// 应用一次 fill(基于 taker_side 调整 cash + position),指定 symbol
    ///
    /// 从 taker 视角记账:
    /// - Buy taker: `cash -= price * qty`;position 按加权平均更新
    /// - Sell taker: `cash += price * qty`;position.qty -= qty(avg_price 不变)
    pub(crate) fn apply_fill(&mut self, fill: &MatchFill, symbol: &Symbol) {
        let price = fill.price.as_f64();
        let qty = fill.quantity.as_f64();
        let turnover = price * qty;

        match fill.taker_side {
            Side::Buy => {
                self.cash -= turnover;
                let entry = self.positions.entry(symbol.clone()).or_insert((0.0, 0.0));
                let (prev_qty, prev_avg) = *entry;
                let new_qty = prev_qty + qty;
                let new_avg = if new_qty == 0.0 {
                    0.0
                } else {
                    (prev_qty * prev_avg + qty * price) / new_qty
                };
                *entry = (new_qty, new_avg);
            }
            Side::Sell => {
                self.cash += turnover;
                if let Some(entry) = self.positions.get_mut(symbol) {
                    entry.0 -= qty;
                }
                // 空仓 sell 不报错(oversell 兜底,允许负 qty)
            }
        }
    }
}

// ==================== 类型转换:辅助函数 ====================

/// 把 "BASE-QUOTE" 拆为 (base, quote) Symbol 对
///
/// 无分隔符时返回 (原symbol, "USDT")。
pub(crate) fn split_symbol(symbol: &Symbol) -> (Symbol, Symbol) {
    let s = symbol.as_str();
    match s.split_once('-') {
        Some((b, q)) => (Symbol::from(b), Symbol::from(q)),
        None => (symbol.clone(), Symbol::from("USDT")),
    }
}

/// `PlaceOrderArgs` 转 `axon_core::Order`(避开 orphan rule)
///
/// OrderKind 翻译:
/// - `Limit` + `price=Some(p)` → `OrderType::Limit { price: Price::from_f64(p) }`
/// - `Limit` + `price=None` → `Err(InvalidArguments)`
/// - `Market` + `price=None` → `OrderType::Market`
/// - `Market` + `price=Some(_)` → 忽略 price,强制 `OrderType::Market`
///
/// Side 翻译:`OrderSide::Buy` → `Side::Buy`,`Sell` → `Side::Sell`。
pub(crate) fn args_to_backtest_order(
    args: &PlaceOrderArgs,
    symbol: Symbol,
    order_id: u64,
) -> Result<Order, TradingError> {
    let side = match args.side {
        OrderSide::Buy => Side::Buy,
        OrderSide::Sell => Side::Sell,
    };
    let order_type = match (args.order_type, args.price) {
        (OrderKind::Limit, Some(p)) => OrderType::Limit {
            price: Price::from_f64(p),
        },
        (OrderKind::Limit, None) => {
            return Err(TradingError::InvalidArguments(
                "Limit order requires price".into(),
            ));
        }
        (OrderKind::Market, _) => OrderType::Market,
    };
    let (base, quote) = split_symbol(&symbol);
    Ok(Order::spot(
        order_id,
        base,
        quote,
        side,
        order_type,
        Quantity::from_f64(args.quantity),
        TimeInForce::GTC,
    ))
}

// ==================== 错误映射:MatchingError -> TradingError ====================

/// `MatchingError` → `TradingError` 统一映射
///
/// 二分语义:
/// - 参数层(用户可修复):`InvalidPrice` / `InvalidQuantity` / `InvalidModification` /
///   `UnsupportedOrderType` / `FokPartialFill` → `InvalidArguments`
/// - 后端层:`OrderNotFound` / `OrderAlreadyFilled` / `OrderBookEmpty` → `Backend`
///
/// **当前状态**:`L1MatchingEngine::submit` 内部消化 validate 错误并返回
/// `SubmitResult::empty`(不向外暴露 `MatchingError`),故本函数当前未被
/// `BacktestTradingBackend::place_order` 调用。保留为公共 API 留待未来 L2/L3
/// 撮合引擎适配时复用(其 `submit` 路径可能返回 `Result`)。
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "L1MatchingEngine::submit 不暴露 MatchingError,本函数为未来 L2/L3 预留"
    )
)]
pub(crate) fn map_backtest_error(e: axon_backtest::matching::MatchingError) -> TradingError {
    use axon_backtest::matching::MatchingError as ME;
    match e {
        ME::InvalidPrice { price } => {
            TradingError::InvalidArguments(format!("invalid price: {}", price))
        }
        ME::InvalidQuantity { quantity } => {
            TradingError::InvalidArguments(format!("invalid quantity: {}", quantity))
        }
        ME::InvalidModification { reason } => {
            TradingError::InvalidArguments(format!("invalid modification: {}", reason))
        }
        ME::UnsupportedOrderType(ty) => {
            TradingError::InvalidArguments(format!("unsupported order type: {}", ty))
        }
        ME::FokPartialFill {
            required,
            available,
        } => TradingError::InvalidArguments(format!(
            "FOK cannot fully fill: required {}, available {}",
            required, available
        )),
        ME::OrderNotFound { order_id } => {
            TradingError::Backend(format!("order not found: {}", order_id))
        }
        ME::OrderAlreadyFilled => TradingError::Backend("order already filled".into()),
        ME::OrderBookEmpty { side } => {
            TradingError::Backend(format!("order book empty: {:?}", side))
        }
    }
}

// ==================== BacktestInner / BacktestTradingBackend ====================

/// Backtest 内部状态(锁内)
///
/// - `engine` 同步撮合,`submit` 需 `&mut self`
/// - `portfolio` 自维护 cash + positions(简单 f64)
/// - `order_id_seq` 原子递增分配 OrderId
/// - symbol 存储在外层 `BacktestTradingBackend` 中,避免每次访问都加锁
pub(crate) struct BacktestInner {
    /// L1 撮合引擎(单 symbol)
    pub(crate) engine: L1MatchingEngine,
    /// 自维护 portfolio(cash + positions)
    pub(crate) portfolio: PortfolioState,
    /// OrderId 分配序列
    pub(crate) order_id_seq: AtomicU64,
}

impl BacktestInner {
    /// 构造 Backtest 内部状态
    pub(crate) fn new(engine: L1MatchingEngine, initial_cash: f64) -> Self {
        Self {
            engine,
            portfolio: PortfolioState::new(initial_cash),
            order_id_seq: AtomicU64::new(1),
        }
    }
}

/// 回测交易后端:包装 `L1MatchingEngine` + 自维护 portfolio,提供 `TradingBackend` 接口。
///
/// **关键设计**:
/// - 内部状态持 `Arc<RwLock<BacktestInner>>`,L1MatchingEngine 同步撮合,
///   write 锁内不跨 await(直接调 matching.submit / portfolio.apply_fill)。
/// - 自维护 `PortfolioState`(不复用 OMS Portfolio),简单 f64 cash + HashMap
///   positions,基于 MatchFill.taker_side 调整。
/// - `place_order` 同步返回 ack,status 取自 `SubmitResult.is_filled` /
///   `is_partially_filled` / 默认("Submitted")。
/// - 假设单 symbol + 单 base currency(USDT);多 symbol 由多个实例拼合。
/// - `OrderId` 内部从 `AtomicU64` 序列分配,字符串化为 `"bt-{n}"`(与 OMS Uuid / Exchange 区分)。
pub struct BacktestTradingBackend {
    inner: Arc<RwLock<BacktestInner>>,
    /// 绑定的交易对(构造后不变,直接存储避免锁依赖)
    symbol: Symbol,
}

impl BacktestTradingBackend {
    /// 创建 `BacktestTradingBackend`,绑 symbol + 初始 cash
    pub fn new(symbol: impl Into<Symbol>, initial_cash: f64) -> Self {
        let sym = symbol.into();
        let engine = L1MatchingEngine::with_symbol(sym.clone());
        let inner = BacktestInner::new(engine, initial_cash);
        Self {
            inner: Arc::new(RwLock::new(inner)),
            symbol: sym,
        }
    }

    /// 获取绑定的交易对
    ///
    /// symbol 在构造后不会改变,直接返回引用,无需加锁。
    pub fn symbol(&self) -> &str {
        self.symbol.as_str()
    }

    /// 获取订单簿快照
    ///
    /// `levels=0` 时自动改为默认值 10
    pub async fn book_snapshot(&self, levels: usize) -> OrderBookSnapshot {
        // levels=0 时使用默认深度 10
        let levels = if levels == 0 { 10 } else { levels };

        let inner = self.inner.read().await;
        let (bids_raw, asks_raw) = inner.engine.depth(levels);

        let bids: Vec<OrderBookLevel> = bids_raw
            .iter()
            .map(|level| OrderBookLevel {
                price: level.price.as_f64(),
                quantity: level.quantity.as_f64(),
            })
            .collect();

        let asks: Vec<OrderBookLevel> = asks_raw
            .iter()
            .map(|level| OrderBookLevel {
                price: level.price.as_f64(),
                quantity: level.quantity.as_f64(),
            })
            .collect();

        OrderBookSnapshot {
            symbol: self.symbol.to_string(),
            bids,
            asks,
            as_of_ms: now_ms_i64(),
        }
    }

    /// 推进到下一 bar(向订单簿注入流动性)
    ///
    /// 清空原有订单簿,按 mid_price ± half_spread * (i+1) 注入 depth_levels 档买卖盘。
    /// 订单 ID 从 order_id_seq 分配,避免与 place_order 冲突。
    ///
    /// # Panics
    /// `half_spread <= 0.0` 时 panic,因为零或负价差会导致买卖盘交叉立即成交。
    pub async fn advance_bar(
        &self,
        mid_price: f64,
        half_spread: f64,
        depth_levels: usize,
        size_per_level: f64,
    ) {
        // 参数校验(在获取锁之前)
        assert!(
            mid_price > 0.0,
            "mid_price must be positive, got {}",
            mid_price
        );
        assert!(
            half_spread > 0.0,
            "half_spread must be positive, got {}",
            half_spread
        );
        assert!(
            size_per_level > 0.0,
            "size_per_level must be positive, got {}",
            size_per_level
        );
        // depth_levels 为 0 时使用默认值 10
        let depth_levels = if depth_levels == 0 { 10 } else { depth_levels };

        let mut inner = self.inner.write().await;
        // 清空原有订单簿,注入新的流动性
        inner.engine = L1MatchingEngine::with_symbol(self.symbol.clone());

        // 复用 split_symbol 解析 base/quote
        let (base, quote) = split_symbol(&self.symbol);

        for i in 0..depth_levels {
            // 从全局序列分配 ID,避免与 place_order 的 ID 冲突
            let ask_id = inner.order_id_seq.fetch_add(1, Ordering::Relaxed);
            let bid_id = inner.order_id_seq.fetch_add(1, Ordering::Relaxed);

            // 卖盘: mid_price + half_spread * (i+1)
            let ask_price = mid_price + half_spread * (i + 1) as f64;
            let ask_order = Order::spot(
                ask_id,
                base.clone(),
                quote.clone(),
                Side::Sell,
                OrderType::Limit {
                    price: Price::from_f64(ask_price),
                },
                Quantity::from_f64(size_per_level),
                TimeInForce::GTC,
            );
            inner.engine.submit(ask_order);

            // 买盘: mid_price - half_spread * (i+1)
            let bid_price = mid_price - half_spread * (i + 1) as f64;
            let bid_order = Order::spot(
                bid_id,
                base.clone(),
                quote.clone(),
                Side::Buy,
                OrderType::Limit {
                    price: Price::from_f64(bid_price),
                },
                Quantity::from_f64(size_per_level),
                TimeInForce::GTC,
            );
            inner.engine.submit(bid_order);
        }
    }
}

#[async_trait]
impl TradingBackend for BacktestTradingBackend {
    fn name(&self) -> &str {
        "backtest"
    }
    async fn place_order(&self, req: &PlaceOrderArgs) -> Result<OrderAck, TradingError> {
        // 1. 校验 symbol 匹配(L1MatchingEngine::validate 会拒,但这里提前 fail-fast)
        let symbol = Symbol::from(req.symbol.clone());

        // 2. 提前校验(在获取锁之前)
        // symbol 匹配校验
        if self.symbol != symbol {
            return Err(TradingError::InvalidArguments(format!(
                "symbol mismatch: backend bound to {}, request {}",
                self.symbol, symbol
            )));
        }
        // quantity 必须大于 0
        if req.quantity <= 0.0 {
            return Err(TradingError::InvalidArguments(format!(
                "quantity must be positive, got {}",
                req.quantity
            )));
        }
        // price 对于限价单必须大于 0
        if req.order_type == OrderKind::Limit && req.price.map(|p| p <= 0.0).unwrap_or(true) {
            return Err(TradingError::InvalidArguments(format!(
                "limit order price must be positive, got {:?}",
                req.price
            )));
        }

        // 3. 写锁内完整提交流程(分配 ID + 转换 + submit + 应用 fills)
        let mut inner = self.inner.write().await;

        // 分配 OrderId
        let order_id = inner.order_id_seq.fetch_add(1, Ordering::Relaxed);

        // PlaceOrderArgs -> axon_core::Order
        let order = args_to_backtest_order(req, symbol.clone(), order_id)?;

        // submit 到 L1MatchingEngine(同步)
        let result = inner.engine.submit(order);

        // 应用 fills 到 portfolio(同步)
        for fill in &result.fills {
            inner.portfolio.apply_fill(fill, &symbol);
        }

        // 决定 status 字符串
        let status_str = if result.is_filled {
            "Filled"
        } else if result.is_partially_filled {
            "PartiallyFilled"
        } else {
            "Submitted"
        };

        Ok(OrderAck {
            order_id: format!("bt-{}", order_id),
            symbol: req.symbol.clone(),
            side: req.side,
            quantity: req.quantity,
            status: OrderStatus(status_str.into()),
            timestamp_ms: now_ms_i64(),
            confirm_token: None,
        })
    }

    async fn get_balance(&self) -> Result<BalanceSnapshot, TradingError> {
        let inner = self.inner.read().await;
        let ts = now_ms_i64();
        Ok(BalanceSnapshot {
            currencies: vec![CurrencyBalance {
                currency: "USDT".into(),
                free: inner.portfolio.cash,
                locked: 0.0,
            }],
            as_of_ms: ts,
        })
    }

    async fn get_positions(&self) -> Result<Vec<PositionSnapshot>, TradingError> {
        let inner = self.inner.read().await;
        let ts = now_ms_i64();

        // 获取当前订单簿参考价格(用于计算浮动盈亏)
        // 买盘用卖一价,卖盘用买一价,无数据时用 0
        let (bids, asks) = inner.engine.depth(1);
        let best_bid = bids.first().map(|b| b.price.as_f64()).unwrap_or(0.0);
        let best_ask = asks.first().map(|a| a.price.as_f64()).unwrap_or(0.0);
        let mid_price = if best_bid > 0.0 && best_ask > 0.0 {
            (best_bid + best_ask) / 2.0
        } else if best_bid > 0.0 {
            best_bid
        } else if best_ask > 0.0 {
            best_ask
        } else {
            0.0
        };

        let positions: Vec<PositionSnapshot> = inner
            .portfolio
            .positions
            .iter()
            .map(|(sym, &(qty, avg))| {
                // 计算浮动盈亏: (当前价 - 开仓均价) * 持仓数量
                let unrealized_pnl = if mid_price > 0.0 && avg > 0.0 {
                    (mid_price - avg) * qty
                } else {
                    0.0
                };
                PositionSnapshot {
                    symbol: sym.to_string(),
                    quantity: qty,
                    entry_price: avg,
                    unrealized_pnl,
                    as_of_ms: ts,
                }
            })
            .collect();
        Ok(positions)
    }
}

// ==================== 单元测试 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trading::types::{OrderKind, OrderSide, TimeInForce};
    use serde_json::json;

    // ── now_ms_i64 ──────────────────────────────────────

    #[test]
    fn now_ms_i64_returns_positive_unix_millis() {
        let ms = now_ms_i64();
        // 2020-01-01 UTC = 1577836800 sec = 1577836800000 ms
        assert!(ms > 1_577_836_800_000, "now_ms_i64 应大于 2020-01-01");
    }

    // ── args_to_backtest_order ─────────────────────────

    fn mk_args(side: OrderSide, qty: f64, price: Option<f64>) -> PlaceOrderArgs {
        PlaceOrderArgs {
            symbol: "BTC-USDT".into(),
            side,
            quantity: qty,
            order_type: OrderKind::Limit,
            price,
            stop_loss: None,
            take_profit: None,
            time_in_force: TimeInForce::GTC,
            extras: json!({}),
        }
    }

    #[test]
    fn args_to_backtest_order_translates_limit_with_price() {
        let args = mk_args(OrderSide::Buy, 0.1, Some(50_000.0));
        let order = args_to_backtest_order(&args, Symbol::from("BTC-USDT"), 1).unwrap();
        assert_eq!(order.id, 1);
        assert_eq!(order.instrument.base().as_str(), "BTC");
        assert_eq!(order.instrument.quote().as_str(), "USDT");
        assert_eq!(order.side, Side::Buy);
        assert!(matches!(
            order.order_type,
            OrderType::Limit { price } if price == Price::from_f64(50_000.0)
        ));
        assert_eq!(order.quantity, Quantity::from_f64(0.1));
    }

    #[test]
    fn args_to_backtest_order_translates_market_drops_price() {
        let mut args = mk_args(OrderSide::Sell, 0.5, Some(50_000.0));
        args.order_type = OrderKind::Market;
        let order = args_to_backtest_order(&args, Symbol::from("BTC-USDT"), 2).unwrap();
        assert_eq!(order.side, Side::Sell);
        // price 字段被忽略,OrderType 是 Market
        assert!(matches!(order.order_type, OrderType::Market));
    }

    #[test]
    fn args_to_backtest_order_rejects_limit_without_price() {
        let args = mk_args(OrderSide::Buy, 0.1, None);
        let result = args_to_backtest_order(&args, Symbol::from("BTC-USDT"), 1);
        match result {
            Err(TradingError::InvalidArguments(msg)) => {
                assert!(msg.contains("Limit order requires price"));
            }
            other => panic!("expected InvalidArguments, got {:?}", other),
        }
    }

    #[test]
    fn args_to_backtest_order_assigns_id_and_symbol() {
        let args = mk_args(OrderSide::Buy, 1.0, Some(100.0));
        let order = args_to_backtest_order(&args, Symbol::from("ETH-USDT"), 42).unwrap();
        assert_eq!(order.id, 42);
        assert_eq!(order.instrument.base().as_str(), "ETH");
        assert_eq!(order.instrument.quote().as_str(), "USDT");
    }

    // ── PortfolioState::apply_fill ────────────────────

    fn dummy_buy_fill(price: f64, qty: f64) -> MatchFill {
        MatchFill {
            fill_id: 1,
            taker_order_id: 1,
            maker_order_id: 2,
            price: Price::from_f64(price),
            quantity: Quantity::from_f64(qty),
            taker_side: Side::Buy,
            timestamp: axon_core::time::Timestamp::from_nanos(0),
        }
    }

    fn dummy_sell_fill(price: f64, qty: f64) -> MatchFill {
        MatchFill {
            fill_id: 2,
            taker_order_id: 1,
            maker_order_id: 2,
            price: Price::from_f64(price),
            quantity: Quantity::from_f64(qty),
            taker_side: Side::Sell,
            timestamp: axon_core::time::Timestamp::from_nanos(0),
        }
    }

    #[test]
    fn apply_fill_buy_creates_position_with_avg_price() {
        let mut p = PortfolioState::new(10_000.0);
        let sym = Symbol::from("BTC-USDT");
        p.apply_fill(&dummy_buy_fill(50_000.0, 0.1), &sym);
        // cash: 10000 - 0.1 * 50000 = 5000
        assert!((p.cash - 5_000.0).abs() < 1e-9);
        let (qty, avg) = p.positions.get(&sym).copied().unwrap();
        assert!((qty - 0.1).abs() < 1e-9);
        assert!((avg - 50_000.0).abs() < 1e-9);
    }

    #[test]
    fn apply_fill_buy_increases_position_with_weighted_avg() {
        let mut p = PortfolioState::new(100_000.0);
        let sym = Symbol::from("BTC-USDT");
        // 第一次:0.1 @ 50000 (成本 5000,余 95000)
        p.apply_fill(&dummy_buy_fill(50_000.0, 0.1), &sym);
        // 第二次:0.2 @ 60000 (成本 12000,余 83000)
        p.apply_fill(&dummy_buy_fill(60_000.0, 0.2), &sym);
        // qty: 0.3
        let (qty, _) = p.positions.get(&sym).copied().unwrap();
        assert!((qty - 0.3).abs() < 1e-9);
        // avg: (0.1*50000 + 0.2*60000) / 0.3 = 56666.666...
        let (_, avg) = p.positions.get(&sym).copied().unwrap();
        assert!(
            (avg - 56_666.666_666_666_664).abs() < 1.0,
            "actual avg={}",
            avg
        );
    }

    #[test]
    fn apply_fill_sell_reduces_position_keeps_avg() {
        let mut p = PortfolioState::new(100_000.0);
        let sym = Symbol::from("BTC-USDT");
        p.apply_fill(&dummy_buy_fill(50_000.0, 0.5), &sym);
        let avg_before = p.positions.get(&sym).copied().unwrap().1;
        p.apply_fill(&dummy_sell_fill(60_000.0, 0.2), &sym);
        let (qty, avg) = p.positions.get(&sym).copied().unwrap();
        assert!((qty - 0.3).abs() < 1e-9, "qty 应减到 0.3,实际 {}", qty);
        assert!(
            (avg - avg_before).abs() < 1e-9,
            "avg_price 不变,before={}, after={}",
            avg_before,
            avg
        );
    }

    #[test]
    fn apply_fill_buy_decreases_cash() {
        let mut p = PortfolioState::new(10_000.0);
        p.apply_fill(&dummy_buy_fill(100.0, 1.0), &Symbol::from("BTC-USDT"));
        assert!((p.cash - 9_900.0).abs() < 1e-9);
    }

    #[test]
    fn apply_fill_sell_increases_cash() {
        let mut p = PortfolioState::new(0.0);
        // 先 buy 1.0
        p.apply_fill(&dummy_buy_fill(100.0, 1.0), &Symbol::from("BTC-USDT"));
        assert!((p.cash - (-100.0)).abs() < 1e-9, "buy 后 cash={}", p.cash);
        // 再 sell 0.5
        p.apply_fill(&dummy_sell_fill(100.0, 0.5), &Symbol::from("BTC-USDT"));
        assert!((p.cash - (-50.0)).abs() < 1e-9, "sell 后 cash={}", p.cash);
    }

    #[test]
    fn apply_fill_empty_position_sell_no_effect_on_position() {
        // 注:oversell 兜底,空仓 sell 不报错,但 positions 表无 entry
        let mut p = PortfolioState::new(0.0);
        let sym = Symbol::from("BTC-USDT");
        p.apply_fill(&dummy_sell_fill(100.0, 0.1), &sym);
        // cash 仍 + turnover
        assert!((p.cash - 10.0).abs() < 1e-9);
        // positions 仍空(没有 entry 被创建)
        assert!(!p.positions.contains_key(&sym));
    }

    // ── map_backtest_error ─────────────────────────────

    #[test]
    fn map_backtest_error_invalid_price() {
        let e = axon_backtest::matching::MatchingError::InvalidPrice {
            price: Price::from_f64(0.0),
        };
        match map_backtest_error(e) {
            TradingError::InvalidArguments(msg) => assert!(msg.contains("invalid price")),
            other => panic!("expected InvalidArguments, got {:?}", other),
        }
    }

    #[test]
    fn map_backtest_error_invalid_quantity() {
        let e = axon_backtest::matching::MatchingError::InvalidQuantity {
            quantity: Quantity::from_f64(0.0),
        };
        match map_backtest_error(e) {
            TradingError::InvalidArguments(msg) => assert!(msg.contains("invalid quantity")),
            other => panic!("expected InvalidArguments, got {:?}", other),
        }
    }

    #[test]
    fn map_backtest_error_invalid_modification() {
        let e = axon_backtest::matching::MatchingError::InvalidModification {
            reason: "test reason".into(),
        };
        match map_backtest_error(e) {
            TradingError::InvalidArguments(msg) => assert!(msg.contains("test reason")),
            other => panic!("expected InvalidArguments, got {:?}", other),
        }
    }

    #[test]
    fn map_backtest_error_order_not_found() {
        let e = axon_backtest::matching::MatchingError::OrderNotFound { order_id: 42 };
        match map_backtest_error(e) {
            TradingError::Backend(msg) => assert!(msg.contains("42")),
            other => panic!("expected Backend, got {:?}", other),
        }
    }

    #[test]
    fn map_backtest_error_unsupported_order_type() {
        let e = axon_backtest::matching::MatchingError::UnsupportedOrderType("Stop".into());
        match map_backtest_error(e) {
            TradingError::InvalidArguments(msg) => assert!(msg.contains("unsupported")),
            other => panic!("expected InvalidArguments, got {:?}", other),
        }
    }

    #[test]
    fn map_backtest_error_order_already_filled() {
        let e = axon_backtest::matching::MatchingError::OrderAlreadyFilled;
        match map_backtest_error(e) {
            TradingError::Backend(msg) => assert!(msg.contains("already filled")),
            other => panic!("expected Backend, got {:?}", other),
        }
    }

    #[test]
    fn map_backtest_error_order_book_empty() {
        let e = axon_backtest::matching::MatchingError::OrderBookEmpty { side: Side::Buy };
        match map_backtest_error(e) {
            TradingError::Backend(msg) => assert!(msg.contains("order book empty")),
            other => panic!("expected Backend, got {:?}", other),
        }
    }

    // ==================== BacktestTradingBackend 集成测试 ====================
    //
    // 已知简化语义(详见模块 doc + design spec):
    // - 挂单不预先锁定 cash(回测场景容忍)
    // - portfolio 仅在 taker 撮合后调 apply_fill
    //   - buy taker 撮合 → cash -= price*qty, positions += qty
    //   - sell taker 撮合 → cash += price*qty, positions -= qty
    // - buy 单作为 maker 被 sell taker 撮合时,buy maker 那侧 cash 减不记录
    //   (L1 撮合引擎不感知 portfolio)

    /// 构造 BacktestTradingBackend(sym + initial cash)
    fn make_backend(cash: f64) -> BacktestTradingBackend {
        BacktestTradingBackend::new("BTC-USDT", cash)
    }

    /// 验证 backend 的 USDT cash
    async fn assert_usdt_cash(backend: &BacktestTradingBackend, expected: f64) {
        let balance = backend.get_balance().await.unwrap();
        assert_eq!(balance.currencies.len(), 1);
        assert_eq!(balance.currencies[0].currency, "USDT");
        assert!(
            (balance.currencies[0].free - expected).abs() < 1e-6,
            "USDT cash 应≈{},实际 {}",
            expected,
            balance.currencies[0].free
        );
    }

    #[tokio::test]
    async fn backtest_backend_new_initial_state() {
        let backend = make_backend(100_000.0);
        assert_usdt_cash(&backend, 100_000.0).await;
        let positions = backend.get_positions().await.unwrap();
        assert!(positions.is_empty());
    }

    #[tokio::test]
    async fn place_order_limit_buy_no_maker_status_submitted_cash_unchanged() {
        let backend = make_backend(100_000.0);
        // 空 ask book,buy 0.1 @ 50000 进 bids
        let ack = backend
            .place_order(&mk_args(OrderSide::Buy, 0.1, Some(50_000.0)))
            .await
            .unwrap();
        assert_eq!(ack.status.0, "Submitted");
        assert_eq!(ack.order_id, "bt-1");
        // 挂单阶段不锁 cash
        assert_usdt_cash(&backend, 100_000.0).await;
        // 持仓仍空(buy maker 端无 fill)
        let positions = backend.get_positions().await.unwrap();
        assert!(positions.is_empty());
    }

    #[tokio::test]
    async fn place_order_buy_taker_fills_against_sell_maker_updates_portfolio() {
        let backend = make_backend(100_000.0);
        // 1. sell 0.1 @ 50000 进 asks book(maker)
        backend
            .place_order(&mk_args(OrderSide::Sell, 0.1, Some(50_000.0)))
            .await
            .unwrap();
        // 2. buy 0.1 @ 50000 taker 撮合 sell maker
        let ack = backend
            .place_order(&mk_args(OrderSide::Buy, 0.1, Some(50_000.0)))
            .await
            .unwrap();
        assert_eq!(ack.status.0, "Filled");
        // Buy taker 撮合 → cash -= 0.1*50000 = 5000
        assert_usdt_cash(&backend, 95_000.0).await;
        // 持仓:Buy taker 买 0.1 @ 50000
        let positions = backend.get_positions().await.unwrap();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].symbol, "BTC-USDT");
        assert!((positions[0].quantity - 0.1).abs() < 1e-9);
        assert!((positions[0].entry_price - 50_000.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn place_order_sell_taker_fills_against_buy_maker_only_taker_side_applied() {
        let backend = make_backend(100_000.0);
        // 1. buy 0.1 @ 50000 进 bids book(maker)
        backend
            .place_order(&mk_args(OrderSide::Buy, 0.1, Some(50_000.0)))
            .await
            .unwrap();
        // 2. sell 0.1 @ 50000 taker 撮合 buy maker
        let ack = backend
            .place_order(&mk_args(OrderSide::Sell, 0.1, Some(50_000.0)))
            .await
            .unwrap();
        assert_eq!(ack.status.0, "Filled");
        // Sell taker 撮合 → cash += 0.1*50000 = 5000
        // 注:buy maker 那侧 cash 减未记录(已知 L1 简化)
        assert_usdt_cash(&backend, 105_000.0).await;
        // 持仓:buy 单进 book 时未调 apply_fill,positions 表无 entry
        // sell taker 调 apply_fill 时,空仓 sell 兜底,positions 仍空
        let positions = backend.get_positions().await.unwrap();
        assert!(positions.is_empty());
    }

    #[tokio::test]
    async fn place_order_unique_order_ids_increment() {
        let backend = make_backend(100_000.0);
        let ack1 = backend
            .place_order(&mk_args(OrderSide::Buy, 0.1, Some(50_000.0)))
            .await
            .unwrap();
        let ack2 = backend
            .place_order(&mk_args(OrderSide::Buy, 0.2, Some(51_000.0)))
            .await
            .unwrap();
        let ack3 = backend
            .place_order(&mk_args(OrderSide::Buy, 0.3, Some(52_000.0)))
            .await
            .unwrap();
        assert_eq!(ack1.order_id, "bt-1");
        assert_eq!(ack2.order_id, "bt-2");
        assert_eq!(ack3.order_id, "bt-3");
    }

    #[tokio::test]
    async fn place_order_rejects_limit_without_price() {
        let backend = make_backend(100_000.0);
        let result = backend
            .place_order(&mk_args(OrderSide::Buy, 0.1, None))
            .await;
        match result {
            Err(TradingError::InvalidArguments(msg)) => {
                assert!(msg.contains("Limit order requires price"));
            }
            other => panic!("expected InvalidArguments, got {:?}", other),
        }
        // 失败时 cash / positions 不变
        assert_usdt_cash(&backend, 100_000.0).await;
        let positions = backend.get_positions().await.unwrap();
        assert!(positions.is_empty());
    }

    #[tokio::test]
    async fn place_order_rejects_symbol_mismatch() {
        let backend = make_backend(100_000.0);
        let args = PlaceOrderArgs {
            symbol: "ETH-USDT".into(),
            side: OrderSide::Buy,
            quantity: 0.1,
            order_type: OrderKind::Limit,
            price: Some(2_000.0),
            stop_loss: None,
            take_profit: None,
            time_in_force: TimeInForce::GTC,
            extras: json!({}),
        };
        let result = backend.place_order(&args).await;
        match result {
            Err(TradingError::InvalidArguments(msg)) => {
                assert!(
                    msg.contains("symbol mismatch"),
                    "错误信息应含 'symbol mismatch',实际: {}",
                    msg
                );
            }
            other => panic!("expected InvalidArguments, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn place_order_market_buy_no_liquidity_status_submitted() {
        // Market 单无 maker 时,L1 内部 validate 通过但 ask book 空,
        // taker 进入 book 然后无 fill,is_filled=false,remaining=qty
        let backend = make_backend(100_000.0);
        let args = PlaceOrderArgs {
            symbol: "BTC-USDT".into(),
            side: OrderSide::Buy,
            quantity: 0.1,
            order_type: OrderKind::Market,
            price: None,
            stop_loss: None,
            take_profit: None,
            time_in_force: TimeInForce::GTC,
            extras: json!({}),
        };
        let ack = backend.place_order(&args).await.unwrap();
        // L1 Market 单无 maker 不会填充
        assert_eq!(ack.status.0, "Submitted");
        assert_usdt_cash(&backend, 100_000.0).await;
        let positions = backend.get_positions().await.unwrap();
        assert!(positions.is_empty());
    }

    #[tokio::test]
    async fn get_portfolio_default_impl_concurrent() {
        // 验证 trait 默认 get_portfolio 走 tokio::try_join 并发拉取
        let backend = make_backend(50_000.0);
        let snap = backend.get_portfolio().await.unwrap();
        assert_eq!(snap.balance.currencies.len(), 1);
        assert!((snap.balance.currencies[0].free - 50_000.0).abs() < 1e-9);
        assert!(snap.positions.is_empty());
    }

    #[tokio::test]
    async fn get_balance_and_get_positions_consistent_with_fills() {
        // 端到端:buy taker 撮合 → 验证 balance + positions 一致
        let backend = make_backend(200_000.0);
        // 1. 下 2 个 sell maker
        backend
            .place_order(&mk_args(OrderSide::Sell, 0.1, Some(50_000.0)))
            .await
            .unwrap();
        backend
            .place_order(&mk_args(OrderSide::Sell, 0.2, Some(60_000.0)))
            .await
            .unwrap();
        // 2. 下 1 个 buy taker 撮合第一个 sell maker
        backend
            .place_order(&mk_args(OrderSide::Buy, 0.1, Some(50_000.0)))
            .await
            .unwrap();
        let balance = backend.get_balance().await.unwrap();
        // buy taker 减 cash:0.1*50000=5000
        assert!((balance.currencies[0].free - 195_000.0).abs() < 1e-9);
        let positions = backend.get_positions().await.unwrap();
        assert_eq!(positions.len(), 1);
        assert!((positions[0].quantity - 0.1).abs() < 1e-9);
        assert!((positions[0].entry_price - 50_000.0).abs() < 1e-9);
    }
}
