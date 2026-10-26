//! 收市事件视图与周期换算
//!
//! `axon_core::MarketDataPayload::BarClosed` 携带的数据是「值 + 品种 + 周期」，
//! 内核再补一个事件时间戳（收市时刻）组成借用视图 [`BarClosedEvent`]，避免
//! 策略侧反复解包 `Event::MarketData`。

use axon_core::Bar;
use axon_core::time::Timestamp;
use axon_core::types::Instrument;

/// 收市 K 线视图（借用，不拷贝 bar）
///
/// `ts` 是**收市时刻**（`bar.timestamp + interval`），不是 `bar.timestamp` 本身。
/// 策略可见的所有时间都应读 `ts`，直接用 `bar.timestamp` 会前视。
#[derive(Debug, Clone, Copy)]
pub struct BarClosedEvent<'a> {
    /// 交易品种
    pub instrument: &'a Instrument,
    /// K 线周期（`"1m"` / `"15m"` / `"1h"`）
    pub interval: &'a str,
    /// 已收市的 K 线（`timestamp` 是起始时间）
    pub bar: &'a Bar,
    /// 事件时间戳 = 收市时刻（纳秒）
    pub ts: Timestamp,
    /// 本 bar 的逐 bar state（策略因子/上下文）；缺省为 `Value::Null`
    ///
    /// 不放进 `MarketDataPayload::BarClosed`：那是市场数据契约，state 是编排层
    /// 注入的策略输入。内核从帧预注册表按 `(帧时间戳, 品种)` 查表回填。
    pub state: &'a serde_json::Value,
}

/// 周期字符串 → 纳秒数
///
/// 支持 `m`（分钟）/ `h`（小时）/ `d`（天）/ `w`（周）四个后缀，前缀必须是
/// 正整数（如 `"1m"` = 60e9、`"15m"`、`"1h"`、`"1d"`、`"1w"`）。
/// 其它输入（空串、`"0m"`、`"1x"`、`"1.5m"`、大写后缀）一律返回 `None`。
///
/// 注：只认小写后缀，`"1M"` 这种大写形式在交易所口径里含义不统一（可能是月），
/// 故意不支持，避免误解。
pub fn interval_to_nanos(interval: &str) -> Option<i64> {
    let unit = interval.chars().last()?;
    let digits = &interval[..interval.len() - unit.len_utf8()];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let count: i64 = digits.parse().ok()?;
    if count <= 0 {
        return None;
    }
    let unit_nanos = match unit {
        'm' => 60 * 1_000_000_000,
        'h' => 60 * 60 * 1_000_000_000,
        'd' => 24 * 60 * 60 * 1_000_000_000,
        'w' => 7 * 24 * 60 * 60 * 1_000_000_000,
        _ => return None,
    };
    count.checked_mul(unit_nanos)
}

/// 由 bar 起始时间与周期推导收市时刻
///
/// 这是防前视的关键：`Bar.timestamp` 是 bar 起始时间，事件时间必须是
/// `bar.timestamp + interval`。`interval` 非法或溢出时返回 `None`，
/// 由调用方决定回退策略（内核回退用 `bar.timestamp`）。
pub fn close_time(bar_timestamp: Timestamp, interval: &str) -> Option<Timestamp> {
    let end = bar_timestamp
        .nanos
        .checked_add(interval_to_nanos(interval)?)?;
    Some(Timestamp::from_nanos(end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interval_to_nanos_supported_units() {
        assert_eq!(interval_to_nanos("1m"), Some(60_000_000_000));
        assert_eq!(interval_to_nanos("15m"), Some(15 * 60_000_000_000));
        assert_eq!(interval_to_nanos("1h"), Some(3_600_000_000_000));
        assert_eq!(interval_to_nanos("1d"), Some(86_400_000_000_000));
        assert_eq!(interval_to_nanos("1w"), Some(604_800_000_000_000));
    }

    #[test]
    fn test_interval_to_nanos_rejects_invalid() {
        assert_eq!(interval_to_nanos(""), None, "空串非法");
        assert_eq!(interval_to_nanos("m"), None, "缺数字非法");
        assert_eq!(interval_to_nanos("0m"), None, "零周期非法");
        assert_eq!(interval_to_nanos("1x"), None, "未知后缀非法");
        assert_eq!(interval_to_nanos("1.5m"), None, "小数非法");
        assert_eq!(interval_to_nanos("1M"), None, "大写后缀不支持");
    }

    #[test]
    fn test_close_time_strictly_after_bar_timestamp() {
        let start = Timestamp::from_nanos(1_700_000_000_000_000_000);
        let closed = close_time(start, "1m").expect("1m 合法");
        assert!(
            closed.nanos > start.nanos,
            "收市时刻必须严格大于 bar 起始时间"
        );
        assert_eq!(closed.nanos - start.nanos, 60_000_000_000);
    }

    #[test]
    fn test_close_time_invalid_interval_returns_none() {
        assert!(close_time(Timestamp::from_nanos(0), "1x").is_none());
    }
}
