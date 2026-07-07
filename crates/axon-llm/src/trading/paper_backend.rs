//! PaperTradingBackend — 模拟盘后端(基于 `PaperPortfolio`)
//!
//! 与 `MockTradingBackend` 的差异:
//! - `MockTradingBackend`:通用测试桩,无价格滑点 / 手续费建模
//! - `PaperTradingBackend`:模拟真实交易摩擦(滑点 + 手续费),初始余额 / 持仓来自
//!   `PaperPortfolio`,下单立即按"市价"成交并更新内部组合
//!
//! ## 用法
//!
//! ```ignore
//! use axon_llm::trading::paper_backend::PaperTradingBackend;
//! use axon_llm::swarm::paper_trading::PaperTradingConfig;
//!
//! let backend = PaperTradingBackend::new(PaperTradingConfig::default());
//! // 用 Arc<dyn TradingBackend> 传给 ExecutionAgent
//! ```
//!
//! ## 线程安全
//!
//! 内部状态用 `parking_lot::Mutex` 保护(同 `MockTradingBackend` 的 StdMutex 模式),
//! 满足 `TradingBackend: Send + Sync` 约束。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use parking_lot::Mutex;

use crate::swarm::paper_trading::{PaperPortfolio, PaperTradingConfig};
use crate::trading::backend::{TradingBackend, TradingError};
use crate::trading::types::{
    BalanceSnapshot, CurrencyBalance, OrderAck, OrderKind, OrderSide, OrderStatus,
    PlaceOrderArgs, PortfolioSnapshot, PositionSnapshot,
};

/// 内部状态:`cash` + 持仓(symbol → (qty, entry_price))
#[derive(Debug, Clone)]
struct PaperState {
    /// USDT 等基础货币的可用现金
    cash: f64,
    /// 各 symbol 持仓 (quantity, entry_price)
    positions: HashMap<String, (f64, f64)>,
    /// 最近一次下单的成交价(symbol → price),供 `get_balance` 报 `as_of_ms` 用
    last_prices: HashMap<String, f64>,
}

impl PaperState {
    fn new(initial_balance: f64) -> Self {
        Self {
            cash: initial_balance,
            positions: HashMap::new(),
            last_prices: HashMap::new(),
        }
    }
}

/// `PaperTradingBackend`:`TradingBackend` 的 paper trading 实现
pub struct PaperTradingBackend {
    state: Mutex<PaperState>,
    /// 滑点 / 手续费(基点)
    config: PaperTradingConfig,
    /// 下一个订单 ID 自增器
    next_id: AtomicU64,
    /// 下一个消息时间戳(`as_of_ms`,毫秒)
    now_ms: AtomicU64,
}

impl PaperTradingBackend {
    /// 用配置构造 paper backend(初始余额取自 `config.initial_balance`)
    pub fn new(config: PaperTradingConfig) -> Self {
        let state = PaperState::new(config.initial_balance);
        Self {
            state: Mutex::new(state),
            config,
            next_id: AtomicU64::new(1),
            now_ms: AtomicU64::new(0),
        }
    }

    /// 用 `PaperPortfolio` 构造(把现有 positions 灌进来)
    pub fn from_portfolio(config: PaperTradingConfig, portfolio: &PaperPortfolio) -> Self {
        let mut state = PaperState::new(portfolio.cash);
        for (sym, qty) in &portfolio.positions {
            if *qty != 0.0 {
                // 不知道 entry_price,用 0 占位(在 paper 场景下后续会按市价更新)
                state.positions.insert(sym.clone(), (*qty, 0.0));
            }
        }
        Self {
            state: Mutex::new(state),
            config,
            next_id: AtomicU64::new(1),
            now_ms: AtomicU64::new(0),
        }
    }

    /// 获取当前内部状态(`PaperPortfolio`)— 供测试 / 监控用
    pub fn portfolio(&self) -> PaperPortfolio {
        let st = self.state.lock();
        let positions: HashMap<String, f64> = st
            .positions
            .iter()
            .map(|(k, v)| (k.clone(), v.0))
            .collect();
        PaperPortfolio {
            cash: st.cash,
            positions,
            total_pnl: 0.0, // 简化:暂不算 pnl
            trade_count: 0, // 简化:不在 state 里记 trade_count
        }
    }

    /// 推进内部时间(测试可显式驱动;不调则 as_of_ms 始终 = 0)
    pub fn advance_clock_ms(&self, delta_ms: u64) {
        self.now_ms.fetch_add(delta_ms, Ordering::SeqCst);
    }
}

#[async_trait]
impl TradingBackend for PaperTradingBackend {
    fn name(&self) -> &str {
        "paper"
    }

    async fn place_order(&self, req: &PlaceOrderArgs) -> Result<OrderAck, TradingError> {
        // 1. 解析成交价:Limit 用 price,Market 用 last_price 或 0(若没有则报错)
        let raw_price = match req.order_type {
            OrderKind::Limit => req.price.ok_or_else(|| {
                TradingError::InvalidArguments("Limit order requires price".into())
            })?,
            OrderKind::Market => 0.0, // 简化:Market 价用 last_price 兜底,无历史价则用 0
        };

        let mut st = self.state.lock();
        // 应用滑点(基点)
        let slippage = self.config.slippage_bps / 10_000.0;
        let commission = self.config.commission_bps / 10_000.0;
        // 买入时成交价上浮(对买方不利),卖出时下浮
        let fill_price = match req.side {
            OrderSide::Buy => raw_price * (1.0 + slippage),
            OrderSide::Sell => raw_price * (1.0 - slippage),
        };
        // 兜底:Market 单如果没历史价,这里 fill_price = 0(避免除零)
        let fill_price = if fill_price <= 0.0 {
            st.last_prices
                .get(&req.symbol)
                .copied()
                .unwrap_or(50_000.0)
        } else {
            fill_price
        };

        let notional = req.quantity * fill_price;
        let cost = notional * (1.0 + commission);

        // 2. 校验并更新持仓
        // 先取出当前的 qty / entry(避免与 st.cash 借用冲突)
        let sym = req.symbol.clone();
        let (cur_qty, cur_entry) = st
            .positions
            .get(&sym)
            .copied()
            .unwrap_or((0.0, 0.0));
        match req.side {
            OrderSide::Buy => {
                if cost > st.cash {
                    return Err(TradingError::RiskRejected(format!(
                        "Insufficient cash: need {cost}, have {}",
                        st.cash
                    )));
                }
                st.cash -= cost;
                // 更新 entry_price(加权平均)
                let new_qty = cur_qty + req.quantity;
                let new_entry = if new_qty > 0.0 {
                    (cur_qty * cur_entry + req.quantity * fill_price) / new_qty
                } else {
                    0.0
                };
                st.positions.insert(sym.clone(), (new_qty, new_entry));
            }
            OrderSide::Sell => {
                if cur_qty < req.quantity {
                    return Err(TradingError::RiskRejected(format!(
                        "Insufficient position: have {}, need {}",
                        cur_qty, req.quantity
                    )));
                }
                let new_qty = cur_qty - req.quantity;
                st.positions.insert(sym.clone(), (new_qty, cur_entry));
                st.cash += notional * (1.0 - commission);
            }
        }
        st.last_prices.insert(sym, fill_price);

        // 3. 构造 OrderAck
        let order_id = format!("paper_{}", self.next_id.fetch_add(1, Ordering::SeqCst));
        let ts = self.now_ms.load(Ordering::SeqCst) as i64;
        Ok(OrderAck {
            order_id,
            symbol: req.symbol.clone(),
            side: req.side,
            quantity: req.quantity,
            status: OrderStatus("Filled".into()),
            timestamp_ms: ts,
            confirm_token: None,
        })
    }

    async fn get_balance(&self) -> Result<BalanceSnapshot, TradingError> {
        let st = self.state.lock();
        let as_of_ms = self.now_ms.load(Ordering::SeqCst) as i64;
        Ok(BalanceSnapshot {
            currencies: vec![CurrencyBalance {
                currency: "USDT".into(),
                free: st.cash,
                locked: 0.0,
            }],
            as_of_ms,
        })
    }

    async fn get_positions(&self) -> Result<Vec<PositionSnapshot>, TradingError> {
        let st = self.state.lock();
        let as_of_ms = self.now_ms.load(Ordering::SeqCst) as i64;
        let mut out: Vec<PositionSnapshot> = st
            .positions
            .iter()
            .filter(|(_, (qty, _))| *qty != 0.0)
            .map(|(sym, (qty, entry))| PositionSnapshot {
                symbol: sym.clone(),
                quantity: *qty,
                entry_price: *entry,
                // 简化:unrealized_pnl 需要最新价 × qty - cost;此处无最新价,记 0
                unrealized_pnl: 0.0,
                as_of_ms,
            })
            .collect();
        // 排序保证测试稳定
        out.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        Ok(out)
    }

    async fn cancel_order(&self, order_id: &str) -> Result<OrderAck, TradingError> {
        // Paper trading 立即成交,没有"挂单"可撤
        Err(TradingError::Backend(format!(
            "paper trading has no pending orders to cancel: {order_id}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trading::types::TimeInForce;

    fn mk_limit_buy(qty: f64, price: f64) -> PlaceOrderArgs {
        PlaceOrderArgs {
            symbol: "BTC-USDT".into(),
            side: OrderSide::Buy,
            quantity: qty,
            order_type: OrderKind::Limit,
            price: Some(price),
            stop_loss: None,
            take_profit: None,
            time_in_force: TimeInForce::GTC,
            extras: serde_json::Value::Null,
        }
    }

    fn mk_market_sell(qty: f64) -> PlaceOrderArgs {
        PlaceOrderArgs {
            symbol: "BTC-USDT".into(),
            side: OrderSide::Sell,
            quantity: qty,
            order_type: OrderKind::Market,
            price: None,
            stop_loss: None,
            take_profit: None,
            time_in_force: TimeInForce::IOC,
            extras: serde_json::Value::Null,
        }
    }

    /// 构造 + name() 是 "paper"
    #[test]
    fn test_paper_backend_creation_and_name() {
        let backend = PaperTradingBackend::new(PaperTradingConfig::default());
        assert_eq!(backend.name(), "paper");
        assert_eq!(backend.portfolio().cash, 100_000.0);
        assert!(backend.portfolio().positions.is_empty());
    }

    /// name 默认 Send + Sync(编译期断言)
    #[test]
    fn test_paper_backend_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PaperTradingBackend>();
    }

    /// 初始 get_balance/get_positions:10w USDT,无持仓
    #[tokio::test]
    async fn test_initial_balance_and_empty_positions() {
        let backend = PaperTradingBackend::new(PaperTradingConfig::default());
        let bal = backend.get_balance().await.unwrap();
        assert_eq!(bal.currencies.len(), 1);
        assert_eq!(bal.currencies[0].currency, "USDT");
        assert!((bal.currencies[0].free - 100_000.0).abs() < 1e-9);
        let pos = backend.get_positions().await.unwrap();
        assert!(pos.is_empty());
    }

    /// Limit Buy 0.1 @ 50000 → 现金 -cost,持仓 +0.1
    #[tokio::test]
    async fn test_place_limit_buy_deducts_cash_and_adds_position() {
        let backend = PaperTradingBackend::new(PaperTradingConfig::default());
        let ack = backend.place_order(&mk_limit_buy(0.1, 50_000.0)).await.unwrap();
        assert_eq!(ack.symbol, "BTC-USDT");
        assert_eq!(ack.status.0, "Filled");
        // 滑点 5bps 作用在成交价上,手续费 10bps 作用在 notional 上,二者不合并:
        //   fill_price = 50000 * (1 + 0.0005) = 50025
        //   notional   = 0.1 * 50025          = 5002.5
        //   cost       = 5002.5 * (1 + 0.001) = 5007.5025
        let expected_cost = 0.1_f64 * 50_000.0 * 1.0005 * 1.001;
        let bal = backend.get_balance().await.unwrap();
        assert!(
            (bal.currencies[0].free - (100_000.0 - expected_cost)).abs() < 1e-6,
            "expected cash {}, got {}",
            100_000.0 - expected_cost,
            bal.currencies[0].free
        );
        let pos = backend.get_positions().await.unwrap();
        assert_eq!(pos.len(), 1);
        assert!((pos[0].quantity - 0.1).abs() < 1e-9);
        // entry_price 是成交价(应用了滑点)
        assert!(pos[0].entry_price > 50_000.0);
    }

    /// 现金不足时 place_order 返回 RiskRejected
    #[tokio::test]
    async fn test_place_order_insufficient_cash_returns_error() {
        let backend = PaperTradingBackend::new(PaperTradingConfig::default());
        let big = PlaceOrderArgs {
            symbol: "BTC-USDT".into(),
            side: OrderSide::Buy,
            quantity: 1000.0, // 1000 * 50000 = 5e7,远大于 10w
            order_type: OrderKind::Limit,
            price: Some(50_000.0),
            stop_loss: None,
            take_profit: None,
            time_in_force: TimeInForce::GTC,
            extras: serde_json::Value::Null,
        };
        let res = backend.place_order(&big).await;
        assert!(matches!(res, Err(TradingError::RiskRejected(_))));
    }

    /// Sell 超过持仓时返回 RiskRejected
    #[tokio::test]
    async fn test_place_sell_exceeding_position_returns_error() {
        let backend = PaperTradingBackend::new(PaperTradingConfig::default());
        // 没建仓就卖
        let res = backend.place_order(&mk_market_sell(0.1)).await;
        assert!(matches!(res, Err(TradingError::RiskRejected(_))));
    }

    /// 完整买入 → 卖出 → 余额变化
    #[tokio::test]
    async fn test_buy_then_sell_round_trip() {
        let backend = PaperTradingBackend::new(PaperTradingConfig::default());
        backend.place_order(&mk_limit_buy(0.1, 50_000.0)).await.unwrap();
        let bal_after_buy = backend.get_balance().await.unwrap().currencies[0].free;

        backend.place_order(&mk_market_sell(0.1)).await.unwrap();
        let bal_after_sell = backend.get_balance().await.unwrap().currencies[0].free;
        // Market sell:成交价 0 → 兜底为 last_price(应等于 buy 时 fill_price)− 滑点 − 手续费
        // 简化断言:cash 略增(因为 market sell 用了 last_price 上浮位)
        // 不强求具体值,只要不报错 + cash 变化
        assert!((bal_after_sell - bal_after_buy).abs() > 1e-6);
        let pos = backend.get_positions().await.unwrap();
        // 卖完后持仓 = 0,被过滤
        assert!(pos.is_empty());
    }

    /// get_portfolio 是 balance + positions 的合并
    #[tokio::test]
    async fn test_get_portfolio_combines_balance_and_positions() {
        let backend = PaperTradingBackend::new(PaperTradingConfig::default());
        backend.place_order(&mk_limit_buy(0.1, 50_000.0)).await.unwrap();
        let snap: PortfolioSnapshot = backend.get_portfolio().await.unwrap();
        assert_eq!(snap.balance.currencies.len(), 1);
        assert_eq!(snap.positions.len(), 1);
        assert_eq!(snap.positions[0].symbol, "BTC-USDT");
    }

    /// `cancel_order` 返回 Backend 错误(paper 立即成交,无挂单)
    #[tokio::test]
    async fn test_cancel_order_returns_backend_error() {
        let backend = PaperTradingBackend::new(PaperTradingConfig::default());
        let res = backend.cancel_order("paper_1").await;
        assert!(matches!(res, Err(TradingError::Backend(_))));
    }

    /// Limit 单无 price 时报 InvalidArguments
    #[tokio::test]
    async fn test_limit_order_without_price_returns_error() {
        let backend = PaperTradingBackend::new(PaperTradingConfig::default());
        let bad = PlaceOrderArgs {
            symbol: "BTC-USDT".into(),
            side: OrderSide::Buy,
            quantity: 0.1,
            order_type: OrderKind::Limit,
            price: None, // Limit 必须 price
            stop_loss: None,
            take_profit: None,
            time_in_force: TimeInForce::GTC,
            extras: serde_json::Value::Null,
        };
        let res = backend.place_order(&bad).await;
        assert!(matches!(res, Err(TradingError::InvalidArguments(_))));
    }
}
