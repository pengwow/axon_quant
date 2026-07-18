//! 订单事件

use serde::{Deserialize, Serialize};

use crate::order::Order;
use crate::order::OrderId;
use crate::time::Timestamp;
use crate::types::Quantity;

/// 订单事件
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderEvent {
    /// 事件序列号
    pub seq: u64,
    /// 事件时间戳
    pub timestamp: Timestamp,
    /// 订单 ID
    pub order_id: OrderId,
    /// 订单操作
    pub action: OrderAction,
}

/// 订单操作
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum OrderAction {
    /// 订单提交
    Submitted(Order),
    /// 订单取消
    Cancelled(OrderId),
    /// 订单修改
    Modified {
        /// 订单 ID
        order_id: OrderId,
        /// 新的总数量
        new_quantity: Quantity,
    },
    /// 订单拒绝
    Rejected {
        /// 订单 ID
        order_id: OrderId,
        /// 拒绝原因
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::Side;
    use crate::order::{OrderType, TimeInForce};
    use crate::types::Price;

    #[test]
    fn test_order_event_creation() {
        let ts = Timestamp::from_nanos(1_000);
        let order = Order::spot(
            1,
            "BTC",
            "USDT",
            Side::Buy,
            OrderType::Limit {
                price: Price::from_f64(100.0),
            },
            Quantity::from_f64(10.0),
            TimeInForce::GTC,
        );
        let event = OrderEvent {
            seq: 0,
            timestamp: ts,
            order_id: 1,
            action: OrderAction::Submitted(order),
        };
        assert_eq!(event.order_id, 1);
    }

    #[test]
    fn test_order_action_modified() {
        let action = OrderAction::Modified {
            order_id: 42,
            new_quantity: Quantity::from_f64(20.0),
        };
        match action {
            OrderAction::Modified {
                order_id,
                new_quantity,
            } => {
                assert_eq!(order_id, 42);
                assert_eq!(new_quantity, Quantity::from_f64(20.0));
            }
            _ => panic!("expected Modified"),
        }
    }
}
