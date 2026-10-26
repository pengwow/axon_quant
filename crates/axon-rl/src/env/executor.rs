//! 订单执行与组合更新

use crate::action::converter::{Order, OrderSide};
use crate::action::state::PortfolioState;
use crate::env::config::EnvConfig;
use crate::env::cost::CostModel;
use crate::env::error::{EnvError, EnvResult};
use crate::env::types::{ExecutionResult, MarketBar};

/// 订单执行器
pub struct Executor {
    /// 配置（交易成本、初始资金等）
    config: EnvConfig,
    /// 成本模型（与内核共用同一份费率 / 滑点口径）
    cost_model: CostModel,
}

impl Executor {
    /// 构造新执行器
    pub fn new(config: EnvConfig) -> Self {
        let cost_model = CostModel::from_env(&config);
        Self { config, cost_model }
    }

    /// 成本模型（只读）
    pub fn cost_model(&self) -> &CostModel {
        &self.cost_model
    }

    /// 执行订单列表
    ///
    /// 简化执行模型（无撮合延迟，假设按 `bar.close` 成交，含滑点与手续费）。
    /// 返回每个订单的执行结果。
    pub fn execute(
        &self,
        orders: &[Order],
        bar: &MarketBar,
        portfolio: &mut PortfolioState,
    ) -> EnvResult<Vec<ExecutionResult>> {
        let mut results = Vec::with_capacity(orders.len());

        for order in orders {
            let result = self.execute_one(order, bar, portfolio)?;
            results.push(result);
        }

        Ok(results)
    }

    /// 执行单个订单
    fn execute_one(
        &self,
        order: &Order,
        bar: &MarketBar,
        portfolio: &mut PortfolioState,
    ) -> EnvResult<ExecutionResult> {
        if !order.quantity.is_finite() || order.quantity <= 0.0 {
            return Ok(ExecutionResult {
                symbol: order.symbol.clone(),
                side: order.side,
                quantity: 0.0,
                price: bar.close,
                filled: false,
                cost: 0.0,
            });
        }

        // 简化：以 K 线 close 成交，按 slippage 调整价格
        let exec_price = self.cost_model.fill_price(order.side, bar.close);

        let notional = order.quantity * exec_price;
        let cost = self.cost_model.fee(notional);
        let filled = notional > 0.0 && order.quantity > 0.0;

        if filled {
            match order.side {
                OrderSide::Buy => {
                    // 校验现金
                    if portfolio.cash + 1e-9 < notional + cost {
                        return Ok(ExecutionResult {
                            symbol: order.symbol.clone(),
                            side: order.side,
                            quantity: 0.0,
                            price: exec_price,
                            filled: false,
                            cost: 0.0,
                        });
                    }
                    portfolio.cash -= notional + cost;
                    portfolio.position += order.quantity;
                }
                OrderSide::Sell => {
                    // 校验持仓
                    if portfolio.position < order.quantity - 1e-9 {
                        return Ok(ExecutionResult {
                            symbol: order.symbol.clone(),
                            side: order.side,
                            quantity: 0.0,
                            price: exec_price,
                            filled: false,
                            cost: 0.0,
                        });
                    }
                    portfolio.cash += notional - cost;
                    portfolio.position -= order.quantity;
                }
            }
        }

        Ok(ExecutionResult {
            symbol: order.symbol.clone(),
            side: order.side,
            quantity: if filled { order.quantity } else { 0.0 },
            price: exec_price,
            filled,
            cost: if filled { cost } else { 0.0 },
        })
    }

    /// 按最新价重估组合市值
    pub fn revalue(&self, portfolio: &mut PortfolioState, last_price: f64) -> EnvResult<()> {
        if !last_price.is_finite() || last_price <= 0.0 {
            return Err(EnvError::InvalidAction(format!(
                "invalid last_price: {last_price}"
            )));
        }
        portfolio.last_price = last_price;
        let pos_value = portfolio.position * last_price;
        portfolio.portfolio_value = portfolio.cash + pos_value;
        portfolio.unrealized_pnl = pos_value - (portfolio.margin_used);
        Ok(())
    }

    /// 获取配置
    pub fn config(&self) -> &EnvConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::converter::{Order, OrderSide, OrderType};

    fn default_config() -> EnvConfig {
        EnvConfig {
            initial_capital: 100_000.0,
            transaction_cost: 0.001,
            slippage: 0.0005,
            ..Default::default()
        }
    }

    fn empty_portfolio() -> PortfolioState {
        PortfolioState {
            portfolio_value: 100_000.0,
            cash: 100_000.0,
            last_price: 5_000.0,
            ..Default::default()
        }
    }

    #[test]
    fn test_buy_executes_and_updates_cash() {
        let executor = Executor::new(default_config());
        let mut p = empty_portfolio();
        let order = Order {
            symbol: "BTCUSDT".to_string(),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: 1.0,
        };
        let bar = MarketBar::new(0, 0.0, 0.0, 0.0, 5_000.0, 0.0);
        let results = executor.execute(&[order], &bar, &mut p).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].filled);
        assert!(p.position > 0.0);
        assert!(p.cash < 100_000.0);
    }

    #[test]
    fn test_sell_rejected_without_position() {
        let executor = Executor::new(default_config());
        let mut p = empty_portfolio();
        let order = Order {
            symbol: "BTCUSDT".to_string(),
            side: OrderSide::Sell,
            order_type: OrderType::Market,
            quantity: 1.0,
        };
        let bar = MarketBar::new(0, 0.0, 0.0, 0.0, 5_000.0, 0.0);
        let results = executor.execute(&[order], &bar, &mut p).unwrap();
        assert!(!results[0].filled);
    }

    #[test]
    fn test_buy_rejected_insufficient_cash() {
        let executor = Executor::new(default_config());
        let mut p = empty_portfolio();
        p.cash = 100.0; // only 100 USD
        let order = Order {
            symbol: "BTCUSDT".to_string(),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: 1.0, // would cost 5000+
        };
        let bar = MarketBar::new(0, 0.0, 0.0, 0.0, 5_000.0, 0.0);
        let results = executor.execute(&[order], &bar, &mut p).unwrap();
        assert!(!results[0].filled);
    }

    #[test]
    fn test_revalue_updates_portfolio_value() {
        let executor = Executor::new(default_config());
        let mut p = empty_portfolio();
        p.position = 1.0;
        p.cash = 50_000.0;
        executor.revalue(&mut p, 60_000.0).unwrap();
        assert!((p.portfolio_value - 110_000.0).abs() < 1e-6);
    }

    #[test]
    fn test_zero_quantity_no_fill() {
        let executor = Executor::new(default_config());
        let mut p = empty_portfolio();
        let order = Order {
            symbol: "BTCUSDT".to_string(),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: 0.0,
        };
        let bar = MarketBar::new(0, 0.0, 0.0, 0.0, 5_000.0, 0.0);
        let results = executor.execute(&[order], &bar, &mut p).unwrap();
        assert!(!results[0].filled);
    }

    /// 重构前 `execute_one` 的逐字副本，仅用于等价性回归
    ///
    /// 保留旧实现（内联 `bar.close * (1 ± slippage)` + `notional * transaction_cost`），
    /// 与 `CostModel` 版本逐字段比对。任何一处运算顺序变化都会让用例变红。
    fn legacy_execute_one(
        config: &EnvConfig,
        order: &Order,
        bar: &MarketBar,
        portfolio: &mut PortfolioState,
    ) -> ExecutionResult {
        if !order.quantity.is_finite() || order.quantity <= 0.0 {
            return ExecutionResult {
                symbol: order.symbol.clone(),
                side: order.side,
                quantity: 0.0,
                price: bar.close,
                filled: false,
                cost: 0.0,
            };
        }
        let exec_price = match order.side {
            OrderSide::Buy => bar.close * (1.0 + config.slippage),
            OrderSide::Sell => bar.close * (1.0 - config.slippage),
        };
        let notional = order.quantity * exec_price;
        let cost = notional * config.transaction_cost;
        let filled = notional > 0.0 && order.quantity > 0.0;
        if filled {
            match order.side {
                OrderSide::Buy => {
                    if portfolio.cash + 1e-9 < notional + cost {
                        return ExecutionResult {
                            symbol: order.symbol.clone(),
                            side: order.side,
                            quantity: 0.0,
                            price: exec_price,
                            filled: false,
                            cost: 0.0,
                        };
                    }
                    portfolio.cash -= notional + cost;
                    portfolio.position += order.quantity;
                }
                OrderSide::Sell => {
                    if portfolio.position < order.quantity - 1e-9 {
                        return ExecutionResult {
                            symbol: order.symbol.clone(),
                            side: order.side,
                            quantity: 0.0,
                            price: exec_price,
                            filled: false,
                            cost: 0.0,
                        };
                    }
                    portfolio.cash += notional - cost;
                    portfolio.position -= order.quantity;
                }
            }
        }
        ExecutionResult {
            symbol: order.symbol.clone(),
            side: order.side,
            quantity: if filled { order.quantity } else { 0.0 },
            price: exec_price,
            filled,
            cost: if filled { cost } else { 0.0 },
        }
    }

    /// CostModel 重构前后：同一输入下 `ExecutionResult` 逐字段相等
    #[test]
    fn test_cost_model_refactor_is_field_identical_to_legacy() {
        let config = default_config();
        let bar = MarketBar::new(0, 0.0, 0.0, 0.0, 5_000.0, 0.0);
        let orders = [
            // 正常买入
            Order {
                symbol: "BTCUSDT".to_string(),
                side: OrderSide::Buy,
                order_type: OrderType::Market,
                quantity: 1.0,
            },
            // 正常卖出
            Order {
                symbol: "BTCUSDT".to_string(),
                side: OrderSide::Sell,
                order_type: OrderType::Market,
                quantity: 0.5,
            },
            // 无持仓卖出（拒单）
            Order {
                symbol: "BTCUSDT".to_string(),
                side: OrderSide::Sell,
                order_type: OrderType::Market,
                quantity: 100.0,
            },
            // 现金不足（拒单）
            Order {
                symbol: "BTCUSDT".to_string(),
                side: OrderSide::Buy,
                order_type: OrderType::Market,
                quantity: 100.0,
            },
            // 非法数量
            Order {
                symbol: "BTCUSDT".to_string(),
                side: OrderSide::Buy,
                order_type: OrderType::Market,
                quantity: f64::NAN,
            },
        ];

        let mut new_portfolio = PortfolioState {
            portfolio_value: 100_000.0,
            cash: 100_000.0,
            position: 2.0,
            last_price: 5_000.0,
            ..Default::default()
        };
        let mut legacy_portfolio = new_portfolio.clone();

        let executor = Executor::new(config.clone());
        for order in &orders {
            let new_result = executor
                .execute_one(order, &bar, &mut new_portfolio)
                .unwrap();
            let legacy_result = legacy_execute_one(&config, order, &bar, &mut legacy_portfolio);
            assert_eq!(new_result, legacy_result, "订单 {order:?} 结果字段不一致");
        }
        assert_eq!(new_portfolio.cash, legacy_portfolio.cash);
        assert_eq!(new_portfolio.position, legacy_portfolio.position);
    }
}
