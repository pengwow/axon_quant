//! Portfolio substructure:多币种现金 + 多 symbol 持仓 + 加权平均成本法
//!
//! **职责**:消费 `Fill` 事件,更新 cash + positions + 实现盈亏。
//! 不知道 OrderManager 存在,不关心订单状态。
//!
//! **成本基础法**:加权平均(WAC)。
//! 每次 buy:新 cost = (old_qty * old_avg + new_qty * new_price) / (old_qty + new_qty)
//! 每次 sell:cost basis 不变(卖不改变 avg price),qty 减少
//! 平仓(qty=0):保留 entry(realized_pnl 累计),avg_price=0
//! 反向开仓(跨 0):用 fill.price 重置 avg_price
//!
//! **quote currency 硬编码为 "USDT"**(Stage B-MVP+ 改可配,见 design §8 风险)。

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::types::Fill;

/// 单个 symbol 的持仓状态
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub symbol: String,
    /// 净持仓:正=多,负=空
    pub quantity: Decimal,
    /// 加权平均成本(空头为开仓均价)
    pub avg_price: Decimal,
    /// 实现盈亏累计
    pub realized_pnl: Decimal,
    /// 最近一次成交时间
    pub updated_at: DateTime<Utc>,
}

/// Portfolio 账务模式
///
/// 决定 [`Portfolio::apply_fill`] 如何把成交映射到 `cash`:
///
/// - [`AccountingMode::Cash`] —— **现货口径**,买入按全额名义价值扣现金;
/// - [`AccountingMode::Wallet`] —— **合约钱包口径**,开仓不占现金(只扣手续费),
///   平仓把已实现盈亏计入现金。
///
/// 默认 `Cash`,保持既有回测 / paper 行为不变。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AccountingMode {
    /// 现货:买入扣全额名义价值(默认,保持既有行为)
    #[default]
    Cash,
    /// 合约钱包:开仓只扣手续费,平仓把已实现盈亏计入现金
    Wallet,
}

/// 多币种现金余额 + 多 symbol 持仓
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Portfolio {
    /// 现金余额:币种 -> 数量
    pub cash: HashMap<String, Decimal>,
    /// 持仓:symbol -> Position
    pub positions: HashMap<String, Position>,
    /// 账务模式(见 [`AccountingMode`]);老快照缺该字段时按默认现货口径恢复
    #[serde(default)]
    accounting_mode: AccountingMode,
}

/// Portfolio 快照(供 OmsSnapshot 嵌入 / 序列化)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortfolioSnapshot {
    pub cash: HashMap<String, Decimal>,
    pub positions: Vec<Position>,
    pub as_of: DateTime<Utc>,
}

/// Portfolio 错误
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PortfolioError {
    /// 数量为 0 的 fill(语义无效)
    #[error("zero-quantity fill: {fill_id}")]
    ZeroFill { fill_id: String },

    /// 加权平均计算溢出
    #[error("cost basis overflow: {context}")]
    CostBasisOverflow { context: String },

    /// 现金不足(买入时现金不足以覆盖所需现金)
    ///
    /// `Cash` 模式需要 `gross + fee`,`Wallet` 模式只需要 `fee`(开仓不占现金)。
    #[error("insufficient cash: need {need} {currency}, have {have}")]
    InsufficientCash {
        currency: String,
        need: Decimal,
        have: Decimal,
    },
}

impl Portfolio {
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置初始现金(OMS 启动时调)
    ///
    /// **增量 deposit**:对同一 currency 累加,非覆盖。
    pub fn deposit(&mut self, currency: &str, amount: Decimal) {
        *self.cash.entry(currency.into()).or_insert(Decimal::ZERO) += amount;
    }

    /// 以指定账务模式构造(链式;默认 [`AccountingMode::Cash`])
    pub fn with_accounting_mode(mut self, mode: AccountingMode) -> Self {
        self.accounting_mode = mode;
        self
    }

    /// 切换账务模式
    ///
    /// 只改后续 `apply_fill` 的现金流口径,不回填已有 cash / 持仓;切换前后
    /// 需要调用方自己与交易所对账。
    pub fn set_accounting_mode(&mut self, mode: AccountingMode) {
        self.accounting_mode = mode;
    }

    /// 当前账务模式
    pub fn accounting_mode(&self) -> AccountingMode {
        self.accounting_mode
    }

    /// 取出现金(出金)
    ///
    /// 与 deposit() 对称:扣减对应币种 cash 余额,
    /// 余额不足时返回 `InsufficientCash` 错误。
    pub fn withdraw(&mut self, currency: &str, amount: Decimal) -> Result<(), PortfolioError> {
        let have = self.cash.get(currency).copied().unwrap_or(Decimal::ZERO);
        if have < amount {
            return Err(PortfolioError::InsufficientCash {
                currency: currency.into(),
                need: amount,
                have,
            });
        }
        *self.cash.entry(currency.into()).or_insert(Decimal::ZERO) -= amount;
        Ok(())
    }

    /// 导出快照
    pub fn snapshot(&self) -> PortfolioSnapshot {
        PortfolioSnapshot {
            cash: self.cash.clone(),
            positions: self.positions.values().cloned().collect(),
            as_of: Utc::now(),
        }
    }

    /// 从快照恢复(in-place)
    pub fn recover(&mut self, snap: PortfolioSnapshot) {
        self.cash = snap.cash;
        self.positions = snap
            .positions
            .into_iter()
            .map(|p| (p.symbol.clone(), p))
            .collect();
    }

    /// 从快照构造新 Portfolio
    ///
    /// 账务模式不在 [`PortfolioSnapshot`] 里,新实例总是默认 [`AccountingMode::Cash`];
    /// 合约调用方恢复后必须再 `set_accounting_mode(Wallet)`。
    pub fn from_snapshot(snap: PortfolioSnapshot) -> Self {
        let mut p = Self::new();
        p.recover(snap);
        p
    }

    /// 应用一个 fill:更新 cash + 持仓 + 实现盈亏
    ///
    /// **side 由 fill.quantity 符号隐含**:正 = buy,负 = sell。
    ///
    /// 现金如何变化取决于 [`Self::accounting_mode`]:
    ///
    /// | 成交 | `Cash`(现货) | `Wallet`(合约钱包) |
    /// |------|---------------|---------------------|
    /// | 买入(开多 / 加多) | `cash -= price*qty + fee` | `cash -= fee` |
    /// | 卖出(平多) | `cash += price*qty - fee` | `cash += realized_pnl - fee` |
    /// | 卖空(开空) | `cash += price*qty - fee` | `cash -= fee` |
    /// | 买平(平空) | `cash -= price*qty + fee` | `cash += realized_pnl - fee` |
    ///
    /// 两种模式的持仓与 `realized_pnl` 计算完全相同(加权平均成本法)。
    ///
    /// **`Wallet` 的 `cash` 语义是交易所钱包余额(`totalWalletBalance`)**:
    /// 开仓占用的保证金与未实现盈亏都不在 `cash` 里,所以
    /// `权益 = cash + 未实现盈亏 + 占用保证金` 必须由调用方另行计算。
    ///
    /// **quote currency 硬编码为 "USDT"**。
    pub fn apply_fill(&mut self, fill: &Fill) -> Result<(), PortfolioError> {
        let qty = fill.quantity;
        if qty.is_zero() {
            return Err(PortfolioError::ZeroFill {
                fill_id: fill.fill_id.clone(),
            });
        }
        let quote = "USDT";
        let gross = fill.price * qty.abs();
        let fee = fill.fee;
        let mode = self.accounting_mode;

        // 0. 现金预检(仅买入会减少现金):现货要覆盖全额名义价值,合约钱包只需手续费
        if qty.is_sign_positive() {
            let need = match mode {
                AccountingMode::Cash => gross + fee,
                AccountingMode::Wallet => fee,
            };
            let have = self.cash.get(quote).copied().unwrap_or(Decimal::ZERO);
            if have < need {
                return Err(PortfolioError::InsufficientCash {
                    currency: quote.into(),
                    need,
                    have,
                });
            }
        }

        // 1. 更新持仓,并算出本次 fill 中**平仓部分**的已实现盈亏
        //    Wallet 模式的现金流要用它,所以先算再动 cash。
        let pos = self
            .positions
            .entry(fill.symbol.clone())
            .or_insert(Position {
                symbol: fill.symbol.clone(),
                quantity: Decimal::ZERO,
                avg_price: Decimal::ZERO,
                realized_pnl: Decimal::ZERO,
                updated_at: fill.timestamp,
            });
        let old_qty = pos.quantity;
        let new_qty = old_qty + qty;
        // 从零开仓 / 同向加仓;反向则可能平仓或跨 0 反手
        let same_direction =
            old_qty.is_zero() || old_qty.is_sign_positive() == qty.is_sign_positive();
        // 跨 0 时只结算旧仓位那部分,剩余数量按 fill.price 反向开仓
        let closing_qty = qty.abs().min(old_qty.abs());
        let realized_delta = if same_direction {
            Decimal::ZERO
        } else if old_qty.is_sign_positive() {
            // 平多
            (fill.price - pos.avg_price) * closing_qty
        } else {
            // 平空
            (pos.avg_price - fill.price) * closing_qty
        };

        if same_direction {
            // 从零开仓 / 同向加仓:更新 avg price
            let total_cost = old_qty.abs() * pos.avg_price + qty.abs() * fill.price;
            let total_qty = old_qty.abs() + qty.abs();
            pos.avg_price = if total_qty.is_zero() {
                Decimal::ZERO
            } else {
                total_cost / total_qty
            };
        } else {
            // 反向:平仓 / 反向开仓
            pos.realized_pnl += realized_delta;
            // 反向后开仓:用 fill.price 重置 avg
            if new_qty != Decimal::ZERO && old_qty.is_sign_positive() != new_qty.is_sign_positive()
            {
                pos.avg_price = fill.price;
            } else if new_qty == Decimal::ZERO {
                pos.avg_price = Decimal::ZERO;
            }
        }
        pos.quantity = new_qty;
        pos.updated_at = fill.timestamp;

        // 2. 更新 cash:唯一随账务模式分叉的一步
        let cash_delta = match mode {
            // 现货:全额名义价值进出
            AccountingMode::Cash => {
                if qty.is_sign_positive() {
                    -(gross + fee)
                } else {
                    gross - fee
                }
            }
            // 合约钱包:只有手续费与已实现盈亏进出;开仓名义价值只占保证金
            AccountingMode::Wallet => realized_delta - fee,
        };
        *self.cash.entry(quote.into()).or_insert(Decimal::ZERO) += cash_delta;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rust_decimal_macros::dec;

    /// 构造 USDT 报价的 Fill:qty 整数(可为负),price/fee 精度 2 位,默认 symbol = "BTC-USDT"
    fn usdt_fill(fill_id: &str, qty: i64, price: i64, fee: i64) -> Fill {
        Fill {
            fill_id: fill_id.into(),
            symbol: "BTC-USDT".into(),
            instrument: None,
            price: Decimal::new(price, 2),
            quantity: Decimal::from(qty),
            fee: Decimal::new(fee, 2),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn deposit_increases_cash() {
        let mut p = Portfolio::new();
        p.deposit("USDT", dec!(100000));
        assert_eq!(p.cash.get("USDT"), Some(&dec!(100000)));

        p.deposit("USDT", dec!(50000));
        assert_eq!(p.cash.get("USDT"), Some(&dec!(150000)));

        p.deposit("BTC", dec!(1));
        assert_eq!(p.cash.get("BTC"), Some(&dec!(1)));
    }

    #[test]
    fn withdraw_decreases_cash() {
        let mut p = Portfolio::new();
        p.deposit("USDT", dec!(100000));
        p.withdraw("USDT", dec!(30000)).unwrap();
        assert_eq!(p.cash.get("USDT"), Some(&dec!(70000)));
    }

    #[test]
    fn withdraw_insufficient_cash_returns_error() {
        let mut p = Portfolio::new();
        p.deposit("USDT", dec!(100));
        let result = p.withdraw("USDT", dec!(200));
        assert_eq!(
            result,
            Err(PortfolioError::InsufficientCash {
                currency: "USDT".into(),
                need: dec!(200),
                have: dec!(100),
            })
        );
        // 余额不变
        assert_eq!(p.cash.get("USDT"), Some(&dec!(100)));
    }

    #[test]
    fn withdraw_missing_currency_returns_error() {
        let mut p = Portfolio::new();
        let result = p.withdraw("USDT", dec!(1));
        assert_eq!(
            result,
            Err(PortfolioError::InsufficientCash {
                currency: "USDT".into(),
                need: dec!(1),
                have: dec!(0),
            })
        );
    }

    #[test]
    fn withdraw_exact_balance_leaves_zero() {
        let mut p = Portfolio::new();
        p.deposit("USDT", dec!(50000));
        p.withdraw("USDT", dec!(50000)).unwrap();
        assert_eq!(p.cash.get("USDT"), Some(&dec!(0)));
    }

    #[test]
    fn apply_fill_buy_creates_long_position() {
        let mut p = Portfolio::new();
        // 50 * 50000 = 2_500_000 需要 deposit 足够 USDT
        p.deposit("USDT", dec!(3_000_000));
        p.apply_fill(&usdt_fill("f1", 50, 5000000, 0))
            .expect("apply_fill should succeed");

        let pos = p.positions.get("BTC-USDT").expect("position exists");
        assert_eq!(pos.quantity, dec!(50));
        assert_eq!(pos.avg_price, dec!(50000.00));
        assert_eq!(pos.realized_pnl, dec!(0));

        // cash = 3_000_000 - 50 * 50000 = 3_000_000 - 2_500_000 = 500_000
        assert_eq!(p.cash.get("USDT"), Some(&dec!(500_000)));
    }

    #[test]
    fn apply_fill_sell_creates_short_position() {
        let mut p = Portfolio::new();
        p.deposit("USDT", dec!(1_000_000_000));
        // 开空(从空 portfolio 直接 sell)
        p.apply_fill(&usdt_fill("f1", -50, 5000000, 0)).unwrap();
        let pos = p.positions.get("BTC-USDT").unwrap();
        assert_eq!(pos.quantity, dec!(-50));
        assert_eq!(pos.avg_price, dec!(50000));
        // cash 增加(收 USDT):1_000_000_000 + 50*50000 = 1_002_500_000
        assert_eq!(p.cash.get("USDT"), Some(&dec!(1_002_500_000)));
    }

    #[test]
    fn apply_fill_two_buys_average_cost_basis() {
        let mut p = Portfolio::new();
        p.deposit("USDT", dec!(1_000_000_000));
        p.apply_fill(&usdt_fill("f1", 50, 5000000, 0)).unwrap();
        p.apply_fill(&usdt_fill("f2", 30, 5100000, 0)).unwrap();

        let pos = p.positions.get("BTC-USDT").unwrap();
        assert_eq!(pos.quantity, dec!(80));
        // 加权:(50*50000 + 30*51000) / 80 = (2500000 + 1530000) / 80 = 4030000 / 80 = 50375
        assert_eq!(pos.avg_price, dec!(50375));
    }

    #[test]
    fn apply_fill_sell_reduces_qty_keeps_avg() {
        let mut p = Portfolio::new();
        p.deposit("USDT", dec!(1_000_000_000));
        p.apply_fill(&usdt_fill("f1", 50, 5000000, 0)).unwrap();
        let avg_before = p.positions.get("BTC-USDT").unwrap().avg_price;

        // sell 20(用负数表示 sell)
        p.apply_fill(&usdt_fill("f2", -20, 5200000, 0)).unwrap();

        let pos = p.positions.get("BTC-USDT").unwrap();
        assert_eq!(pos.quantity, dec!(30));
        assert_eq!(
            pos.avg_price, avg_before,
            "avg price should not change on sell"
        );
    }

    #[test]
    fn apply_fill_sell_realizes_pnl_on_long() {
        let mut p = Portfolio::new();
        p.deposit("USDT", dec!(1_000_000_000));
        p.apply_fill(&usdt_fill("f1", 50, 5000000, 0)).unwrap();
        // 完全平仓(50 @ 52000):realized = (52000 - 50000) * 50 = 100000
        p.apply_fill(&usdt_fill("f2", -50, 5200000, 0)).unwrap();

        let pos = p.positions.get("BTC-USDT").unwrap();
        assert_eq!(pos.quantity, dec!(0));
        assert_eq!(pos.realized_pnl, dec!(100000));
        // 平仓后 avg_price 重置为 0
        assert_eq!(pos.avg_price, dec!(0));
    }

    #[test]
    fn apply_fill_sell_realizes_pnl_on_short() {
        let mut p = Portfolio::new();
        p.deposit("USDT", dec!(1_000_000_000));
        // 开空: sell 50 @ 50000(用负 qty)
        p.apply_fill(&usdt_fill("f1", -50, 5000000, 0)).unwrap();
        let pos = p.positions.get("BTC-USDT").unwrap();
        assert_eq!(pos.quantity, dec!(-50));
        assert_eq!(pos.avg_price, dec!(50000));

        // 平空: buy 50 @ 48000(反向)
        // realized = (avg - price) * 50 = (50000 - 48000) * 50 = 100000
        p.apply_fill(&usdt_fill("f2", 50, 4800000, 0)).unwrap();
        let pos = p.positions.get("BTC-USDT").unwrap();
        assert_eq!(pos.quantity, dec!(0));
        assert_eq!(pos.realized_pnl, dec!(100000));
    }

    #[test]
    fn apply_fill_sell_fees_reduce_cash() {
        let mut p = Portfolio::new();
        p.deposit("USDT", dec!(100000));
        // fee 参数 100 经 Decimal::new(_, 2) = 1.00
        // cash = 100000 - 50000 - 1.00 = 49999.00
        p.apply_fill(&usdt_fill("f1", 1, 5000000, 100)).unwrap();
        assert_eq!(p.cash.get("USDT"), Some(&dec!(49999.00)));
    }

    #[test]
    fn accounting_mode_defaults_to_cash() {
        assert_eq!(Portfolio::new().accounting_mode(), AccountingMode::Cash);
        let mut p = Portfolio::new();
        p.set_accounting_mode(AccountingMode::Wallet);
        assert_eq!(p.accounting_mode(), AccountingMode::Wallet);
    }

    /// Wallet 模式:买入只扣手续费,不扣名义价值(合约开仓只占保证金)
    #[test]
    fn wallet_mode_buy_only_deducts_fee() {
        let mut p = Portfolio::new().with_accounting_mode(AccountingMode::Wallet);
        p.deposit("USDT", dec!(1000));
        p.apply_fill(&usdt_fill("f1", 1, 10000, 10)).unwrap(); // 1 @ 100,fee 0.10

        let pos = p.positions.get("BTC-USDT").expect("position exists");
        assert_eq!(pos.quantity, dec!(1));
        assert_eq!(pos.avg_price, dec!(100));
        assert_eq!(pos.realized_pnl, dec!(0));
        // 现金只减手续费:1000 - 0.10
        assert_eq!(p.cash.get("USDT"), Some(&dec!(999.90)));
    }

    /// Wallet 模式:平多把 realized_pnl - fee 计入现金
    #[test]
    fn wallet_mode_close_long_credits_realized_pnl_minus_fee() {
        let mut p = Portfolio::new().with_accounting_mode(AccountingMode::Wallet);
        p.deposit("USDT", dec!(1000));
        p.apply_fill(&usdt_fill("f1", 1, 10000, 10)).unwrap();
        p.apply_fill(&usdt_fill("f2", -1, 11000, 11)).unwrap(); // 平 1 @ 110,fee 0.11

        let pos = p.positions.get("BTC-USDT").expect("position exists");
        assert_eq!(pos.quantity, dec!(0));
        assert_eq!(pos.realized_pnl, dec!(10));
        // 999.90 + (10 - 0.11) = 1009.79
        assert_eq!(p.cash.get("USDT"), Some(&dec!(1009.79)));
    }

    /// 回归:Cash 模式下的现金流保持现货全额口径不变
    #[test]
    fn cash_mode_cash_flow_unchanged() {
        let mut p = Portfolio::new(); // 默认 Cash
        p.deposit("USDT", dec!(1000));
        p.apply_fill(&usdt_fill("f1", 1, 10000, 10)).unwrap();
        // 1000 - 100 - 0.10
        assert_eq!(p.cash.get("USDT"), Some(&dec!(899.90)));

        p.apply_fill(&usdt_fill("f2", -1, 11000, 11)).unwrap();
        assert_eq!(p.positions.get("BTC-USDT").unwrap().realized_pnl, dec!(10));
        // 899.90 + 110 - 0.11
        assert_eq!(p.cash.get("USDT"), Some(&dec!(1009.79)));
    }

    /// Wallet 模式空头一组:开空只扣 fee,买平按 realized - fee 入账
    #[test]
    fn wallet_mode_short_open_close() {
        let mut p = Portfolio::new().with_accounting_mode(AccountingMode::Wallet);
        p.deposit("USDT", dec!(1000));
        p.apply_fill(&usdt_fill("f1", -1, 10000, 10)).unwrap(); // 开空 @ 100,fee 0.10
        assert_eq!(p.positions.get("BTC-USDT").unwrap().quantity, dec!(-1));
        assert_eq!(p.cash.get("USDT"), Some(&dec!(999.90)));

        p.apply_fill(&usdt_fill("f2", 1, 9000, 9)).unwrap(); // 买平 @ 90,fee 0.09
        let pos = p.positions.get("BTC-USDT").expect("position exists");
        assert_eq!(pos.quantity, dec!(0));
        assert_eq!(pos.realized_pnl, dec!(10));
        // 999.90 + (10 - 0.09) = 1009.81
        assert_eq!(p.cash.get("USDT"), Some(&dec!(1009.81)));
    }

    #[test]
    fn apply_fill_crosses_zero_reverses_position() {
        let mut p = Portfolio::new();
        p.deposit("USDT", dec!(1_000_000_000));
        // long 50 @ 50000
        p.apply_fill(&usdt_fill("f1", 50, 5000000, 0)).unwrap();
        // sell 80 @ 52000(平多 50 + 反向开空 30)
        // 平多 realized = (52000 - 50000) * 50 = 100000
        // 反向后 qty = -30, avg_price = 52000
        p.apply_fill(&usdt_fill("f2", -80, 5200000, 0)).unwrap();

        let pos = p.positions.get("BTC-USDT").unwrap();
        assert_eq!(pos.quantity, dec!(-30));
        assert_eq!(pos.avg_price, dec!(52000));
        assert_eq!(pos.realized_pnl, dec!(100000));
    }

    #[test]
    fn apply_fill_zero_quantity_returns_error() {
        let mut p = Portfolio::new();
        p.deposit("USDT", dec!(100000));
        let result = p.apply_fill(&usdt_fill("f1", 0, 5000000, 0));
        assert_eq!(
            result,
            Err(PortfolioError::ZeroFill {
                fill_id: "f1".into()
            })
        );
    }

    #[test]
    fn apply_fill_insufficient_cash_returns_error() {
        let mut p = Portfolio::new();
        p.deposit("USDT", dec!(100));
        // buy 1 @ 50000:需要 50000 + fee,只有 100
        let result = p.apply_fill(&usdt_fill("f1", 1, 5000000, 0));
        assert_eq!(
            result,
            Err(PortfolioError::InsufficientCash {
                currency: "USDT".into(),
                need: dec!(50000),
                have: dec!(100),
            })
        );
        // 失败时 portfolio 状态不变
        assert_eq!(p.cash.get("USDT"), Some(&dec!(100)));
        assert!(p.positions.is_empty());
    }

    #[test]
    fn apply_fill_multi_currency_cash_tracked() {
        let mut p = Portfolio::new();
        p.deposit("USDT", dec!(100000));
        p.apply_fill(&usdt_fill("f1", 1, 5000000, 0)).unwrap(); // buy BTC
        // cash 减少到 50000
        assert_eq!(p.cash.get("USDT"), Some(&dec!(50000)));

        // 注:ETH-USDT 也只走 USDT cash(quote hardcode),positions 分别跟踪
        p.apply_fill(&Fill {
            fill_id: "f2".into(),
            symbol: "ETH-USDT".into(),
            instrument: None,
            price: dec!(3000),
            quantity: dec!(1),
            fee: dec!(0),
            timestamp: Utc::now(),
        })
        .unwrap();
        assert_eq!(p.cash.get("USDT"), Some(&dec!(47000)));
        assert!(p.positions.contains_key("BTC-USDT"));
        assert!(p.positions.contains_key("ETH-USDT"));
    }

    #[test]
    fn snapshot_round_trip_preserves_state() {
        let mut p = Portfolio::new();
        // 50 * 50000 = 2_500_000 需要 deposit 足够 USDT
        p.deposit("USDT", dec!(3_000_000));
        p.apply_fill(&usdt_fill("f1", 50, 5000000, 0)).unwrap();

        let snap = p.snapshot();
        let p2 = Portfolio::from_snapshot(snap);
        assert_eq!(p, p2);
    }

    #[test]
    fn apply_fill_empty_portfolio_initializes_position() {
        let mut p = Portfolio::new();
        // 10 * 50000 = 500000 需要 deposit 足够 USDT
        p.deposit("USDT", dec!(1_000_000));
        p.apply_fill(&usdt_fill("f1", 10, 5000000, 0)).unwrap();
        let pos = p.positions.get("BTC-USDT").expect("position initialized");
        assert_eq!(pos.symbol, "BTC-USDT");
        assert_eq!(pos.quantity, dec!(10));
        assert_eq!(pos.avg_price, dec!(50000));
        assert_eq!(pos.realized_pnl, dec!(0));
        assert!(pos.updated_at <= Utc::now());
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;
    use rust_decimal_macros::dec;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(50))]

        #[test]
        fn weighted_avg_cost_basis_invariant(
            qty_a in 1u32..1000u32,
            price_a in 1u32..1_000_000u32,
            qty_b in 1u32..1000u32,
            price_b in 1u32..1_000_000u32,
        ) {
            // deposit 10B USDT:最坏 case qty*price * 2 = 999*999999*2 ≈ 2e9,留 ~5x buffer
            let mut p = Portfolio::new();
            p.deposit("USDT", dec!(10_000_000_000));
            p.apply_fill(&Fill {
                fill_id: "f1".into(),
                symbol: "BTC-USDT".into(),
                instrument: None,
                price: Decimal::from(price_a),
                quantity: Decimal::from(qty_a),
                fee: dec!(0),
                timestamp: Utc::now(),
            }).unwrap();
            p.apply_fill(&Fill {
                fill_id: "f2".into(),
                symbol: "BTC-USDT".into(),
                instrument: None,
                price: Decimal::from(price_b),
                quantity: Decimal::from(qty_b),
                fee: dec!(0),
                timestamp: Utc::now(),
            }).unwrap();
            let pos = p.positions.get("BTC-USDT").unwrap();
            let total_qty = qty_a + qty_b;
            let total_cost = Decimal::from(qty_a) * Decimal::from(price_a)
                           + Decimal::from(qty_b) * Decimal::from(price_b);
            let expected_avg = total_cost / Decimal::from(total_qty);
            // 容差 1e-9:Decimal 精确除法,实际差应为 0
            let diff = (pos.avg_price - expected_avg).abs();
            prop_assert!(diff < dec!(0.000000001),
                "avg {} != expected {} (diff {})", pos.avg_price, expected_avg, diff);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn sell_does_not_change_avg_price(
            initial_qty in 1u32..1000u32,
            initial_price in 1u32..1_000_000u32,
            sell_qty in 1u32..500u32,
        ) {
            // 不跨 0:sell_qty < initial_qty
            prop_assume!(sell_qty < initial_qty);
            // deposit 足够覆盖初始 buy(initial_qty * initial_price 最大 ~1e9)
            let mut p = Portfolio::new();
            p.deposit("USDT", dec!(10_000_000_000));
            p.apply_fill(&Fill {
                fill_id: "f1".into(),
                symbol: "BTC-USDT".into(),
                instrument: None,
                price: Decimal::from(initial_price),
                quantity: Decimal::from(initial_qty),
                fee: dec!(0),
                timestamp: Utc::now(),
            }).unwrap();
            let avg_before = p.positions.get("BTC-USDT").unwrap().avg_price;
            p.apply_fill(&Fill {
                fill_id: "f2".into(),
                symbol: "BTC-USDT".into(),
                instrument: None,
                price: dec!(99999),
                quantity: -Decimal::from(sell_qty),
                fee: dec!(0),
                timestamp: Utc::now(),
            }).unwrap();
            let avg_after = p.positions.get("BTC-USDT").unwrap().avg_price;
            prop_assert_eq!(avg_before, avg_after);
        }
    }
}
