//! 每笔 fill 记录
//!
//! 与 [`TradeRecord`](crate::portfolio::TradeRecord) 区别:
//! - `TradeRecord` 只记录 **round-trip**(开/平仓配对 + 已实现 PnL),0.6.0 之前已存在
//! - `FillRecord` 记录 **每笔 fill** —— 0.7.0 新增,补齐 L3 级别的可观测性:
//!   - 同向加仓(没有 round-trip close,不开新 trade 但有 fill)
//!   - 反手 / 部分平仓(产生的每笔 fill 都有独立 ID,信息可审计)
//!   - partial fill(同一 order_id 多次 partial fill)
//!
//! 热路径考虑:每根 bar 最多几次 fill,允许额外字段(不像 `Trade` 必须 40 字节紧凑)。
//! 不进 hot path,走 `RunResult.fills_detail: Vec<FillRecord>` 审计/PnL 拆分。

use serde::{Deserialize, Serialize};

use crate::market::Side;
use crate::time::Timestamp;
use crate::types::{Instrument, Price, Quantity};

/// 单笔 fill 的完整记录(0.7.0 新增)
///
/// 字段:
/// - `timestamp` / `price` / `quantity` 来自 `MatchFill`
/// - `instrument` 由 backtest 在 `apply_fill` 时注入(0.6.0 multi-leg 起)
/// - `taker_order_id` / `maker_order_id` / `taker_side` 来自 `MatchFill`(`taker_side` 由 backtest 从 submit 时传入的 side 注入)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FillRecord {
    /// fill 发生时间
    pub timestamp: Timestamp,
    /// fill 所属 instrument(0.6.0 multi-leg 起,backtest 在 `apply_fill` 时注入)
    pub instrument: Instrument,
    /// 主动方订单 ID(消耗流动性的吃单方)
    pub taker_order_id: u64,
    /// 被动方订单 ID(提供流动性的挂单方)
    pub maker_order_id: u64,
    /// 主动方方向(由 backtest 在 `apply_fill(instrument, side, fill)` 传入)
    pub taker_side: Side,
    /// 成交价格
    pub price: Price,
    /// 成交量
    pub quantity: Quantity,
}

impl FillRecord {
    /// 构造新 fill 记录
    ///
    /// # 参数
    ///
    /// - `timestamp` / `price` / `quantity`:直接来自 `MatchFill`
    /// - `instrument`:backtest 在 `apply_fill` 时持有的 instrument(0.6.0 起)
    /// - `taker_order_id` / `maker_order_id` / `taker_side`:来自 `MatchFill` 加上 backtest 传入的 taker side
    pub fn new(
        timestamp: Timestamp,
        instrument: Instrument,
        taker_order_id: u64,
        maker_order_id: u64,
        taker_side: Side,
        price: Price,
        quantity: Quantity,
    ) -> Self {
        Self {
            timestamp,
            instrument,
            taker_order_id,
            maker_order_id,
            taker_side,
            price,
            quantity,
        }
    }

    /// fill 成交金额(单位 base×quote,f64)
    #[inline]
    pub fn turnover(&self) -> f64 {
        self.price.as_f64() * self.quantity.as_f64()
    }
}
