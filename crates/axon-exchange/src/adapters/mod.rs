//! 交易所适配器：实现统一的 `ExchangeAdapter` trait
//!
//! - `binance`：Binance 现货 / 合约 REST + WebSocket
//! - `okx`：OKX V5 REST + WebSocket
//!
//! 子模块共享统一的 `parse_positions_from_json` 函数，支持 Binance 合约与 OKX 持仓响应。

pub mod binance;
pub mod okx;

use crate::types::{Position, Side, Symbol};

/// 共享的持仓 JSON 解析器
///
/// 支持两种 schema：
/// - OKX：`{ "instId": "BTC-USDT", "posSide": "long", "pos": "0.1", "avgPx": "50000", "upl": "10" }`
/// - Binance 合约：`{ "symbol": "BTCUSDT", "positionAmt": "0.1", "entryPrice": "50000", "unRealizedProfit": "10" }`
///
/// 解析失败（字段缺失或类型错误）的条目会被静默跳过，不影响其他条目。
pub fn parse_positions_from_json(arr: &[serde_json::Value]) -> Vec<Position> {
    arr.iter().filter_map(parse_one_position).collect()
}

/// 解析单条持仓
fn parse_one_position(d: &serde_json::Value) -> Option<Position> {
    // 优先识别 OKX 字段，其次 Binance
    let inst_id = d
        .get("instId")
        .and_then(|v| v.as_str())
        .or_else(|| d.get("symbol").and_then(|v| v.as_str()))?;

    // 数量：OKX 用 "pos"，Binance 用 "positionAmt"
    let pos_str = d
        .get("pos")
        .and_then(|v| v.as_str())
        .or_else(|| d.get("positionAmt").and_then(|v| v.as_str()))?;
    let qty_raw: rust_decimal::Decimal = pos_str.parse().ok()?;
    if qty_raw.is_zero() {
        return None;
    }

    // 方向：OKX 用 posSide，Binance 由 positionAmt 符号推断
    let side = if let Some(s) = d.get("posSide").and_then(|v| v.as_str()) {
        match s {
            "long" => Side::Buy,
            "short" => Side::Sell,
            // net / 其他：按数量符号推断
            _ => {
                if qty_raw >= rust_decimal::Decimal::ZERO {
                    Side::Buy
                } else {
                    Side::Sell
                }
            }
        }
    } else if qty_raw >= rust_decimal::Decimal::ZERO {
        Side::Buy
    } else {
        Side::Sell
    };

    // 平均入场价：OKX avgPx / Binance entryPrice
    let avg_entry_price: rust_decimal::Decimal = d
        .get("avgPx")
        .and_then(|v| v.as_str())
        .or_else(|| d.get("entryPrice").and_then(|v| v.as_str()))?
        .parse()
        .ok()?;

    // 未实现盈亏：OKX upl / Binance unRealizedProfit
    let unrealized_pnl: rust_decimal::Decimal = d
        .get("upl")
        .and_then(|v| v.as_str())
        .or_else(|| d.get("unRealizedProfit").and_then(|v| v.as_str()))
        .and_then(|s| s.parse().ok())
        .unwrap_or_default();

    Some(Position {
        symbol: Symbol::new(inst_id),
        side,
        // 数量取绝对值，方向由 side 字段表达
        quantity: qty_raw.abs(),
        avg_entry_price,
        unrealized_pnl,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    #[test]
    fn test_parse_okx_long_position() {
        let arr = vec![serde_json::json!({
            "instId": "BTC-USDT",
            "posSide": "long",
            "pos": "0.1",
            "avgPx": "50000",
            "upl": "10.5"
        })];
        let positions = parse_positions_from_json(&arr);
        assert_eq!(positions.len(), 1);
        let p = &positions[0];
        assert_eq!(p.symbol, Symbol::new("BTC-USDT"));
        assert_eq!(p.side, Side::Buy);
        assert_eq!(p.quantity, Decimal::from_str("0.1").unwrap());
        assert_eq!(p.avg_entry_price, Decimal::from_str("50000").unwrap());
        assert_eq!(p.unrealized_pnl, Decimal::from_str("10.5").unwrap());
    }

    #[test]
    fn test_parse_okx_short_position() {
        let arr = vec![serde_json::json!({
            "instId": "ETH-USDT",
            "posSide": "short",
            "pos": "-0.5",
            "avgPx": "3000",
            "upl": "-5"
        })];
        let positions = parse_positions_from_json(&arr);
        assert_eq!(positions.len(), 1);
        let p = &positions[0];
        assert_eq!(p.side, Side::Sell);
        assert_eq!(p.quantity, Decimal::from_str("0.5").unwrap()); // abs
    }

    #[test]
    fn test_parse_binance_contract_position() {
        let arr = vec![serde_json::json!({
            "symbol": "BTCUSDT",
            "positionAmt": "0.01",
            "entryPrice": "60000",
            "unRealizedProfit": "5"
        })];
        let positions = parse_positions_from_json(&arr);
        assert_eq!(positions.len(), 1);
        let p = &positions[0];
        assert_eq!(p.symbol, Symbol::new("BTCUSDT"));
        assert_eq!(p.side, Side::Buy);
        assert_eq!(p.quantity, Decimal::from_str("0.01").unwrap());
    }

    #[test]
    fn test_parse_skips_zero_position() {
        let arr = vec![serde_json::json!({
            "instId": "XRP-USDT",
            "posSide": "net",
            "pos": "0",
            "avgPx": "0.5",
            "upl": "0"
        })];
        let positions = parse_positions_from_json(&arr);
        assert!(positions.is_empty());
    }

    #[test]
    fn test_parse_skips_malformed() {
        // 缺少 instId/symbol
        let arr = vec![serde_json::json!({"pos": "0.1"})];
        let positions = parse_positions_from_json(&arr);
        assert!(positions.is_empty());
    }
}
