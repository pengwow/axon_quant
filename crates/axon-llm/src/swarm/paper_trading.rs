//! PaperTrading 模拟盘验证

use std::collections::HashMap;

use super::message::{MarketSignal, OrderSide, SignalType, TradeOrder};

/// PaperTrading 配置
#[derive(Debug, Clone)]
pub struct PaperTradingConfig {
    /// 初始资金
    pub initial_balance: f64,
    /// 滑点（基点）
    pub slippage_bps: f64,
    /// 手续费（基点）
    pub commission_bps: f64,
}

impl Default for PaperTradingConfig {
    fn default() -> Self {
        Self {
            initial_balance: 100_000.0,
            slippage_bps: 5.0,
            commission_bps: 10.0,
        }
    }
}

/// PaperTrading 组合状态
#[derive(Debug, Clone)]
pub struct PaperPortfolio {
    /// 现金余额
    pub cash: f64,
    /// 持仓
    pub positions: HashMap<String, f64>,
    /// 总收益
    pub total_pnl: f64,
    /// 交易次数
    pub trade_count: usize,
}

impl PaperPortfolio {
    /// 创建新的组合
    pub fn new(initial_balance: f64) -> Self {
        Self {
            cash: initial_balance,
            positions: HashMap::new(),
            total_pnl: 0.0,
            trade_count: 0,
        }
    }

    /// 执行交易
    pub fn execute_trade(
        &mut self,
        order: &TradeOrder,
        price: f64,
        slippage_bps: f64,
        commission_bps: f64,
    ) {
        let adjusted_price = price * (1.0 + slippage_bps / 10000.0);
        let notional = order.quantity * adjusted_price;
        let commission = notional * commission_bps / 10000.0;

        match order.side {
            OrderSide::Buy => {
                let cost = notional + commission;
                if cost <= self.cash {
                    self.cash -= cost;
                    *self.positions.entry(order.symbol.clone()).or_insert(0.0) += order.quantity;
                    self.trade_count += 1;
                }
            }
            OrderSide::Sell => {
                if let Some(qty) = self.positions.get_mut(&order.symbol)
                    && *qty >= order.quantity
                {
                    *qty -= order.quantity;
                    self.cash += notional - commission;
                    self.trade_count += 1;
                }
            }
        }
    }

    /// 计算总价值
    pub fn total_value(&self, prices: &HashMap<String, f64>) -> f64 {
        let position_value: f64 = self
            .positions
            .iter()
            .map(|(symbol, qty)| {
                let price = prices.get(symbol).copied().unwrap_or(0.0);
                qty * price
            })
            .sum();
        self.cash + position_value
    }
}

/// 测试场景
#[derive(Debug, Clone)]
pub struct TestScenario {
    /// 场景名称
    pub name: String,
    /// 市场数据序列
    pub market_data: Vec<MarketSignal>,
    /// 预期结果
    pub expected: ExpectedOutcome,
}

/// 预期结果
#[derive(Debug, Clone)]
pub struct ExpectedOutcome {
    /// 最大回撤限制
    pub max_drawdown: f64,
    /// 最小交易次数
    pub min_trades: usize,
    /// 最大亏损
    pub max_loss: f64,
}

/// 测试报告
#[derive(Debug, Clone)]
pub struct TestReport {
    /// 场景结果
    pub scenarios: Vec<ScenarioResult>,
    /// 总体通过
    pub passed: bool,
}

/// 场景结果
#[derive(Debug, Clone)]
pub struct ScenarioResult {
    /// 场景名称
    pub name: String,
    /// 是否通过
    pub passed: bool,
    /// 最终净值
    pub final_value: f64,
    /// 交易次数
    pub trade_count: usize,
    /// 最大回撤
    pub max_drawdown: f64,
    /// 错误信息
    pub errors: Vec<String>,
}

/// PaperTrading 运行器
pub struct PaperTradingRunner {
    config: PaperTradingConfig,
    portfolio: PaperPortfolio,
}

impl PaperTradingRunner {
    /// 创建新的运行器
    pub fn new(config: PaperTradingConfig) -> Self {
        let portfolio = PaperPortfolio::new(config.initial_balance);
        Self { config, portfolio }
    }

    /// 获取组合状态
    pub fn portfolio(&self) -> &PaperPortfolio {
        &self.portfolio
    }

    /// 模拟执行交易信号
    pub fn simulate_signal(&mut self, signal: &MarketSignal, price: f64) {
        // 简单策略：Buy 信号买入，Sell 信号卖出
        let order = match signal.signal_type {
            SignalType::Buy => Some(TradeOrder {
                symbol: signal.symbol.clone(),
                side: OrderSide::Buy,
                quantity: 0.1, // 固定数量
                order_type: "market".into(),
                price: Some(price),
                reason: signal.reasoning.clone(),
            }),
            SignalType::Sell => {
                // 只有在有持仓时才卖出
                let position = self
                    .portfolio
                    .positions
                    .get(&signal.symbol)
                    .copied()
                    .unwrap_or(0.0);
                if position > 0.0 {
                    Some(TradeOrder {
                        symbol: signal.symbol.clone(),
                        side: OrderSide::Sell,
                        quantity: position,
                        order_type: "market".into(),
                        price: Some(price),
                        reason: signal.reasoning.clone(),
                    })
                } else {
                    None
                }
            }
            SignalType::Hold => None,
        };

        if let Some(order) = order {
            self.portfolio.execute_trade(
                &order,
                price,
                self.config.slippage_bps,
                self.config.commission_bps,
            );
        }
    }

    /// 运行场景测试
    pub fn run_scenario(&mut self, scenario: &TestScenario) -> ScenarioResult {
        let mut prices = HashMap::new();
        let mut max_value = self.config.initial_balance;
        let mut min_value = self.config.initial_balance;
        let mut errors = Vec::new();

        for signal in &scenario.market_data {
            // 更新价格（使用信号中的置信度作为价格代理）
            let price = 50000.0 * signal.confidence;
            prices.insert(signal.symbol.clone(), price);

            // 模拟交易
            self.simulate_signal(signal, price);

            // 更新净值
            let current_value = self.portfolio.total_value(&prices);
            max_value = max_value.max(current_value);
            min_value = min_value.min(current_value);
        }

        let final_value = self.portfolio.total_value(&prices);
        let max_drawdown = if max_value > 0.0 {
            (max_value - min_value) / max_value
        } else {
            0.0
        };

        // 检查预期结果
        let passed = max_drawdown <= scenario.expected.max_drawdown
            && self.portfolio.trade_count >= scenario.expected.min_trades
            && (self.config.initial_balance - final_value) <= scenario.expected.max_loss;

        if max_drawdown > scenario.expected.max_drawdown {
            errors.push(format!(
                "Max drawdown {:.2}% exceeds limit {:.2}%",
                max_drawdown * 100.0,
                scenario.expected.max_drawdown * 100.0
            ));
        }

        ScenarioResult {
            name: scenario.name.clone(),
            passed,
            final_value,
            trade_count: self.portfolio.trade_count,
            max_drawdown,
            errors,
        }
    }

    /// 运行所有场景
    pub fn run_all_scenarios(&mut self, scenarios: &[TestScenario]) -> TestReport {
        let mut results = Vec::new();

        for scenario in scenarios {
            // 重置组合
            self.portfolio = PaperPortfolio::new(self.config.initial_balance);
            let result = self.run_scenario(scenario);
            results.push(result);
        }

        let passed = results.iter().all(|r| r.passed);

        TestReport {
            scenarios: results,
            passed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_paper_portfolio_creation() {
        let portfolio = PaperPortfolio::new(100_000.0);
        assert_eq!(portfolio.cash, 100_000.0);
        assert!(portfolio.positions.is_empty());
        assert_eq!(portfolio.trade_count, 0);
    }

    #[test]
    fn test_paper_portfolio_buy() {
        let mut portfolio = PaperPortfolio::new(100_000.0);
        let order = TradeOrder {
            symbol: "BTC-USDT".into(),
            side: OrderSide::Buy,
            quantity: 0.1,
            order_type: "market".into(),
            price: Some(50000.0),
            reason: "Test".into(),
        };

        portfolio.execute_trade(&order, 50000.0, 5.0, 10.0);

        assert_eq!(portfolio.trade_count, 1);
        assert!(portfolio.cash < 100_000.0);
        assert_eq!(portfolio.positions.get("BTC-USDT"), Some(&0.1));
    }

    #[test]
    fn test_paper_portfolio_sell() {
        let mut portfolio = PaperPortfolio::new(100_000.0);

        // 先买入
        let buy_order = TradeOrder {
            symbol: "BTC-USDT".into(),
            side: OrderSide::Buy,
            quantity: 0.1,
            order_type: "market".into(),
            price: Some(50000.0),
            reason: "Test".into(),
        };
        portfolio.execute_trade(&buy_order, 50000.0, 5.0, 10.0);
        let cash_after_buy = portfolio.cash;

        // 卖出
        let sell_order = TradeOrder {
            symbol: "BTC-USDT".into(),
            side: OrderSide::Sell,
            quantity: 0.1,
            order_type: "market".into(),
            price: Some(51000.0),
            reason: "Test".into(),
        };
        portfolio.execute_trade(&sell_order, 51000.0, 5.0, 10.0);

        assert_eq!(portfolio.trade_count, 2);
        assert!(portfolio.cash > cash_after_buy);
        assert_eq!(portfolio.positions.get("BTC-USDT"), Some(&0.0));
    }

    #[test]
    fn test_paper_trading_runner_creation() {
        let config = PaperTradingConfig::default();
        let runner = PaperTradingRunner::new(config);

        assert_eq!(runner.portfolio().cash, 100_000.0);
    }

    #[test]
    fn test_paper_trading_simulate_buy_signal() {
        let config = PaperTradingConfig::default();
        let mut runner = PaperTradingRunner::new(config);

        let signal = MarketSignal {
            symbol: "BTC-USDT".into(),
            signal_type: SignalType::Buy,
            confidence: 0.9,
            reasoning: "Bullish".into(),
        };

        runner.simulate_signal(&signal, 50000.0);

        assert_eq!(runner.portfolio().trade_count, 1);
        assert!(runner.portfolio().positions.contains_key("BTC-USDT"));
    }

    #[test]
    fn test_paper_trading_simulate_hold_signal() {
        let config = PaperTradingConfig::default();
        let mut runner = PaperTradingRunner::new(config);

        let signal = MarketSignal {
            symbol: "BTC-USDT".into(),
            signal_type: SignalType::Hold,
            confidence: 0.5,
            reasoning: "Neutral".into(),
        };

        runner.simulate_signal(&signal, 50000.0);

        assert_eq!(runner.portfolio().trade_count, 0);
    }

    #[test]
    fn test_paper_trading_run_scenario() {
        let config = PaperTradingConfig::default();
        let mut runner = PaperTradingRunner::new(config);

        let scenario = TestScenario {
            name: "Simple Test".into(),
            market_data: vec![
                MarketSignal {
                    symbol: "BTC-USDT".into(),
                    signal_type: SignalType::Buy,
                    confidence: 0.9,
                    reasoning: "Buy".into(),
                },
                MarketSignal {
                    symbol: "BTC-USDT".into(),
                    signal_type: SignalType::Hold,
                    confidence: 0.95,
                    reasoning: "Hold".into(),
                },
            ],
            expected: ExpectedOutcome {
                max_drawdown: 0.1,
                min_trades: 1,
                max_loss: 10000.0,
            },
        };

        let result = runner.run_scenario(&scenario);

        assert_eq!(result.name, "Simple Test");
        assert_eq!(result.trade_count, 1);
    }

    #[test]
    fn test_paper_trading_run_all_scenarios() {
        let config = PaperTradingConfig::default();
        let mut runner = PaperTradingRunner::new(config);

        let scenarios = vec![
            TestScenario {
                name: "Scenario 1".into(),
                market_data: vec![MarketSignal {
                    symbol: "BTC-USDT".into(),
                    signal_type: SignalType::Buy,
                    confidence: 0.9,
                    reasoning: "Buy".into(),
                }],
                expected: ExpectedOutcome {
                    max_drawdown: 0.2,
                    min_trades: 1,
                    max_loss: 50000.0,
                },
            },
            TestScenario {
                name: "Scenario 2".into(),
                market_data: vec![MarketSignal {
                    symbol: "ETH-USDT".into(),
                    signal_type: SignalType::Hold,
                    confidence: 0.5,
                    reasoning: "Hold".into(),
                }],
                expected: ExpectedOutcome {
                    max_drawdown: 0.1,
                    min_trades: 0,
                    max_loss: 10000.0,
                },
            },
        ];

        let report = runner.run_all_scenarios(&scenarios);

        assert_eq!(report.scenarios.len(), 2);
    }
}
