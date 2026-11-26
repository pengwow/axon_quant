//! 永续合约保证金核算（币安分档模型）。
//!
//! 这里只做纯计算与账本状态，不碰撮合：强平动作由回测引擎执行
//! （见 `engine.rs::risk_reduce` / `check_margin`）。

use std::collections::HashMap;

/// 币安 `/fapi/v1/leverageBracket` 的 brackets[] 一一对应。
#[derive(Debug, Clone, PartialEq)]
pub struct Bracket {
    /// 该档名义下界（含）
    pub notional_floor: f64,
    /// 该档名义上界（不含）；`None` 表示最高档无上限
    pub notional_cap: Option<f64>,
    /// 该档维持保证金率
    pub mmr: f64,
    /// 币安 maintenanceAmount：仅用于自洽校验，不参与相加
    /// （分段累加与 `名义 × mmr − cum` 等价，两者相加会双重计入）
    pub cum: f64,
    /// 该档强平手续费率（币安接口不直接给，采集侧按 taker 填充）
    pub liquidation_fee_rate: f64,
    /// 该档开放的最高杠杆，用于校验用户设定的杠杆
    pub max_leverage: f64,
}

/// 保证金模式。
///
/// 带 `Serialize`：强平事件经 `BacktestResult`（`axon-worker`）序列化时会被直接透传。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum MarginMode {
    /// 逐仓：每品种独立子账户，爆仓只影响该品种
    Isolated,
    /// 全仓：所有品种共享账户权益
    Cross,
}

/// 单品种的保证金规格。
#[derive(Debug, Clone, PartialEq)]
pub struct MarginSpec {
    /// 用户设定的杠杆倍数
    pub leverage: f64,
    /// 逐仓 / 全仓
    pub mode: MarginMode,
    /// 按 `notional_floor` 升序；至少 1 档
    pub brackets: Vec<Bracket>,
    /// 档位来源：`db` / `snapshot` / `default`
    ///
    /// 只在结果回显时使用（前端展示"档位来自哪里"），不参与任何计算；
    /// 由 QuantCell 侧 `build_margin_specs` 记录后随 spec 传入。
    pub bracket_source: String,
}

impl MarginSpec {
    /// 维持保证金：各档区间内的名义分别乘该档 MMR 后累加。
    ///
    /// 之所以分段累加而不是 `名义 × 当前档 mmr − cum`：后者依赖档位数据里的 cum
    /// 完全正确；分段累加只需要 mmr 与边界，档位漂移时更稳，且两者在数据自洽时等价。
    pub fn maintenance_margin(&self, notional: f64) -> f64 {
        if notional <= 0.0 {
            return 0.0;
        }
        let mut total = 0.0;
        for b in &self.brackets {
            let upper = b.notional_cap.unwrap_or(f64::INFINITY);
            if notional <= b.notional_floor {
                break;
            }
            let slice = notional.min(upper) - b.notional_floor;
            if slice > 0.0 {
                total += slice * b.mmr;
            }
        }
        total
    }

    /// 当前名义落在第几档（0-based），用于强平事件的 tier_from / tier_to。
    pub fn tier_index(&self, notional: f64) -> usize {
        let mut idx = 0;
        for (i, b) in self.brackets.iter().enumerate() {
            if notional > b.notional_floor {
                idx = i;
            }
        }
        idx
    }

    /// 当前名义所在档位的强平手续费率；档位表为空时按 0 处理（上层保证至少 1 档）。
    pub fn liquidation_fee_rate(&self, notional: f64) -> f64 {
        self.brackets
            .get(self.tier_index(notional))
            .map_or(0.0, |b| b.liquidation_fee_rate)
    }
}

/// 强平检查那一刻的账户只读视图。
///
/// 用快照而不是让 tracker 反向依赖引擎类型：引擎负责把持仓 / 价格 / 浮盈亏喂进来，
/// tracker 只做纯函数式的判定，方便单测且不改引擎状态。
#[derive(Debug, Clone, Default)]
pub struct MarginSnapshot {
    /// symbol → 持仓数量（带符号，负为空头）
    pub positions: HashMap<String, f64>,
    /// symbol → 本 bar 判定价
    pub prices: HashMap<String, f64>,
    /// symbol → 未实现盈亏（计价货币）
    pub unrealized: HashMap<String, f64>,
    /// 主账户现金（自由余额，保证金模式下不含持仓名义）
    pub cash: f64,
    /// 账户 NAV（cash + Σ逐仓子账户 + Σ浮盈亏 + Σ非保证金品种市值）
    ///
    /// 全仓权益直接用 NAV，而不是 `cash + 浮盈亏`：保证金模式下 cash 只记
    /// 自由余额，"开仓支出"已不再体现在 cash 里，再用旧式相加会把名义重复
    /// 算成亏损（曾导致 20x 开仓瞬间判爆仓）。
    pub nav: f64,
}

impl MarginSnapshot {
    /// 某品种的当前名义（数量绝对值 × 判定价）。
    pub fn notional(&self, symbol: &str) -> f64 {
        let qty = self.positions.get(symbol).copied().unwrap_or(0.0).abs();
        let px = self.prices.get(symbol).copied().unwrap_or(0.0);
        qty * px
    }
}

/// 强平原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum LiquidateReason {
    /// 逐档部分强平：减到更低的档位即可恢复
    Partial,
    /// 降到最低档仍不够，该品种（逐仓）或账户（全仓）清零
    Depleted,
}

/// 一次计划中的强平减仓动作（尚未执行）。
#[derive(Debug, Clone, PartialEq)]
pub struct LiquidateAction {
    /// 品种 key（"BTC/USDT"）
    pub symbol: String,
    /// 减仓后的目标数量（绝对数量，符号与当前持仓一致）
    pub target_qty: f64,
    /// 减仓前所在档位（0-based）
    pub tier_from: usize,
    /// 减仓后所在档位（0-based）
    pub tier_to: usize,
    /// 本档强平手续费率
    pub fee_rate: f64,
    /// 部分强平 / 清零
    pub reason: LiquidateReason,
    /// 该品种的保证金模式，事件透传时直接使用
    pub margin_mode: MarginMode,
}

/// 一次已执行的强平事件(结果层时间线)。
///
/// 放在 `margin.rs` 而不是 `engine.rs`:Python 绑定 / 结果序列化都要用,
/// 与 `MarginTracker` 的语义字段保持同名同义(`tier_from` / `reason` 等)。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LiquidationEvent {
    /// 事件时间戳(纳秒,取模拟时钟)
    pub timestamp_ns: i64,
    /// 品种 key("BTC/USDT")
    pub symbol: String,
    /// 逐仓 / 全仓
    pub margin_mode: MarginMode,
    /// 强平前所在档位(0-based)
    pub tier_from: usize,
    /// 强平后所在档位(0-based)
    pub tier_to: usize,
    /// 实际减掉的数量(绝对值)
    pub reduced_qty: f64,
    /// 判定价
    pub price: f64,
    /// 强平手续费(计价货币)
    pub fee: f64,
    /// 部分强平 / 清零
    pub reason: LiquidateReason,
    /// 判定价口径;v1 固定 `"close"`
    pub price_basis: &'static str,
}

/// 保证金追踪器：持有每品种规格与逐仓子账户账本，产出强平计划。
pub struct MarginTracker {
    specs: HashMap<String, MarginSpec>,
    /// 逐仓子账户保证金余额（cross 模式下为空）
    isolated: HashMap<String, f64>,
}

impl MarginTracker {
    /// 用每品种规格构造；`specs` 为空时等价于未启用保证金。
    pub fn new(specs: HashMap<String, MarginSpec>) -> Self {
        Self {
            specs,
            isolated: HashMap::new(),
        }
    }

    /// 设置逐仓子账户余额（0 或负数即视为没有子账户）。
    pub fn set_isolated(&mut self, symbol: &str, margin: f64) {
        if margin <= 0.0 {
            self.isolated.remove(symbol);
        } else {
            self.isolated.insert(symbol.to_string(), margin);
        }
    }

    /// 逐仓子账户余额；不存在时 0。
    pub fn isolated(&self, symbol: &str) -> f64 {
        self.isolated.get(symbol).copied().unwrap_or(0.0)
    }

    /// 逐仓保证金需求：剩余名义 / leverage。
    ///
    /// 杠杆下限夹到 1：0 杠杆会把需求量算成无穷大，属于配置错误而非业务语义。
    pub fn required_isolated(&self, symbol: &str, notional: f64) -> f64 {
        match self.specs.get(symbol) {
            Some(spec) if notional > 0.0 => notional / spec.leverage.max(1.0),
            _ => 0.0,
        }
    }

    /// 强平成交后回写子账户：先扣强平费，再按剩余名义所需保证金"压回"，
    /// 多余部分由调用方释放回主账户。
    ///
    /// **必须在每档减仓后调用**：不释放多余保证金，下一 bar 的 `equity`
    /// 仍按旧保证金计算，会再次判定爆仓 → 连锁误强平（设计 §2.3 的关键点）。
    ///
    /// 这里只负责把子账户压到"剩余名义所需保证金"，不处理穿仓缺口：
    /// 缺口统一由引擎在强平结束后按账户级 NAV 口径兜底，避免两处重复计入
    /// `insurance_fund_loss`。
    pub fn settle_after_reduce(&mut self, symbol: &str, remaining_notional: f64, fee: f64) {
        let required = self.required_isolated(symbol, remaining_notional);
        let after_fee = self.isolated(symbol) - fee.max(0.0);
        // 子账户只保留"剩余名义所需保证金"；负数按 0 处理，不会留负余额
        let kept = after_fee.max(0.0).min(required);
        self.set_isolated(symbol, kept);
    }

    /// 每品种规格（只读）
    pub fn specs(&self) -> &HashMap<String, MarginSpec> {
        &self.specs
    }

    /// 某品种的保证金模式；未配置时按全仓处理（与"启用即全品种参与"一致）
    pub fn mode_of(&self, symbol: &str) -> MarginMode {
        self.specs
            .get(symbol)
            .map(|s| s.mode)
            .unwrap_or(MarginMode::Cross)
    }

    /// 生成强平动作，不改任何状态：引擎先执行成交、再回写账本，
    /// 避免「判定与执行之间」状态被悄悄改写。
    pub fn plan(&self, snap: &MarginSnapshot) -> Vec<LiquidateAction> {
        let mut actions: Vec<LiquidateAction> = Vec::new();

        // 逐仓：只看自己的子账户，与其他品种互不影响
        //
        // 0.26.0 起:同帧多品种决策 / 强平序 = 品种标签字典序。
        // `specs` 是 HashMap,迭代序不稳定,而 plan 的动作顺序决定引擎按什么
        // 次序减仓(order id 也随之漂移);这里与下方全仓段统一按 symbol
        // 字典序收集,口径与 `engine.rs::instrument_order_key` 一致。
        let mut isolated_specs: Vec<(&String, &MarginSpec)> = self
            .specs
            .iter()
            .filter(|(_, s)| s.mode == MarginMode::Isolated)
            .collect();
        isolated_specs.sort_by(|a, b| a.0.cmp(b.0));
        for (sym, spec) in isolated_specs {
            let notional = snap.notional(sym);
            if notional <= 0.0 {
                continue;
            }
            let equity = self.isolated(sym) + snap.unrealized.get(sym).copied().unwrap_or(0.0);
            if spec.maintenance_margin(notional) < equity {
                continue;
            }
            if let Some(act) = self.reduce_plan(sym, spec, notional, equity, snap) {
                actions.push(act);
            }
        }

        // 全仓：账户级判定，按名义从大到小依次减（贴近币安全仓部分强平顺序）
        let cross_specs: Vec<(&String, &MarginSpec)> = self
            .specs
            .iter()
            .filter(|(_, s)| s.mode == MarginMode::Cross)
            .collect();
        if !cross_specs.is_empty() {
            let mut order: Vec<(&String, f64)> = cross_specs
                .iter()
                .map(|(sym, _)| (*sym, snap.notional(sym)))
                .filter(|(_, n)| *n > 0.0)
                .collect();
            // 同名次的名义可能相等，用 symbol 兜底保证顺序确定（HashMap 迭代顺序不稳定）
            order.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));

            // 全仓权益 = NAV：保证金模式下 cash 不含持仓名义（开仓只扣手续费），
            // 用 cash + 浮盈亏会把"开仓支出"重复算成亏损（曾导致开仓即爆仓）。
            let equity = snap.nav;
            let mm_total: f64 = order
                .iter()
                .map(|(sym, n)| self.specs[*sym].maintenance_margin(*n))
                .sum();
            if mm_total < equity {
                return actions;
            }
            for (sym, notional) in order {
                if let Some(act) = self.reduce_plan(sym, &self.specs[sym], notional, equity, snap) {
                    actions.push(act);
                }
            }
        }
        actions
    }

    /// 逐档下降：把名义减到低于该档下限，直到维持保证金低于权益。
    fn reduce_plan(
        &self,
        symbol: &str,
        spec: &MarginSpec,
        notional: f64,
        equity: f64,
        snap: &MarginSnapshot,
    ) -> Option<LiquidateAction> {
        let price = snap.prices.get(symbol).copied().unwrap_or(0.0);
        if price <= 0.0 {
            return None;
        }
        let tier_from = spec.tier_index(notional);
        // 从当前档位下界开始逐档下探：落到哪一档还能恢复，就停在哪一档
        for bracket in spec.brackets.iter().take(tier_from + 1).rev() {
            let candidate_notional = bracket.notional_floor;
            if candidate_notional <= 0.0 {
                continue;
            }
            if spec.maintenance_margin(candidate_notional) < equity {
                let qty = candidate_notional / price;
                return Some(LiquidateAction {
                    symbol: symbol.to_string(),
                    target_qty: qty,
                    tier_from,
                    tier_to: spec.tier_index(candidate_notional),
                    fee_rate: spec.liquidation_fee_rate(notional),
                    reason: LiquidateReason::Partial,
                    margin_mode: spec.mode,
                });
            }
        }
        // 最低档仍不够 → 清零（穿仓部分由上层按保险基金口径兜底）
        Some(LiquidateAction {
            symbol: symbol.to_string(),
            target_qty: 0.0,
            tier_from,
            tier_to: 0,
            fee_rate: spec.liquidation_fee_rate(notional),
            reason: LiquidateReason::Depleted,
            margin_mode: spec.mode,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn btc_brackets() -> Vec<Bracket> {
        // 币安 BTCUSDT 前两档（真实量级，cum 用于自洽校验）
        vec![
            Bracket {
                notional_floor: 0.0,
                notional_cap: Some(50_000.0),
                mmr: 0.004,
                cum: 0.0,
                liquidation_fee_rate: 0.004,
                max_leverage: 125.0,
            },
            Bracket {
                notional_floor: 50_000.0,
                notional_cap: Some(600_000.0),
                mmr: 0.005,
                cum: 50.0,
                liquidation_fee_rate: 0.004,
                max_leverage: 100.0,
            },
        ]
    }

    #[test]
    fn maintenance_margin_sums_across_tiers() {
        let spec = MarginSpec {
            leverage: 10.0,
            mode: MarginMode::Cross,
            brackets: btc_brackets(),
            bracket_source: "default".into(),
        };
        // 10 万名义 = 5 万 × 0.4% + 5 万 × 0.5% = 450
        assert!((spec.maintenance_margin(100_000.0) - 450.0).abs() < 1e-9);
    }

    #[test]
    fn cum_matches_tier_formula_for_sanity_check() {
        let spec = MarginSpec {
            leverage: 10.0,
            mode: MarginMode::Cross,
            brackets: btc_brackets(),
            bracket_source: "default".into(),
        };
        // 币安语义：MM = 名义 × 该档 mmr − cum；与分段累加必须等价
        let notional = 100_000.0;
        let flat = notional * 0.005 - 50.0;
        assert!((spec.maintenance_margin(notional) - flat).abs() < 1e-9);
    }

    #[test]
    fn zero_or_negative_notional_has_no_maintenance_margin() {
        let spec = MarginSpec {
            leverage: 5.0,
            mode: MarginMode::Isolated,
            brackets: btc_brackets(),
            bracket_source: "default".into(),
        };
        assert_eq!(spec.maintenance_margin(0.0), 0.0);
        assert_eq!(spec.maintenance_margin(-1.0), 0.0);
    }

    fn view(
        positions: Vec<(&str, f64)>,
        prices: Vec<(&str, f64)>,
        unrealized: Vec<(&str, f64)>,
        cash: f64,
    ) -> MarginSnapshot {
        // 测试默认把 NAV 取成旧式 `cash + Σ浮盈亏`，需要"保证金口径 NAV"的
        // 用例在构造后直接改 `snap.nav`（见 cross_no_liquidation_at_open...）
        let nav = cash + unrealized.iter().map(|(_, v)| *v).sum::<f64>();
        MarginSnapshot {
            positions: positions
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            prices: prices
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            unrealized: unrealized
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            cash,
            nav,
        }
    }

    fn tracker(specs: Vec<(&str, MarginSpec)>) -> MarginTracker {
        MarginTracker::new(specs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    #[test]
    fn isolated_liquidation_reduces_to_lower_tier_not_zero() {
        // 1x、10 万名义（2 张 × 5 万）、5 万保证金；亏 4.97 万 → 权益 300
        // 维持保证金 450 > 权益 → 触发，减到第 1 档下限 5 万名义（1 张）后恢复
        let spec = MarginSpec {
            leverage: 1.0,
            mode: MarginMode::Isolated,
            brackets: btc_brackets(),
            bracket_source: "default".into(),
        };
        let mut t = tracker(vec![("BTC/USDT", spec)]);
        t.set_isolated("BTC/USDT", 50_000.0);
        let snap = view(
            vec![("BTC/USDT", 2.0)],
            vec![("BTC/USDT", 50_000.0)],
            vec![("BTC/USDT", -49_700.0)],
            0.0,
        );
        let plan = t.plan(&snap);
        assert_eq!(plan.len(), 1);
        let act = &plan[0];
        assert_eq!(act.reason, LiquidateReason::Partial);
        // 减到该档下限（5 万名义）即可，不该直接清零
        assert!(act.target_qty <= 2.0);
        assert!(act.target_qty >= 0.0);
        assert_eq!(act.target_qty, 1.0);
        assert_eq!(act.margin_mode, MarginMode::Isolated);
    }

    #[test]
    fn isolated_no_action_when_equity_above_maintenance() {
        let spec = MarginSpec {
            leverage: 10.0,
            mode: MarginMode::Isolated,
            brackets: btc_brackets(),
            bracket_source: "default".into(),
        };
        let mut t = tracker(vec![("BTC/USDT", spec)]);
        t.set_isolated("BTC/USDT", 1_000.0);
        let snap = view(
            vec![("BTC/USDT", 0.1)],
            vec![("BTC/USDT", 50_000.0)],
            vec![("BTC/USDT", 100.0)],
            9_000.0,
        );
        assert!(t.plan(&snap).is_empty());
    }

    #[test]
    fn cross_liquidates_largest_notional_first() {
        let mk = |lev: f64| MarginSpec {
            leverage: lev,
            mode: MarginMode::Cross,
            brackets: btc_brackets(),
            bracket_source: "default".into(),
        };
        let t = tracker(vec![("BTC/USDT", mk(10.0)), ("ETH/USDT", mk(10.0))]);
        // 账户权益 -3.05 万，维持保证金 160 远大于它 → 两个品种都要减
        let snap = view(
            vec![("BTC/USDT", 0.4), ("ETH/USDT", 10.0)],
            vec![("BTC/USDT", 50_000.0), ("ETH/USDT", 2_000.0)],
            vec![("BTC/USDT", -30_000.0), ("ETH/USDT", -500.0)],
            0.0,
        );
        let plan = t.plan(&snap);
        assert!(!plan.is_empty());
        // 名义更大者先被减（两者名义相等时按 symbol 兜底，BTC 在前）
        assert_eq!(plan[0].symbol, "BTC/USDT");
    }

    /// 全仓权益取 NAV：20x 开仓时名义 5000 由持仓承担，权益仍是 995 →
    /// 维持保证金 20 远小于权益，不应判爆仓（旧口径 cash+浮盈亏会算出负数）。
    #[test]
    fn cross_no_liquidation_at_open_when_equity_is_nav() {
        let spec = MarginSpec {
            leverage: 20.0,
            mode: MarginMode::Cross,
            brackets: btc_brackets(),
            bracket_source: "default".into(),
        };
        let t = tracker(vec![("LTC/USDT", spec)]);
        let mut snap = view(
            vec![("LTC/USDT", 50.0)],
            vec![("LTC/USDT", 100.0)],
            vec![("LTC/USDT", 0.0)],
            995.0,
        );
        snap.nav = 995.0; // 名义 5000 由持仓承担，权益仍是 995
        assert!(t.plan(&snap).is_empty());
    }

    #[test]
    fn depleted_reason_when_zeroed() {
        let spec = MarginSpec {
            leverage: 20.0,
            mode: MarginMode::Isolated,
            brackets: btc_brackets(),
            bracket_source: "default".into(),
        };
        let mut t = tracker(vec![("BTC/USDT", spec)]);
        t.set_isolated("BTC/USDT", 2_500.0);
        let snap = view(
            vec![("BTC/USDT", 1.0)],
            vec![("BTC/USDT", 50_000.0)],
            vec![("BTC/USDT", -60_000.0)],
            0.0,
        );
        let plan = t.plan(&snap);
        assert_eq!(plan[0].reason, LiquidateReason::Depleted);
        assert_eq!(plan[0].target_qty, 0.0);
    }
}
