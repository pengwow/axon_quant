//! `MarketDataSource` trait 与历史市场数据抽象
//!
//! 0.8.0 Phase 2 B1 新增,B3 扩展(`mark_returns`)。
//! 提供 `PortfolioRiskEngine` 计算 gamma / vega 时所需的历史 mark 与 IV 数据。
//! 0.7.0 现状:gamma / vega 全 0,因 `push_mark` 只存最新 1 帧、无 IV 源。
//!
//! # 范围
//!
//! 本 trait 抽象 4 类查询:
//!
//! 1. [`mark_history`](MarketDataSource::mark_history):instrument 的 mark 时间序列
//! 2. [`implied_vol`](MarketDataSource::implied_vol):instrument 的隐含波动率
//! 3. [`latest_mark`](MarketDataSource::latest_mark):最新一帧 mark
//! 4. [`mark_returns`](MarketDataSource::mark_returns) (0.8.0 B3 新增):mark 简单收益率序列
//!
//! 内置实现(0.8.0 范围):
//!
//! - `InMemoryMarketData`:测试 / 单元回测用,`push` 增量写入
//! - `CsvMarketData`:从 CSV 文件加载,适合离线 / 单元测试
//!
//! 真实源(Deribit / Akash / Binance options)推迟到 0.9.0。

use crate::time::Timestamp;
use crate::types::Instrument;

/// 单帧 mark 数据(时间戳 + 价格)
pub type MarkPoint = (Timestamp, f64);

/// 历史市场数据源抽象
///
/// 为 `PortfolioRiskEngine::gamma_exposure` / `vega` 计算提供 mark 历史与 IV。
/// 0.8.0 范围:仅查询接口(只读 `&self`),写入通过具体 impl 的方法(如
/// `InMemoryMarketData::push_mark`)。
///
/// # Send + Sync
///
/// `Send + Sync` 是必要的,让 `MarketDataSource` 可作为 `Arc<dyn MarketDataSource>`
/// 跨线程共享(回测主循环 + Python PyO3 绑定)。
///
/// # 性能注意
///
/// 实现应保证 `mark_history` 在常见 lookback(≤ 1024)下的摊销 O(1) 单帧访问。
/// `InMemoryMarketData` 用 `Vec` + 末尾追加即可;`CsvMarketData` 在构造时一次性
/// 加载到内存。
pub trait MarketDataSource: Send + Sync {
    /// 获取 instrument 的 mark 历史,按时间升序,长度 ≤ `lookback`。
    ///
    /// 返回的 `Vec` 末尾是最新一帧,长度 = min(实际帧数, lookback)。
    /// 若 instrument 无数据,返回空 `Vec`(非 `None`,避免调用方 `.unwrap()`)。
    fn mark_history(&self, instrument: &Instrument, lookback: usize) -> Vec<MarkPoint>;

    /// 获取 instrument 的隐含波动率(IV),用于 vega 计算。
    ///
    /// 返回 `None` 表示:
    /// - 该 instrument 不在数据源中
    /// - 该 instrument 无对应期权(spot 通常无 IV)
    ///
    /// IV 是年化小数(0.5 = 50%),与 `VolatilityEstimator` 输出口径一致。
    fn implied_vol(&self, instrument: &Instrument) -> Option<f64>;

    /// 获取最新一帧 mark。
    ///
    /// 返回 `None` 表示该 instrument 无 mark 数据。
    /// 是 `mark_history(instrument, 1).last()` 的便捷封装。
    fn latest_mark(&self, instrument: &Instrument) -> Option<MarkPoint> {
        self.mark_history(instrument, 1).last().copied()
    }

    /// 0.8.0 B3 新增:计算 mark 历史的简单收益率(returns)
    ///
    /// 公式:`r_t = mark_t / mark_{t-1} - 1.0`
    ///
    /// 输入:`mark_history(instrument, lookback)`,长度 `N`(`N >= 2`)。
    /// 输出:`Vec<f64>` 长度 `N - 1`,按时间升序对应每帧的收益率。
    ///
    /// # 边界
    ///
    /// - `mark_history.len() < 2` → 返回空 `Vec`(无法计算 returns)
    /// - `mark_t = 0` → 对应 `r_t = +∞`(理论极端,实盘几乎不出现,本实现不防)
    /// - 默认实现从 `mark_history` 派生;`InMemoryMarketData` / `CsvMarketData` 继承
    /// - 高频实现可 override(预计算 returns 缓存)
    ///
    /// # 用途
    ///
    /// 协方差矩阵(gamma_covariance_matrix)用 returns 序列而非 prices:
    /// - prices 非平稳(non-stationary),直接算 cov 不稳定
    /// - returns 平稳,样本协方差才有统计意义
    fn mark_returns(&self, instrument: &Instrument, lookback: usize) -> Vec<f64> {
        let history = self.mark_history(instrument, lookback);
        if history.len() < 2 {
            return Vec::new();
        }
        history
            .windows(2)
            .map(|w| {
                let prev = w[0].1;
                let curr = w[1].1;
                if prev == 0.0 {
                    f64::INFINITY
                } else {
                    curr / prev - 1.0
                }
            })
            .collect()
    }

    /// 0.8.0 B3 新增:最新一帧 mark 收益率
    ///
    /// 返回 `None` 表示该 instrument 不足以计算 returns(< 2 帧 mark)。
    /// 是 `mark_returns(instrument, n).last()` 的便捷封装(`n >= 2`)。
    fn latest_return(&self, instrument: &Instrument) -> Option<f64> {
        // lookback=2 是最小开销路径,只取最后 1 个 return
        self.mark_returns(instrument, 2).last().copied()
    }
}
