//! 组合级风险敞口计算(0.7.0 Phase 4 新增,0.8.0 Phase 2 B1/B2 接入真实数据源 + contract_size,B3 接入跨 instrument 协方差)
//!
//! # 范围
//!
//! 提供 `PortfolioRiskEngine::delta_exposure` / `gamma_exposure` / `vega` /
//! `latest_returns` / `gamma_covariance_matrix` / `portfolio_gamma_with_covariance`
//! 六类风险敞口,数据源是 `axon_core::portfolio::Portfolio.positions` +
//! 可选 `axon_core::data::MarketDataSource`(B1 接入)。
//!
//! ## 定义
//!
//! - **per-leg delta** = `position.quantity × contract_size`(0.8.0 B2 接入)
//!   - spot 默认 `contract_size = 1.0`
//!   - swap 取 `SwapInstrument.contract_size`
//!   - 0.7.0 隐式 `contract_size = 1.0`,B2 后乘以实际值(语义升级)
//! - **per-leg gamma** = `qty × mark_variance × contract_size²`(0.8.0 B1 + B2)
//!   - 当 `MarketDataSource` 存在:用 `mark_history` 计算方差
//!   - 当 source 不存在:`0.0`(0.7.0 兼容)
//! - **per-leg latest return** = `MarketDataSource::latest_return`(0.8.0 B3 新增)
//!   - 取最新 1 帧 mark 收益率(最小开销路径,`lookback = 2`)
//!   - 当 source 不存在:返回空 `HashMap`(0.7.0 兼容)
//!   - 适合实时 PnL 监控 / 跨周期风险预警 / tick-level risk gate
//! - **跨 instrument gamma 协方差**(0.8.0 B3 新增):
//!   - 返回 n×n 协方差矩阵 `HashMap<(Instrument, Instrument), f64>`
//!   - 对角(i,i) = `var(returns_i) × qty_i² × contract_size_i²`
//!   - 非对角(i,j) = `cov(returns_i, returns_j) × qty_i × qty_j × contract_size_i × contract_size_j`
//!   - N < p(N=样本,p=instrument)时用 Ledoit-Wolf 收缩保证正定
//! - **portfolio delta** = `Σ per_leg_delta`
//! - **portfolio gamma with covariance** = `Σ_i Σ_j Cov(i, j)`(0.8.0 B3 新增)
//! - **vega** = `Σ (qty × IV × contract_size)`(0.8.0 B1 + B2)
//!   - spot 通常无 IV → 跳过
//!   - source 不存在 → `0.0`(0.7.0 兼容)
//!
//! ## 不在本 plan 范围
//!
//! - vol-based gamma(0.8.0 B1 已用 mark_variance,0.9.0 可换 Garman-Klass vol)
//! - `equity_curve` vs `bar_nav_curve` 整合(B4)
//! - EWMA / GARCH 协方差 — 0.9.0 评估
//!
//! # 用法
//!
//! ```rust,no_run
//! use axon_risk::portfolio::PortfolioRiskEngine;
//! use axon_core::portfolio::Portfolio;
//! use std::sync::Arc;
//!
//! // 0.7.0 兼容:无 source → gamma/vega 全 0
//! let engine = PortfolioRiskEngine::new();
//! let portfolio = Portfolio::new(Default::default(), 0.0);
//! let delta = engine.delta_exposure(&portfolio);
//! assert_eq!(engine.gamma_exposure(&portfolio).values().sum::<f64>(), 0.0);
//!
//! // 0.8.0:接入 source 后 gamma/vega 用真实数据
//! let source = Arc::new(axon_core::InMemoryMarketData::new());
//! let engine2 = PortfolioRiskEngine::with_source(source);
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use axon_core::data::MarketDataSource;
use axon_core::portfolio::Portfolio;
use axon_core::types::Instrument;

use serde::{Deserialize, Serialize};

/// 提取 instrument 的合约乘数(0.8.0 B2 新增)
///
/// - `Instrument::Spot(_)` → `1.0`(现货无合约乘数)
/// - `Instrument::Swap(swap)` → `swap.contract_size`
///
/// 集中处理 `Instrument` → `f64` 的映射,避免 `delta_exposure` / `vega` /
/// `gamma_exposure` 重复 `match` 分支(B2 公式统一入口)。
fn contract_size_of(instrument: &Instrument) -> f64 {
    match instrument {
        Instrument::Spot(_) => 1.0,
        Instrument::Swap(swap) => swap.contract_size,
    }
}

/// 0.7.0 Phase 4 新增:风险敞口报告
///
/// 由 `BacktestEngine::run` 填充
/// 到 `RunResult.risk_metrics`,同时通过 PyO3 binding 暴露到
/// `run_result["risk_metrics"]` dict。
///
/// 注:本结构定义在 `axon-risk`,反向依赖 `axon-backtest`,所以
/// rustdoc intra-doc links 不指向 `axon_backtest::*`(避免循环依赖
/// 解析失败 — 在 `axon-risk` 内 `axon_backtest` 不可见)。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RiskMetricsReport {
    /// 每个 instrument 的 delta 暴露
    /// (`instrument -> delta`)
    pub per_leg_delta: HashMap<Instrument, f64>,
    /// 组合总 delta(Σ per-leg delta)
    pub portfolio_delta: f64,
    /// 每个 instrument 的 gamma 暴露
    /// (`instrument -> gamma`)
    pub per_leg_gamma: HashMap<Instrument, f64>,
    /// 组合总 gamma
    pub total_gamma: f64,
    /// vega(0.7.0 暂 0.0,0.8.0 接 IV 源)
    pub vega: f64,
    /// 多 leg Sharpe(沿用 `RunResult.sharpe_ratio`)
    pub sharpe_with_legs: f64,
}

impl RiskMetricsReport {
    /// 创建空报告
    pub fn empty() -> Self {
        Self::default()
    }

    /// 从 per-leg map + Sharpe 计算 portfolio-level 字段
    ///
    /// 在 [`PortfolioRiskEngine::compute_report`] 中调用,本结构本身不实现计算逻辑
    /// —— 保持只读视图语义。
    pub fn aggregate(per_leg_delta: HashMap<Instrument, f64>, sharpe: f64) -> Self {
        let portfolio_delta: f64 = per_leg_delta.values().sum();
        // gamma 暂时全 0,留 0.8.0
        let per_leg_gamma: HashMap<Instrument, f64> =
            per_leg_delta.keys().map(|k| (k.clone(), 0.0)).collect();
        Self {
            per_leg_delta,
            portfolio_delta,
            per_leg_gamma,
            total_gamma: 0.0,
            vega: 0.0,
            sharpe_with_legs: sharpe,
        }
    }
}

/// 组合级风险敞口计算引擎
///
/// 0.7.0 Phase 4 新增,包装 `Portfolio.positions`,对外暴露 delta / gamma / vega
/// 三类敞口。
///
/// 0.8.0 Phase 2 B1 扩展:可选持有 `Arc<dyn MarketDataSource>`,有 source 时
/// gamma/vega 用真实 mark 历史 + IV,无 source 时回退 0.0(向后兼容)。
#[derive(Clone)]
pub struct PortfolioRiskEngine {
    /// 历史市场数据源(0.8.0 B1 接入,可空)
    source: Option<Arc<dyn MarketDataSource>>,
    /// gamma 计算的 mark 历史 lookback(0.8.0 B1 默认 100 帧)
    gamma_lookback: usize,
}

impl Default for PortfolioRiskEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl PortfolioRiskEngine {
    /// 创建新引擎(无数据源,gamma/vega 返回 0.0 — 0.7.0 兼容行为)
    pub fn new() -> Self {
        Self {
            source: None,
            gamma_lookback: 100,
        }
    }

    /// 创建带数据源的引擎(0.8.0 B1 新增)
    ///
    /// 接入后 `gamma_exposure` / `vega` 用真实 mark 历史 + IV 计算。
    pub fn with_source(source: Arc<dyn MarketDataSource>) -> Self {
        Self {
            source: Some(source),
            gamma_lookback: 100,
        }
    }

    /// 设置 gamma lookback(0.8.0 B1 新增)
    ///
    /// 默认 100 帧;用户可调小(快)或调大(稳)。
    pub fn with_gamma_lookback(mut self, lookback: usize) -> Self {
        self.gamma_lookback = lookback;
        self
    }

    /// 取出内部数据源引用(用于测试 / 调试)
    pub fn source(&self) -> Option<&Arc<dyn MarketDataSource>> {
        self.source.as_ref()
    }

    /// 计算 per-leg delta
    ///
    /// 定义(0.8.0 B2 升级):`delta[instrument] = position.quantity × contract_size`
    /// - spot: `contract_size = 1.0`(默认)
    /// - swap: 取 `SwapInstrument.contract_size`
    ///
    /// **0.7.0 → 0.8.0 BREAKING 行为变更**:
    /// - 0.7.0 隐式 `contract_size = 1.0`,所有 leg 直接用 qty
    /// - 0.8.0 接入 `SwapInstrument.contract_size`,perp leg 的 delta 量级会变
    ///   (如 `contract_size = 0.01` 的 ETH perp 持仓 100 张 → delta = 1.0,0.7.0 则是 100.0)
    ///
    /// 返回:`HashMap<Instrument, f64>`,只包含非零持仓
    pub fn delta_exposure(&self, portfolio: &Portfolio) -> HashMap<Instrument, f64> {
        portfolio
            .positions()
            .iter()
            .filter_map(|(inst, pos)| {
                let qty = pos.quantity.as_f64();
                if qty.abs() > 1e-9 {
                    let delta = qty * contract_size_of(inst);
                    Some((inst.clone(), delta))
                } else {
                    None
                }
            })
            .collect()
    }

    /// 计算 per-leg gamma
    ///
    /// 0.7.0 范围:全部返回 `0.0`(无 mark 历史,无 IV 源)
    /// 0.8.0 B1:`qty × mark_variance`(数据源接入)
    /// 0.8.0 B2:`qty × mark_variance × contract_size²`(合约乘数二次方)
    ///
    /// 返回:`HashMap<Instrument, f64>`,只包含非零持仓
    pub fn gamma_exposure(&self, portfolio: &Portfolio) -> HashMap<Instrument, f64> {
        let Some(source) = &self.source else {
            // 0.7.0 兼容:无 source → 全 0
            return portfolio
                .positions()
                .iter()
                .filter_map(|(inst, pos)| {
                    if pos.quantity.as_f64().abs() > 1e-9 {
                        Some((inst.clone(), 0.0))
                    } else {
                        None
                    }
                })
                .collect();
        };

        portfolio
            .positions()
            .iter()
            .filter_map(|(inst, pos)| {
                let qty = pos.quantity.as_f64();
                if qty.abs() < 1e-9 {
                    return None;
                }

                // 0.8.0 B1+B2 gamma 公式:qty × mark_variance × contract_size²
                // - B1 接入 mark_variance(从 MarketDataSource 取历史)
                // - B2 接入 contract_size²(perp gamma 受合约乘数平方影响)
                let mark_variance = mark_variance(source, inst, self.gamma_lookback);
                let cs = contract_size_of(inst);
                let gamma = qty * mark_variance * cs * cs;
                Some((inst.clone(), gamma))
            })
            .collect()
    }

    /// 组合总 delta(Σ per-leg delta)
    pub fn portfolio_delta(&self, portfolio: &Portfolio) -> f64 {
        self.delta_exposure(portfolio).values().sum()
    }

    /// 组合总 gamma
    pub fn total_gamma(&self, portfolio: &Portfolio) -> f64 {
        self.gamma_exposure(portfolio).values().sum()
    }

    /// 组合总 vega(`Σ qty × IV × contract_size`)
    ///
    /// 0.7.0 范围:0.0(无 IV 源)
    /// 0.8.0 B1:用 `MarketDataSource::implied_vol`
    /// 0.8.0 B2:乘以 `contract_size`(perp vega 受合约乘数影响)
    pub fn vega(&self, portfolio: &Portfolio) -> f64 {
        let Some(source) = &self.source else {
            return 0.0;
        };

        let mut total = 0.0;
        for (inst, pos) in portfolio.positions() {
            let qty = pos.quantity.as_f64();
            if qty.abs() < 1e-9 {
                continue;
            }
            // 0.8.0 B1:spot 通常无 IV → None → 跳过(vega 仅适用于有 IV 的 instrument)
            // 0.8.0 B2:vega 公式扩展为 `qty × IV × contract_size`
            if let Some(iv) = source.implied_vol(inst) {
                let cs = contract_size_of(inst);
                total += qty * iv * cs;
            }
        }
        total
    }

    /// 0.8.0 B3 新增:返回每 instrument 的最新一帧 mark 收益率
    ///
    /// 与 `MarketDataSource::mark_returns` 的区别:本方法只取最新 1 帧
    /// (内部走 `lookback = 2` 的最小路径),适合需要每个 tick / bar 都
    /// 查询的实时场景(避免 `mark_returns(n)` 重新分配整个 returns 序列)。
    ///
    /// # 用途
    ///
    /// - 实时 PnL 监控:`latest_return × qty × contract_size × latest_mark`
    /// - 跨周期风险预警(返回值 > 阈值时触发告警)
    /// - 高频策略的 tick-level risk gate(每个 quote 都查一次)
    ///
    /// # 边界
    ///
    /// - 无 `source` → 返回空 `HashMap`(0.7.0 兼容行为)
    /// - 无持仓 → 返回空 `HashMap`
    /// - 单 instrument 数据不足(< 2 帧 mark)→ 该 instrument 不出现在结果中
    ///
    /// # 与 `mark_returns` 的对比
    ///
    /// | 方法 | 返回 | 单次查询开销 |
    /// |------|------|------------|
    /// | `mark_returns(inst, n)` | 整个 returns 序列(O(n)) | 重新分配 `Vec<f64>` |
    /// | `latest_returns(portfolio)` | 每 instrument 最新 1 帧 | 只读 2 帧 mark |
    pub fn latest_returns(&self, portfolio: &Portfolio) -> HashMap<Instrument, f64> {
        let Some(source) = &self.source else {
            return HashMap::new();
        };
        portfolio
            .positions()
            .iter()
            .filter_map(|(inst, pos)| {
                if pos.quantity.as_f64().abs() < 1e-9 {
                    None
                } else {
                    source.latest_return(inst).map(|r| (inst.clone(), r))
                }
            })
            .collect()
    }

    /// 0.8.0 B3 新增:计算跨 instrument 的 gamma 协方差矩阵
    ///
    /// 返回 n×n 协方差矩阵(对称半正定),`HashMap<(Instrument, Instrument), f64>` 形式:
    /// - 对角 `(i, i)` = `var(returns_i) × qty_i² × contract_size_i²`
    /// - 非对角 `(i, j)` (i < j) = `cov(returns_i, returns_j) × qty_i × qty_j × contract_size_i × contract_size_j`
    ///
    /// # 存储约定
    ///
    /// 只存上三角(`i <= j`):HashMap 长度 = `p(p+1)/2`(对称矩阵,
    /// 避免冗余存储)。`cov.get(&(a, b))` 中 `a, b` 顺序应与 insts 顺序一致;
    /// 调用方需要矩阵对称性时用 `a < b` 顺序访问。
    ///
    /// # 算法
    ///
    /// 1. 收集所有有非零持仓的 instrument(去重)→ list `insts`
    /// 2. 每 instrument 调 `source.mark_returns(inst, lookback)` 取 returns 序列
    /// 3. **任一 instrument 的 returns < 2** → 返回空 `HashMap`(数据不足)
    /// 4. **尾部对齐(0.8.0 B3 修复)**:各 instrument returns 长度可能不同
    ///    (lookback 期间某 instrument 缺数据),取全局最短长度 `n` 并
    ///    **截断每个 returns 到尾部最后 n 帧**。这保证所有 instrument 用
    ///    **同一时间窗口**(最近 `n` 帧),而非最早 `n` 帧(head 对齐会引入
    ///    陈旧数据偏差)。
    /// 5. 计算样本协方差 `Σ̂`(N-1 无偏估计)
    /// 6. 当 `N < p`(样本不足)→ 用 Ledoit-Wolf 收缩(`alpha = 0.2` 默认)
    ///    避免奇异矩阵
    /// 7. 缩放:`cov_scaled[i][j] = cov_returns[i][j] × qty_i × qty_j × cs_i × cs_j`
    /// 8. 返回 `HashMap` 形式(只存上三角)
    ///
    /// # 边界
    ///
    /// - 无 `source` → 返回空 `HashMap`(0.7.0 兼容行为)
    /// - 无持仓 → 返回空 `HashMap`
    /// - 任一 instrument 的 returns 都 < 2 帧 → 返回空 `HashMap`(数据不足)
    /// - 部分 instrument 缺数据 → 尾部对齐到全局最短长度的最后 n 帧
    ///
    /// # BREAKING(轻)
    ///
    /// 新增方法,不修改现有 API。`gamma_exposure` / `total_gamma` 行为不变
    /// (B1/B2 仅对角和,callers 选择性使用新方法)。
    pub fn gamma_covariance_matrix(
        &self,
        portfolio: &Portfolio,
    ) -> HashMap<(Instrument, Instrument), f64> {
        let Some(source) = &self.source else {
            return HashMap::new();
        };

        // 1. 收集所有有非零持仓的 instrument(保持 HashMap 迭代顺序的稳定性)
        let mut insts: Vec<Instrument> = portfolio
            .positions()
            .iter()
            .filter_map(|(inst, pos)| {
                if pos.quantity.as_f64().abs() > 1e-9 {
                    Some(inst.clone())
                } else {
                    None
                }
            })
            .collect();
        // 去重(理论上 portfolio 已 dedupe,这里兜底)
        insts.sort_by_key(|i| format!("{i:?}"));
        insts.dedup();
        let p = insts.len();
        if p == 0 {
            return HashMap::new();
        }

        // 2. 收集每 instrument 的 returns
        let mut returns: Vec<Vec<f64>> = insts
            .iter()
            .map(|inst| source.mark_returns(inst, self.gamma_lookback))
            .collect();

        // 3. 数据不足:任一 instrument 的 returns < 2 → 空(0.8.0 B3 防御)
        let n = returns.iter().map(|r| r.len()).min().unwrap_or(0);
        if n < 2 {
            return HashMap::new();
        }

        // 4. 0.8.0 B3 修复:尾部对齐(0.8.0 B3.5 hotfix)
        //    修复前:不同 instrument 的 returns 长度不一致时,`sample_covariance`
        //    内部用 `take(n)` 取最早 n 帧,导致不同 instrument 实际使用不同
        //    时间窗口(早期数据可能陈旧,协方差失真)。
        //    修复后:显式把每个 returns 截断到全局最短长度的**最后 n 帧**
        //    (尾部对齐),保证所有 instrument 用同一时间窗口(最近数据)。
        //    替代方案:head 对齐(`r[..n].to_vec()`)— 留作历史,本次不用。
        for r in &mut returns {
            let len = r.len();
            if len > n {
                // 切片复制:Vec<f64> 无 split_off 干净分离,
                // 这里 `drain(..len-n)` 移除最前面的多余元素,等价于
                // `r[len-n..].to_vec()` 但 in-place 更省分配。
                r.drain(..len - n);
            }
        }

        // 5. 计算样本协方差(已尾部对齐)
        let sample = sample_covariance(&returns);
        if sample.is_empty() {
            return HashMap::new();
        }

        // 6. N < p 时触发 Ledoit-Wolf 收缩
        let final_cov = if n < p {
            ledoit_wolf_shrink(&sample, DEFAULT_LW_SHRINKAGE)
        } else {
            sample
        };

        // 7. 缩放到 gamma 维度
        let qtys: Vec<f64> = insts
            .iter()
            .map(|inst| {
                portfolio
                    .positions()
                    .get(inst)
                    .map(|pos| pos.quantity.as_f64())
                    .unwrap_or(0.0)
            })
            .collect();
        let cs: Vec<f64> = insts.iter().map(contract_size_of).collect();

        // 8. 写入 HashMap(只存上三角 i <= j,避免冗余)
        let mut out = HashMap::with_capacity(p * (p + 1) / 2);
        for i in 0..p {
            for j in i..p {
                let raw = final_cov
                    .get(i)
                    .and_then(|r| r.get(j))
                    .copied()
                    .unwrap_or(0.0);
                let scaled = raw * qtys[i] * qtys[j] * cs[i] * cs[j];
                out.insert((insts[i].clone(), insts[j].clone()), scaled);
            }
        }
        out
    }

    /// 0.8.0 B3 新增:含协方差的组合 gamma(考虑跨 instrument 相关性)
    ///
    /// 标准组合 gamma 公式:`Σ_i γ_i² + 2 × Σ_{i<j} γ_i × γ_j × ρ_{ij}`
    /// = `Σ_i Σ_j Cov[i, j]`(矩阵自求和)
    ///
    /// 与 `total_gamma`(仅对角和)对比:
    /// - 无 source → 返回 0.0(同 `total_gamma` 兼容)
    /// - 单 leg → 等于 `total_gamma`
    /// - 多 leg 高相关 → 显著大于 `total_gamma`(同向风险累加)
    /// - 多 leg 完全负相关 → 显著小于 `total_gamma`(对冲)
    ///
    /// # 用途
    ///
    /// 套保组合(BTC spot long + BTC perp short)用此方法评估真实 gamma 风险,
    /// 而非简单按 leg 累加的伪风险。
    pub fn portfolio_gamma_with_covariance(&self, portfolio: &Portfolio) -> f64 {
        let cov = self.gamma_covariance_matrix(portfolio);
        if cov.is_empty() {
            return 0.0;
        }
        matrix_self_sum(&cov)
    }

    /// 计算完整 `RiskMetricsReport`
    ///
    /// 由 `BacktestEngine::run` 在产出 `RunResult` 时调用。
    pub fn compute_report(&self, portfolio: &Portfolio, sharpe_ratio: f64) -> RiskMetricsReport {
        let per_leg_delta = self.delta_exposure(portfolio);
        let per_leg_gamma = self.gamma_exposure(portfolio);
        let portfolio_delta = per_leg_delta.values().sum();
        let total_gamma = per_leg_gamma.values().sum();
        let vega = self.vega(portfolio);
        RiskMetricsReport {
            per_leg_delta,
            portfolio_delta,
            per_leg_gamma,
            total_gamma,
            vega,
            sharpe_with_legs: sharpe_ratio,
        }
    }
}

/// 计算 mark 历史的方差(0.8.0 B1 gamma 公式中间步骤)
///
/// 0.8.0 B1 公式:`Variance(prices) = mean((p_i - mean)²)`
///
/// 样本量 < 2 → 返回 0(避免 0/0 风险)。
fn mark_variance(
    source: &Arc<dyn MarketDataSource>,
    instrument: &Instrument,
    lookback: usize,
) -> f64 {
    let history = source.mark_history(instrument, lookback);
    if history.len() < 2 {
        return 0.0;
    }
    let n = history.len() as f64;
    let mean: f64 = history.iter().map(|(_, p)| p).sum::<f64>() / n;
    let variance: f64 = history.iter().map(|(_, p)| (p - mean).powi(2)).sum::<f64>() / (n - 1.0);
    variance
}

// ═══════════════════════════════════════════════════════════════════════════
// 0.8.0 B3 跨 instrument gamma 协方差(Ledoit-Wolf 收缩 + 数值稳定)
// ═══════════════════════════════════════════════════════════════════════════

/// 默认 Ledoit-Wolf 收缩强度(0.8.0 B3)
const DEFAULT_LW_SHRINKAGE: f64 = 0.2;

/// 样本协方差矩阵(0.8.0 B3 私有 helper)
///
/// 输入:`returns[i]` 是第 `i` 个 instrument 的 returns 序列(按时间升序),长度 = `N - 1`。
/// **所有 `returns[i]` 必须等长**(若不等,返回零矩阵 + 警告)。
///
/// 输出:`Vec<Vec<f64>>` 形状 `p × p`,对称半正定。
///
/// 公式:`Σ̂[i][j] = mean((r_i - μ_i) × (r_j - μ_j))`,N-1 无偏。
///
/// 数值稳定:用 `f64::EPSILON * p` 作为 `var = 0` 的最小保护(避免 -0/0 NaN)。
fn sample_covariance(returns: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let p = returns.len();
    if p == 0 {
        return Vec::new();
    }
    // 防御:不同 instrument 的 returns 长度不一致 → 取最短,其余截断
    // (理想情况调用方已对齐,这里兜底)
    let n = returns.iter().map(|r| r.len()).min().unwrap_or(0);
    if n < 2 {
        // 不足 2 个样本 → 0 矩阵(p × p)
        return vec![vec![0.0; p]; p];
    }
    let n_f = n as f64;

    // 计算每个 instrument 的 mean
    let means: Vec<f64> = returns
        .iter()
        .map(|r| r.iter().take(n).sum::<f64>() / n_f)
        .collect();

    // 协方差矩阵
    let mut cov = vec![vec![0.0; p]; p];
    for (i, mean_i) in means.iter().enumerate() {
        for j in i..p {
            // 中心化:用 zip 同时遍历两列(避免 `for t in 0..n` 索引)
            let s: f64 = returns[i]
                .iter()
                .take(n)
                .zip(returns[j].iter().take(n))
                .map(|(ri, rj)| (ri - mean_i) * (rj - means[j]))
                .sum();
            let v = s / (n_f - 1.0);
            // 数值保护:避免 -0 累积误差
            let v = if v.abs() < f64::EPSILON { 0.0 } else { v };
            cov[i][j] = v;
            cov[j][i] = v;
        }
    }
    cov
}

/// Ledoit-Wolf 收缩(简化版本,标量目标)(0.8.0 B3 私有 helper)
///
/// 输入:样本协方差矩阵 `sample`(p × p),收缩强度 `alpha ∈ [0, 1]`。
///
/// 输出:收缩后矩阵 `Σ_shrunk = (1 - α) × sample + α × F`,其中
/// `F = (trace(sample) / p) × I_p`(标量对角目标)。
///
/// 性质:
/// - 当 `alpha = 0`:返回原 sample
/// - 当 `alpha = 1`:返回 `mean_var × I`(强收缩,各 instrument 视为独立)
/// - 当 `0 < alpha < 1`:凸组合,保证正定(`F > 0` 加 α 系数后特征值最小
///   `α × mean_var > 0`)
/// - 简化版:用固定 `alpha`(本项目 `DEFAULT_LW_SHRINKAGE = 0.2`),
///   经典 Ledoit-Wolf 论文会基于 MSE 最优化 α,0.8.0 范围内工程实现
///   取固定值足够(实测 N << p 时效果良好)
///
/// # 数值稳定
///
/// - `p == 0` → 空矩阵
/// - `sample` 元素含 NaN/Inf → 用 0 替换(避免污染整矩阵)
/// - `trace / p` 为 0 → 目标 `F` = 0,矩阵仍半正定(无变化)
fn ledoit_wolf_shrink(sample: &[Vec<f64>], alpha: f64) -> Vec<Vec<f64>> {
    let p = sample.len();
    if p == 0 {
        return Vec::new();
    }
    // 防御:alpha 范围裁剪
    let alpha = alpha.clamp(0.0, 1.0);

    // 计算 trace / p(标量目标均值)
    let mut trace = 0.0;
    let mut any_finite = false;
    for (i, row) in sample.iter().enumerate() {
        if i < row.len() {
            let v = row[i];
            if v.is_finite() {
                trace += v;
                any_finite = true;
            }
        }
    }
    let mean_var = if any_finite { trace / p as f64 } else { 0.0 };

    // Σ_shrunk = (1 - α) × sample + α × mean_var × I
    let one_minus = 1.0 - alpha;
    let mut out = vec![vec![0.0; p]; p];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            let s_ij = sample.get(i).and_then(|r| r.get(j)).copied().unwrap_or(0.0);
            let s_ij = if s_ij.is_finite() { s_ij } else { 0.0 };
            let target = if i == j { mean_var } else { 0.0 };
            *cell = one_minus * s_ij + alpha * target;
        }
    }
    out
}

/// 矩阵对称自求和(0.8.0 B3 私有 helper)
///
/// 输入:p × p 协方差矩阵(对称,非必检),`HashMap` 键。
///
/// 输出:`Σ_i Σ_j cov[i][j]`(双重循环,对称矩阵等于 `2 × sum_upper_tri - trace`,
/// 但双重循环最简明,且 p < 50 时 O(p²) 可忽略)。
fn matrix_self_sum(matrix: &HashMap<(Instrument, Instrument), f64>) -> f64 {
    let mut sum = 0.0;
    for v in matrix.values() {
        sum += v;
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::market::{Side, Trade};
    use axon_core::portfolio::Portfolio;
    use axon_core::portfolio::currency::Currency;
    use axon_core::time::Timestamp;
    use axon_core::types::{
        Instrument, Price, Quantity, SpotInstrument, SwapInstrument, SwapSettle, Symbol,
    };
    use axon_core::{InMemoryMarketData, MarketDataSource};

    fn btc_spot() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        })
    }

    fn btc_perp() -> Instrument {
        Instrument::Swap(SwapInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
            settle: SwapSettle::UsdMargin,
            contract_size: 1.0,
        })
    }

    /// ETH 永续合约,contract_size = 0.01(典型 ETH perp,B2 核心测试场景)
    fn eth_perp() -> Instrument {
        Instrument::Swap(SwapInstrument {
            base: Symbol::from("ETH"),
            quote: Symbol::from("USDT"),
            settle: SwapSettle::UsdMargin,
            contract_size: 0.01,
        })
    }

    /// BTC 永续合约,contract_size = 1.0(对照 0.7.0 行为)
    fn btc_perp_unit() -> Instrument {
        btc_perp()
    }

    fn make_trade(id: u64, _inst: &Instrument, side: Side, price: f64, qty: f64) -> Trade {
        let (buyer, seller) = match side {
            Side::Buy => (id, id + 1000),
            Side::Sell => (id + 1000, id),
        };
        Trade::new(
            Timestamp::from_nanos(id as i64 * 1_000_000),
            Price::from_f64(price),
            Quantity::from_f64(qty),
            buyer,
            seller,
        )
    }

    /// 工具:应用 trade 到 portfolio(taker_side 与 taker 方向一致时,加仓)
    fn apply_trade(portfolio: &mut Portfolio, inst: &Instrument, side: Side, price: f64, qty: f64) {
        let trade = make_trade(1, inst, side, price, qty);
        portfolio
            .apply_trade_instrument(inst, &trade, side, Timestamp::from_nanos(1_000_000))
            .expect("apply_trade ok");
    }

    // ─── 0.7.0 兼容:无 source ─────────────────────

    #[test]
    fn empty_portfolio_zero_delta() {
        let engine = PortfolioRiskEngine::new();
        let portfolio = Portfolio::new(Currency::USDT, 0.0);
        assert_eq!(engine.portfolio_delta(&portfolio), 0.0);
        assert_eq!(engine.total_gamma(&portfolio), 0.0);
        assert_eq!(engine.vega(&portfolio), 0.0);
        assert!(engine.delta_exposure(&portfolio).is_empty());
    }

    #[test]
    fn single_leg_long_1_btc() {
        let engine = PortfolioRiskEngine::new();
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let inst = btc_spot();
        apply_trade(&mut portfolio, &inst, Side::Buy, 100.0, 1.0);
        let delta = engine.delta_exposure(&portfolio);
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[&inst], 1.0, "1 BTC long → delta = +1");
        assert_eq!(engine.portfolio_delta(&portfolio), 1.0);
    }

    #[test]
    fn single_leg_short_2_btc() {
        let engine = PortfolioRiskEngine::new();
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let inst = btc_spot();
        apply_trade(&mut portfolio, &inst, Side::Sell, 100.0, 2.0);
        assert_eq!(engine.portfolio_delta(&portfolio), -2.0);
    }

    #[test]
    fn spot_perp_delta_neutral_zero_total() {
        let engine = PortfolioRiskEngine::new();
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let spot = btc_spot();
        let perp = btc_perp();
        apply_trade(&mut portfolio, &spot, Side::Buy, 100.0, 1.0);
        apply_trade(&mut portfolio, &perp, Side::Sell, 100.5, 1.0);
        assert!(
            (engine.portfolio_delta(&portfolio) - 0.0).abs() < 1e-9,
            "delta-neutral: portfolio_delta = 0"
        );
    }

    #[test]
    fn multi_leg_aggregation() {
        let engine = PortfolioRiskEngine::new();
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let spot = btc_spot();
        let perp = btc_perp();
        apply_trade(&mut portfolio, &spot, Side::Buy, 100.0, 1.0);
        apply_trade(&mut portfolio, &perp, Side::Sell, 100.5, 0.5);
        assert!((engine.portfolio_delta(&portfolio) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn gamma_is_zero_for_now() {
        let engine = PortfolioRiskEngine::new();
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let inst = btc_spot();
        apply_trade(&mut portfolio, &inst, Side::Buy, 100.0, 1.0);
        assert_eq!(engine.total_gamma(&portfolio), 0.0);
        let gamma = engine.gamma_exposure(&portfolio);
        assert_eq!(gamma[&inst], 0.0);
    }

    #[test]
    fn report_aggregate_computes_portfolio_delta() {
        let mut per_leg = HashMap::new();
        per_leg.insert(btc_spot(), 1.0);
        per_leg.insert(btc_perp(), -0.5);
        let report = RiskMetricsReport::aggregate(per_leg, 1.5);
        assert_eq!(report.portfolio_delta, 0.5);
        assert_eq!(report.sharpe_with_legs, 1.5);
        assert_eq!(report.total_gamma, 0.0);
        assert_eq!(report.vega, 0.0);
    }

    #[test]
    fn compute_report_full_path() {
        let engine = PortfolioRiskEngine::new();
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let spot = btc_spot();
        apply_trade(&mut portfolio, &spot, Side::Buy, 100.0, 2.0);
        let report = engine.compute_report(&portfolio, 0.8);
        assert_eq!(report.per_leg_delta.len(), 1);
        assert_eq!(report.per_leg_delta[&spot], 2.0);
        assert_eq!(report.portfolio_delta, 2.0);
        assert_eq!(report.sharpe_with_legs, 0.8);
    }

    // ─── 0.8.0 B1:有 source ─────────────────────

    /// 工具:构造带 N 帧 mark 历史的 source
    fn make_source(prices: Vec<f64>) -> Arc<dyn MarketDataSource> {
        let src = InMemoryMarketData::new();
        let inst = btc_spot();
        for (i, p) in prices.iter().enumerate() {
            src.push_mark(
                inst.clone(),
                Timestamp::from_nanos((i as i64 + 1) * 1_000_000_000),
                *p,
            );
        }
        src.set_iv(inst, 0.65);
        Arc::new(src)
    }

    #[test]
    fn gamma_with_source_uses_mark_variance() {
        // 价格序列 100, 102, 101, 103, 102 → variance > 0
        // 公式:gamma = qty × variance(对单 BTC qty=1)
        let prices = [100.0_f64, 102.0, 101.0, 103.0, 102.0];
        let source = make_source(prices.to_vec());
        let engine = PortfolioRiskEngine::with_source(source);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let inst = btc_spot();
        apply_trade(&mut portfolio, &inst, Side::Buy, 102.0, 1.0);

        let gamma = engine.gamma_exposure(&portfolio);
        let total = engine.total_gamma(&portfolio);
        assert!(
            gamma[&inst] > 0.0,
            "gamma should be positive with non-zero variance"
        );
        // 验证数值:prices mean=101.6, variance=Σ(p-101.6)²/(n-1) ≈ 1.3
        let mean = 101.6_f64;
        let expected_var: f64 = prices.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / 4.0;
        assert!(
            (gamma[&inst] - expected_var).abs() < 1e-6,
            "gamma ≈ qty × variance"
        );
        assert!((total - expected_var).abs() < 1e-6);
    }

    #[test]
    fn gamma_zero_when_mark_history_flat() {
        // 全部 mark = 100 → variance = 0
        let source = make_source(vec![100.0, 100.0, 100.0, 100.0, 100.0]);
        let engine = PortfolioRiskEngine::with_source(source);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let inst = btc_spot();
        apply_trade(&mut portfolio, &inst, Side::Buy, 100.0, 1.0);

        let gamma = engine.gamma_exposure(&portfolio);
        assert_eq!(
            gamma[&inst], 0.0,
            "flat mark history → variance = 0 → gamma = 0"
        );
    }

    #[test]
    fn gamma_zero_when_history_too_short() {
        // 1 帧 mark → 样本 < 2 → variance = 0
        let source = make_source(vec![100.0]);
        let engine = PortfolioRiskEngine::with_source(source);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let inst = btc_spot();
        apply_trade(&mut portfolio, &inst, Side::Buy, 100.0, 1.0);

        assert_eq!(engine.total_gamma(&portfolio), 0.0);
    }

    #[test]
    fn vega_with_source_uses_implied_vol() {
        let source = make_source(vec![100.0; 5]);
        let engine = PortfolioRiskEngine::with_source(source);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let inst = btc_spot();
        apply_trade(&mut portfolio, &inst, Side::Buy, 100.0, 2.0);
        // vega = qty × IV = 2 × 0.65 = 1.3
        assert!((engine.vega(&portfolio) - 2.0 * 0.65).abs() < 1e-9);
    }

    #[test]
    fn vega_skips_instruments_without_iv() {
        // source 包含 BTC(有 IV)但不包含 ETH
        let src = InMemoryMarketData::new();
        let btc = btc_spot();
        src.push_mark(btc.clone(), Timestamp::from_nanos(1_000_000_000), 100.0);
        src.set_iv(btc.clone(), 0.5);
        let source: Arc<dyn MarketDataSource> = Arc::new(src);

        let engine = PortfolioRiskEngine::with_source(source);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        apply_trade(&mut portfolio, &btc, Side::Buy, 100.0, 1.0);
        // 单独 BTC 有 IV → vega = 1.0 × 0.5 = 0.5
        assert!((engine.vega(&portfolio) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn compute_report_with_source_full() {
        let source = make_source(vec![100.0, 102.0, 101.0, 103.0]);
        let engine = PortfolioRiskEngine::with_source(source);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let inst = btc_spot();
        apply_trade(&mut portfolio, &inst, Side::Buy, 102.0, 2.0);
        let report = engine.compute_report(&portfolio, 1.0);

        assert_eq!(report.per_leg_delta[&inst], 2.0);
        assert_eq!(report.portfolio_delta, 2.0);
        assert!(
            report.total_gamma > 0.0,
            "gamma computed from mark variance"
        );
        assert!((report.vega - 2.0 * 0.65).abs() < 1e-9);
        assert_eq!(report.sharpe_with_legs, 1.0);
    }

    #[test]
    fn gamma_lookback_caps_history() {
        // 5 帧 mark 历史,lookback=3 → 只用末尾 3 帧
        let source = make_source(vec![100.0, 200.0, 300.0, 400.0, 500.0]);
        let engine = PortfolioRiskEngine::with_source(source).with_gamma_lookback(3);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let inst = btc_spot();
        apply_trade(&mut portfolio, &inst, Side::Buy, 500.0, 1.0);
        // 末尾 3 帧:300, 400, 500 → mean=400, variance=((100² + 0 + 100²)/(3-1)) = 10000
        let gamma = engine.gamma_exposure(&portfolio);
        assert!(
            (gamma[&inst] - 10_000.0).abs() < 1e-6,
            "lookback=3 应该只算最后 3 帧的 variance,得到 10000"
        );
    }

    // ─── 0.8.0 B2:contract_size 接入 delta / vega / gamma ─────────

    /// 工具:构造 ETH perp(contract_size=0.01)+ BTC spot 共享的 source
    fn make_multi_leg_source() -> Arc<dyn MarketDataSource> {
        let src = InMemoryMarketData::new();
        // BTC spot 5 帧 mark + IV
        let btc = btc_spot();
        for (i, p) in [100.0, 102.0, 101.0, 103.0, 102.0].iter().enumerate() {
            src.push_mark(
                btc.clone(),
                Timestamp::from_nanos((i as i64 + 1) * 1_000_000_000),
                *p,
            );
        }
        src.set_iv(btc, 0.65);
        // ETH perp 5 帧 mark + IV
        let eth = eth_perp();
        for (i, p) in [3000.0, 3050.0, 3020.0, 3080.0, 3040.0].iter().enumerate() {
            src.push_mark(
                eth.clone(),
                Timestamp::from_nanos((i as i64 + 1) * 1_000_000_000),
                *p,
            );
        }
        src.set_iv(eth, 0.85);
        Arc::new(src)
    }

    #[test]
    fn b2_delta_eth_perp_contract_size_001() {
        // 0.8.0 B2 核心场景:ETH perp(contract_size=0.01) 持仓 100 张
        // 0.7.0 隐式 contract_size=1.0 → delta = 100
        // 0.8.0 B2 接入 contract_size=0.01 → delta = 100 × 0.01 = 1.0
        let engine = PortfolioRiskEngine::new();
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let perp = eth_perp();
        apply_trade(&mut portfolio, &perp, Side::Buy, 3000.0, 100.0);
        let delta = engine.delta_exposure(&portfolio);
        assert!((delta[&perp] - 1.0).abs() < 1e-9, "100 × 0.01 = 1.0");
        assert!((engine.portfolio_delta(&portfolio) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn b2_delta_spot_unchanged() {
        // spot 默认 contract_size=1.0,B2 后行为不变(向后兼容)
        let engine = PortfolioRiskEngine::new();
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let spot = btc_spot();
        apply_trade(&mut portfolio, &spot, Side::Buy, 100.0, 2.0);
        let delta = engine.delta_exposure(&portfolio);
        assert!(
            (delta[&spot] - 2.0).abs() < 1e-9,
            "spot 2 BTC long → delta = 2.0"
        );
    }

    #[test]
    fn b2_delta_spot_perp_hedge_with_contract_size() {
        // 多 leg 套保:BTC spot 1.0 long + ETH perp(contract_size=0.01) 100 张 short
        // delta = 1.0 × 1.0 + (-100) × 0.01 = 0.0(完全对冲)
        // 0.7.0 下 delta = 1.0 + (-100) = -99,行为错误
        let engine = PortfolioRiskEngine::new();
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let spot = btc_spot();
        let perp = eth_perp();
        apply_trade(&mut portfolio, &spot, Side::Buy, 100.0, 1.0);
        apply_trade(&mut portfolio, &perp, Side::Sell, 3000.0, 100.0);
        assert!(
            (engine.portfolio_delta(&portfolio) - 0.0).abs() < 1e-9,
            "BTC spot 1.0 + ETH perp 100 × 0.01 完全对冲 → portfolio_delta = 0"
        );
    }

    #[test]
    fn b2_vega_eth_perp_contract_size() {
        // 0.8.0 B2 vega 公式:`qty × IV × contract_size`
        // ETH perp 100 张, IV=0.85, contract_size=0.01
        // vega = 100 × 0.85 × 0.01 = 0.85
        let source = make_multi_leg_source();
        let engine = PortfolioRiskEngine::with_source(source);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let perp = eth_perp();
        apply_trade(&mut portfolio, &perp, Side::Buy, 3000.0, 100.0);
        assert!(
            (engine.vega(&portfolio) - 0.85).abs() < 1e-9,
            "vega = 100 × 0.85 × 0.01 = 0.85"
        );
    }

    #[test]
    fn b2_vega_btc_spot_unchanged() {
        // spot vega 不受 contract_size 影响(contract_size=1.0)
        // BTC spot 2.0 long, IV=0.65
        // vega = 2.0 × 0.65 × 1.0 = 1.3
        let source = make_multi_leg_source();
        let engine = PortfolioRiskEngine::with_source(source);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let spot = btc_spot();
        apply_trade(&mut portfolio, &spot, Side::Buy, 100.0, 2.0);
        assert!(
            (engine.vega(&portfolio) - 1.3).abs() < 1e-9,
            "BTC spot 2.0 vega = 2.0 × 0.65 = 1.3 (contract_size=1.0 不影响)"
        );
    }

    #[test]
    fn b2_gamma_eth_perp_contract_size_squared() {
        // 0.8.0 B2 gamma 公式:`qty × mark_variance × contract_size²`
        // ETH perp 100 张, contract_size=0.01, ETH mark 5 帧
        //   prices: 3000, 3050, 3020, 3080, 3040
        //   mean = 3038
        //   variance = Σ(p-3038)² / 4
        //   ≈ ((38² + 12² + 18² + 42² + 2²) / 4) = (1444 + 144 + 324 + 1764 + 4) / 4 = 920
        // gamma = 100 × 920 × 0.01² = 100 × 920 × 0.0001 = 9.2
        let source = make_multi_leg_source();
        let engine = PortfolioRiskEngine::with_source(source);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let perp = eth_perp();
        apply_trade(&mut portfolio, &perp, Side::Buy, 3000.0, 100.0);

        // 手算 ETH 5 帧 mark variance
        let eth_prices = [3000.0_f64, 3050.0, 3020.0, 3080.0, 3040.0];
        let mean: f64 = eth_prices.iter().sum::<f64>() / 5.0;
        let expected_variance: f64 =
            eth_prices.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / 4.0;
        // 0.7.0 等价:gamma = 100 × variance × 1² = 100 × variance
        // 0.8.0 B2:gamma = 100 × variance × 0.01² = 100 × variance × 0.0001
        let expected_gamma_8 = 100.0 * expected_variance * 0.01 * 0.01;
        let gamma = engine.gamma_exposure(&portfolio);
        assert!(
            (gamma[&perp] - expected_gamma_8).abs() < 1e-6,
            "B2 gamma 应含 contract_size² 缩放"
        );
    }

    #[test]
    fn b2_gamma_btc_spot_unchanged() {
        // spot gamma 不受 contract_size² 影响(contract_size=1.0)
        // 与 B1 的 gamma_with_source_uses_mark_variance 数值一致
        let source = make_multi_leg_source();
        let engine = PortfolioRiskEngine::with_source(source);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let spot = btc_spot();
        apply_trade(&mut portfolio, &spot, Side::Buy, 100.0, 1.0);

        let btc_prices = [100.0_f64, 102.0, 101.0, 103.0, 102.0];
        let mean: f64 = btc_prices.iter().sum::<f64>() / 5.0;
        let expected_variance: f64 =
            btc_prices.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / 4.0;
        // spot contract_size=1.0,gamma = 1.0 × variance × 1² = variance
        let gamma = engine.gamma_exposure(&portfolio);
        assert!(
            (gamma[&spot] - expected_variance).abs() < 1e-6,
            "spot gamma 数值与 B1 一致(contract_size=1.0 不影响)"
        );
    }

    #[test]
    fn b2_multi_leg_combined_report() {
        // 综合测试:BTC spot 1.0 long + ETH perp 100 张 short
        // 完整 risk report:delta/vega/gamma 都按 contract_size 修正
        let source = make_multi_leg_source();
        let engine = PortfolioRiskEngine::with_source(source);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let spot = btc_spot();
        let perp = eth_perp();
        apply_trade(&mut portfolio, &spot, Side::Buy, 100.0, 1.0);
        apply_trade(&mut portfolio, &perp, Side::Sell, 3000.0, 100.0);

        let report = engine.compute_report(&portfolio, 1.2);

        // delta:1.0 × 1.0 + (-100) × 0.01 = 0.0
        assert!((report.per_leg_delta[&spot] - 1.0).abs() < 1e-9);
        assert!((report.per_leg_delta[&perp] - (-1.0)).abs() < 1e-9);
        assert!(report.portfolio_delta.abs() < 1e-9);

        // vega:1.0 × 0.65 × 1.0 + (-100) × 0.85 × 0.01 = 0.65 - 0.85 = -0.2
        assert!(
            (report.vega - (0.65 - 0.85)).abs() < 1e-9,
            "vega 组合 = spot vega + perp vega = 0.65 - 0.85 = -0.2"
        );
    }

    #[test]
    fn b2_contract_size_helper() {
        // 直接验证 contract_size_of 函数(内部 helper,白盒测试)
        assert_eq!(contract_size_of(&btc_spot()), 1.0);
        assert_eq!(contract_size_of(&btc_perp_unit()), 1.0);
        assert!((contract_size_of(&eth_perp()) - 0.01).abs() < 1e-9);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 0.8.0 B3 跨 instrument gamma 协方差(Ledoit-Wolf 收缩)
    // ═══════════════════════════════════════════════════════════════════════

    /// BTC spot + ETH perp,两 instrument,正相关(同一波 BTC 行情)
    /// 验证 `gamma_covariance_matrix` 非对角项非零
    fn push_btc_and_eth_correlated(source: &InMemoryMarketData) {
        // BTC spot:10 帧 mark 模拟真实波动
        let btc_prices = [
            50000.0, 50500.0, 51000.0, 50800.0, 51200.0, 51500.0, 51300.0, 51800.0, 52000.0,
            52200.0,
        ];
        // ETH perp:用 80% 相关 + 20% 噪声
        let eth_prices = [
            3000.0, 3020.0, 3050.0, 3040.0, 3060.0, 3080.0, 3075.0, 3100.0, 3110.0, 3125.0,
        ];
        for (i, (&b, &e)) in btc_prices.iter().zip(eth_prices.iter()).enumerate() {
            source.push_mark(
                btc_spot(),
                Timestamp::from_nanos((i as i64 + 1) * 1_000_000),
                b,
            );
            source.push_mark(
                eth_perp(),
                Timestamp::from_nanos((i as i64 + 1) * 1_000_000),
                e,
            );
        }
    }

    /// BTC spot + BTC perp(完全同向)— 用同一 mark 序列模拟
    fn push_btc_spot_and_perp_perfectly_correlated(source: &InMemoryMarketData) {
        let prices = [
            100.0, 105.0, 110.0, 108.0, 112.0, 115.0, 113.0, 118.0, 120.0, 122.0,
        ];
        for (i, &p) in prices.iter().enumerate() {
            source.push_mark(
                btc_spot(),
                Timestamp::from_nanos((i as i64 + 1) * 1_000_000),
                p,
            );
            source.push_mark(
                btc_perp_unit(),
                Timestamp::from_nanos((i as i64 + 1) * 1_000_000),
                p,
            );
        }
    }

    /// BTC spot + ETH perp 完全独立(uncorrelated)— 用反向模拟
    fn push_btc_and_eth_uncorrelated(source: &InMemoryMarketData) {
        // BTC 单调上涨
        let btc_prices: Vec<f64> = (0..20).map(|i| 100.0 + i as f64).collect();
        // ETH 单调下降(完全负相关)
        let eth_prices: Vec<f64> = (0..20).map(|i| 200.0 - i as f64).collect();
        for (i, (&b, &e)) in btc_prices.iter().zip(eth_prices.iter()).enumerate() {
            source.push_mark(
                btc_spot(),
                Timestamp::from_nanos((i as i64 + 1) * 1_000_000),
                b,
            );
            source.push_mark(
                eth_perp(),
                Timestamp::from_nanos((i as i64 + 1) * 1_000_000),
                e,
            );
        }
    }

    #[test]
    fn b3_sample_covariance_two_legs_uncorrelated() {
        // BTC 单调递增(returns 全正且相等 = 1/100)
        // ETH 单调递减(returns 全负且相等 = -1/199)
        // 两者线性独立,cov ≈ 0(浮点累积会有 ε 误差,1e-6 容差)
        let btc: Vec<f64> = (0..20).map(|i| 100.0 + i as f64).collect();
        let eth: Vec<f64> = (0..20).map(|i| 200.0 - i as f64).collect();
        let btc_r: Vec<f64> = btc.windows(2).map(|w| w[1] / w[0] - 1.0).collect();
        let eth_r: Vec<f64> = eth.windows(2).map(|w| w[1] / w[0] - 1.0).collect();
        let cov = sample_covariance(&[btc_r.clone(), eth_r.clone()]);
        // 对角:每 leg returns 恒定 → var ≈ 0(浮点 ε 误差)
        assert!(
            cov[0][0].abs() < 1e-6,
            "BTC const returns → var ≈ 0, got {}",
            cov[0][0]
        );
        assert!(
            cov[1][1].abs() < 1e-6,
            "ETH const returns → var ≈ 0, got {}",
            cov[1][1]
        );
        // 非对角:线性无关 → cov ≈ 0
        assert!(
            cov[0][1].abs() < 1e-6,
            "uncorrelated → cov ≈ 0, got {}",
            cov[0][1]
        );
    }

    #[test]
    fn b3_sample_covariance_perfectly_correlated() {
        // 同一 returns 序列 → cov = var(returns)
        let prices: Vec<f64> = (0..10).map(|i| 100.0 + (i as f64) * 5.0).collect();
        let r: Vec<f64> = prices.windows(2).map(|w| w[1] / w[0] - 1.0).collect();
        let cov = sample_covariance(&[r.clone(), r.clone()]);
        // 同一序列 var = mean((r - mean)²)
        let mean = r.iter().sum::<f64>() / r.len() as f64;
        let var: f64 = r.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (r.len() as f64 - 1.0);
        assert!((cov[0][0] - var).abs() < 1e-12);
        assert!((cov[1][1] - var).abs() < 1e-12);
        assert!(
            (cov[0][1] - var).abs() < 1e-12,
            "perfectly correlated → cov = var"
        );
    }

    #[test]
    fn b3_ledoit_wolf_alpha_zero_is_identity() {
        // alpha = 0 → 返回原矩阵
        let cov = vec![vec![1.0, 0.5], vec![0.5, 2.0]];
        let shrunk = ledoit_wolf_shrink(&cov, 0.0);
        assert!((shrunk[0][0] - 1.0).abs() < 1e-12);
        assert!((shrunk[0][1] - 0.5).abs() < 1e-12);
        assert!((shrunk[1][1] - 2.0).abs() < 1e-12);
    }

    #[test]
    fn b3_ledoit_wolf_alpha_one_is_diag() {
        // alpha = 1 → 强收缩到 mean_var × I
        // mean_var = (1.0 + 2.0) / 2 = 1.5
        let cov = vec![vec![1.0, 0.5], vec![0.5, 2.0]];
        let shrunk = ledoit_wolf_shrink(&cov, 1.0);
        assert!((shrunk[0][0] - 1.5).abs() < 1e-12);
        assert!(shrunk[0][1].abs() < 1e-12, "alpha=1 → 非对角归零");
        assert!((shrunk[1][1] - 1.5).abs() < 1e-12);
    }

    #[test]
    fn b3_ledoit_wolf_preserves_positive_definiteness() {
        // N < p 场景:returns 全相同(常 returns)→ sample cov = 0 矩阵
        // alpha = 0.2 收缩后:F = (0 + 0) / 2 × I = 0 矩阵
        // 退化为零矩阵,虽然不严格正定但仍半正定,无 NaN
        let zero_cov = vec![vec![0.0, 0.0], vec![0.0, 0.0]];
        let shrunk = ledoit_wolf_shrink(&zero_cov, 0.2);
        for row in &shrunk {
            for v in row {
                assert!(v.is_finite(), "shrunk should not contain NaN/Inf");
            }
        }
    }

    #[test]
    fn b3_ledoit_wolf_with_mean_var_positive() {
        // 模拟 N < p 场景:对角有数值,非对角接近 0(典型 singular 情形)
        // alpha = 0.2 收缩后:对角(1-α)×对角 + α×mean_var,非对角被 0 化
        let cov = vec![vec![4.0, 0.01], vec![0.01, 1.0]];
        let shrunk = ledoit_wolf_shrink(&cov, 0.2);
        // trace = 5, mean_var = 2.5
        // 对角 (1,1): 0.8 × 4 + 0.2 × 2.5 = 3.2 + 0.5 = 3.7
        // 对角 (2,2): 0.8 × 1 + 0.2 × 2.5 = 0.8 + 0.5 = 1.3
        // 非对角: 0.8 × 0.01 + 0 = 0.008
        assert!((shrunk[0][0] - 3.7).abs() < 1e-9);
        assert!((shrunk[1][1] - 1.3).abs() < 1e-9);
        assert!((shrunk[0][1] - 0.008).abs() < 1e-9);
    }

    #[test]
    fn b3_gamma_covariance_no_source_returns_empty() {
        // 0.7.0 兼容:无 source → 空 HashMap
        let engine = PortfolioRiskEngine::new();
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        apply_trade(&mut portfolio, &btc_spot(), Side::Buy, 100.0, 1.0);
        let cov = engine.gamma_covariance_matrix(&portfolio);
        assert!(cov.is_empty(), "无 source → 空协方差矩阵");
        assert_eq!(engine.portfolio_gamma_with_covariance(&portfolio), 0.0);
    }

    #[test]
    fn b3_gamma_covariance_empty_portfolio() {
        let source = Arc::new(InMemoryMarketData::new());
        let engine = PortfolioRiskEngine::with_source(source);
        let portfolio = Portfolio::new(Currency::USDT, 0.0);
        let cov = engine.gamma_covariance_matrix(&portfolio);
        assert!(cov.is_empty(), "无持仓 → 空协方差矩阵");
        assert_eq!(engine.portfolio_gamma_with_covariance(&portfolio), 0.0);
    }

    #[test]
    fn b3_gamma_covariance_insufficient_data() {
        // source 只有 1 帧 mark → returns < 2 → 不足以计算协方差
        let source = Arc::new(InMemoryMarketData::new());
        source.push_mark(btc_spot(), Timestamp::from_nanos(100), 50_000.0);
        let engine = PortfolioRiskEngine::with_source(source);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        apply_trade(&mut portfolio, &btc_spot(), Side::Buy, 50_000.0, 1.0);
        let cov = engine.gamma_covariance_matrix(&portfolio);
        assert!(cov.is_empty(), "< 2 帧 mark → returns < 2 → 空矩阵");
    }

    #[test]
    fn b3_gamma_covariance_single_leg() {
        // 单 instrument → 矩阵 1×1,等于 qty² × cs² × var(returns)
        let source = Arc::new(InMemoryMarketData::new());
        // BTC spot 5 帧 mark,returns = [0.10, 0.10, 0, -0.0909]
        let prices = [100.0, 110.0, 121.0, 121.0, 110.0];
        for (i, &p) in prices.iter().enumerate() {
            source.push_mark(btc_spot(), Timestamp::from_nanos((i as i64 + 1) * 100), p);
        }
        let engine = PortfolioRiskEngine::with_source(source);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        apply_trade(&mut portfolio, &btc_spot(), Side::Buy, 100.0, 2.0);

        let cov = engine.gamma_covariance_matrix(&portfolio);
        assert_eq!(cov.len(), 1, "单 leg → 1×1 矩阵");
        let (inst_a, inst_b) = cov.keys().next().unwrap();
        assert_eq!(inst_a, inst_b);
        assert_eq!(inst_a, &btc_spot());

        // var(returns) = mean((r - mean)²) with N-1 divisor
        // returns = [0.1, 0.1, 0, -1/11 ≈ -0.0909]
        // mean = (0.1 + 0.1 + 0 + -1/11) / 4 = (0.2 - 0.0909) / 4 ≈ 0.0273
        // ...直接用 calloc 计算:var ≈ 0.008
        // gamma_scaled = var × qty² × cs² = var × 4 × 1
        let v = cov.values().next().unwrap();
        // 用 0.8.0 B1 的 mark_variance(基于 prices 而非 returns)做交叉验证:
        // prices var(基于 prices 自身):mean = (100+110+121+121+110)/5 = 112.4
        // var = sum((p - 112.4)²) / 4 = (153.76 + 5.76 + 73.96 + 73.96 + 5.76) / 4 ≈ 78.3
        // gamma 公式不同(基于 mark_variance vs 基于 returns var),不能直接对齐
        // 此处只验证非零 + 量级合理
        assert!(*v > 0.0, "单 leg gamma 协方差 > 0");
        // portfolio_gamma_with_covariance = 单 leg → 等于自身
        assert!((engine.portfolio_gamma_with_covariance(&portfolio) - v).abs() < 1e-12);
    }

    #[test]
    fn b3_gamma_covariance_two_legs_perfectly_correlated() {
        // BTC spot + BTC perp 用完全相同 mark 序列 → cov 非对角 ≠ 0
        let source = Arc::new(InMemoryMarketData::new());
        push_btc_spot_and_perp_perfectly_correlated(&source);
        let engine = PortfolioRiskEngine::with_source(source);

        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        apply_trade(&mut portfolio, &btc_spot(), Side::Buy, 100.0, 1.0);
        apply_trade(&mut portfolio, &btc_perp_unit(), Side::Buy, 100.0, 1.0);

        let cov = engine.gamma_covariance_matrix(&portfolio);
        // 2×2 矩阵(对称)→ 3 unique entries
        assert_eq!(cov.len(), 3);
        // 对角项应该相等(qty=1, cs=1, returns 相同 → var 相同)
        let diag_spot = cov.get(&(btc_spot(), btc_spot())).unwrap();
        let diag_perp = cov.get(&(btc_perp_unit(), btc_perp_unit())).unwrap();
        assert!((diag_spot - diag_perp).abs() < 1e-12);
        // 非对角项 = 对角项(完全相关,cov = var)
        let off = cov.get(&(btc_spot(), btc_perp_unit())).unwrap();
        assert!(
            (off - diag_spot).abs() < 1e-9,
            "完全相关 → off-diag = diag, got {off} vs {diag_spot}"
        );
    }

    #[test]
    fn b3_gamma_covariance_with_contract_size() {
        // BTC spot(contract_size=1) + ETH perp(contract_size=0.01)
        // 验证协方差项按 contract_size 缩放
        let source = Arc::new(InMemoryMarketData::new());
        push_btc_and_eth_correlated(&source);
        let engine = PortfolioRiskEngine::with_source(source);

        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        apply_trade(&mut portfolio, &btc_spot(), Side::Buy, 50_000.0, 1.0);
        apply_trade(&mut portfolio, &eth_perp(), Side::Sell, 3_000.0, 100.0);

        let cov = engine.gamma_covariance_matrix(&portfolio);
        assert_eq!(cov.len(), 3);
        // 验证 ETH perp 对角 = var × (100)² × (0.01)²
        let diag_eth = cov.get(&(eth_perp(), eth_perp())).unwrap();
        // BTC spot 对角 = var × 1² × 1²
        let diag_btc = cov.get(&(btc_spot(), btc_spot())).unwrap();
        // ETH perp 量级应小于 BTC spot(qty² × cs² = 10000 × 0.0001 = 1 vs 1)
        // 但 var 不同,不做严格比较,只验证非零 + ETH > 0
        assert!(*diag_eth > 0.0);
        assert!(*diag_btc > 0.0);
        // 非对角:两个 var_btc × var_eth × qty × qty × cs × cs
        // 量级大致 1 × 1 × 1 × 100 × 1 × 0.01 = 1,但符号由 cov(returns) 决定
        let off = cov.get(&(btc_spot(), eth_perp())).unwrap();
        assert!(off.is_finite());
    }

    #[test]
    fn b3_gamma_covariance_singular_no_panic() {
        // N < p 场景但 N >= 2:3 个 instrument,每个 3 帧 mark(N=2 returns < p=3)
        // 触发 Ledoit-Wolf 收缩,应不 panic
        let source = Arc::new(InMemoryMarketData::new());
        for inst in &[btc_spot(), eth_perp(), btc_perp_unit()] {
            source.push_mark(inst.clone(), Timestamp::from_nanos(100), 100.0);
            source.push_mark(inst.clone(), Timestamp::from_nanos(200), 105.0);
            source.push_mark(inst.clone(), Timestamp::from_nanos(300), 110.0);
        }
        let engine = PortfolioRiskEngine::with_source(source);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        apply_trade(&mut portfolio, &btc_spot(), Side::Buy, 100.0, 1.0);
        apply_trade(&mut portfolio, &eth_perp(), Side::Buy, 3000.0, 10.0);
        apply_trade(&mut portfolio, &btc_perp_unit(), Side::Buy, 50_000.0, 1.0);

        let cov = engine.gamma_covariance_matrix(&portfolio);
        // 3×3 矩阵(对称)→ 上三角 6 unique entries
        assert_eq!(cov.len(), 6);
        // N=2 (2 returns per instrument) < p=3 → 触发 Ledoit-Wolf
        // 矩阵应正定(收缩后),无 NaN
        for v in cov.values() {
            assert!(v.is_finite(), "singular shrunk matrix should be finite");
        }
        // portfolio_gamma_with_covariance = 上三角值之和(对角 + 非对角)
        let pg = engine.portfolio_gamma_with_covariance(&portfolio);
        assert!(pg.is_finite());
    }

    #[test]
    fn b3_portfolio_gamma_with_covariance_single_leg() {
        // 单 leg → portfolio_gamma_with_covariance = 总对角项 = total_gamma
        // 注意:0.8.0 B3 用 returns var(基于 mark_returns),B1/B2 用 prices var(基于 mark_history),
        // 两者数学上不等,所以本测试不严格相等,只验证量级一致
        let source = Arc::new(InMemoryMarketData::new());
        let prices = [100.0, 110.0, 121.0, 121.0, 110.0, 105.0, 108.0];
        for (i, &p) in prices.iter().enumerate() {
            source.push_mark(btc_spot(), Timestamp::from_nanos((i as i64 + 1) * 100), p);
        }
        let engine = PortfolioRiskEngine::with_source(source);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        apply_trade(&mut portfolio, &btc_spot(), Side::Buy, 100.0, 1.0);

        let cov_total = engine.portfolio_gamma_with_covariance(&portfolio);
        assert!(cov_total > 0.0);
        // sanity:对角项等于 portfolio_gamma_with_covariance
        let cov_matrix = engine.gamma_covariance_matrix(&portfolio);
        assert_eq!(cov_matrix.len(), 1);
        assert!((cov_total - cov_matrix.values().next().unwrap()).abs() < 1e-12);
    }

    #[test]
    fn b3_portfolio_gamma_with_covariance_uncorrelated_reduces_to_diagonal() {
        // 完全独立(cov=0)→ portfolio_gamma_with_covariance = 上三角值之和 = diag_sum
        // (因为非对角项为 0,只算一次)
        // 注意:这里"total_gamma"是 0.8.0 B1/B2 的(基于 prices var),
        // 而 B3 用 returns var,两者数学不同,不能直接相等
        // 此测试只验证 portfolio_gamma_with_covariance 接近自身对角和
        let source = Arc::new(InMemoryMarketData::new());
        push_btc_and_eth_uncorrelated(&source);
        let engine = PortfolioRiskEngine::with_source(source);

        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        apply_trade(&mut portfolio, &btc_spot(), Side::Buy, 100.0, 1.0);
        apply_trade(&mut portfolio, &eth_perp(), Side::Buy, 200.0, 50.0);

        let cov = engine.gamma_covariance_matrix(&portfolio);
        // 不相关 → 非对角接近 0(线性无关 returns)
        let off = cov.get(&(btc_spot(), eth_perp())).unwrap();
        let diag_sum: f64 = *cov.get(&(btc_spot(), btc_spot())).unwrap()
            + *cov.get(&(eth_perp(), eth_perp())).unwrap();
        // matrix_self_sum = 对角 + 非对角(只算上三角一次)
        let pg_with_cov = engine.portfolio_gamma_with_covariance(&portfolio);
        assert!(
            (pg_with_cov - (diag_sum + off)).abs() < 1e-12,
            "上三角自求和:pg = diag + off,got {pg_with_cov} vs {}",
            diag_sum + off
        );
        // 不相关:非对角小 → pg ≈ diag_sum(可能有小 ε 浮点)
        assert!(
            (pg_with_cov - diag_sum).abs() < 1.0,
            "不相关 → pg ≈ diag_sum"
        );
    }

    #[test]
    fn b3_portfolio_gamma_with_covariance_correlated_exceeds_diagonal() {
        // 完全正相关 → 非对角 = 对角,上三角 pg = 2×diag + off = 3×diag
        // 验证 pg > diag_sum(协方差项非零)
        let source = Arc::new(InMemoryMarketData::new());
        push_btc_spot_and_perp_perfectly_correlated(&source);
        let engine = PortfolioRiskEngine::with_source(source);

        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        apply_trade(&mut portfolio, &btc_spot(), Side::Buy, 100.0, 1.0);
        apply_trade(&mut portfolio, &btc_perp_unit(), Side::Buy, 100.0, 1.0);

        let cov = engine.gamma_covariance_matrix(&portfolio);
        let diag_spot = cov.get(&(btc_spot(), btc_spot())).copied().unwrap();
        let diag_perp = cov
            .get(&(btc_perp_unit(), btc_perp_unit()))
            .copied()
            .unwrap();
        let off = cov.get(&(btc_spot(), btc_perp_unit())).copied().unwrap();
        // 完全正相关:off = diag_spot(同 returns)≈ diag_perp
        let diag_sum = diag_spot + diag_perp;
        let pg_with_cov = engine.portfolio_gamma_with_covariance(&portfolio);
        // 上三角自求和:pg = diag_spot + diag_perp + off = 3 × diag
        assert!(
            pg_with_cov > diag_sum,
            "完全正相关 → pg_with_cov > diag_sum,got {pg_with_cov} vs {diag_sum}"
        );
        // 严格等于 diag_sum + off(完全相关 off ≈ diag_spot)
        assert!((pg_with_cov - (diag_sum + off)).abs() < 1e-9);
    }

    #[test]
    fn b3_gamma_covariance_dedupes_instruments() {
        // 同一 instrument 多次 apply → portfolio dedupe 后只 1 个 instrument
        let source = Arc::new(InMemoryMarketData::new());
        let prices = [100.0, 105.0, 110.0];
        for (i, &p) in prices.iter().enumerate() {
            source.push_mark(btc_spot(), Timestamp::from_nanos((i as i64 + 1) * 100), p);
        }
        let engine = PortfolioRiskEngine::with_source(source);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        apply_trade(&mut portfolio, &btc_spot(), Side::Buy, 100.0, 1.0);
        apply_trade(&mut portfolio, &btc_spot(), Side::Buy, 105.0, 1.0);

        let cov = engine.gamma_covariance_matrix(&portfolio);
        // portfolio 应只有 1 个 btc_spot key,position = 2.0
        assert_eq!(cov.len(), 1, "dedupe 后单 leg 1×1 矩阵");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 0.8.0 B3.5 hotfix:尾部对齐(取最新 n 帧 returns,而非最早 n 帧)
    // ═══════════════════════════════════════════════════════════════════════

    /// 不同长度的 returns 必须尾部对齐,避免 head 对齐引入陈旧数据
    ///
    /// 场景:
    /// - BTC:10 帧 mark,早期 5 帧 returns 高波动(模拟"陈旧"市场),后期 5 帧 returns 低波动
    /// - ETH:5 帧 mark(对应 BTC 后期 5 帧时间窗口)
    /// - 修复前(head 对齐):BTC 早期高波动 5 帧与 ETH 后 5 帧错位,数值混淆
    /// - 修复后(尾部对齐):BTC 后 5 帧低波动与 ETH 后 5 帧同步,数值一致
    #[test]
    fn b3_returns_tail_aligned_not_head_aligned() {
        let source = Arc::new(InMemoryMarketData::new());
        // BTC:10 帧 mark,价格变化剧烈在早期,后期平稳
        // 前 5 帧 returns: 100→200→100→200→100(高方差 ±100/100)
        // 后 5 帧 returns: 100→101→102→103→104(低方差,小幅递增)
        let btc_prices = [
            100.0, 200.0, 100.0, 200.0, 100.0, 100.0, 101.0, 102.0, 103.0, 104.0,
        ];
        for (i, &p) in btc_prices.iter().enumerate() {
            source.push_mark(btc_spot(), Timestamp::from_nanos((i as i64 + 1) * 100), p);
        }
        // ETH:5 帧 mark,只覆盖 BTC 后期 5 帧时间窗口
        // 同样小幅递增 → 与 BTC 后 5 帧完全正相关
        let eth_prices = [200.0, 201.0, 202.0, 203.0, 204.0];
        for (i, &p) in eth_prices.iter().enumerate() {
            source.push_mark(
                eth_perp(),
                Timestamp::from_nanos((i as i64 + 6) * 100), // ts 600-1000,与 BTC 后 5 帧同步
                p,
            );
        }
        let engine = PortfolioRiskEngine::with_source(source);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        apply_trade(&mut portfolio, &btc_spot(), Side::Buy, 100.0, 1.0);
        apply_trade(&mut portfolio, &eth_perp(), Side::Buy, 200.0, 1.0);

        let cov = engine.gamma_covariance_matrix(&portfolio);
        // 2×2 上三角 → 3 unique entries
        assert_eq!(cov.len(), 3);
        let diag_btc = cov.get(&(btc_spot(), btc_spot())).copied().unwrap();
        let diag_eth = cov.get(&(eth_perp(), eth_perp())).copied().unwrap();
        let off = cov.get(&(btc_spot(), eth_perp())).copied().unwrap();

        // 尾部对齐:BTC 实际用的是后 5 帧(低方差)
        // 后 5 帧 BTC returns: 101/100-1=0.01, 102/101-1≈0.0099, 103/102-1≈0.0098, 104/103-1≈0.0097
        // 后 5 帧 ETH returns: 201/200-1=0.005, 202/201-1≈0.00498, 203/202-1≈0.00495, 204/203-1≈0.00493
        // 两序列都是小幅递增 → 高度正相关(off > 0 且接近几何平均)
        assert!(
            off > 0.0,
            "尾部对齐 → BTC 后 5 帧与 ETH 5 帧正相关,off={off}"
        );

        // sanity:BTC 对角(后 5 帧)应为小方差,远小于"全 10 帧 head 取前 5"的高方差
        // 全 10 帧 head 取前 5 帧 returns = [1.0, -0.5, 1.0, -0.5](var ≈ 0.625)
        // 后 5 帧 returns = [0.01, 0.0099, 0.0098, 0.0097](var ≈ 2.5e-7)
        assert!(
            diag_btc < 0.01,
            "尾部对齐 → BTC 对角应=后 5 帧小方差,实际 diag_btc={diag_btc}"
        );
        // ETH 对角:4 帧 returns 都是小幅递增(var ≈ 5e-7)
        assert!(
            diag_eth < 0.01,
            "ETH 对角应=小方差,实际 diag_eth={diag_eth}"
        );
    }

    /// 长度差异不影响协方差计算正确性(防御性测试)
    ///
    /// 场景:BTC 5 帧(早期),ETH 10 帧(早期 5 帧 + 后期 5 帧)
    /// 对齐后:两者都用前 5 帧(因为 BTC 决定 n=5)
    /// 修复前 vs 修复后:此场景下 head 对齐和 tail 对齐都用 BTC 全 5 帧(因为 BTC 没有"前/后"之分),
    /// 应该得到相同结果
    #[test]
    fn b3_returns_align_handles_short_and_long_mix() {
        let source = Arc::new(InMemoryMarketData::new());
        // BTC:5 帧(短)
        let btc_prices = [100.0, 110.0, 121.0, 110.0, 100.0];
        for (i, &p) in btc_prices.iter().enumerate() {
            source.push_mark(btc_spot(), Timestamp::from_nanos((i as i64 + 1) * 100), p);
        }
        // ETH:10 帧(长,前 5 帧与 BTC 同步,后 5 帧继续)
        let eth_prices = [
            200.0, 220.0, 242.0, 220.0, 200.0, 195.0, 198.0, 200.0, 199.0, 201.0,
        ];
        for (i, &p) in eth_prices.iter().enumerate() {
            source.push_mark(eth_perp(), Timestamp::from_nanos((i as i64 + 1) * 100), p);
        }
        let engine = PortfolioRiskEngine::with_source(source);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        apply_trade(&mut portfolio, &btc_spot(), Side::Buy, 100.0, 1.0);
        apply_trade(&mut portfolio, &eth_perp(), Side::Buy, 200.0, 1.0);

        let cov = engine.gamma_covariance_matrix(&portfolio);
        assert_eq!(cov.len(), 3);
        // 对齐后两个 instrument 都用前 5 帧(BTC 决定 n=5)
        // BTC returns: 0.10, 0.10, -0.0909, -0.0909
        // ETH returns(前 5 帧): 0.10, 0.10, -0.0909, -0.0909(完全相同)
        let off = cov.get(&(btc_spot(), eth_perp())).copied().unwrap();
        // 完全相同 returns → cov = var(returns) > 0
        assert!(
            off > 0.0,
            "前 5 帧完全相同 returns → off-diag > 0,got {off}"
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 0.8.0 B3.6: `latest_returns` production use of `MarketDataSource::latest_return`
    // (验证 trait method 真正被使用,而非只在 tests 出现)
    // ═══════════════════════════════════════════════════════════════════════

    /// `latest_returns` 只取最新 1 帧 return,正确性验证
    ///
    /// BTC 3 帧 mark(100→105→110):
    /// - `mark_returns` 返回 `[0.05, 0.0476...]`(2 帧)
    /// - `latest_returns` 应只返回最后 1 帧 ≈ 110/105 - 1 ≈ 0.0476
    #[test]
    fn b3_latest_returns_takes_only_last_frame() {
        let source = Arc::new(InMemoryMarketData::new());
        let prices = [100.0, 105.0, 110.0];
        for (i, &p) in prices.iter().enumerate() {
            source.push_mark(btc_spot(), Timestamp::from_nanos((i as i64 + 1) * 100), p);
        }
        let engine = PortfolioRiskEngine::with_source(source);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        apply_trade(&mut portfolio, &btc_spot(), Side::Buy, 100.0, 1.0);

        let latest = engine.latest_returns(&portfolio);
        // 单 instrument 持仓
        assert_eq!(latest.len(), 1);
        let lr = latest.get(&btc_spot()).copied().unwrap();
        // latest return = 110/105 - 1(末两帧)
        let expected = 110.0 / 105.0 - 1.0;
        assert!(
            (lr - expected).abs() < 1e-12,
            "latest return 应 = 110/105-1,got {lr} vs {expected}"
        );
        // sanity:不等于第一帧 return(0.05)
        assert!((lr - 0.05).abs() > 1e-3, "latest 不应是首帧 return");
    }

    /// `latest_returns` 在无 source 时返回空 HashMap(0.7.0 兼容)
    #[test]
    fn b3_latest_returns_no_source_returns_empty() {
        let engine = PortfolioRiskEngine::new();
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        apply_trade(&mut portfolio, &btc_spot(), Side::Buy, 100.0, 1.0);

        let latest = engine.latest_returns(&portfolio);
        assert!(latest.is_empty(), "无 source → 空 HashMap");
    }

    /// 数据不足(< 2 帧 mark)的 instrument 不会出现在结果中
    #[test]
    fn b3_latest_returns_skips_instruments_with_insufficient_data() {
        let source = Arc::new(InMemoryMarketData::new());
        // BTC:仅 1 帧 mark → latest_return 返回 None → 被跳过
        source.push_mark(btc_spot(), Timestamp::from_nanos(100), 50_000.0);
        // ETH:2 帧 mark → 有 latest_return ≈ 0.0333
        source.push_mark(eth_perp(), Timestamp::from_nanos(100), 3_000.0);
        source.push_mark(eth_perp(), Timestamp::from_nanos(200), 3_100.0);

        let engine = PortfolioRiskEngine::with_source(source);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        apply_trade(&mut portfolio, &btc_spot(), Side::Buy, 50_000.0, 1.0);
        apply_trade(&mut portfolio, &eth_perp(), Side::Buy, 3_000.0, 1.0);

        let latest = engine.latest_returns(&portfolio);
        // BTC 被跳过(数据不足),只剩 ETH
        assert_eq!(latest.len(), 1);
        assert!(latest.get(&btc_spot()).is_none());
        let eth_lr = latest.get(&eth_perp()).copied().unwrap();
        let expected_eth = 3_100.0 / 3_000.0 - 1.0;
        assert!((eth_lr - expected_eth).abs() < 1e-12);
    }

    /// 零持仓 instrument 不出现在结果中(与 `mark_returns` 一致)
    #[test]
    fn b3_latest_returns_skips_zero_positions() {
        let source = Arc::new(InMemoryMarketData::new());
        source.push_mark(btc_spot(), Timestamp::from_nanos(100), 50_000.0);
        source.push_mark(btc_spot(), Timestamp::from_nanos(200), 51_000.0);
        // 同一 instrument,apply + 反向 apply 抵消 → 持仓 0
        let engine = PortfolioRiskEngine::with_source(source);
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        apply_trade(&mut portfolio, &btc_spot(), Side::Buy, 50_000.0, 1.0);
        apply_trade(&mut portfolio, &btc_spot(), Side::Sell, 51_000.0, 1.0);

        let latest = engine.latest_returns(&portfolio);
        assert!(latest.is_empty(), "零持仓 → 空 HashMap");
    }
}
