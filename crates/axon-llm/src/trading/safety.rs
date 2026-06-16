//! 安全模式与风控规则
//!
//! `SafetyMode` 控制 PlaceOrderTool 是否真发订单;
//! `RiskLimits` 叠加在任意模式上做预检;
//! `DailyCounter` 提供单日订单计数。

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::trading::backend::TradingError;
use crate::trading::types::PlaceOrderArgs;

/// 安全模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SafetyMode {
    /// 不真下单,仅 tracing 日志,返回 status="DryRun" 的 OrderAck
    #[default]
    DryRun,
    /// 两次确认:第一次返回 confirm_token,第二次带相同 token 才真发
    TwoPhase,
    /// 直接调后端,无任何拦截
    Direct,
}

/// 风控规则
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RiskLimits {
    /// 单笔最大金额(quantity * price)
    pub max_order_notional: Option<f64>,
    /// 单日最大订单数(进程内计数,重启清零)
    pub max_daily_orders: Option<u32>,
    /// 单 symbol 最大持仓绝对值(本期不实现,留待 OMS 适配 spec 引入)
    pub max_position_abs: Option<f64>,
    /// 允许交易的 symbol 白名单(None = 不限制)
    pub allowed_symbols: Option<Vec<String>>,
}

/// TwoPhase 模式下的待确认订单
#[derive(Debug, Clone)]
pub struct PendingOrder {
    /// 待确认的下单参数
    pub args: PlaceOrderArgs,
    /// 一次性 token(uuid v4)
    pub token: String,
}

impl RiskLimits {
    /// 默认无限制(全部 None)
    pub fn permissive() -> Self {
        Self::default()
    }

    /// 风控预检
    ///
    /// - allowed_symbols:None = 不限制;Some([]) = 拒绝所有
    /// - max_order_notional:None = 不限制;Some(x) = quantity * price <= x
    ///   (price 为 None 的 Market 单本规则不触发,避免误拒市价单)
    /// - max_position_abs:本期不实现(避免 mock 中维护额外状态,留待 OMS 适配 spec 引入)
    ///   注释:位置绝对值检查需要实时持仓上下文,放到具体后端适配 crate 处理
    ///   (见 spec 第 12 节"后续工作")
    /// - max_daily_orders:由调用方在使用 DailyCounter 时检查
    pub fn check(&self, args: &PlaceOrderArgs) -> Result<(), TradingError> {
        // 1. 白名单
        if let Some(allowed) = &self.allowed_symbols
            && !allowed.iter().any(|s| s == &args.symbol)
        {
            return Err(TradingError::RiskRejected(format!(
                "symbol '{}' 不在白名单 {:?} 中",
                args.symbol, allowed
            )));
        }
        // 2. 单笔最大金额(Market 单 price=None → 跳过)
        if let (Some(max), Some(price)) = (self.max_order_notional, args.price) {
            let notional = args.quantity * price;
            if notional > max {
                return Err(TradingError::RiskRejected(format!(
                    "单笔金额 {:.2} 超过限额 {:.2}",
                    notional, max
                )));
            }
        }
        Ok(())
    }
}

/// 进程内单日订单计数器
#[derive(Debug, Default)]
pub struct DailyCounter {
    /// key: "天数(unix_secs / 86400)";value: 当日累计订单数
    inner: Mutex<HashMap<String, u32>>,
}

impl DailyCounter {
    /// 当日计数 +1,若超过 max 则返回错误
    pub fn increment_and_check(&self, max: u32) -> Result<(), TradingError> {
        let today = today_key();
        let mut g = self.inner.lock().expect("poisoned");
        let count = g.entry(today).or_insert(0);
        *count += 1;
        if *count > max {
            return Err(TradingError::RiskRejected(format!(
                "单日订单数 {} 已超过限额 {}",
                *count, max
            )));
        }
        Ok(())
    }
}

/// 用 unix_secs / 86400 作为"天"键(UTC 边界足够,本期不要求本地时区)
fn today_key() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{}", secs / 86_400)
}

// ── 测试 ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trading::types::{OrderKind, OrderSide, TimeInForce};

    fn args(symbol: &str, qty: f64, price: Option<f64>) -> PlaceOrderArgs {
        PlaceOrderArgs {
            symbol: symbol.into(),
            side: OrderSide::Buy,
            quantity: qty,
            order_type: OrderKind::Limit,
            price,
            stop_loss: None,
            take_profit: None,
            time_in_force: TimeInForce::GTC,
            extras: serde_json::Value::Null,
        }
    }

    #[test]
    fn risk_permits_when_no_limits() {
        let l = RiskLimits::permissive();
        assert!(l.check(&args("BTC-USDT", 0.1, Some(50_000.0))).is_ok());
    }

    #[test]
    fn risk_rejects_symbol_not_in_whitelist() {
        let l = RiskLimits {
            allowed_symbols: Some(vec!["ETH-USDT".into()]),
            ..Default::default()
        };
        let e = l.check(&args("BTC-USDT", 0.1, Some(50_000.0))).unwrap_err();
        assert!(matches!(e, TradingError::RiskRejected(_)));
    }

    #[test]
    fn risk_rejects_exceeding_notional() {
        let l = RiskLimits {
            max_order_notional: Some(1_000.0),
            ..Default::default()
        };
        // 0.1 * 50_000 = 5_000 > 1_000
        let e = l.check(&args("BTC-USDT", 0.1, Some(50_000.0))).unwrap_err();
        assert!(matches!(e, TradingError::RiskRejected(_)));
    }

    #[test]
    fn risk_permits_within_notional() {
        let l = RiskLimits {
            max_order_notional: Some(10_000.0),
            ..Default::default()
        };
        assert!(l.check(&args("BTC-USDT", 0.1, Some(50_000.0))).is_ok());
    }

    #[test]
    fn risk_market_order_skips_notional_check() {
        let l = RiskLimits {
            max_order_notional: Some(1.0), // 极小限额
            ..Default::default()
        };
        // Market 单 price=None → 名义金额检查不触发
        let mut a = args("BTC-USDT", 100.0, None);
        a.order_type = OrderKind::Market;
        assert!(l.check(&a).is_ok());
    }

    #[test]
    fn daily_counter_increments_and_blocks() {
        let c = DailyCounter::default();
        c.increment_and_check(2).unwrap();
        c.increment_and_check(2).unwrap();
        let e = c.increment_and_check(2).unwrap_err();
        assert!(matches!(e, TradingError::RiskRejected(_)));
    }

    #[test]
    fn safety_mode_default_is_dry_run() {
        assert_eq!(SafetyMode::default(), SafetyMode::DryRun);
    }

    #[test]
    fn safety_mode_serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&SafetyMode::TwoPhase).unwrap(),
            "\"two_phase\""
        );
        assert_eq!(
            serde_json::to_string(&SafetyMode::Direct).unwrap(),
            "\"direct\""
        );
    }
}
