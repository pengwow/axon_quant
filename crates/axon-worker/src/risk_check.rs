//! 内核风控接缝的引擎实现：`axon-risk::DefaultRiskEngine` → `RiskCheck`
//!
//! QuantCell B3 风控接线：paper / live 驱动配置 [`axon_risk::config::RiskConfig`] 后，把
//! [`EngineRiskCheck`] 装进 `PaperSink::with_risk` / `LiveSink::with_risk`，
//! 三环境（回测除外，历史结果一致性优先）共用同一套 pre-trade 风控。
//!
//! 核心语义是**拒开仓、放减仓**：平仓与同向减仓在进引擎检查链之前直接放行
//! ——熔断触发后引擎拒一切订单，减仓放行分支保证此时仍能止损出场；扩仓
//! （从零建仓 / 同向加仓 / 跨零翻仓）才走
//! [`axon_risk::engine::RiskEngine::check_order`]（熔断 → 限价单名义额 → 杠杆 → 回撤）。
//!
//! 口径注记：引擎的 `max_position_per_instrument` 是**数量**口径，而
//! QuantCell 的 `worker_risk_control` 是**金额/比例**口径，桥层把前者置
//! `f64::INFINITY` 禁用，另以构造参数 `max_position_value` 做目标净仓名义额
//! 检查。paper 侧当日盈亏由 Python 经 [`RiskHandle::update_daily_pnl`] 回灌，
//! 累计亏损触达 `max_daily_loss` 时联动熔断；跨日调用 [`RiskHandle::reset_daily`]。

use std::sync::{Arc, Mutex, MutexGuard};

use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::portfolio::{Currency, Portfolio, Position};
use axon_core::types::{Instrument, Price, Quantity};
use axon_kernel::host::{HostCtx, OrderIntent, OrderSpec};
use axon_kernel::risk::RiskCheck;
use axon_risk::config::RiskConfig;
use axon_risk::engine::{DefaultRiskEngine, RiskEngine};
use axon_risk::error::RiskResult;

/// 引擎背书的 pre-trade 风控（实现内核接缝 `RiskCheck`）
pub struct EngineRiskCheck {
    /// 与 [`RiskHandle`] 共享同一引擎实例：handle 负责 pnl 回灌 / 跨日清零
    engine: Arc<Mutex<DefaultRiskEngine>>,
    /// 单品种目标仓名义额上限（金额口径，替代引擎的数量口径 position limit）；
    /// `None` 表示不做名义额检查（仅引擎检查链）
    max_position_value: Option<f64>,
}

/// 共享引擎状态的运行时句柄：回灌当日盈亏（联动熔断）、跨日清零
///
/// `Clone` 设计：驱动层可以把句柄转交给 Python 侧的 pnl 回灌线程/Sync 线程。
#[derive(Clone)]
pub struct RiskHandle {
    engine: Arc<Mutex<DefaultRiskEngine>>,
}

impl EngineRiskCheck {
    /// 创建（检查器，句柄）对
    ///
    /// `Box<dyn RiskCheck>` 装进 sink 后取不回，运行时状态更新必须走同时
    /// 返回的 `RiskHandle`。
    pub fn new(config: RiskConfig, max_position_value: Option<f64>) -> (Self, RiskHandle) {
        let mut config = config;
        // 兑现模块级文档口径：数量限额对低价标的（如 $0.1 币种，10 万"个"仅 1 万名义）
        // 会误拒正常仓位，统一禁用，仓位限额只走金额口径的 max_position_value
        config.max_position_per_instrument = f64::INFINITY;
        let engine = Arc::new(Mutex::new(DefaultRiskEngine::new(config)));
        (
            Self {
                engine: Arc::clone(&engine),
                max_position_value,
            },
            RiskHandle { engine },
        )
    }

    fn lock_engine(&self) -> MutexGuard<'_, DefaultRiskEngine> {
        // 中毒恢复：单次 panic 不应把风控永久锁死（拒绝服务比带缺陷放行更糟）
        self.engine.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// SetTarget 分支：先按权益换算目标数量，减仓放行、扩仓进引擎链
    fn check_set_target(
        &mut self,
        instrument: &Instrument,
        target_ratio: f64,
        ctx: &HostCtx,
    ) -> Result<(), String> {
        // 无报价 / 非正价：无法换算目标数量也无从评险，放行（撮合层处理无价场景）
        let Some(&price) = ctx.last_price.get(instrument) else {
            return Ok(());
        };
        if price <= 0.0 {
            return Ok(());
        }
        let current = ctx.positions.get(instrument).copied().unwrap_or(0.0);
        // 与 BacktestSink 同款的唯一换算点：target_qty = ratio × equity / price
        let target_qty = target_ratio * ctx.equity / price;
        // 平仓放行
        if target_qty == 0.0 {
            return Ok(());
        }
        // 同向减仓放行（风险只降不升；熔断后的出场通道，必须位于引擎链之前）
        if current * target_qty > 0.0 && target_qty.abs() <= current.abs() {
            return Ok(());
        }
        // 爆仓（权益耗尽）禁扩仓
        if ctx.equity <= 0.0 {
            return Err(format!("equity {:+.2} <= 0，禁止扩仓", ctx.equity));
        }
        let side = if target_qty > 0.0 {
            Side::Buy
        } else {
            Side::Sell
        };
        self.check_notional(target_qty.abs(), price, "目标仓位")?;
        self.run_engine(
            instrument,
            side,
            (target_qty - current).abs(),
            Some(price),
            ctx,
        )
    }

    /// Submit 分支：按目标净仓判断减仓 / 扩仓，市价无价单只跑引擎链
    fn check_submit(&mut self, spec: &OrderSpec, ctx: &HostCtx) -> Result<(), String> {
        let qty = spec.quantity.as_f64();
        // 非正数量是异常规格，交由撮合 / 网关层校验，风控不拦（避免误伤）
        if qty <= 0.0 {
            return Ok(());
        }
        let current = ctx.positions.get(&spec.instrument).copied().unwrap_or(0.0);
        let delta = match spec.side {
            Side::Buy => qty,
            Side::Sell => -qty,
        };
        let new_qty = current + delta;
        // 平仓放行
        if new_qty == 0.0 {
            return Ok(());
        }
        // 同向减仓放行（同上：熔断后的出场通道）
        if current * new_qty > 0.0 && new_qty.abs() <= current.abs() {
            return Ok(());
        }
        if ctx.equity <= 0.0 {
            return Err(format!("equity {:+.2} <= 0，禁止扩仓", ctx.equity));
        }
        // 参考价：限价单用委托价，市价单用盯市价；两者皆无（市价 + 无报价）
        // 则名义额不可估，跳过名义额检查、只跑引擎链——宁可放行也不卡死交易
        let ref_price = spec
            .price
            .map(|p| p.as_f64())
            .filter(|p| *p > 0.0)
            .or_else(|| {
                ctx.last_price
                    .get(&spec.instrument)
                    .copied()
                    .filter(|p| *p > 0.0)
            });
        if let Some(price) = ref_price {
            self.check_notional(new_qty.abs(), price, "订单")?;
        }
        self.run_engine(&spec.instrument, spec.side, qty, ref_price, ctx)
    }

    /// 金额口径名义额上限（引擎 `max_position_per_instrument` 是数量口径，
    /// 桥层已禁用，这里代管 QuantCell 的金额/比例语义）
    fn check_notional(&self, net_qty: f64, price: f64, label: &str) -> Result<(), String> {
        if let Some(limit) = self.max_position_value {
            let notional = (net_qty * price).abs();
            if notional > limit {
                return Err(format!(
                    "{label}名义额 {notional:.2} 超过单品种上限 {limit:.2}"
                ));
            }
        }
        Ok(())
    }

    /// 镜像 Portfolio 后跑引擎检查链（熔断 → 限价单名义额 → 杠杆 → 回撤）
    fn run_engine(
        &self,
        instrument: &Instrument,
        side: Side,
        qty: f64,
        ref_price: Option<f64>,
        ctx: &HostCtx,
    ) -> Result<(), String> {
        let engine = self.lock_engine();
        let portfolio = mirror_portfolio(ctx);
        let order = build_order(instrument, side, qty, ref_price);
        match engine.check_order(&order, &portfolio) {
            RiskResult::Allow => Ok(()),
            // Warn 是引擎的预警档：记日志、不拦截
            RiskResult::Warn(msg) => {
                tracing::warn!(risk_warning = %msg, "风控预警（放行）");
                Ok(())
            }
            RiskResult::Reject(reason) => Err(format!("{reason:?}")),
        }
    }
}

impl RiskCheck for EngineRiskCheck {
    fn check(&mut self, intent: &OrderIntent, ctx: &HostCtx) -> Result<(), String> {
        match intent {
            // 撤单只动既有挂单，不改变敞口，恒放行
            OrderIntent::Cancel(_) => Ok(()),
            OrderIntent::SetTarget {
                instrument,
                target_ratio,
            } => self.check_set_target(instrument, *target_ratio, ctx),
            OrderIntent::Submit(spec) => self.check_submit(spec, ctx),
        }
    }
}

impl RiskHandle {
    /// 回灌当日盈亏（**累加**语义：每次传增量 delta，不是累计值）
    ///
    /// 引擎内部累计触达 `-max_daily_loss` 即触发熔断，之后的扩仓全部被拒。
    pub fn update_daily_pnl(&self, pnl: f64) {
        self.lock_engine().update_daily_pnl(pnl);
    }

    /// 跨日清零：清当日累计盈亏与熔断器（不清 VaR 历史窗口）
    pub fn reset_daily(&self) {
        self.lock_engine().reset_daily();
    }

    fn lock_engine(&self) -> MutexGuard<'_, DefaultRiskEngine> {
        self.engine.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// 用 ctx 镜像一份保证金语义的 Portfolio：
/// cash = equity − 持仓盯市市值（有报价的仓位），NAV ≈ equity。
///
/// 杠杆 = NAV/cash ≈ 权益/(权益−敞口)，满仓（cash<=0）由引擎的杠杆检查
/// 拒绝扩仓（`InsufficientMargin`）。ponytail: `Position::market_value` 带符号
/// 而 `Portfolio::nav()` 直加，空头仓位镜像后 NAV = equity − 2|mv| 数值失真
/// （更保守），cash<=0 的兜底仍在；升级路径是引擎侧换 NAV 口径的杠杆。
fn mirror_portfolio(ctx: &HostCtx) -> Portfolio {
    let mut pf = Portfolio::new(Currency::USDT, 0.0);
    // equity 全额入保证金账户
    pf.deposit(Currency::USDT, ctx.equity);
    for (instrument, &qty) in &ctx.positions {
        if qty == 0.0 {
            continue;
        }
        let market = ctx.last_price.get(instrument).copied().filter(|p| *p > 0.0);
        if let Some(price) = market {
            // 有报价的仓位才扣盯市市值——无报价仓位对杠杆无贡献（保守侧可接受）
            pf.deposit(Currency::USDT, -(qty * price).abs());
        }
        let mut pos = Position::with_instrument(
            instrument.clone(),
            Quantity::from_f64(qty),
            Price::from_f64(market.unwrap_or(0.0)),
        );
        pos.market_price = market.map(Price::from_f64);
        pf.add_position(pos);
    }
    pf
}

/// 构造风控评估用的订单：限价用参考价（GTC）、市价无价（IOC）。
/// quantity 是本笔**增量**（扩仓时 |target − current|），与订单本义一致；
/// 引擎的数量口径 position limit 已在桥层禁用，这里仅供 order_size 查限价单名义额。
fn build_order(instrument: &Instrument, side: Side, qty: f64, ref_price: Option<f64>) -> Order {
    let quantity = Quantity::from_f64(qty.abs());
    let (order_type, tif) = match ref_price {
        Some(price) => (
            OrderType::Limit {
                price: Price::from_f64(price),
            },
            TimeInForce::GTC,
        ),
        None => (OrderType::Market, TimeInForce::IOC),
    };
    match instrument {
        Instrument::Spot(s) => Order::spot(
            0,
            s.base.clone(),
            s.quote.clone(),
            side,
            order_type,
            quantity,
            tif,
        ),
        Instrument::Swap(s) => Order::swap(
            0,
            s.base.clone(),
            s.quote.clone(),
            s.settle,
            s.contract_size,
            side,
            order_type,
            quantity,
            tif,
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use axon_core::market::Side;
    use axon_core::types::{Instrument, Quantity, SpotInstrument, Symbol};

    use super::*;

    fn btc() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        })
    }

    /// 默认 RiskConfig + 金额口径上限 `max_value`
    fn make_pair(max_value: f64) -> (EngineRiskCheck, RiskHandle) {
        EngineRiskCheck::new(RiskConfig::default(), Some(max_value))
    }

    fn ctx(equity: f64, price: f64, current: f64) -> HostCtx {
        HostCtx {
            positions: if current != 0.0 {
                HashMap::from([(btc(), current)])
            } else {
                HashMap::new()
            },
            equity,
            last_price: HashMap::from([(btc(), price)]),
        }
    }

    fn set_target(ratio: f64) -> OrderIntent {
        OrderIntent::SetTarget {
            instrument: btc(),
            target_ratio: ratio,
        }
    }

    fn submit(side: Side, qty: f64, price: Option<f64>) -> OrderIntent {
        OrderIntent::Submit(OrderSpec {
            instrument: btc(),
            side,
            quantity: Quantity::from_f64(qty),
            price: price.map(Price::from_f64),
        })
    }

    #[test]
    fn cancel_and_priceless_set_target_pass() {
        let (mut check, _h) = make_pair(5_000.0);
        check
            .check(&OrderIntent::Cancel(7), &HostCtx::default())
            .expect("撤单恒放行");
        // 无报价：无法换算目标数量，放行
        let ctx = HostCtx {
            positions: HashMap::new(),
            equity: 10_000.0,
            last_price: HashMap::new(),
        };
        check
            .check(&set_target(0.5), &ctx)
            .expect("无价 SetTarget 放行");
    }

    #[test]
    fn set_target_reduce_passes_and_expansion_blocked_by_notional() {
        let (mut check, _h) = make_pair(5_000.0);
        // equity 10k / price 100：0.6 目标 = 60 qty × 100 = 6000 名义 > 5000 → 拒
        let err = check
            .check(&set_target(0.6), &ctx(10_000.0, 100.0, 0.0))
            .expect_err("扩仓应被名义额上限拒绝");
        assert!(err.contains("名义额"), "实际拒因: {err}");
        // 已有 60 qty，目标 0.4 = 40 qty：同向减仓放行（不查名义额）
        check
            .check(&set_target(0.4), &ctx(10_000.0, 100.0, 60.0))
            .expect("同向减仓放行");
    }

    #[test]
    fn circuit_breaker_blocks_expansion_but_reduce_and_reset_work() {
        let (mut check, handle) = make_pair(5_000.0);
        // 默认 max_daily_loss = 10_000：累计 −10_000 触发熔断
        handle.update_daily_pnl(-10_000.0);
        let err = check
            .check(&set_target(0.4), &ctx(10_000.0, 100.0, 0.0))
            .expect_err("熔断后扩仓必须被拒");
        assert!(err.contains("CircuitBreaker"), "实际拒因: {err}");
        // 减仓 / 平仓仍放行（熔断下的出场通道）
        check
            .check(&set_target(0.4), &ctx(10_000.0, 100.0, 60.0))
            .expect("熔断后减仓放行");
        check
            .check(&set_target(0.0), &ctx(10_000.0, 100.0, 60.0))
            .expect("熔断后平仓放行");
        // 跨日清零后扩仓恢复受名义额约束（0.4 → 4000 < 5000 → 放行）
        handle.reset_daily();
        check
            .check(&set_target(0.4), &ctx(10_000.0, 100.0, 0.0))
            .expect("熔断恢复后扩仓过名义额即放行");
    }

    #[test]
    fn exhausted_equity_blocks_expansion() {
        let (mut check, _h) = make_pair(5_000.0);
        // 负权益（爆仓态）：0.5 目标换算出非零空头 target（ratio × equity < 0），
        // 非减仓 → 权益检查拒。注意 equity 恰为 0 时 target 换算恒 0、走"平仓
        // 放行"短路——那是无敞口变化的空操作，放行本就安全
        let err = check
            .check(&set_target(0.5), &ctx(-1_000.0, 100.0, 0.0))
            .expect_err("负权益禁止扩仓");
        assert!(err.contains("equity"), "实际拒因: {err}");
    }

    #[test]
    fn leverage_link_rejects_near_full_position() {
        // 名义额放宽，验证引擎杠杆链（数量口径 position limit 已禁用不挡路）
        let (mut check, _h) = make_pair(1_000_000.0);
        // 已有 85 qty（mv 8500），目标 0.999 → 99.9 qty：镜像 cash = 10000−8500 = 1500
        // leverage = nav/cash = 10000/1500 ≈ 6.7 > 5 → MaxLeverageExceeded
        let err = check
            .check(&set_target(0.999), &ctx(10_000.0, 100.0, 85.0))
            .expect_err("接近满仓应被引擎杠杆检查拒绝");
        assert!(
            err.contains("MaxLeverage") || err.contains("Exceeded"),
            "实际拒因: {err}"
        );
    }

    #[test]
    fn submit_reduce_passes_priceless_market_only_engine_chain() {
        let (mut check, _h) = make_pair(5_000.0);
        // 无价环境下的减仓：new = 5 − 2 = 3，同向减仓放行
        let ctx = HostCtx {
            positions: HashMap::from([(btc(), 5.0)]),
            equity: 10_000.0,
            last_price: HashMap::new(),
        };
        check
            .check(&submit(Side::Sell, 2.0, None), &ctx)
            .expect("无价市价减仓放行");
        // 无仓 + 无价 + 市价：跨零扩仓，名义额不可估 → 跳过名义额只跑引擎链 → 放行
        let ctx_flat = HostCtx {
            positions: HashMap::new(),
            equity: 10_000.0,
            last_price: HashMap::new(),
        };
        check
            .check(&submit(Side::Buy, 100.0, None), &ctx_flat)
            .expect("无价市价扩仓只跑引擎链（跳过名义额）");
    }

    #[test]
    fn submit_cross_zero_expansion_blocked_by_notional() {
        let (mut check, _h) = make_pair(500.0);
        // 持多 5，卖出 12 → 目标净空 7 × 100 = 700 名义 > 500 → 拒
        let err = check
            .check(
                &submit(Side::Sell, 12.0, Some(100.0)),
                &ctx(10_000.0, 100.0, 5.0),
            )
            .expect_err("跨零翻仓按目标净仓名义额拒绝");
        assert!(err.contains("名义额"), "实际拒因: {err}");
    }
}
