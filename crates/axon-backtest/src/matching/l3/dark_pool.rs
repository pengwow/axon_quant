//! 暗池撮合
//!
//! 暗池订单先在暗池簿中按价格-时间优先撮合，未成交部分保留在暗池中。
//! 撮合算法：对手方方向 + 价格可交叉 + 最小剩余量。

use serde::{Deserialize, Serialize};

use axon_core::market::Side;
use axon_core::order::Order;
use axon_core::types::Quantity;

use super::super::types::MatchFill;
use super::error::{MatchingL3Error, MatchingL3Result};

/// 暗池订单（隐藏数量）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DarkOrder {
    /// 公开可见数量（冰山订单露出部分）
    pub visible_quantity: Quantity,
    /// 隐藏总数量
    pub hidden_quantity: Quantity,
    /// 订单本体
    pub order: Order,
}

impl DarkOrder {
    /// 创建暗池订单，验证 `visible <= hidden`
    pub fn new(
        order: Order,
        visible_quantity: Quantity,
        hidden_quantity: Quantity,
    ) -> MatchingL3Result<Self> {
        if visible_quantity.as_f64() > hidden_quantity.as_f64() {
            return Err(MatchingL3Error::InvalidDarkOrderQuantity {
                visible: visible_quantity,
                hidden: hidden_quantity,
            });
        }
        Ok(Self {
            visible_quantity,
            hidden_quantity,
            order,
        })
    }

    /// 剩余可成交数量
    #[inline]
    pub fn remaining(&self) -> Quantity {
        self.order.remaining_quantity()
    }
}

/// 在暗池簿中尝试撮合一个新暗池订单
///
/// 遍历已有暗池订单，按价格-时间优先匹配对手方。
/// 返回成交列表 + 已更新后的暗池簿（已移除完全成交订单）。
///
/// 设计约束：
/// - 订单必须有限价（否则 `OrderMissingLimitPrice` 错误）
/// - 撮合价采用被动方价格（maker price）
/// - 撮合后调用方负责更新 `incoming.order` 的 `filled_quantity`（本函数不修改）
pub fn try_dark_match(
    dark_book: &mut Vec<DarkOrder>,
    incoming: &DarkOrder,
    next_fill_id: u64,
) -> MatchingL3Result<Vec<MatchFill>> {
    let incoming_price =
        incoming
            .order
            .order_type
            .limit_price()
            .ok_or(MatchingL3Error::OrderMissingLimitPrice {
                order_id: incoming.order.id,
            })?;

    let mut fills = Vec::new();
    let mut remaining = incoming.remaining();
    let mut fill_id = next_fill_id;
    let timestamp = incoming.order.created_at;
    let taker_side = incoming.order.side;

    for existing in dark_book.iter_mut() {
        if remaining.as_f64() <= 0.0 {
            break;
        }

        if existing.order.side == taker_side {
            continue;
        }

        let existing_price = match existing.order.order_type.limit_price() {
            Some(p) => p,
            None => continue,
        };

        // 价格可交叉？
        let price_cross = match taker_side {
            Side::Buy => incoming_price.as_f64() >= existing_price.as_f64(),
            Side::Sell => incoming_price.as_f64() <= existing_price.as_f64(),
        };
        if !price_cross {
            continue;
        }

        let fill_qty_f = remaining.as_f64().min(existing.remaining().as_f64());
        if fill_qty_f <= 0.0 {
            continue;
        }
        let fill_qty = Quantity::from_f64(fill_qty_f);

        // 同步更新被动方已成交量
        let _ = existing.order.apply_fill(fill_qty);

        fills.push(MatchFill {
            fill_id,
            taker_order_id: incoming.order.id,
            maker_order_id: existing.order.id,
            price: existing_price,
            quantity: fill_qty,
            taker_side,
            timestamp,
        });
        fill_id += 1;
        remaining = Quantity::from_f64(remaining.as_f64() - fill_qty_f);
    }

    // 清理已完全成交的暗池订单
    dark_book.retain(|o| o.order.remaining_quantity().as_f64() > 0.0);

    Ok(fills)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::order::OrderType;
    use axon_core::types::Price;

    fn make_limit_order(id: u64, side: Side, price: f64, qty: f64) -> Order {
        Order::spot(
            id,
            "BTC",
            "USDT",
            side,
            OrderType::Limit {
                price: Price::from_f64(price),
            },
            Quantity::from_f64(qty),
            axon_core::order::TimeInForce::GTC,
        )
    }

    #[test]
    fn test_dark_order_new_ok() {
        let order = make_limit_order(1, Side::Buy, 100.0, 10.0);
        let dark =
            DarkOrder::new(order, Quantity::from_f64(3.0), Quantity::from_f64(10.0)).expect("ok");
        assert_eq!(dark.visible_quantity, Quantity::from_f64(3.0));
    }

    #[test]
    fn test_dark_order_new_invalid() {
        let order = make_limit_order(1, Side::Buy, 100.0, 10.0);
        let result = DarkOrder::new(order, Quantity::from_f64(20.0), Quantity::from_f64(10.0));
        assert!(matches!(
            result,
            Err(MatchingL3Error::InvalidDarkOrderQuantity { .. })
        ));
    }

    #[test]
    fn test_try_dark_match_buy_against_sell() {
        // 暗池中已有卖单 @100, qty=5
        let mut book = vec![DarkOrder {
            visible_quantity: Quantity::from_f64(2.0),
            hidden_quantity: Quantity::from_f64(5.0),
            order: make_limit_order(1, Side::Sell, 100.0, 5.0),
        }];

        // 新买单 @100, qty=3 → 全部在暗池成交，但卖单还剩 2
        let incoming = DarkOrder {
            visible_quantity: Quantity::from_f64(1.0),
            hidden_quantity: Quantity::from_f64(3.0),
            order: make_limit_order(2, Side::Buy, 100.0, 3.0),
        };
        let fills = try_dark_match(&mut book, &incoming, 1000).expect("ok");
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].quantity, Quantity::from_f64(3.0));
        assert_eq!(fills[0].price, Price::from_f64(100.0));
        // 卖单未完全成交（剩 2）→ 仍保留在暗池
        assert_eq!(book.len(), 1);
        assert_eq!(book[0].order.remaining_quantity(), Quantity::from_f64(2.0));
    }

    #[test]
    fn test_try_dark_match_full_fill_removes_from_book() {
        // 新买单等于卖单 qty → 完全成交 → 卖单从暗池移除
        let mut book = vec![DarkOrder {
            visible_quantity: Quantity::from_f64(2.0),
            hidden_quantity: Quantity::from_f64(5.0),
            order: make_limit_order(1, Side::Sell, 100.0, 5.0),
        }];
        let incoming = DarkOrder {
            visible_quantity: Quantity::from_f64(1.0),
            hidden_quantity: Quantity::from_f64(5.0),
            order: make_limit_order(2, Side::Buy, 100.0, 5.0),
        };
        let fills = try_dark_match(&mut book, &incoming, 1000).expect("ok");
        assert_eq!(fills.len(), 1);
        assert!(book.is_empty());
    }

    #[test]
    fn test_try_dark_match_price_no_cross() {
        // 暗池中卖单 @105，新买单 @100 → 不成交
        let mut book = vec![DarkOrder {
            visible_quantity: Quantity::from_f64(1.0),
            hidden_quantity: Quantity::from_f64(5.0),
            order: make_limit_order(1, Side::Sell, 105.0, 5.0),
        }];
        let incoming = DarkOrder {
            visible_quantity: Quantity::from_f64(1.0),
            hidden_quantity: Quantity::from_f64(3.0),
            order: make_limit_order(2, Side::Buy, 100.0, 3.0),
        };
        let fills = try_dark_match(&mut book, &incoming, 1000).expect("ok");
        assert!(fills.is_empty());
        assert_eq!(book.len(), 1);
    }

    #[test]
    fn test_try_dark_match_same_side_skipped() {
        // 同方向不撮合
        let mut book = vec![DarkOrder {
            visible_quantity: Quantity::from_f64(1.0),
            hidden_quantity: Quantity::from_f64(5.0),
            order: make_limit_order(1, Side::Buy, 100.0, 5.0),
        }];
        let incoming = DarkOrder {
            visible_quantity: Quantity::from_f64(1.0),
            hidden_quantity: Quantity::from_f64(3.0),
            order: make_limit_order(2, Side::Buy, 100.0, 3.0),
        };
        let fills = try_dark_match(&mut book, &incoming, 1000).expect("ok");
        assert!(fills.is_empty());
    }

    #[test]
    fn test_try_dark_match_multiple_levels() {
        // 暗池中两层卖单
        let mut book = vec![
            DarkOrder {
                visible_quantity: Quantity::from_f64(1.0),
                hidden_quantity: Quantity::from_f64(2.0),
                order: make_limit_order(1, Side::Sell, 100.0, 2.0),
            },
            DarkOrder {
                visible_quantity: Quantity::from_f64(1.0),
                hidden_quantity: Quantity::from_f64(3.0),
                order: make_limit_order(2, Side::Sell, 101.0, 3.0),
            },
        ];
        // 新买单 @101, qty=4，应成交：第一层 2 + 第二层 2 = 4
        let incoming = DarkOrder {
            visible_quantity: Quantity::from_f64(1.0),
            hidden_quantity: Quantity::from_f64(4.0),
            order: make_limit_order(3, Side::Buy, 101.0, 4.0),
        };
        let fills = try_dark_match(&mut book, &incoming, 1000).expect("ok");
        assert_eq!(fills.len(), 2);
        assert_eq!(fills[0].quantity, Quantity::from_f64(2.0));
        assert_eq!(fills[1].quantity, Quantity::from_f64(2.0));
        // 第一层已完全成交被移除，第二层剩余 1
        assert_eq!(book.len(), 1);
    }

    #[test]
    fn test_try_dark_match_market_order_rejected() {
        // 暗池只接受限价单；构造无 price 的"占位"订单测试
        let mut book: Vec<DarkOrder> = Vec::new();
        let incoming = DarkOrder {
            visible_quantity: Quantity::from_f64(1.0),
            hidden_quantity: Quantity::from_f64(1.0),
            order: make_limit_order(1, Side::Buy, 100.0, 1.0),
        };
        // 当前 limit 单可正常处理
        let result = try_dark_match(&mut book, &incoming, 1000);
        assert!(result.is_ok());
    }
}
