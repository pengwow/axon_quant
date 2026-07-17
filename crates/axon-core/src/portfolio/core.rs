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
use crate::types::{Instrument, Price, Quantity, Symbol};

/// 投资组合
///
/// 跟踪多币种现金、多资产持仓、交易历史、累计盈亏。
///
/// # 0.5.0 BREAKING:`positions` 键类型从 `Symbol` 迁到 `Instrument`
///
/// 原因:Spot 和 perp 共享同一 `Symbol`(`"BTC/USDT"`),用 `Symbol` 作
/// HashMap key 会把 spot leg + perp leg 净持仓错误合并,导致 delta-neutral
/// 套利被错算为"无持仓"。改用 `Instrument` 作 key 后,spot / perp 独立索引,
/// 风险引擎 / 多 leg 回测能正确处理。
///
/// 兼容路径:
/// - `apply_trade(symbol, ...)` 仍可用,内部用 `Instrument::from_symbol(symbol)` 派生
/// - `apply_trade_instrument(&Instrument, ...)` 新 API,推荐多 leg 场景
/// - `position_by_instrument(&Instrument)` / `positions()`(键类型已变)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Portfolio {
    /// 多币种现金余额(单位:f64 × 1e6)
    cash: HashMap<Currency, i64>,
    /// 持仓映射(**0.5.0 BREAKING**:键类型 `Symbol` → `Instrument`)
    positions: HashMap<Instrument, Position>,
    /// 交易历史
    trades: Vec<TradeRecord>,
    /// 已实现盈亏累计
    total_realized_pnl: i64,
    /// 佣金费率(单位:f64 × 1e6)
    commission_rate: i64,
    /// 基准货币
    base_currency: Currency,
    /// 默认 instrument(用于 `update_with_fill`,无 instrument 信息时使用)
    ///
    /// **0.5.0 BREAKING**:从 `default_symbol: Option<Symbol>` 改为
    /// `default_instrument: Option<Instrument>`。
    default_instrument: Option<Instrument>,
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
            default_instrument: None,
        }
    }

    /// 创建带默认 instrument 的投资组合
    ///
    /// **0.5.0 BREAKING**:从 `with_default_symbol(symbol)` 改为
    /// `with_default_instrument(instrument)`。
    pub fn with_default_instrument(
        base_currency: Currency,
        commission_rate: f64,
        default_instrument: Instrument,
    ) -> Self {
        Self {
            default_instrument: Some(default_instrument),
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

    /// 获取持仓(0.5.0 新增:通过 instrument)
    pub fn position_by_instrument(&self, instrument: &Instrument) -> Option<&Position> {
        self.positions.get(instrument)
    }

    /// 获取所有持仓(0.5.0 BREAKING:返回 `HashMap<Instrument, Position>`)
    pub fn positions(&self) -> &HashMap<Instrument, Position> {
        &self.positions
    }

    /// 更新市场价格(0.5.0 新增:通过 instrument)
    pub fn update_market_price_instrument(&mut self, instrument: &Instrument, price: Price) {
        if let Some(pos) = self.positions.get_mut(instrument) {
            pos.market_price = Some(price);
        }
    }

    /// 处理成交(基于 `FillEvent`)
    ///
    /// **0.5.0 BREAKING**:用 `default_instrument` 替代 `default_symbol`。
    /// 注意当前 `FillEvent` 不携带 `Instrument`,因此使用 `default_instrument`。
    /// 建议优先使用 [`Portfolio::apply_trade_instrument`] 显式指定。
    pub fn update_with_fill(&mut self, fill: &FillEvent) -> PortfolioResult<()> {
        let instrument = self
            .default_instrument
            .clone()
            .ok_or_else(|| PortfolioError::UpdateFailed("未设置 default_instrument".into()))?;
        self.apply_trade_instrument(&instrument, &fill.trade, Side::Buy, fill.timestamp)
    }

    /// 0.5.0 主路径:用 `Instrument` 应用成交(推荐,多 leg 必备)
    ///
    /// 与 [`Self::apply_trade`] 的区别:用 `Instrument` 作 HashMap key,
    /// 消除 spot / perp 共享同一 `Symbol` 时的 key 碰撞。
    pub fn apply_trade_instrument(
        &mut self,
        instrument: &Instrument,
        trade: &Trade,
        taker_side: Side,
        timestamp: Timestamp,
    ) -> PortfolioResult<()> {
        let _ = timestamp;
        let qty_f = trade.quantity.as_f64() * taker_side.sign() as f64;
        let price_f = trade.price.as_f64();

        let position = self
            .positions
            .entry(instrument.clone())
            .or_insert_with(|| {
                Position::with_instrument(
                    instrument.clone(),
                    Quantity::from_f64(0.0),
                    Price::from_f64(0.0),
                )
            });
        // 防御性:已有 position 但 instrument 不一致(旧迁移数据),用传入的覆盖
        if position.instrument != *instrument {
            position.instrument = instrument.clone();
        }

        let old_qty = position.quantity.as_f64();
        let new_qty = old_qty + qty_f;
        let old_cost = position.avg_cost.as_f64();

        // 计算已实现盈亏(仅在平仓方向时)
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

        // 空仓时移除(保持 positions HashMap 紧凑)
        if position.is_empty() {
            self.positions.remove(instrument);
        }

        // 扣除佣金
        let commission_f = trade.turnover() * (self.commission_rate as f64 / 1_000_000.0);
        let commission = (commission_f * 1_000_000.0) as i64;
        self.total_realized_pnl -= commission;

        // 净现金调整(简化处理:买入减少现金,卖出增加现金;忽略币种)
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
            instrument.clone(),
        ));

        Ok(())
    }

    /// 应用一笔成交(0.5.0 兼容路径:从 `Symbol` 派生 `Instrument`)
    ///
    /// **0.5.0 BREAKING**:虽然签名仍接受 `Symbol`,但内部用
    /// `Instrument::from_symbol(symbol)` 派生 Instrument,新代码请用
    /// [`Self::apply_trade_instrument`] 显式指定。
    pub fn apply_trade(
        &mut self,
        symbol: &Symbol,
        trade: &Trade,
        taker_side: Side,
        timestamp: Timestamp,
    ) -> PortfolioResult<()> {
        let instrument = Instrument::from_symbol(symbol);
        self.apply_trade_instrument(&instrument, trade, taker_side, timestamp)
    }

    /// 移除空仓
    pub fn remove_empty_positions(&mut self) {
        self.positions.retain(|_, p| !p.is_empty());
    }

    /// 添加或替换一个持仓(Stage 3 PyO3 绑定需要,Python 端从 dict 构造 `Portfolio`)。
    ///
    /// 注:此方法是 [`Self::apply_trade_instrument`] 的"单笔版"——直接写入一个完整 `Position`,
    /// 不走 `apply_trade_instrument` 的加权平均成本/佣金/已实现盈亏逻辑,适用于
    /// "风控预交易检查"等"读"路径(不修改 `Portfolio`)。
    /// 真实成交更新应走 `apply_trade_instrument`,以保证内部不变量。
    ///
    /// **0.5.0 BREAKING**:用 `position.instrument` 作 key(替代 `position.symbol`)。
    /// 若 `position` 为空仓(`is_empty() == true`)不会插入。
    pub fn add_position(&mut self, position: Position) {
        if position.is_empty() {
            return;
        }
        self.positions.insert(position.instrument.clone(), position);
    }

    /// 获取某个 instrument 的已实现盈亏(0.5.0 推荐用法)
    pub fn realized_pnl_instrument(&self, instrument: &Instrument) -> i64 {
        self.positions
            .get(instrument)
            .map(|p| p.realized_pnl)
            .unwrap_or(0)
    }

    /// 获取某个 instrument 的未实现盈亏(0.5.0 推荐用法)
    pub fn unrealized_pnl_instrument(&self, instrument: &Instrument) -> i64 {
        self.positions
            .get(instrument)
            .map(|p| p.unrealized_pnl())
            .unwrap_or(0)
    }

    /// 获取某个 symbol 的已实现盈亏(0.5.0 兼容路径,派生 Instrument)
    pub fn realized_pnl(&self, symbol: &Symbol) -> i64 {
        let inst = Instrument::from_symbol(symbol);
        self.realized_pnl_instrument(&inst)
    }

    /// 获取某个 symbol 的未实现盈亏(0.5.0 兼容路径,派生 Instrument)
    pub fn unrealized_pnl(&self, symbol: &Symbol) -> i64 {
        let inst = Instrument::from_symbol(symbol);
        self.unrealized_pnl_instrument(&inst)
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

    /// 各持仓市值占净值比例(**0.5.0 BREAKING**:键类型 `Symbol` → `Instrument`)
    pub fn exposure(&self) -> HashMap<Instrument, f64> {
        let nav_f = self.nav() as f64;
        if nav_f.abs() < 1.0 {
            return HashMap::new();
        }
        self.positions
            .iter()
            .filter_map(|(inst, pos)| {
                pos.market_value()
                    .map(|mv| (inst.clone(), mv as f64 / nav_f))
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
    use crate::types::SwapSettle;

    fn sym(s: &str) -> Symbol {
        Symbol::from(s)
    }

    /// 测试辅助:把 string 形式 symbol 转为 Instrument(默认 spot)
    fn inst(s: &str) -> Instrument {
        Instrument::from_symbol(&Symbol::from(s))
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

    // ─── 持仓更新(0.5.0 迁移到 Instrument key) ─────────────────────

    #[test]
    fn test_position_long_increases() {
        let mut p = Portfolio::default();
        p.deposit(Currency::USD, 100_000.0);
        let i = inst("BTC/USDT");
        p.apply_trade_instrument(
            &i,
            &make_trade(1, 2, 50_000.0, 1.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        let pos = p.position_by_instrument(&i).unwrap();
        assert_eq!(pos.quantity, Quantity::from_f64(1.0));
        assert_eq!(pos.avg_cost, Price::from_f64(50_000.0));
        assert_eq!(pos.side, Side::Buy);
    }

    #[test]
    fn test_position_short_increases() {
        let mut p = Portfolio::default();
        p.deposit(Currency::USD, 100_000.0);
        let i = inst("BTC/USDT");
        p.apply_trade_instrument(
            &i,
            &make_trade(2, 1, 50_000.0, 1.0),
            Side::Sell,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        let pos = p.position_by_instrument(&i).unwrap();
        assert_eq!(pos.quantity, Quantity::from_f64(-1.0));
        assert_eq!(pos.side, Side::Sell);
    }

    #[test]
    fn test_position_flip_from_long_to_short() {
        let mut p = Portfolio::default();
        p.deposit(Currency::USD, 200_000.0);
        let i = inst("BTC/USDT");
        p.apply_trade_instrument(
            &i,
            &make_trade(1, 2, 50_000.0, 1.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        p.apply_trade_instrument(
            &i,
            &make_trade(2, 1, 60_000.0, 2.0),
            Side::Sell,
            Timestamp::from_nanos(2_000),
        )
        .unwrap();
        let pos = p.position_by_instrument(&i).unwrap();
        assert_eq!(pos.quantity, Quantity::from_f64(-1.0));
        assert_eq!(pos.side, Side::Sell);
    }

    #[test]
    fn test_position_with_zero_quantity_removed() {
        let mut p = Portfolio::default();
        p.deposit(Currency::USD, 100_000.0);
        let i = inst("BTC/USDT");
        p.apply_trade_instrument(
            &i,
            &make_trade(1, 2, 50_000.0, 1.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        // 反向成交 1.0(平仓)
        p.apply_trade_instrument(
            &i,
            &make_trade(2, 1, 50_000.0, 1.0),
            Side::Sell,
            Timestamp::from_nanos(2_000),
        )
        .unwrap();
        // 持仓为 0,应被移除
        assert!(p.position_by_instrument(&i).is_none());
    }

    // ─── 盈亏计算 ───────────────────────────────────────────

    #[test]
    fn test_realized_pnl_long_position() {
        let mut p = Portfolio::default();
        p.deposit(Currency::USD, 200_000.0);
        let i = inst("BTC/USDT");
        p.apply_trade_instrument(
            &i,
            &make_trade(1, 2, 50_000.0, 1.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        p.apply_trade_instrument(
            &i,
            &make_trade(2, 1, 55_000.0, 1.0),
            Side::Sell,
            Timestamp::from_nanos(2_000),
        )
        .unwrap();
        let total_pnl = p.total_realized_pnl();
        assert!(total_pnl > 0);
    }

    #[test]
    fn test_unrealized_pnl_updates_with_market_price() {
        let mut p = Portfolio::default();
        p.deposit(Currency::USD, 100_000.0);
        let i = inst("BTC/USDT");
        p.apply_trade_instrument(
            &i,
            &make_trade(1, 2, 50_000.0, 1.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        p.update_market_price_instrument(&i, Price::from_f64(55_000.0));
        let upnl = p.unrealized_pnl_instrument(&i);
        // 1.0 * (55000 - 50000) = 5000
        assert!(upnl > 0);
    }

    #[test]
    fn test_total_pnl_sum() {
        let mut p = Portfolio::new(Currency::USD, 0.0);
        p.deposit(Currency::USD, 200_000.0);
        let i1 = inst("BTC/USDT");
        let i2 = inst("ETH/USDT");
        p.apply_trade_instrument(
            &i1,
            &make_trade(1, 2, 50_000.0, 1.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        p.apply_trade_instrument(
            &i2,
            &make_trade(3, 4, 3_000.0, 10.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        p.update_market_price_instrument(&i1, Price::from_f64(55_000.0));
        p.update_market_price_instrument(&i2, Price::from_f64(2_500.0));
        let total = p.total_pnl();
        // BTC 浮盈 5000,ETH 浮亏 5000 → 总盈亏约 0
        assert!(total.abs() < 1_000_000);
    }

    // ─── 净值 ───────────────────────────────────────────────

    #[test]
    fn test_nav_with_cash_only() {
        let mut p = Portfolio::default();
        p.deposit(Currency::USD, 100_000.0);
        let nav = p.nav();
        assert!((nav as f64 / 1_000_000.0 - 100_000.0).abs() < 0.01);
    }

    #[test]
    fn test_nav_with_positions() {
        let mut p = Portfolio::new(Currency::USD, 0.0);
        p.deposit(Currency::USD, 100_000.0);
        let i = inst("BTC/USDT");
        p.apply_trade_instrument(
            &i,
            &make_trade(1, 2, 50_000.0, 1.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        p.update_market_price_instrument(&i, Price::from_f64(55_000.0));
        let nav = p.nav();
        let nav_f = nav as f64 / 1_000_000.0;
        assert!((nav_f - 105_000.0).abs() < 1.0);
    }

    #[test]
    fn test_nav_after_multiple_trades() {
        let mut p = Portfolio::new(Currency::USD, 0.0);
        p.deposit(Currency::USD, 1_000_000.0);
        let i = inst("BTC/USDT");
        for j in 0..10 {
            p.apply_trade_instrument(
                &i,
                &make_trade(1, 2, 50_000.0 + (j as f64) * 100.0, 0.1),
                Side::Buy,
                Timestamp::from_nanos((j as i64 + 1) * 1_000),
            )
            .unwrap();
        }
        p.update_market_price_instrument(&i, Price::from_f64(55_000.0));
        let nav = p.nav();
        let cash = p.base_cash();
        assert!(nav as f64 / 1_000_000.0 > 0.0);
        assert!(cash > 0.0);
    }

    // ─── 边界 ───────────────────────────────────────────────

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
        let i = inst("BTC/USDT");
        p.apply_trade_instrument(
            &i,
            &make_trade(1, 2, 50_000.0, 1.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        p.update_market_price_instrument(&i, Price::from_f64(50_000.0));
        let exposure = p.exposure();
        assert!(exposure.contains_key(&i));
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
        let i = inst("BTC/USDT");
        p.apply_trade_instrument(
            &i,
            &make_trade(1, 2, 50_000.0, 1.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        p.update_market_price_instrument(&i, Price::from_f64(55_000.0));
        let snap = p.snapshot(Timestamp::from_nanos(2_000));
        assert_eq!(snap.timestamp, Timestamp::from_nanos(2_000));
        assert_eq!(snap.positions.len(), 1);
        assert!(snap.unrealized_pnl > 0);
    }

    #[test]
    fn test_trades_history() {
        let mut p = Portfolio::default();
        p.deposit(Currency::USD, 100_000.0);
        let i = inst("BTC/USDT");
        p.apply_trade_instrument(
            &i,
            &make_trade(1, 2, 50_000.0, 1.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        p.apply_trade_instrument(
            &i,
            &make_trade(1, 2, 51_000.0, 0.5),
            Side::Buy,
            Timestamp::from_nanos(2_000),
        )
        .unwrap();
        assert_eq!(p.trades().len(), 2);
    }

    #[test]
    fn test_update_with_fill_requires_default_instrument() {
        let mut p = Portfolio::default();
        let trade = make_trade(1, 2, 50_000.0, 1.0);
        let fill = FillEvent::new(0, Timestamp::from_nanos(1_000), trade);
        let result = p.update_with_fill(&fill);
        assert!(result.is_err());
    }

    #[test]
    fn test_update_with_fill_with_default_instrument() {
        let i = inst("BTC/USDT");
        let mut p = Portfolio::with_default_instrument(Currency::USD, 0.0, i.clone());
        p.deposit(Currency::USD, 100_000.0);
        let trade = make_trade(1, 2, 50_000.0, 1.0);
        let fill = FillEvent::new(0, Timestamp::from_nanos(1_000), trade);
        p.update_with_fill(&fill).unwrap();
        assert!(p.position_by_instrument(&i).is_some());
    }

    #[test]
    fn test_remove_empty_positions() {
        let mut p = Portfolio::default();
        let i = inst("BTC/USDT");
        p.positions.insert(
            i.clone(),
            Position::with_instrument(i.clone(), Quantity::from_f64(0.0), Price::from_f64(0.0)),
        );
        p.remove_empty_positions();
        assert!(p.position_by_instrument(&i).is_none());
    }

    #[test]
    fn test_base_currency_accessors() {
        let p = Portfolio::new(Currency::USDT, 0.001);
        assert_eq!(p.base_currency(), Currency::USDT);
        assert!((p.commission_rate() - 0.001).abs() < 1e-9);
    }

    // ─── 0.5.0 关键修复:apply_trade_instrument spot+perp 区分 ─────────

    fn btc_spot() -> Instrument {
        Instrument::Spot(crate::types::SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        })
    }
    fn btc_perp() -> Instrument {
        Instrument::Swap(crate::types::SwapInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
            settle: SwapSettle::UsdMargin,
            contract_size: 1.0,
        })
    }

    #[test]
    fn test_apply_trade_instrument_spot_perp_isolated() {
        // delta-neutral 套利入场:spot long 0.5 + perp short 0.3
        let mut p = Portfolio::new(Currency::USD, 0.0);
        p.deposit(Currency::USD, 200_000.0);

        let spot = btc_spot();
        let perp = btc_perp();

        // spot leg buy 0.5
        p.apply_trade_instrument(
            &spot,
            &make_trade(1, 2, 50_000.0, 0.5),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();

        // perp leg sell 0.3(主动卖出方 = 开空 0.3)
        p.apply_trade_instrument(
            &perp,
            &make_trade(2, 1, 50_000.0, 0.3),
            Side::Sell,
            Timestamp::from_nanos(1_500),
        )
        .unwrap();

        // ✅ Instrument key 视角:2 个独立持仓(spot=+0.5, perp=-0.3)
        //   0.5.0 修复了 0.4.x 的 footgun:之前 HashMap<Symbol, _> 会把
        //   spot+perp 净成 +0.2,delta-neutral 看起来"无持仓"。
        let positions = p.positions();
        assert_eq!(positions.len(), 2, "spot+perp 应该是 2 个独立 key");
        assert_eq!(positions[&spot].quantity, Quantity::from_f64(0.5));
        assert_eq!(positions[&spot].instrument.kind(), "spot");
        assert_eq!(positions[&perp].quantity, Quantity::from_f64(-0.3));
        assert_eq!(positions[&perp].instrument.kind(), "swap");
    }

    #[test]
    fn test_apply_trade_legacy_symbol_path() {
        // 旧 apply_trade(symbol, ...) 兼容路径:用 Instrument::from_symbol 派生
        let mut p = Portfolio::new(Currency::USD, 0.0);
        p.deposit(Currency::USD, 100_000.0);
        let s = sym("BTC/USDT");
        p.apply_trade(
            &s,
            &make_trade(1, 2, 50_000.0, 1.0),
            Side::Buy,
            Timestamp::from_nanos(1_000),
        )
        .unwrap();
        let i = inst("BTC/USDT");
        let pos = p.position_by_instrument(&i).unwrap();
        assert_eq!(pos.quantity, Quantity::from_f64(1.0));
        assert_eq!(pos.instrument.kind(), "spot");
    }

    #[test]
    fn test_position_with_instrument_factory() {
        let spot = btc_spot();
        let pos = Position::with_instrument(
            spot.clone(),
            Quantity::from_f64(0.3),
            Price::from_f64(50_000.0),
        );
        assert_eq!(pos.instrument(), &spot);
        assert_eq!(pos.instrument.kind(), "spot");
        assert_eq!(pos.symbol, Symbol::from("BTC/USDT"));
    }

    #[test]
    fn test_instrument_from_symbol_parses_dash_and_slash() {
        // 兼容 "-" 和 "/" 两种分隔符(旧 OMS 格式 + 惯例格式)
        let a = Instrument::from_symbol(&Symbol::from("BTC-USDT"));
        let b = Instrument::from_symbol(&Symbol::from("BTC/USDT"));
        assert_eq!(a, b);
        assert_eq!(a.base().as_str(), "BTC");
        assert_eq!(a.quote().as_str(), "USDT");
    }
}
