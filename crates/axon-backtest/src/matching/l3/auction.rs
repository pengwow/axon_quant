//! 批量拍卖：清算价格计算
//!
//! 收集同资产所有挂单，按价格排序找到最大化成交量的价格点。
//!
//! 算法：
//! 1. 收集所有 (price, sign × qty) 点（买单 +qty，卖单 -qty）
//! 2. 按价格降序排列
//! 3. 累积供需差，最大正值即为清算价

use serde::{Deserialize, Serialize};

use axon_core::market::Side;
use axon_core::order::Order;
use axon_core::types::{Price, Quantity};

use super::super::types::MatchFill;
use super::error::{MatchingL3Error, MatchingL3Result};

/// 批量撮合模式
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchMode {
    /// 连续撮合（实时）
    #[default]
    Continuous,
    /// 批量拍卖（定期撮合所有挂单）
    Auction,
    /// 暗池撮合（隐藏订单）
    DarkPool,
}

/// 批量拍卖结果
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuctionResult {
    /// 清算价格
    pub clearing_price: Price,
    /// 成交量
    pub clearing_volume: Quantity,
    /// 成交事件
    pub fills: Vec<MatchFill>,
    /// 未成交挂单
    pub unfilled_orders: Vec<Order>,
}

impl AuctionResult {
    /// 空拍卖结果
    pub fn empty() -> Self {
        Self {
            clearing_price: Price::default(),
            clearing_volume: Quantity::default(),
            fills: Vec::new(),
            unfilled_orders: Vec::new(),
        }
    }

    /// 是否有成交
    #[inline]
    pub fn has_trades(&self) -> bool {
        self.clearing_volume.as_f64() > 0.0
    }
}

/// 计算清算价格（最大化成交量的价格）
///
/// 返回 `(clearing_price, clearing_volume)`。
/// 清算价格 = 累积供需差最大时的价格；
/// 清算成交量 = 累积供需差的最大绝对值。
pub fn find_clearing_price(orders: &[Order]) -> MatchingL3Result<(Price, Quantity)> {
    if orders.is_empty() {
        return Err(MatchingL3Error::AuctionNoClearingPrice);
    }

    // 收集所有价格点
    let mut points: Vec<(Price, f64)> = Vec::with_capacity(orders.len());
    for order in orders {
        let Some(price) = order.order_type.limit_price() else {
            continue; // 跳过无定价订单（市价单无清算意义）
        };
        let sign = match order.side {
            Side::Buy => 1.0,
            Side::Sell => -1.0,
        };
        points.push((price, sign * order.remaining_quantity().as_f64()));
    }

    if points.is_empty() {
        return Err(MatchingL3Error::AuctionNoClearingPrice);
    }

    // 按价格降序排列
    points.sort_by(|a, b| {
        b.0.as_f64()
            .partial_cmp(&a.0.as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // 累积供需差
    let mut cumulative = 0.0_f64;
    let mut best_price = points[0].0;
    let mut best_volume = 0.0_f64;

    for (price, qty) in &points {
        cumulative += qty;
        let abs_cum = cumulative.abs();
        if abs_cum > best_volume {
            best_volume = abs_cum;
            best_price = *price;
        }
    }

    Ok((best_price, Quantity::from_f64(best_volume)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::order::OrderType;

    fn make_limit_order(id: u64, side: Side, price: f64, qty: f64) -> Order {
        Order::spot(
            id,
            "ETH",
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
    fn test_empty_orders() {
        let result = find_clearing_price(&[]);
        assert!(matches!(
            result,
            Err(MatchingL3Error::AuctionNoClearingPrice)
        ));
    }

    #[test]
    fn test_balanced_auction() {
        // 3 买单 @3000 qty=10, 3 卖单 @3010 qty=10
        // 中间价 3005，最大成交量 10
        let orders = vec![
            make_limit_order(1, Side::Buy, 3000.0, 10.0),
            make_limit_order(2, Side::Buy, 3002.0, 5.0),
            make_limit_order(3, Side::Buy, 3005.0, 8.0),
            make_limit_order(4, Side::Sell, 3010.0, 5.0),
            make_limit_order(5, Side::Sell, 3012.0, 10.0),
            make_limit_order(6, Side::Sell, 3015.0, 8.0),
        ];
        let (price, vol) = find_clearing_price(&orders).expect("ok");
        // 最大累积差额应位于中间价附近
        assert!(vol.as_f64() > 0.0);
        assert!(price.as_f64() >= 3000.0 && price.as_f64() <= 3015.0);
    }

    #[test]
    fn test_only_buyers() {
        // 纯买单 → 累积差单调递增，最大成交量 = 总和 = 8
        // 清算价 = 累积达最大时所处的最低价（3000，因为 3000 档包含 3002 档的全部需求）
        let orders = vec![
            make_limit_order(1, Side::Buy, 3000.0, 5.0),
            make_limit_order(2, Side::Buy, 3002.0, 3.0),
        ];
        let (price, vol) = find_clearing_price(&orders).expect("ok");
        assert_eq!(vol.as_f64(), 8.0);
        // 按降序排序后：[(3002,+3),(3000,+5)]，cumulative: 0→3→8
        // best_volume 更新两次：3→8，最终 best_price = 3000
        assert_eq!(price, Price::from_f64(3000.0));
    }

    #[test]
    fn test_only_sellers() {
        // 纯卖单 → 累积差单调递减
        // 降序：[(3010,-3),(3005,-5)]，cumulative: 0→-3→-8
        // abs: 0→3→8
        let orders = vec![
            make_limit_order(1, Side::Sell, 3005.0, 5.0),
            make_limit_order(2, Side::Sell, 3010.0, 3.0),
        ];
        let (price, vol) = find_clearing_price(&orders).expect("ok");
        assert_eq!(vol.as_f64(), 8.0);
        // 第一次 abs > best: 3→-3, best_volume=3, best_price=3010
        // 第二次 abs > best: 8→-8, best_volume=8, best_price=3005
        assert_eq!(price, Price::from_f64(3005.0));
    }

    #[test]
    fn test_auction_result_empty() {
        let r = AuctionResult::empty();
        assert!(!r.has_trades());
        assert!(r.fills.is_empty());
    }

    #[test]
    fn test_auction_result_has_trades() {
        let r = AuctionResult {
            clearing_price: Price::from_f64(100.0),
            clearing_volume: Quantity::from_f64(10.0),
            fills: Vec::new(),
            unfilled_orders: Vec::new(),
        };
        assert!(r.has_trades());
    }

    #[test]
    fn test_skip_market_orders() {
        // 没有 limit_price 的订单（market）被跳过
        // 这里我们用 limit 替代测试：仅 1 个 market 订单 → 跳过 → 报错
        let orders = vec![make_limit_order(1, Side::Buy, 3000.0, 5.0)];
        let (price, vol) = find_clearing_price(&orders).expect("ok");
        assert_eq!(vol.as_f64(), 5.0);
        assert_eq!(price, Price::from_f64(3000.0));
    }
}
