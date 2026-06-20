//! 投资组合主结构

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::currency::Currency;
use super::error::{PortfolioError, PortfolioResult};
use super::position::Position;
use super::snapshot::PortfolioSnapshot;
use super::trade_record::TradeRecord;
use crate::event::FillEvent;
use crate::market::{Side, Trade};
use crate::time::Timestamp;
use crate::types::{Price, Quantity, Symbol};

/// 投资组合
///
/// 跟踪多币种现金、多资产持仓、交易历史、累计盈亏。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Portfolio {
    /// 多币种现金余额（单位：f64 × 1e6）
    cash: HashMap<Currency, i64>,
    /// 持仓映射
    positions: HashMap<Symbol, Position>,
    /// 交易历史
    trades: Vec<TradeRecord>,
    /// 已实现盈亏累计
    total_realized_pnl: i64,
    /// 佣金费率（单位：f64 × 1e6）
    commission_rate: i64,
    /// 基准货币
    base_currency: Currency,
    /// 默认符号（用于 `update_with_fill`，无 symbol 信息时使用）
    default_symbol: Option<Symbol>,
}

impl Portfolio {
    /// 创建空投资组合
    pub fn new(base_currency: Currency, commission_rate: f64) -> Self {
        Self {
            cash: HashMap::new(),
            positions: HashMap::new(),
            trades: Vec::new(),
            total_realized_pnl: 0,
            commission_rate: (commission_rate * 1_000_000.0) as i64,
            base_currency,
            default_symbol: None,
        }
    }

    /// 创建带默认符号的投资组合
    pub fn with_default_symbol(
        base_currency: Currency,
        commission_rate: f64,
        default_symbol: Symbol,
    ) -> Self {
        Self {
            default_symbol: Some(default_symbol),
            ..Self::new(base_currency, commission_rate)
        }
    }

    /// 存入现金
    pub fn deposit(&mut self, currency: Currency, amount: f64) {
        let amt_i = (amount * 1_000_000.0) as i64;
        *self.cash.entry(currency).or_insert(0) += amt_i;
    }

    /// 取出现金
    pub fn withdraw(&mut self, currency: Currency, amount: f64) -> PortfolioResult<()> {
        let amt_i = (amount * 1_000_000.0) as i64;
        let balance = self.cash.get(&currency).copied().unwrap_or(0);
        if balance < amt_i {
            return Err(PortfolioError::InsufficientCash {
                currency,
                required: amt_i,
                available: balance,
            });
        }
        *self.cash.entry(currency).or_insert(0) -= amt_i;
        Ok(())
    }

    /// 获取现金余额
    pub fn cash_balance(&self, currency: Currency) -> f64 {
        self.cash.get(&currency).copied().unwrap_or(0) as f64 / 1_000_000.0
    }

    /// 获取基准货币现金余额
    pub fn base_cash(&self) -> f64 {
        self.cash_balance(self.base_currency)
    }

    /// 获取持仓
    pub fn position(&self, symbol: &Symbol) -> Option<&Position> {
        self.positions.get(symbol)
    }

    /// 获取所有持仓
    pub fn positions(&self) -> &HashMap<Symbol, Position> {
        &self.positions
    }

    /// 更新市场价格
    pub fn update_market_price(&mut self, symbol: &Symbol, price: Price) {
        if let Some(pos) = self.positions.get_mut(symbol) {
            pos.market_price = Some(price);
        }
    }

    /// 处理成交（基于 `FillEvent`）
    ///
    /// 注意：当前 `FillEvent` 不携带 `Symbol`，因此使用 `default_symbol`（如设置）。
    /// 建议优先使用 [`Portfolio::apply_trade`] 显式指定符号与方向。
    pub fn update_with_fill(&mut self, fill: &FillEvent) -> PortfolioResult<()> {
        let symbol = self
            .default_symbol
            .clone()
            .ok_or_else(|| PortfolioError::UpdateFailed("未设置 default_symbol".into()))?;
        // FillEvent 不携带 taker 方向，按 Buy 处理
        self.apply_trade(&symbol, &fill.trade, Side::Buy, fill.timestamp)
    }

    /// 应用一笔成交（显式指定符号与方向）
    pub fn apply_trade(
        &mut self,
        symbol: &Symbol,
        trade: &Trade,
        taker_side: Side,
        timestamp: Timestamp,
    ) -> PortfolioResult<()> {
        let _ = timestamp; // 暂未使用，预留用于扩展
        let qty_f = trade.quantity.as_f64() * taker_side.sign() as f64;
        let price_f = trade.price.as_f64();

        let position = self.positions.entry(symbol.clone()).or_insert_with(|| {
            Position::new(
                symbol.clone(),
                Quantity::from_f64(0.0),
                Price::from_f64(0.0),
            )
        });

        let old_qty = position.quantity.as_f64();
        let new_qty = old_qty + qty_f;
        let old_cost = position.avg_cost.as_f64();

        // 计算已实现盈亏（仅在平仓方向时）
        let realized_f = if (old_qty > 0.0 && qty_f < 0.0) || (old_qty < 0.0 && qty_f > 0.0) {
            let close_qty = qty_f.abs().min(old_qty.abs());
            let direction = if old_qty > 0.0 { 1.0 } else { -1.0 };
            close_qty * (price_f - old_cost) * direction
        } else {
            0.0
        };
        let realized = (realized_f * 1_000_000.0) as i64;

        // 更新加权平均成本
        if new_qty.abs() > f64::EPSILON {
            let old_cost_basis = old_qty.abs() * old_cost;
            let new_cost_basis = old_cost_basis + qty_f.abs() * price_f;
            position.avg_cost = Price::from_f64(new_cost_basis / new_qty.abs());
        } else {
            position.avg_cost = Price::from_f64(0.0);
        }

        position.quantity = Quantity::from_f64(new_qty);
        position.realized_pnl += realized;
        self.total_realized_pnl += realized;

        // 更新方向
        position.side = if new_qty >= 0.0 {
            Side::Buy
        } else {
            Side::Sell
        };

        // 空仓时移除（保持 positions HashMap 紧凑）
        if position.is_empty() {
            self.positions.remove(symbol);
        }

        // 扣除佣金
        let commission_f = trade.turnover() * (self.commission_rate as f64 / 1_000_000.0);
        let commission = (commission_f * 1_000_000.0) as i64;
        self.total_realized_pnl -= commission;

        // 净现金调整（简化处理：买入减少现金，卖出增加现金；忽略币种）
        let cash_delta = match taker_side {
            Side::Buy => -(trade.turnover() as i64 * 1_000_000) - commission,
            Side::Sell => (trade.turnover() as i64 * 1_000_000) - commission,
        };
        *self.cash.entry(self.base_currency).or_insert(0) += cash_delta;

        // 记录交易
        self.trades.push(TradeRecord::new(
            *trade,
            realized,
            commission,
            (qty_f * 1_000_000.0) as i64,
        ));

        Ok(())
    }

    /// 移除空仓
    pub fn remove_empty_positions(&mut self) {
        self.positions.retain(|_, p| !p.is_empty());
    }

    /// 添加或替换一个持仓(Stage 3 PyO3 绑定需要,Python 端从 dict 构造 `Portfolio`)。
    ///
    /// 注:此方法是 [`Self::apply_trade`] 的"单笔版"——直接写入一个完整 `Position`,
    /// 不走 `apply_trade` 的加权平均成本/佣金/已实现盈亏逻辑,适用于
    /// "风控预交易检查"等"读"路径(不修改 `Portfolio`)。
    /// 真实成交更新应走 `apply_trade`,以保证内部不变量。
    ///
    /// 若 `symbol` 已存在则覆盖;空仓(`is_empty() == true`)不会插入。
    pub fn add_position(&mut self, position: Position) {
        if position.is_empty() {
            return;
        }
        self.positions.insert(position.symbol.clone(), position);
    }

    /// 获取某个符号的已实现盈亏
    pub fn realized_pnl(&self, symbol: &Symbol) -> i64 {
        self.positions
            .get(symbol)
            .map(|p| p.realized_pnl)
            .unwrap_or(0)
    }

    /// 获取某个符号的未实现盈亏
    pub fn unrealized_pnl(&self, symbol: &Symbol) -> i64 {
        self.positions
            .get(symbol)
            .map(|p| p.unrealized_pnl())
            .unwrap_or(0)
    }

    /// 总已实现盈亏
    pub fn total_realized_pnl(&self) -> i64 {
        self.total_realized_pnl
    }

    /// 总未实现盈亏
    pub fn total_unrealized_pnl(&self) -> i64 {
        self.positions.values().map(|p| p.unrealized_pnl()).sum()
    }

    /// 总盈亏
    pub fn total_pnl(&self) -> i64 {
        self.total_realized_pnl() + self.total_unrealized_pnl()
    }

    /// 净值 (NAV) = 现金 + 持仓市值
    pub fn nav(&self) -> i64 {
        let cash_total: i64 = self.cash.values().sum();
        let position_value: i64 = self
            .positions
            .values()
            .filter_map(|p| p.market_value())
            .sum();
        cash_total + position_value
    }

    /// 各持仓市值占净值比例
    pub fn exposure(&self) -> HashMap<Symbol, f64> {
        let nav_f = self.nav() as f64;
        if nav_f.abs() < 1.0 {
            return HashMap::new();
        }
        self.positions
            .iter()
            .filter_map(|(sym, pos)| {
                pos.market_value()
                    .map(|mv| (sym.clone(), mv as f64 / nav_f))
            })
            .collect()
    }

    /// 交易历史
    pub fn trades(&self) -> &[TradeRecord] {
        &self.trades
    }

    /// 获取投资组合快照
    pub fn snapshot(&self, timestamp: Timestamp) -> PortfolioSnapshot {
        PortfolioSnapshot {
            timestamp,
            nav: self.nav(),
            cash: self.cash.clone(),
            positions: self.positions.clone(),
            realized_pnl: self.total_realized_pnl,
            unrealized_pnl: self.total_unrealized_pnl(),
        }
    }

    /// 基准货币
    pub fn base_currency(&self) -> Currency {
        self.base_currency
    }

    /// 佣金费率
    pub fn commission_rate(&self) -> f64 {
        self.commission_rate as f64 / 1_000_000.0
    }
}

impl Default for Portfolio {
    fn default() -> Self {
        // 默认 0.1% 佣金
        Self::new(Currency::USD, 0.001)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Timestamp;

    fn sym(s: &str) -> Symbol {
        Symbol::from(s)
    }

    fn make_trade(buyer: u64, seller: u64, price: f64, qty: f64) -> Trade {
        Trade::new(
            Timestamp::from_nanos(1_000),
            Price::from_f64(price),
            Quantity::from_f64(qty),
            buyer,
            seller,
        )
    }

    // ─── 持仓更新 ─────────────────────────────────────────────

    #[test]
    fn test_position_long_increases() {
        let mut p = Portfolio::default();
        p.deposit(Currency::USD, 100_000.0);
        let s = sym("BTC-USDT");
        p.apply_trade(
            &s,
            &make_trade(1, 2, 50_000.0, 1.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        let pos = p.position(&s).unwrap();
        assert_eq!(pos.quantity, Quantity::from_f64(1.0));
        assert_eq!(pos.avg_cost, Price::from_f64(50_000.0));
        assert_eq!(pos.side, Side::Buy);
    }

    #[test]
    fn test_position_short_increases() {
        let mut p = Portfolio::default();
        p.deposit(Currency::USD, 100_000.0);
        let s = sym("BTC-USDT");
        // 卖空 = 主动卖出方
        p.apply_trade(
            &s,
            &make_trade(2, 1, 50_000.0, 1.0),
            Side::Sell,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        let pos = p.position(&s).unwrap();
        assert_eq!(pos.quantity, Quantity::from_f64(-1.0));
        assert_eq!(pos.side, Side::Sell);
    }

    #[test]
    fn test_position_flip_from_long_to_short() {
        let mut p = Portfolio::default();
        p.deposit(Currency::USD, 200_000.0);
        let s = sym("BTC-USDT");
        // 多头 1.0 @ 50000
        p.apply_trade(
            &s,
            &make_trade(1, 2, 50_000.0, 1.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        // 卖空 2.0 @ 60000（先平 1.0 多头 + 开 1.0 空头）
        p.apply_trade(
            &s,
            &make_trade(2, 1, 60_000.0, 2.0),
            Side::Sell,
            Timestamp::from_nanos(2_000),
        )
        .unwrap();
        let pos = p.position(&s).unwrap();
        assert_eq!(pos.quantity, Quantity::from_f64(-1.0));
        assert_eq!(pos.side, Side::Sell);
    }

    #[test]
    fn test_position_with_zero_quantity_removed() {
        let mut p = Portfolio::default();
        p.deposit(Currency::USD, 100_000.0);
        let s = sym("BTC-USDT");
        p.apply_trade(
            &s,
            &make_trade(1, 2, 50_000.0, 1.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        // 反向成交 1.0（平仓）
        p.apply_trade(
            &s,
            &make_trade(2, 1, 50_000.0, 1.0),
            Side::Sell,
            Timestamp::from_nanos(2_000),
        )
        .unwrap();
        // 持仓为 0，应被移除
        assert!(p.position(&s).is_none());
    }

    // ─── 盈亏计算 ─────────────────────────────────────────────

    #[test]
    fn test_realized_pnl_long_position() {
        let mut p = Portfolio::default();
        p.deposit(Currency::USD, 200_000.0);
        let s = sym("BTC-USDT");
        // 买入 1.0 @ 50000
        p.apply_trade(
            &s,
            &make_trade(1, 2, 50_000.0, 1.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        // 卖出 1.0 @ 55000 (盈利 5000)
        p.apply_trade(
            &s,
            &make_trade(2, 1, 55_000.0, 1.0),
            Side::Sell,
            Timestamp::from_nanos(2_000),
        )
        .unwrap();
        // 实现的盈亏应大于 0
        let total_pnl = p.total_realized_pnl();
        assert!(total_pnl > 0);
    }

    #[test]
    fn test_unrealized_pnl_updates_with_market_price() {
        let mut p = Portfolio::default();
        p.deposit(Currency::USD, 100_000.0);
        let s = sym("BTC-USDT");
        p.apply_trade(
            &s,
            &make_trade(1, 2, 50_000.0, 1.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        p.update_market_price(&s, Price::from_f64(55_000.0));
        let upnl = p.unrealized_pnl(&s);
        // 1.0 * (55000 - 50000) = 5000
        assert!(upnl > 0);
    }

    #[test]
    fn test_total_pnl_sum() {
        let mut p = Portfolio::new(Currency::USD, 0.0); // 0 佣金
        p.deposit(Currency::USD, 200_000.0);
        let s1 = sym("BTC-USDT");
        let s2 = sym("ETH-USDT");
        p.apply_trade(
            &s1,
            &make_trade(1, 2, 50_000.0, 1.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        p.apply_trade(
            &s2,
            &make_trade(3, 4, 3_000.0, 10.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        p.update_market_price(&s1, Price::from_f64(55_000.0));
        p.update_market_price(&s2, Price::from_f64(2_500.0));
        let total = p.total_pnl();
        // BTC 浮盈 5000，ETH 浮亏 5000 → 总盈亏约 0
        assert!(total.abs() < 1_000_000);
    }

    // ─── 净值 ─────────────────────────────────────────────────

    #[test]
    fn test_nav_with_cash_only() {
        let mut p = Portfolio::default();
        p.deposit(Currency::USD, 100_000.0);
        // nav = 100_000（基准货币）
        let nav = p.nav();
        assert!((nav as f64 / 1_000_000.0 - 100_000.0).abs() < 0.01);
    }

    #[test]
    fn test_nav_with_positions() {
        let mut p = Portfolio::new(Currency::USD, 0.0); // 0 佣金
        p.deposit(Currency::USD, 100_000.0);
        let s = sym("BTC-USDT");
        p.apply_trade(
            &s,
            &make_trade(1, 2, 50_000.0, 1.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        p.update_market_price(&s, Price::from_f64(55_000.0));
        // 现金 = 100000 - 50000 = 50000，持仓 = 55000，nav = 105000
        let nav = p.nav();
        let nav_f = nav as f64 / 1_000_000.0;
        assert!((nav_f - 105_000.0).abs() < 1.0);
    }

    #[test]
    fn test_nav_after_multiple_trades() {
        let mut p = Portfolio::new(Currency::USD, 0.0); // 0 佣金
        p.deposit(Currency::USD, 1_000_000.0);
        let s = sym("BTC-USDT");
        for i in 0..10 {
            p.apply_trade(
                &s,
                &make_trade(1, 2, 50_000.0 + (i as f64) * 100.0, 0.1),
                Side::Buy,
                Timestamp::from_nanos((i as i64 + 1) * 1_000),
            )
            .unwrap();
        }
        p.update_market_price(&s, Price::from_f64(55_000.0));
        let nav = p.nav();
        // 现金 + 持仓市值 = 总净值
        let cash = p.base_cash();
        assert!(nav as f64 / 1_000_000.0 > 0.0);
        assert!(cash > 0.0);
    }

    // ─── 边界 ─────────────────────────────────────────────────

    #[test]
    fn test_empty_portfolio_nav_is_zero() {
        let p = Portfolio::default();
        assert_eq!(p.nav(), 0);
    }

    #[test]
    fn test_withdraw_insufficient_cash() {
        let mut p = Portfolio::default();
        p.deposit(Currency::USD, 100.0);
        let result = p.withdraw(Currency::USD, 200.0);
        assert!(matches!(
            result,
            Err(PortfolioError::InsufficientCash { .. })
        ));
    }

    #[test]
    fn test_withdraw_sufficient_cash() {
        let mut p = Portfolio::default();
        p.deposit(Currency::USD, 100.0);
        p.withdraw(Currency::USD, 50.0).unwrap();
        assert!((p.cash_balance(Currency::USD) - 50.0).abs() < 1e-6);
    }

    #[test]
    fn test_cash_balance_unknown_currency() {
        let p = Portfolio::default();
        assert_eq!(p.cash_balance(Currency::EUR), 0.0);
    }

    #[test]
    fn test_exposure_with_positions() {
        let mut p = Portfolio::default();
        p.deposit(Currency::USD, 100_000.0);
        let s = sym("BTC-USDT");
        p.apply_trade(
            &s,
            &make_trade(1, 2, 50_000.0, 1.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        p.update_market_price(&s, Price::from_f64(50_000.0));
        let exposure = p.exposure();
        assert!(exposure.contains_key(&s));
    }

    #[test]
    fn test_exposure_empty() {
        let p = Portfolio::default();
        let exposure = p.exposure();
        assert!(exposure.is_empty());
    }

    #[test]
    fn test_snapshot_captures_state() {
        let mut p = Portfolio::default();
        p.deposit(Currency::USD, 100_000.0);
        let s = sym("BTC-USDT");
        p.apply_trade(
            &s,
            &make_trade(1, 2, 50_000.0, 1.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        p.update_market_price(&s, Price::from_f64(55_000.0));
        let snap = p.snapshot(Timestamp::from_nanos(2_000));
        assert_eq!(snap.timestamp, Timestamp::from_nanos(2_000));
        assert_eq!(snap.positions.len(), 1);
        assert!(snap.unrealized_pnl > 0);
    }

    #[test]
    fn test_trades_history() {
        let mut p = Portfolio::default();
        p.deposit(Currency::USD, 100_000.0);
        let s = sym("BTC-USDT");
        p.apply_trade(
            &s,
            &make_trade(1, 2, 50_000.0, 1.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        p.apply_trade(
            &s,
            &make_trade(1, 2, 51_000.0, 0.5),
            Side::Buy,
            Timestamp::from_nanos(2_000),
        )
        .unwrap();
        assert_eq!(p.trades().len(), 2);
    }

    #[test]
    fn test_update_with_fill_requires_default_symbol() {
        let mut p = Portfolio::default();
        let trade = make_trade(1, 2, 50_000.0, 1.0);
        let fill = FillEvent::new(0, Timestamp::from_nanos(1_000), trade);
        let result = p.update_with_fill(&fill);
        assert!(result.is_err());
    }

    #[test]
    fn test_update_with_fill_with_default_symbol() {
        let mut p = Portfolio::with_default_symbol(Currency::USD, 0.0, sym("BTC-USDT"));
        p.deposit(Currency::USD, 100_000.0);
        let trade = make_trade(1, 2, 50_000.0, 1.0);
        let fill = FillEvent::new(0, Timestamp::from_nanos(1_000), trade);
        p.update_with_fill(&fill).unwrap();
        assert!(p.position(&sym("BTC-USDT")).is_some());
    }

    #[test]
    fn test_remove_empty_positions() {
        let mut p = Portfolio::default();
        let s = sym("BTC-USDT");
        p.positions.insert(
            s.clone(),
            Position::new(s.clone(), Quantity::from_f64(0.0), Price::from_f64(0.0)),
        );
        p.remove_empty_positions();
        assert!(p.position(&s).is_none());
    }

    #[test]
    fn test_base_currency_accessors() {
        let p = Portfolio::new(Currency::USDT, 0.001);
        assert_eq!(p.base_currency(), Currency::USDT);
        assert!((p.commission_rate() - 0.001).abs() < 1e-9);
    }
}
