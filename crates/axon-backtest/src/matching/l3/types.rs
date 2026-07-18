//! L3 撮合引擎相关类型

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use axon_core::types::{Instrument, LegPair, Price, Quantity, SpotInstrument, Symbol};

use super::super::types::OrderBookLevel;
use super::auction::BatchMode;

/// 交易场所标识
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Venue {
    /// Binance
    Binance,
    /// Coinbase
    Coinbase,
    /// Kraken
    Kraken,
    /// Bybit
    Bybit,
    /// OKX
    Okx,
    /// 火币
    Huobi,
    /// 自定义场所(使用 u16 ID)
    Custom(u16),
}

impl Venue {
    /// 场所名称
    pub fn name(&self) -> &'static str {
        match self {
            Self::Binance => "binance",
            Self::Coinbase => "coinbase",
            Self::Kraken => "kraken",
            Self::Bybit => "bybit",
            Self::Okx => "okx",
            Self::Huobi => "huobi",
            Self::Custom(_) => "custom",
        }
    }
}

impl std::fmt::Display for Venue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// 跨资产交易对(0.6.0 改:`leg1/leg2: Symbol` → `pair: LegPair` 薄包装)
///
/// `LegPair` 在 `axon-core` 定义,这里仅作 L3 撮合的"扩展视图"层:
/// - `pair: LegPair` 携带 spot / perp + hedge_ratio
/// - `ratio` 描述两腿的数量等价关系(典型用法:1 leg1 兑换 ratio 个 leg2,
///   与 `LegPair.hedge_ratio` 在不同场景下使用 — `hedge_ratio` 是 delta
///   中性对冲用的,`ratio` 是套利执行价差用的)
/// - `max_quantity` 是 L3 撮合特有的执行上限
///
/// BREAKING 迁移(0.5.0 → 0.6.0):
/// - 旧:`CrossPair::new(leg1, leg2, ratio, max_qty)`(`leg1/leg2: Instrument`)
/// - 新:`CrossPair::from_leg_pair(pair, ratio, max_qty)`(`pair: LegPair`)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrossPair {
    /// 0.6.0 改:leg1/leg2 → `pair: LegPair`(spot/perp 配对)
    pub pair: LegPair,
    /// 交换比率(`leg1 / leg2`,L3 套利专用,与 `pair.hedge_ratio` 解耦)
    pub ratio: f64,
    /// 最大可执行数量
    pub max_quantity: Quantity,
}

impl CrossPair {
    /// 0.6.0 新:从 `LegPair` 构造 `CrossPair`(推荐 API)
    pub fn from_leg_pair(pair: LegPair, ratio: f64, max_quantity: Quantity) -> Self {
        Self {
            pair,
            ratio,
            max_quantity,
        }
    }

    /// 0.6.0 改(BREAKING):从两个 `Instrument` 直接构造
    ///
    /// 旧 `new(leg1, leg2, ratio, max_qty)` 的 `leg1/leg2` 本来就是
    /// `Instrument`,但语义上不强制 spot/perp 配对。这里保持同签名
    /// 但内部走 `LegPair::with_ratio` 包装(默认 `hedge_ratio = 1.0`)。
    pub fn new(leg1: Instrument, leg2: Instrument, ratio: f64, max_quantity: Quantity) -> Self {
        Self {
            pair: LegPair::with_ratio(leg1, leg2, ratio),
            ratio,
            max_quantity,
        }
    }

    /// 0.6.0 兼容 helper:`Symbol` → `Instrument` 桥接(用于旧测试)
    pub fn from_symbols(
        leg1: &Symbol,
        leg2: &Symbol,
        ratio: f64,
        max_quantity: Quantity,
    ) -> Self {
        let (l1, l2) = (symbol_to_instrument(leg1), symbol_to_instrument(leg2));
        Self::new(l1, l2, ratio, max_quantity)
    }

    /// 第一腿(便捷访问,等价于 `self.pair.spot` 或 `self.pair.perp` 取决于注册顺序)
    pub fn leg1(&self) -> &Instrument {
        // 兼容旧 API:取 `pair.spot` 作为 leg1。
        // 注:L3 `CrossPair` 原本 leg1/leg2 无 spot/perp 强制约定,
        // 0.6.0 后如果调用方传入非 spot+perp 配对,需要自己识别
        // `self.pair.spot` 和 `self.pair.perp` 哪个是 leg1。
        // 这里取 `spot` 作为 leg1,保持与 0.5.0 行为一致(旧实现
        // 把第一个传入参数作为 leg1,常见用法是 spot 先传)。
        &self.pair.spot
    }

    /// 第二腿
    pub fn leg2(&self) -> &Instrument {
        &self.pair.perp
    }
}

/// 0.6.0 临时桥:`Symbol` → `Instrument`(`"BTC/USDT"` → Spot BTC/USDT)
fn symbol_to_instrument(symbol: &Symbol) -> Instrument {
    let s = symbol.as_str();
    let (base, quote) = s.split_once('/').unwrap_or((s, "USDT"));
    Instrument::Spot(SpotInstrument {
        base: Symbol::from(base),
        quote: Symbol::from(quote),
    })
}

/// 价格级别
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceLevel {
    /// 价格
    pub price: Price,
    /// 数量
    pub quantity: Quantity,
    /// 订单数量
    pub order_count: usize,
}

impl PriceLevel {
    /// 从 `OrderBookLevel` 转换
    pub fn from_book_level(level: &OrderBookLevel) -> Self {
        Self {
            price: level.price,
            quantity: level.quantity,
            order_count: level.order_count,
        }
    }
}

/// 单资产 L2 快照(0.6.0 改:`symbol: Symbol` → `instrument: Instrument`)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L2Snapshot {
    /// 资产(spot 或 swap)
    pub instrument: Instrument,
    /// 最优买价
    pub best_bid: Option<Price>,
    /// 最优卖价
    pub best_ask: Option<Price>,
    /// 买单深度
    pub bid_depth: Vec<PriceLevel>,
    /// 卖单深度
    pub ask_depth: Vec<PriceLevel>,
    /// 成交笔数
    pub trade_count: u64,
}

/// 多资产订单簿快照(0.6.0 BREAKING:`engines: HashMap<Symbol, _>` → `HashMap<Instrument, _>`)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchingEngineSnapshot {
    /// 各资产的 L2 引擎快照(按 Instrument 索引)
    pub engines: HashMap<Instrument, L2Snapshot>,
    /// 跨资产交易对配置
    pub cross_pairs: Vec<CrossPair>,
    /// 批量撮合模式
    pub batch_mode: BatchMode,
    /// 快照时间戳(Unix 纳秒)
    pub timestamp_ns: u64,
}

impl MatchingEngineSnapshot {
    /// 创建空快照
    pub fn empty(batch_mode: BatchMode) -> Self {
        Self {
            engines: HashMap::new(),
            cross_pairs: Vec::new(),
            batch_mode,
            timestamp_ns: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::types::{SpotInstrument, SwapInstrument, SwapSettle, Symbol};

    #[test]
    fn test_venue_name() {
        assert_eq!(Venue::Binance.name(), "binance");
        assert_eq!(Venue::Coinbase.name(), "coinbase");
        assert_eq!(Venue::Custom(42).name(), "custom");
    }

    #[test]
    fn test_venue_display() {
        assert_eq!(format!("{}", Venue::Binance), "binance");
        assert_eq!(format!("{}", Venue::Kraken), "kraken");
    }

    fn btc_spot() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        })
    }

    fn eth_spot() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from("ETH"),
            quote: Symbol::from("USDT"),
        })
    }

    #[test]
    fn test_cross_pair_new() {
        let p = CrossPair::new(btc_spot(), eth_spot(), 0.06, Quantity::from_f64(10.0));
        assert_eq!(p.ratio, 0.06);
        assert_eq!(p.max_quantity, Quantity::from_f64(10.0));
    }

    #[test]
    fn test_batch_mode_default() {
        assert_eq!(BatchMode::default(), BatchMode::Continuous);
    }

    #[test]
    fn test_price_level_from_book_level() {
        let book = OrderBookLevel::new(Price::from_f64(100.0), Quantity::from_f64(5.0), 3);
        let pl = PriceLevel::from_book_level(&book);
        assert_eq!(pl.price, Price::from_f64(100.0));
        assert_eq!(pl.quantity, Quantity::from_f64(5.0));
        assert_eq!(pl.order_count, 3);
    }

    #[test]
    fn test_snapshot_empty() {
        let s = MatchingEngineSnapshot::empty(BatchMode::Continuous);
        assert!(s.engines.is_empty());
        assert!(s.cross_pairs.is_empty());
        assert_eq!(s.batch_mode, BatchMode::Continuous);
    }

    /// 0.6.0 新增:CrossPair 接受 swap(perp)leg
    #[test]
    fn test_cross_pair_swap_leg() {
        let btc_perp = Instrument::Swap(SwapInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
            settle: SwapSettle::UsdMargin,
            contract_size: 1.0,
        });
        let p = CrossPair::new(btc_spot(), btc_perp.clone(), 1.0, Quantity::from_f64(0.5));
        assert_eq!(p.leg1(), &btc_spot());
        assert_eq!(p.leg2(), &btc_perp);
    }

    /// 0.6.0 新增:`from_leg_pair` 推荐 API
    #[test]
    fn test_cross_pair_from_leg_pair() {
        let btc_perp = Instrument::Swap(SwapInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
            settle: SwapSettle::UsdMargin,
            contract_size: 1.0,
        });
        let pair = LegPair::with_ratio(btc_spot(), btc_perp.clone(), 1.0);
        let p = CrossPair::from_leg_pair(pair.clone(), 0.5, Quantity::from_f64(2.0));
        assert_eq!(p.pair, pair);
        assert!((p.ratio - 0.5).abs() < 1e-9);
    }
}
