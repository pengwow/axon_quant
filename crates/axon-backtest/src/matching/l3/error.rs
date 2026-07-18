//! L3 撮合引擎错误类型

use thiserror::Error;

use axon_core::types::{Instrument, Quantity};

use super::super::error::MatchingError;

/// L3 撮合引擎错误
#[derive(Debug, Error)]
pub enum MatchingL3Error {
    /// 资产不存在
    #[error("资产不存在：{instrument}")]
    AssetNotFound {
        /// 未注册的品种(0.6.0 改:`Symbol` → `Instrument`,与引擎 HashMap key 对齐)
        instrument: Instrument,
    },

    /// 跨资产交易对无效
    #[error("跨资产交易对无效：{leg1}/{leg2}，比率 {ratio}")]
    InvalidCrossPair {
        /// 第一腿资产(0.6.0 改:`String` → `Instrument`,避免与 `leg1/leg2: Instrument`
        /// 不一致的双重表示)
        leg1: Instrument,
        /// 第二腿资产
        leg2: Instrument,
        /// 比率
        ratio: f64,
    },

    /// 暗池订单数量无效
    #[error("暗池订单数量无效：visible({visible}) > hidden({hidden})")]
    InvalidDarkOrderQuantity {
        /// 可见数量
        visible: Quantity,
        /// 隐藏总数量
        hidden: Quantity,
    },

    /// 批量拍卖无可清算价格
    #[error("批量拍卖无可清算价格（供需不平衡）")]
    AuctionNoClearingPrice,

    /// 引擎快照失败
    #[error("引擎快照失败：{0}")]
    SnapshotFailed(String),

    /// 引擎恢复失败
    #[error("引擎恢复失败：{0}")]
    RestoreFailed(String),

    /// 底层 L2 撮合错误
    #[error("底层撮合错误：{0}")]
    Matching(#[from] MatchingError),

    /// 订单缺少限价（L3 仅支持限价相关操作）
    #[error("订单 {order_id} 缺少限价（仅限价单支持 L3 路由）")]
    OrderMissingLimitPrice {
        /// 订单 ID
        order_id: u64,
    },
}

/// L3 撮合结果别名
pub type MatchingL3Result<T> = Result<T, MatchingL3Error>;
