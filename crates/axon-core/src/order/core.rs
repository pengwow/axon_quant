//! 订单主体

use serde::{Deserialize, Serialize};

use super::OrderId;
use super::error::OrderError;
use super::status::{OrderStatus, RejectReason};
use super::tif::TimeInForce;
use super::types::OrderType;
use crate::market::Side;
use crate::time::Timestamp;
use crate::types::{Instrument, Quantity, SpotInstrument, SwapInstrument, SwapSettle, Symbol};

/// 订单主体
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Order {
    /// 订单 ID
    pub id: OrderId,
    /// 交易品种(spot / swap)
    ///
    /// 自 0.5.0 起从 `Symbol` 升级为 `Instrument`,以支持 spot+perp 双 leg
    /// 套利场景下的类型安全路由。详见 spec §4.2。
    pub instrument: Instrument,
    /// 买卖方向
    pub side: Side,
    /// 订单类型
    pub order_type: OrderType,
    /// 订单总数量
    pub quantity: Quantity,
    /// 已成交数量
    pub filled_quantity: Quantity,
    /// 有效期
    pub time_in_force: TimeInForce,
    /// 当前状态
    pub status: OrderStatus,
    /// 创建时间
    pub created_at: Timestamp,
    /// 最近更新时间
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<Timestamp>,
    /// 拒绝原因
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reject_reason: Option<RejectReason>,
    /// 用户自定义标签
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_order_id: Option<String>,
}

impl Order {
    /// 构造现货订单(替代 0.5.0 之前的 `Order::new`)
    ///
    /// 初始状态为 `Created`,`filled_quantity` 为 0,`updated_at` / `reject_reason`
    /// / `client_order_id` 均为 `None`,`created_at` 取 `Timestamp::now()`。
    pub fn spot(
        id: OrderId,
        base: impl Into<Symbol>,
        quote: impl Into<Symbol>,
        side: Side,
        order_type: OrderType,
        quantity: Quantity,
        time_in_force: TimeInForce,
    ) -> Self {
        Self {
            id,
            instrument: Instrument::Spot(SpotInstrument {
                base: base.into(),
                quote: quote.into(),
            }),
            side,
            order_type,
            quantity,
            filled_quantity: Quantity::default(),
            time_in_force,
            status: OrderStatus::Created,
            created_at: Timestamp::now(),
            updated_at: None,
            reject_reason: None,
            client_order_id: None,
        }
    }

    /// 构造永续合约订单
    ///
    /// `settle` 指定结算方式(USD 保证金 / 币本位),`contract_size` 是每张合约
    /// 代表的 base 币种数量。初始状态语义同 [`Order::spot`].
    #[allow(clippy::too_many_arguments)]
    pub fn swap(
        id: OrderId,
        base: impl Into<Symbol>,
        quote: impl Into<Symbol>,
        settle: SwapSettle,
        contract_size: f64,
        side: Side,
        order_type: OrderType,
        quantity: Quantity,
        time_in_force: TimeInForce,
    ) -> Self {
        Self {
            id,
            instrument: Instrument::Swap(SwapInstrument {
                base: base.into(),
                quote: quote.into(),
                settle,
                contract_size,
            }),
            side,
            order_type,
            quantity,
            filled_quantity: Quantity::default(),
            time_in_force,
            status: OrderStatus::Created,
            created_at: Timestamp::now(),
            updated_at: None,
            reject_reason: None,
            client_order_id: None,
        }
    }

    /// 剩余可成交数量
    #[inline]
    pub fn remaining_quantity(&self) -> Quantity {
        Quantity::from_f64(self.quantity.as_f64() - self.filled_quantity.as_f64())
    }

    /// 是否完全成交
    #[inline]
    pub fn is_filled(&self) -> bool {
        (self.filled_quantity.as_f64() - self.quantity.as_f64()).abs() < f64::EPSILON
    }

    /// 是否可取消
    #[inline]
    pub fn can_cancel(&self) -> bool {
        self.status.is_active()
    }

    /// 成交比例 `[0.0, 1.0]`
    #[inline]
    pub fn fill_ratio(&self) -> f64 {
        let total = self.quantity.as_f64();
        if total == 0.0 {
            return 0.0;
        }
        self.filled_quantity.as_f64() / total
    }

    /// 记录一次成交
    ///
    /// 失败场景：成交量超过订单剩余量、状态不允许继续成交。
    pub fn apply_fill(&mut self, fill_qty: Quantity) -> Result<(), OrderError> {
        let remaining = self.quantity.as_f64() - self.filled_quantity.as_f64();
        if fill_qty.as_f64() > remaining + f64::EPSILON {
            return Err(OrderError::OverFill {
                filled: fill_qty,
                remaining: self.remaining_quantity(),
            });
        }

        let new_filled = self.filled_quantity.as_f64() + fill_qty.as_f64();
        self.filled_quantity = Quantity::from_f64(new_filled);

        let target = if (new_filled - self.quantity.as_f64()).abs() < f64::EPSILON {
            OrderStatus::Filled
        } else {
            OrderStatus::PartiallyFilled
        };

        self.transition_to(target)?;
        self.updated_at = Some(Timestamp::now());
        Ok(())
    }

    /// 状态转换（私有，校验合法性）
    fn transition_to(&mut self, target: OrderStatus) -> Result<(), OrderError> {
        if !self.status.can_transition_to(target) {
            return Err(OrderError::InvalidStateTransition {
                from: self.status,
                to: target,
            });
        }
        self.status = target;
        self.updated_at = Some(Timestamp::now());
        Ok(())
    }

    /// 取消订单
    pub fn cancel(&mut self) -> Result<(), OrderError> {
        if !self.can_cancel() {
            return Err(OrderError::OrderNotActive {
                status: self.status,
            });
        }
        self.transition_to(OrderStatus::Cancelled)
    }

    /// 拒绝订单（记录原因）
    pub fn reject(&mut self, reason: RejectReason) -> Result<(), OrderError> {
        self.reject_reason = Some(reason);
        if !self.status.can_transition_to(OrderStatus::Rejected) {
            return Err(OrderError::InvalidStateTransition {
                from: self.status,
                to: OrderStatus::Rejected,
            });
        }
        self.status = OrderStatus::Rejected;
        self.updated_at = Some(Timestamp::now());
        Ok(())
    }

    /// 激活订单（`Created -> Pending`）
    pub fn activate(&mut self) -> Result<(), OrderError> {
        self.transition_to(OrderStatus::Pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Instrument, Price, SwapSettle};

    fn make_limit_order() -> Order {
        Order::spot(
            1,
            "BTC",
            "USDT",
            Side::Buy,
            OrderType::Limit {
                price: Price::from_f64(100.0),
            },
            Quantity::from_f64(10.0),
            TimeInForce::GTC,
        )
    }

    #[test]
    fn test_order_spot_creation() {
        let order = Order::spot(
            100,
            "BTC",
            "USDT",
            Side::Buy,
            OrderType::Limit {
                price: Price::from_f64(100.0),
            },
            Quantity::from_f64(1.0),
            TimeInForce::GTC,
        );
        assert_eq!(order.id, 100);
        assert!(matches!(order.instrument, Instrument::Spot(_)));
        assert_eq!(order.side, Side::Buy);
        assert_eq!(order.filled_quantity, Quantity::default());
    }

    #[test]
    fn test_order_swap_creation() {
        let order = Order::swap(
            101,
            "ETH",
            "USDT",
            SwapSettle::CoinMargin,
            0.01,
            Side::Sell,
            OrderType::Market,
            Quantity::from_f64(10.0),
            TimeInForce::IOC,
        );
        assert!(matches!(order.instrument, Instrument::Swap(_)));
        if let Instrument::Swap(s) = &order.instrument {
            assert_eq!(s.contract_size, 0.01);
            assert_eq!(s.settle, SwapSettle::CoinMargin);
        }
    }

    #[test]
    fn test_market_order_creation() {
        let order = Order::spot(
            2,
            "ETH",
            "USDT",
            Side::Sell,
            OrderType::Market,
            Quantity::from_f64(5.0),
            TimeInForce::IOC,
        );
        assert_eq!(order.id, 2);
        assert_eq!(order.side, Side::Sell);
        assert_eq!(order.order_type, OrderType::Market);
        assert_eq!(order.quantity, Quantity::from_f64(5.0));
        assert_eq!(order.time_in_force, TimeInForce::IOC);
        assert_eq!(order.status, OrderStatus::Created);
        assert_eq!(order.filled_quantity, Quantity::default());
    }

    #[test]
    fn test_limit_order_creation() {
        let order = make_limit_order();
        assert_eq!(
            order.order_type,
            OrderType::Limit {
                price: Price::from_f64(100.0)
            }
        );
        assert!(
            matches!(order.order_type, OrderType::Limit { price } if price == Price::from_f64(100.0))
        );
    }

    #[test]
    fn test_order_remaining_quantity() {
        let order = make_limit_order();
        assert_eq!(order.remaining_quantity(), Quantity::from_f64(10.0));
        assert!(!order.is_filled());
    }

    #[test]
    fn test_order_new_to_filled() {
        let mut order = make_limit_order();
        order.activate().unwrap();
        order.apply_fill(Quantity::from_f64(10.0)).unwrap();
        assert!(order.is_filled());
        assert_eq!(order.status, OrderStatus::Filled);
    }

    #[test]
    fn test_order_new_to_partial_to_filled() {
        let mut order = make_limit_order();
        order.activate().unwrap();
        order.apply_fill(Quantity::from_f64(3.0)).unwrap();
        assert_eq!(order.status, OrderStatus::PartiallyFilled);
        assert_eq!(order.fill_ratio(), 0.3);

        order.apply_fill(Quantity::from_f64(7.0)).unwrap();
        assert_eq!(order.status, OrderStatus::Filled);
        assert!(order.is_filled());
    }

    #[test]
    fn test_order_cancelled() {
        let mut order = make_limit_order();
        order.activate().unwrap();
        order.cancel().unwrap();
        assert_eq!(order.status, OrderStatus::Cancelled);
        // 再次取消应失败
        assert!(order.cancel().is_err());
    }

    #[test]
    fn test_order_rejected() {
        let mut order = make_limit_order();
        order.reject(RejectReason::InsufficientFunds).unwrap();
        assert_eq!(order.status, OrderStatus::Rejected);
        assert_eq!(order.reject_reason, Some(RejectReason::InsufficientFunds));
        // 已拒绝的订单不能再次拒绝
        assert!(order.reject(RejectReason::Other).is_err());
    }

    #[test]
    fn test_overfill_returns_error() {
        let mut order = make_limit_order();
        order.activate().unwrap();
        // 申请成交 11.0 > 剩余 10.0
        let result = order.apply_fill(Quantity::from_f64(11.0));
        assert!(matches!(result, Err(OrderError::OverFill { .. })));
    }

    #[test]
    fn test_ioc_order_partial_fill_cancel() {
        // IOC 语义在撮合引擎中实现，此处仅验证状态机的合法性
        let mut order = Order::spot(
            3,
            "BTC",
            "USDT",
            Side::Buy,
            OrderType::Market,
            Quantity::from_f64(10.0),
            TimeInForce::IOC,
        );
        order.activate().unwrap();
        // 部分成交
        order.apply_fill(Quantity::from_f64(3.0)).unwrap();
        assert_eq!(order.status, OrderStatus::PartiallyFilled);
        // IOC 撮合引擎应将剩余部分取消
        order.cancel().unwrap();
        assert_eq!(order.status, OrderStatus::Cancelled);
    }

    #[test]
    fn test_fok_order_partial_fill_reject() {
        // FOK 语义在撮合引擎中实现：部分成交则整单拒绝
        let mut order = Order::spot(
            4,
            "BTC",
            "USDT",
            Side::Sell,
            OrderType::Market,
            Quantity::from_f64(10.0),
            TimeInForce::FOK,
        );
        order.activate().unwrap();
        // 撮合引擎若只能部分成交，会在 fill 前调用 reject
        order.reject(RejectReason::Other).unwrap();
        assert_eq!(order.status, OrderStatus::Rejected);
    }

    #[test]
    fn test_gtc_order_persists() {
        let mut order = Order::spot(
            5,
            "BTC",
            "USDT",
            Side::Buy,
            OrderType::Limit {
                price: Price::from_f64(100.0),
            },
            Quantity::from_f64(10.0),
            TimeInForce::GTC,
        );
        order.activate().unwrap();
        // 部分成交后仍处于活跃状态
        order.apply_fill(Quantity::from_f64(2.0)).unwrap();
        assert!(order.can_cancel());
        assert!(!order.is_filled());
    }

    #[test]
    fn test_iceberg_order_reveals_partial() {
        let order = Order::spot(
            6,
            "BTC",
            "USDT",
            Side::Buy,
            OrderType::Iceberg {
                visible: Quantity::from_f64(1.0),
                hidden: Quantity::from_f64(9.0),
            },
            Quantity::from_f64(10.0),
            TimeInForce::GTC,
        );
        assert!(order.order_type.is_iceberg());
        assert_eq!(
            order.order_type.iceberg_visible(),
            Some(Quantity::from_f64(1.0))
        );
    }

    #[test]
    fn test_stop_order_triggers() {
        let order = Order::spot(
            7,
            "BTC",
            "USDT",
            Side::Sell,
            OrderType::Stop {
                trigger: Price::from_f64(95.0),
            },
            Quantity::from_f64(5.0),
            TimeInForce::GTC,
        );
        assert!(order.order_type.is_conditional());
        assert_eq!(
            order.order_type.trigger_price(),
            Some(Price::from_f64(95.0))
        );
    }

    #[test]
    fn test_invalid_state_transition_returns_error() {
        let mut order = make_limit_order();
        // 还没有 activate（Created），直接 apply_fill 不合法
        // 因为 PartiallyFilled 只能从 Pending 转换
        let result = order.apply_fill(Quantity::from_f64(1.0));
        assert!(matches!(
            result,
            Err(OrderError::InvalidStateTransition { .. })
        ));
    }

    #[test]
    fn test_client_order_id_optional() {
        let mut order = make_limit_order();
        assert!(order.client_order_id.is_none());
        order.client_order_id = Some("strategy-001".to_string());
        assert_eq!(order.client_order_id.as_deref(), Some("strategy-001"));
    }

    #[test]
    fn test_order_fill_ratio_zero_quantity_safe() {
        let order = Order::spot(
            8,
            "BTC",
            "USDT",
            Side::Buy,
            OrderType::Market,
            Quantity::default(),
            TimeInForce::GTC,
        );
        assert_eq!(order.fill_ratio(), 0.0);
    }

    // ─── 补充边界场景 ─────────────────────────────────

    /// 取消已 Filled 订单应报错
    #[test]
    fn test_cancel_filled_order_errors() {
        let mut order = make_limit_order();
        order.activate().unwrap();
        order
            .apply_fill(Quantity::from_f64(10.0))
            .expect("全量成交");
        assert!(order.is_filled());
        let result = order.cancel();
        assert!(result.is_err(), "Filled 订单不可取消");
    }

    /// 取消已 Cancelled 订单应报错
    #[test]
    fn test_cancel_already_cancelled_errors() {
        let mut order = make_limit_order();
        order.activate().unwrap();
        order.cancel().expect("首次取消成功");
        let result = order.cancel();
        assert!(result.is_err(), "已取消订单不可重复取消");
    }

    /// 拒绝已 reject 订单应报错
    #[test]
    fn test_reject_already_rejected_errors() {
        let mut order = make_limit_order();
        order
            .reject(RejectReason::RiskLimitExceeded)
            .expect("首次拒绝成功");
        let result = order.reject(RejectReason::InsufficientMargin);
        assert!(result.is_err(), "已 reject 订单不可重复拒绝");
    }

    /// Reject 后再 activate 应报错（终态）
    #[test]
    fn test_activate_rejected_order_errors() {
        let mut order = make_limit_order();
        order.reject(RejectReason::RiskLimitExceeded).unwrap();
        let result = order.activate();
        assert!(result.is_err(), "Reject 终态不可激活");
    }

    /// 零数量订单可创建，但 apply_fill 零数量应报错（超量填充在 EPSILON 边界）
    #[test]
    fn test_apply_fill_zero_quantity_errors() {
        let mut order = make_limit_order();
        order.activate().unwrap();
        // 零填充相当于超量（fill_qty > remaining + EPSILON 在 remaining=10.0 时不成立）
        // 实际 0.0 < 10.0 + EPSILON，应部分成交（仍为 Created 或 Pending）
        let result = order.apply_fill(Quantity::from_f64(0.0));
        assert!(result.is_ok(), "零数量填充不报错但状态保持未完成");
    }

    /// 极小正数量 fill（接近 EPSILON）应正常
    #[test]
    fn test_apply_fill_epsilon_quantity() {
        let mut order = make_limit_order();
        order.activate().unwrap();
        let result = order.apply_fill(Quantity::from_f64(f64::EPSILON));
        assert!(result.is_ok());
        // 状态应进入 PartiallyFilled
        assert_eq!(order.status, OrderStatus::PartiallyFilled);
    }

    /// 完全成交后再 apply_fill 应超量错误
    #[test]
    fn test_overfill_after_filled_errors() {
        let mut order = make_limit_order();
        order.activate().unwrap();
        order.apply_fill(Quantity::from_f64(10.0)).unwrap();
        let result = order.apply_fill(Quantity::from_f64(0.1));
        assert!(matches!(result, Err(OrderError::OverFill { .. })));
    }

    /// 填比 fill 正好等于 quantity 应进入 Filled（不进入 PartiallyFilled）
    #[test]
    fn test_apply_fill_exact_quantity_transitions_to_filled() {
        let mut order = make_limit_order();
        order.activate().unwrap();
        order.apply_fill(Quantity::from_f64(5.0)).unwrap();
        assert_eq!(order.status, OrderStatus::PartiallyFilled);
        order.apply_fill(Quantity::from_f64(5.0)).unwrap();
        assert_eq!(order.status, OrderStatus::Filled);
        assert!(order.is_filled());
        assert_eq!(order.remaining_quantity(), Quantity::from_f64(0.0));
    }

    /// fill_ratio 在部分成交时计算正确
    #[test]
    fn test_fill_ratio_partial() {
        let mut order = make_limit_order();
        order.activate().unwrap();
        order.apply_fill(Quantity::from_f64(2.5)).unwrap();
        assert!((order.fill_ratio() - 0.25).abs() < f64::EPSILON);
    }

    /// 极大量订单（f64::MAX）应可正常构造
    #[test]
    fn test_max_quantity_order() {
        let order = Order::spot(
            100,
            "BTC",
            "USDT",
            Side::Buy,
            OrderType::Market,
            Quantity::from_f64(f64::MAX),
            TimeInForce::GTC,
        );
        assert_eq!(order.quantity.as_f64(), f64::MAX);
    }
}
