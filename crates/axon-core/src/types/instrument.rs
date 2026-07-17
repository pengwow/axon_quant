//! 交易品种抽象(Spot / Swap)
//!
//! 区分 spot(现货)与 swap(永续合约),为 spot+perp 双 leg 套利提供
//! 类型安全基础。`Instrument` 是策略与撮合引擎之间共同语言,无歧义地
//! 标识"在哪个品种上交易"。
//!
//! 序列化用 `tag = "kind"` 模式,Python 端 dict 协议简洁:
//! ```json
//! {"kind": "spot",  "details": {"base": "BTC", "quote": "USDT"}}
//! {"kind": "swap",  "details": {"base": "BTC", "quote": "USDT",
//!                               "settle": "UsdMargin", "contract_size": 1.0}}
//! ```

use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

use super::Symbol;

/// 交易品种
///
/// `Clone` 而非 `Copy`:因为 `SpotInstrument` / `SwapInstrument` 内含
/// `Symbol(String)`,是堆分配。详见 spec §4.1.
///
/// `Hash` / `Eq` 手动实现:`SwapInstrument.contract_size: f64` 不可派生
/// `Hash` / `Eq`(`f64` 含 NaN,无法满足 `Eq` 律)。我们对 `f64` 用
/// `to_bits()` 转成 `u64` 后再比较和 hash,语义上"位级相等即相等",
/// NaN 与 NaN 比较也会相等(因为位相同),这在 HashMap key 场景下
/// 是合理选择(不期望不同 NaN 表示"不同的 instrument")。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "details", rename_all = "lowercase")]
pub enum Instrument {
    /// 现货
    Spot(SpotInstrument),
    /// 永续合约
    Swap(SwapInstrument),
}

// T2.4 新增:serde 反序列化时 `#[serde(default)]` 字段需要 Default 实现。
// 默认值用 `SpotInstrument` 的"空币种",仅作为缺失值兜底,业务层不应依赖
// 此默认值构造真实数据(若 instrument 缺失,通常意味着数据有 bug)。
impl Default for Instrument {
    fn default() -> Self {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from(""),
            quote: Symbol::from(""),
        })
    }
}

/// 现货交易品种
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpotInstrument {
    /// 基础币种(如 `BTC` 表示一个 `BTC`)
    pub base: Symbol,
    /// 计价币种(如 `USDT` 表示价格以 USDT 计价)
    pub quote: Symbol,
}

/// 永续合约交易品种
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwapInstrument {
    /// 基础币种(如 `BTC`)
    pub base: Symbol,
    /// 计价币种(如 `USDT`)
    pub quote: Symbol,
    /// 结算方式(USD 保证金 / 币本位)
    pub settle: SwapSettle,
    /// 合约乘数(每张合约代表多少基础币种)
    pub contract_size: f64,
}

/// 永续合约结算方式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SwapSettle {
    /// USD 保证金合约(quote 币种作为保证金)
    UsdMargin,
    /// 币本位合约(base 币种作为保证金)
    CoinMargin,
}

impl Eq for SwapInstrument {}

impl Hash for SwapInstrument {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.base.hash(state);
        self.quote.hash(state);
        self.settle.hash(state);
        self.contract_size.to_bits().hash(state);
    }
}

// 手动实现 `Instrument` 的 `Hash`:同 `SwapInstrument` 的考量 ——
// 直接 derive `Hash` 在含有 `f64` 变体时无法编译,而我们已经为
// `SwapInstrument` 提供了位级 Hash,因此 enum 也需要手动实现以保持一致。
impl Hash for Instrument {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // 先用变体判别符区分,再委托给各变体的 Hash 实现
        match self {
            Instrument::Spot(s) => {
                0u8.hash(state);
                s.hash(state);
            }
            Instrument::Swap(s) => {
                1u8.hash(state);
                s.hash(state);
            }
        }
    }
}

impl Instrument {
    /// 基础币种
    pub fn base(&self) -> &Symbol {
        match self {
            Instrument::Spot(s) => &s.base,
            Instrument::Swap(s) => &s.base,
        }
    }

    /// 计价币种
    pub fn quote(&self) -> &Symbol {
        match self {
            Instrument::Spot(s) => &s.quote,
            Instrument::Swap(s) => &s.quote,
        }
    }

    /// 0.5.0 新增:品种 kind(用于 RiskEngine / Portfolio 区分 spot vs swap)
    ///
    /// 返回 `"spot"` 或 `"swap"`,字符串字面量便于跨语言/序列化比较。
    pub fn kind(&self) -> &'static str {
        match self {
            Instrument::Spot(_) => "spot",
            Instrument::Swap(_) => "swap",
        }
    }

    /// 0.5.0 新增:人类可读 label(`"BTC/USDT"`),用于 `Position::symbol` 字段填充
    ///
    /// 格式:`{base}/{quote}`(不区分 spot/swap;Symbol 字段只是 human-readable label,
    /// 真正的结构化区分在 `kind`)。
    pub fn label(&self) -> String {
        format!("{}/{}", self.base().as_str(), self.quote().as_str())
    }

    /// 0.5.0 新增:从 `Symbol` 派生 `Instrument`(兼容旧 API)
    ///
    /// `Symbol` 格式约定:
    /// - `"BTC-USDT"` 或 `"BTC/USDT"` → base=`BTC`, quote=`USDT`,kind=Spot(默认)
    /// - 其它(空 / 无法解析)→ `Instrument::default()`(空 Spot)
    ///
    /// 用于 `Portfolio::apply_trade(symbol, ...)` 兼容路径;新代码请直接
    /// 构造 `Instrument::Spot` / `Instrument::Swap` 显式指定。
    pub fn from_symbol(symbol: &Symbol) -> Self {
        let raw = symbol.as_str();
        // 兼容 "/"(惯例)和 "-"(旧 OMS 格式)两种分隔符
        let normalized = raw.replace('-', "/");
        let parts: Vec<&str> = normalized.splitn(2, '/').collect();
        match parts.as_slice() {
            [base, quote] if !base.is_empty() && !quote.is_empty() => {
                Instrument::Spot(SpotInstrument {
                    base: Symbol::from(*base),
                    quote: Symbol::from(*quote),
                })
            }
            _ => Instrument::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_spot_instrument_creation() {
        let inst = Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        });
        assert_eq!(inst.base().as_str(), "BTC");
        assert_eq!(inst.quote().as_str(), "USDT");
    }

    #[test]
    fn test_swap_instrument_creation() {
        let inst = Instrument::Swap(SwapInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
            settle: SwapSettle::UsdMargin,
            contract_size: 1.0,
        });
        assert_eq!(inst.base().as_str(), "BTC");
        assert_eq!(inst.quote().as_str(), "USDT");
    }

    #[test]
    fn test_instrument_equality_and_hash() {
        let a = Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        });
        let b = a.clone();
        let c = Instrument::Spot(SpotInstrument {
            base: Symbol::from("ETH"),
            quote: Symbol::from("USDT"),
        });
        assert_eq!(a, b);
        assert_ne!(a, c);
        let mut set = HashSet::new();
        set.insert(a);
        set.insert(b);
        set.insert(c);
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn test_swap_instrument_hash_via_bits() {
        // contract_size = 1.0 和 1.0(位相同)应当相等
        let a = Instrument::Swap(SwapInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
            settle: SwapSettle::UsdMargin,
            contract_size: 1.0,
        });
        let b = Instrument::Swap(SwapInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
            settle: SwapSettle::UsdMargin,
            contract_size: 1.0,
        });
        assert_eq!(a, b);
        let mut set = HashSet::new();
        set.insert(a);
        set.insert(b);
        assert_eq!(set.len(), 1, "相同 contract_size bits 应 hash 到同一 slot");
    }

    #[test]
    fn test_instrument_serde_json_spot() {
        let inst = Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        });
        let json = serde_json::to_string(&inst).unwrap();
        assert!(json.contains("\"kind\":\"spot\""));
        let parsed: Instrument = serde_json::from_str(&json).unwrap();
        assert_eq!(inst, parsed);
    }

    #[test]
    fn test_instrument_serde_json_swap() {
        let inst = Instrument::Swap(SwapInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
            settle: SwapSettle::UsdMargin,
            contract_size: 1.0,
        });
        let json = serde_json::to_string(&inst).unwrap();
        assert!(json.contains("\"kind\":\"swap\""));
        assert!(json.contains("\"settle\":\"UsdMargin\""));
        let parsed: Instrument = serde_json::from_str(&json).unwrap();
        assert_eq!(inst, parsed);
    }
}
