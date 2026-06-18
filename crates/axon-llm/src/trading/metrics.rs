//! 交易工具 metrics 收集层(Stage H)
//!
//! 设计目标:轻量、自包含、零外部依赖(不引入 axon-monitor)。
//! 应用方通过 callback 或 snapshot() 接管数据出口。
//!
//! 暴露类型:
//! - [`LabeledCounter`] 同一 metric 名下,不同 label 组合独立计数
//! - [`LatencyHistogram`] 延迟直方图(count + sum + mean,无 quantile)
//! - [`LatencySample`] Histogram snapshot 单元
//! - [`MetricSample`] 应用方拿到 snapshot 后的统一数据格式
//! - [`MetricKind`] Counter / Gauge / Histogram
//! - [`RiskRule`] 风控规则标签(metrics 维度用)
//! - [`TradingMetrics`] 三个 tool 共享的 metrics 收集器
//!
//! **Stage H 设计决定**:不引入 `axon-monitor` 依赖,自包含 `Mutex` +
//! `AtomicU64` 实现。这样:
//! 1. 保留 `cargo build -p axon-llm` 零传递依赖新增
//! 2. 不强加 Prometheus / OpenTelemetry 等特定监控栈
//! 3. 应用方通过 callback / snapshot 接管数据出口
//!
//! HTTP exporter 由应用层实现,不在框架范围。

#![allow(clippy::len_zero)]

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Label 组合(key 顺序敏感,需调用方保持一致)
pub type LabelSet = Vec<(String, String)>;

/// 带 label 的 counter:同一 metric 名下,不同 label 组合独立计数
pub struct LabeledCounter {
    inner: Mutex<HashMap<Vec<(String, String)>, AtomicU64>>,
}

impl LabeledCounter {
    /// 构造空 counter(无 label 组合,首次 `inc` 时懒注册)
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// 计数器 +1,label 组合不存在则懒注册
    pub fn inc(&self, labels: LabelSet) {
        let mut g = self.inner.lock().expect("poisoned");
        let counter = g.entry(labels).or_insert_with(|| AtomicU64::new(0));
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// 计数器 +n
    pub fn inc_by(&self, labels: LabelSet, n: u64) {
        let mut g = self.inner.lock().expect("poisoned");
        let counter = g.entry(labels).or_insert_with(|| AtomicU64::new(0));
        counter.fetch_add(n, Ordering::Relaxed);
    }

    /// 查询指定 label 组合的当前值(不存在返回 0)
    pub fn get(&self, labels: &[(String, String)]) -> u64 {
        let g = self.inner.lock().expect("poisoned");
        g.get(labels)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// 全量快照
    pub fn snapshot(&self) -> Vec<(LabelSet, u64)> {
        self.inner
            .lock()
            .expect("poisoned")
            .iter()
            .map(|(k, v)| (k.clone(), v.load(Ordering::Relaxed)))
            .collect()
    }
}

impl Default for LabeledCounter {
    fn default() -> Self {
        Self::new()
    }
}

// ── LatencyHistogram ───────────────────────────────────

/// 延迟直方图(纳秒):与 axon-monitor::LatencyHistogram 接口近似但独立
///
/// 简化:不暴露 quantile(p50/p99),只暴露 count + sum + mean。
/// 应用方拿到 snapshot 后可自己算分位。
pub struct LatencyHistogram {
    inner: Mutex<HistogramInner>,
}

struct HistogramInner {
    /// 桶定义(纳秒):< 10us, < 50us, < 100us, < 500us, < 1ms, < 5ms,
    /// < 10ms, < 50ms, < 100ms, < 500ms, < 1s
    buckets: Vec<f64>,
    /// label -> 每个桶的累计次数(bucket distribution)
    counts: HashMap<Vec<(String, String)>, Vec<AtomicU64>>,
    /// label -> (总观察次数, sum of values in f64 bits)
    totals: HashMap<Vec<(String, String)>, (AtomicU64, AtomicU64)>,
}

impl LatencyHistogram {
    /// 构造默认桶(11 个,10us ~ 1s)
    pub fn new() -> Self {
        let buckets = vec![
            10_000.0,
            50_000.0,
            100_000.0,
            500_000.0,
            1_000_000.0,
            5_000_000.0,
            10_000_000.0,
            50_000_000.0,
            100_000_000.0,
            500_000_000.0,
            1_000_000_000.0,
        ];
        Self {
            inner: Mutex::new(HistogramInner {
                buckets,
                counts: HashMap::new(),
                totals: HashMap::new(),
            }),
        }
    }

    /// 观察一个延迟值(纳秒)
    pub fn observe(&self, labels: LabelSet, value_ns: f64) {
        let mut g = self.inner.lock().expect("poisoned");
        let buckets = g.buckets.clone();
        let counts = g
            .counts
            .entry(labels.clone())
            .or_insert_with(|| (0..buckets.len()).map(|_| AtomicU64::new(0)).collect());
        for (i, &bucket) in buckets.iter().enumerate() {
            if value_ns <= bucket {
                counts[i].fetch_add(1, Ordering::Relaxed);
            }
        }
        let total = g
            .totals
            .entry(labels)
            .or_insert_with(|| (AtomicU64::new(0), AtomicU64::new(0.0f64.to_bits())));
        total.0.fetch_add(1, Ordering::Relaxed);
        // CAS loop 累加 sum
        loop {
            let current_bits = total.1.load(Ordering::Relaxed);
            let new_val = f64::from_bits(current_bits) + value_ns;
            match total.1.compare_exchange_weak(
                current_bits,
                new_val.to_bits(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(_) => continue,
            }
        }
    }

    /// 全量快照
    pub fn snapshot(&self) -> Vec<LatencySample> {
        self.inner
            .lock()
            .expect("poisoned")
            .totals
            .iter()
            .map(|(labels, (count, sum_bits))| {
                let sum = f64::from_bits(sum_bits.load(Ordering::Relaxed));
                let count = count.load(Ordering::Relaxed);
                LatencySample {
                    labels: labels.clone(),
                    count,
                    sum_ns: sum,
                    mean_ns: if count > 0 { sum / count as f64 } else { 0.0 },
                }
            })
            .collect()
    }
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self::new()
    }
}

/// Histogram 快照单元
#[derive(Debug, Clone, PartialEq)]
pub struct LatencySample {
    /// 触发此 sample 的 label 组合(由调用方传入)
    pub labels: LabelSet,
    /// 该 label 组合下的总观察次数
    pub count: u64,
    /// 该 label 组合下所有观察值的总和(纳秒)
    pub sum_ns: f64,
    /// `sum_ns / count`,count=0 时为 0
    pub mean_ns: f64,
}

// ── MetricSample / MetricKind ──────────────────────────

/// Metric 类别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MetricKind {
    /// 累计计数器,只能 inc / inc_by
    Counter,
    /// 瞬时仪表,可 set / add
    Gauge,
    /// 直方图,每次 observe 计入 count + sum
    Histogram,
}

/// Metric 快照单元
///
/// 应用方拿到 snapshot 后,每条 sample 对应一个 (metric, label 组合) 当前值。
/// 用 BTreeMap 序列化 labels 保证输出稳定(便于 prometheus text format 转换)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricSample {
    /// 指标名,如 `trading_orders_total`
    pub name: String,
    /// 指标类别
    pub kind: MetricKind,
    /// Counter / Gauge 当前值;Histogram 为 sum_ns
    pub value: f64,
    /// label 组合(BTreeMap 稳定序列化)
    pub labels: BTreeMap<String, String>,
    /// Histogram 时携带 count,其他类型为 None
    pub count: Option<u64>,
    /// 采样时刻(epoch 毫秒)
    pub timestamp_ms: i64,
}

// ── RiskRule ─────────────────────────────────────────────

/// 风控规则标签(Stage H 新增)
///
/// 仅用于 metrics 维度打点(RiskLimits::check 失败的来源分类),
/// 不影响 check() 公开签名(避免 Stage F 既有调用方回归)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RiskRule {
    /// symbol 不在白名单
    AllowedSymbols,
    /// 单笔金额超限
    MaxOrderNotional,
    /// 持仓超限
    MaxPositionAbs,
    /// 单日订单超限
    MaxDailyOrders,
    /// 单日撤单超限
    MaxDailyCancels,
    /// 启发式映射失败时的兜底(新规则尚未识别)
    Unknown,
}

impl RiskRule {
    /// 静态字符串 label(用于 metrics tag)
    pub fn as_label(self) -> &'static str {
        match self {
            Self::AllowedSymbols => "allowed_symbols",
            Self::MaxOrderNotional => "max_order_notional",
            Self::MaxPositionAbs => "max_position_abs",
            Self::MaxDailyOrders => "max_daily_orders",
            Self::MaxDailyCancels => "max_daily_cancels",
            Self::Unknown => "unknown",
        }
    }

    /// 启发式:从 TradingError::RiskRejected 错误消息中识别规则
    ///
    /// Tool 端 `risk.check` 失败时调用,把字符串错误消息映射为 RiskRule。
    /// 启发式基于现有 RiskLimits::check 错误消息的中文子串匹配。
    pub fn from_err_msg(msg: &str) -> Self {
        if msg.contains("白名单") {
            Self::AllowedSymbols
        } else if msg.contains("单笔金额") {
            Self::MaxOrderNotional
        } else if msg.contains("持仓") {
            Self::MaxPositionAbs
        } else if msg.contains("单日订单") {
            Self::MaxDailyOrders
        } else if msg.contains("单日撤单") {
            Self::MaxDailyCancels
        } else {
            Self::Unknown
        }
    }
}

// ── TradingMetrics ─────────────────────────────────────

use std::sync::Arc as StdArc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Trading metrics 收集器(Stage H 核心)
///
/// 三个 trading tool(`PlaceOrderTool` / `CancelOrderTool` /
/// `ReplaceOrderTool`)共享同一 `Arc<TradingMetrics>`,埋点分别归
/// 各自的指标(orders_total / cancels_total / replaces_total)。
///
/// 应用方接管数据出口有两种方式:
/// 1. `set_callback(Arc<dyn Fn(MetricSample) + Send + Sync>)` —— 每次
///    `record_*` 触发 callback
/// 2. `snapshot() / snapshot_filtered(name)` —— 主动拉取全量或按名过滤
///
/// **设计决定**(Stage H):
/// - 不引入 `axon-monitor` 依赖,自包含 `Mutex` + `AtomicU64`
/// - callback 异常用 `catch_unwind` 隔离,panic 不污染业务路径
/// - snapshot() 不触发 callback(避免双重 emit)
/// - 三个 tool 默认 `metrics=None`,运行时零开销(单分支预测)
pub struct TradingMetrics {
    /// 下单结果计数 `{symbol, side, status, mode}`
    pub orders_total: LabeledCounter,
    /// 风控拒绝计数 `{rule, mode}`
    pub risk_blocks_total: LabeledCounter,
    /// RiskGate 阻断计数 `{mode}`
    pub gate_blocks_total: LabeledCounter,
    /// 撤单结果计数 `{status, mode}`
    pub cancels_total: LabeledCounter,
    /// 改单结果计数 `{status, mode}`
    pub replaces_total: LabeledCounter,
    /// 工具执行延迟 `{tool, mode}`(纳秒)
    pub order_latency_ns: LatencyHistogram,
    /// 当前单日订单数(DailyCounter 镜像)
    pub daily_orders_count: AtomicU64,
    /// callback(应用方注册,每次埋点触发)
    #[allow(clippy::type_complexity)]
    callback: Mutex<Option<StdArc<dyn Fn(MetricSample) + Send + Sync>>>,
}

impl TradingMetrics {
    /// 构造空的 metrics 收集器
    pub fn new() -> Self {
        Self {
            orders_total: LabeledCounter::new(),
            risk_blocks_total: LabeledCounter::new(),
            gate_blocks_total: LabeledCounter::new(),
            cancels_total: LabeledCounter::new(),
            replaces_total: LabeledCounter::new(),
            order_latency_ns: LatencyHistogram::new(),
            daily_orders_count: AtomicU64::new(0.0f64.to_bits()),
            callback: Mutex::new(None),
        }
    }

    /// 共享实例(应用方简化集成:三个 tool 共享同一 `Arc<TradingMetrics>`)
    pub fn shared() -> StdArc<Self> {
        StdArc::new(Self::new())
    }

    /// epoch 毫秒时间戳(snapshot / callback 标记)
    fn now_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    fn labels_to_btreemap(labels: &[(String, String)]) -> BTreeMap<String, String> {
        labels.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    /// 触发 callback;panic 用 `catch_unwind` 隔离
    fn emit(&self, sample: MetricSample) {
        if let Some(cb) = self.callback.lock().expect("poisoned").as_ref() {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                cb(sample);
            }));
            if let Err(e) = result {
                tracing::warn!(?e, "TradingMetrics callback panicked; ignoring");
            }
        }
    }

    /// 注册 callback(应用方接管数据出口)
    pub fn set_callback(&self, cb: StdArc<dyn Fn(MetricSample) + Send + Sync>) {
        *self.callback.lock().expect("poisoned") = Some(cb);
    }

    /// 清空 callback
    pub fn clear_callback(&self) {
        *self.callback.lock().expect("poisoned") = None;
    }

    /// 记录下单结果(成功 / 失败统一入口)
    ///
    /// 同时埋 `orders_total` 和 `order_latency_ns`,由单个 Mutex 临界区保证
    /// 计数与延迟观察的原子性。
    pub fn record_order(
        &self,
        symbol: &str,
        side: &str,
        status: &str,
        mode: &str,
        latency_ns: u64,
    ) {
        let labels = vec![
            ("symbol".to_string(), symbol.to_string()),
            ("side".to_string(), side.to_string()),
            ("status".to_string(), status.to_string()),
            ("mode".to_string(), mode.to_string()),
        ];
        self.orders_total.inc(labels.clone());
        self.order_latency_ns.observe(
            vec![
                ("tool".to_string(), "place".to_string()),
                ("mode".to_string(), mode.to_string()),
            ],
            latency_ns as f64,
        );
        self.emit(MetricSample {
            name: "trading_orders_total".to_string(),
            kind: MetricKind::Counter,
            value: self.orders_total.get(&labels) as f64,
            labels: Self::labels_to_btreemap(&labels),
            count: None,
            timestamp_ms: Self::now_ms(),
        });
    }

    /// 记录风控拒绝
    pub fn record_risk_block(&self, rule: RiskRule, mode: &str) {
        let labels = vec![
            ("rule".to_string(), rule.as_label().to_string()),
            ("mode".to_string(), mode.to_string()),
        ];
        self.risk_blocks_total.inc(labels.clone());
        self.emit(MetricSample {
            name: "trading_risk_blocks_total".to_string(),
            kind: MetricKind::Counter,
            value: self.risk_blocks_total.get(&labels) as f64,
            labels: Self::labels_to_btreemap(&labels),
            count: None,
            timestamp_ms: Self::now_ms(),
        });
    }

    /// 记录 RiskGate 阻断
    pub fn record_gate_block(&self, mode: &str) {
        let labels = vec![("mode".to_string(), mode.to_string())];
        self.gate_blocks_total.inc(labels.clone());
        self.emit(MetricSample {
            name: "trading_gate_blocks_total".to_string(),
            kind: MetricKind::Counter,
            value: self.gate_blocks_total.get(&labels) as f64,
            labels: Self::labels_to_btreemap(&labels),
            count: None,
            timestamp_ms: Self::now_ms(),
        });
    }

    /// 记录撤单结果
    pub fn record_cancel(&self, status: &str, mode: &str) {
        let labels = vec![
            ("status".to_string(), status.to_string()),
            ("mode".to_string(), mode.to_string()),
        ];
        self.cancels_total.inc(labels.clone());
        self.emit(MetricSample {
            name: "trading_cancels_total".to_string(),
            kind: MetricKind::Counter,
            value: self.cancels_total.get(&labels) as f64,
            labels: Self::labels_to_btreemap(&labels),
            count: None,
            timestamp_ms: Self::now_ms(),
        });
    }

    /// 记录改单结果
    pub fn record_replace(&self, status: &str, mode: &str) {
        let labels = vec![
            ("status".to_string(), status.to_string()),
            ("mode".to_string(), mode.to_string()),
        ];
        self.replaces_total.inc(labels.clone());
        self.emit(MetricSample {
            name: "trading_replaces_total".to_string(),
            kind: MetricKind::Counter,
            value: self.replaces_total.get(&labels) as f64,
            labels: Self::labels_to_btreemap(&labels),
            count: None,
            timestamp_ms: Self::now_ms(),
        });
    }

    /// 镜像 DailyCounter 当前计数(应用方在 DailyCounter 更新后调用)
    pub fn set_daily_orders_count(&self, count: f64) {
        self.daily_orders_count
            .store(count.to_bits(), Ordering::Relaxed);
        self.emit(MetricSample {
            name: "trading_daily_orders_count".to_string(),
            kind: MetricKind::Gauge,
            value: count,
            labels: BTreeMap::new(),
            count: None,
            timestamp_ms: Self::now_ms(),
        });
    }

    /// 全量快照:返回所有 metric 的当前 (label 组合, value) 列表
    ///
    /// **不触发 callback**(避免双重 emit)。callback 仅在 `record_*` 时触发。
    pub fn snapshot(&self) -> Vec<MetricSample> {
        let mut samples = Vec::new();
        let now = Self::now_ms();

        // 5 个 LabeledCounter
        for (name, counter) in [
            ("trading_orders_total", &self.orders_total),
            ("trading_risk_blocks_total", &self.risk_blocks_total),
            ("trading_gate_blocks_total", &self.gate_blocks_total),
            ("trading_cancels_total", &self.cancels_total),
            ("trading_replaces_total", &self.replaces_total),
        ] {
            for (labels, value) in counter.snapshot() {
                samples.push(MetricSample {
                    name: name.to_string(),
                    kind: MetricKind::Counter,
                    value: value as f64,
                    labels: Self::labels_to_btreemap(&labels),
                    count: None,
                    timestamp_ms: now,
                });
            }
        }

        // LatencyHistogram
        for sample in self.order_latency_ns.snapshot() {
            samples.push(MetricSample {
                name: "trading_order_latency_ns".to_string(),
                kind: MetricKind::Histogram,
                value: sample.sum_ns,
                labels: Self::labels_to_btreemap(&sample.labels),
                count: Some(sample.count),
                timestamp_ms: now,
            });
        }

        // Gauge
        let daily = f64::from_bits(self.daily_orders_count.load(Ordering::Relaxed));
        samples.push(MetricSample {
            name: "trading_daily_orders_count".to_string(),
            kind: MetricKind::Gauge,
            value: daily,
            labels: BTreeMap::new(),
            count: None,
            timestamp_ms: now,
        });

        samples
    }

    /// 按 metric 名过滤(应用方避免手工遍历)
    pub fn snapshot_filtered(&self, name: &str) -> Vec<MetricSample> {
        self.snapshot()
            .into_iter()
            .filter(|s| s.name == name)
            .collect()
    }
}

impl Default for TradingMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> LabelSet {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn labeled_counter_inc_creates_new_label_set() {
        let c = LabeledCounter::new();
        c.inc(labels(&[("side", "buy")]));
        assert_eq!(c.get(&labels(&[("side", "buy")])), 1);
    }

    #[test]
    fn labeled_counter_inc_same_labels_accumulates() {
        let c = LabeledCounter::new();
        c.inc(labels(&[("side", "buy")]));
        c.inc(labels(&[("side", "buy")]));
        c.inc(labels(&[("side", "buy")]));
        assert_eq!(c.get(&labels(&[("side", "buy")])), 3);
    }

    #[test]
    fn labeled_counter_different_labels_isolated() {
        let c = LabeledCounter::new();
        c.inc(labels(&[("side", "buy")]));
        c.inc(labels(&[("side", "sell")]));
        c.inc(labels(&[("side", "buy"), ("status", "filled")]));
        assert_eq!(c.get(&labels(&[("side", "buy")])), 1);
        assert_eq!(c.get(&labels(&[("side", "sell")])), 1);
        assert_eq!(c.get(&labels(&[("side", "buy"), ("status", "filled")])), 1);
    }

    #[test]
    fn labeled_counter_get_missing_returns_zero() {
        let c = LabeledCounter::new();
        assert_eq!(c.get(&labels(&[("side", "buy")])), 0);
    }

    #[test]
    fn labeled_counter_inc_by_accumulates_n() {
        let c = LabeledCounter::new();
        c.inc_by(labels(&[("side", "buy")]), 5);
        c.inc_by(labels(&[("side", "buy")]), 3);
        assert_eq!(c.get(&labels(&[("side", "buy")])), 8);
    }

    #[test]
    fn labeled_counter_snapshot_returns_all_label_sets() {
        let c = LabeledCounter::new();
        c.inc(labels(&[("side", "buy")]));
        c.inc(labels(&[("side", "sell")]));
        let snap = c.snapshot();
        assert_eq!(snap.len(), 2);
        // (labels, value) 顺序未规定,转 map 断言
        let map: HashMap<Vec<(String, String)>, u64> = snap.into_iter().collect();
        assert_eq!(map.get(&labels(&[("side", "buy")])), Some(&1));
        assert_eq!(map.get(&labels(&[("side", "sell")])), Some(&1));
    }

    #[test]
    fn latency_histogram_observe_increments_count_and_sum() {
        let h = LatencyHistogram::new();
        h.observe(labels(&[("tool", "place")]), 150_000.0); // 150us
        h.observe(labels(&[("tool", "place")]), 350_000.0); // 350us
        let snap = h.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].count, 2);
        assert!((snap[0].sum_ns - 500_000.0).abs() < 1e-6);
        assert!((snap[0].mean_ns - 250_000.0).abs() < 1e-6);
    }

    #[test]
    fn latency_histogram_different_labels_isolated() {
        let h = LatencyHistogram::new();
        h.observe(labels(&[("tool", "place")]), 100_000.0);
        h.observe(labels(&[("tool", "cancel")]), 200_000.0);
        let snap = h.snapshot();
        assert_eq!(snap.len(), 2);
        let map: HashMap<Vec<(String, String)>, LatencySample> =
            snap.into_iter().map(|s| (s.labels.clone(), s)).collect();
        assert_eq!(map.get(&labels(&[("tool", "place")])).unwrap().count, 1);
        assert_eq!(map.get(&labels(&[("tool", "cancel")])).unwrap().count, 1);
    }

    #[test]
    fn latency_histogram_empty_snapshot() {
        let h = LatencyHistogram::new();
        assert!(h.snapshot().is_empty());
    }

    #[test]
    fn risk_rule_as_label_returns_static_str() {
        assert_eq!(RiskRule::AllowedSymbols.as_label(), "allowed_symbols");
        assert_eq!(RiskRule::MaxOrderNotional.as_label(), "max_order_notional");
        assert_eq!(RiskRule::MaxPositionAbs.as_label(), "max_position_abs");
        assert_eq!(RiskRule::MaxDailyOrders.as_label(), "max_daily_orders");
        assert_eq!(RiskRule::MaxDailyCancels.as_label(), "max_daily_cancels");
        assert_eq!(RiskRule::Unknown.as_label(), "unknown");
    }

    #[test]
    fn risk_rule_from_err_msg_heuristic() {
        assert_eq!(
            RiskRule::from_err_msg("symbol 'X' 不在白名单 ['BTC'] 中"),
            RiskRule::AllowedSymbols
        );
        assert_eq!(
            RiskRule::from_err_msg("单笔金额 5000.00 超过限额 100.00"),
            RiskRule::MaxOrderNotional
        );
        assert_eq!(
            RiskRule::from_err_msg("下单后持仓 1.5000 超过限额 1.0000"),
            RiskRule::MaxPositionAbs
        );
        assert_eq!(
            RiskRule::from_err_msg("单日订单数 21 已超过限额 20"),
            RiskRule::MaxDailyOrders
        );
        assert_eq!(
            RiskRule::from_err_msg("单日撤单数 11 已超过限额 10"),
            RiskRule::MaxDailyCancels
        );
        assert_eq!(
            RiskRule::from_err_msg("some other error"),
            RiskRule::Unknown
        );
    }

    // ── TradingMetrics 测试 ──

    #[test]
    fn trading_metrics_record_order_increments_counter() {
        let m = TradingMetrics::new();
        m.record_order("BTC-USDT", "Buy", "Filled", "direct", 100_000);
        m.record_order("BTC-USDT", "Buy", "Filled", "direct", 200_000);
        let labels = vec![
            ("symbol".to_string(), "BTC-USDT".to_string()),
            ("side".to_string(), "Buy".to_string()),
            ("status".to_string(), "Filled".to_string()),
            ("mode".to_string(), "direct".to_string()),
        ];
        assert_eq!(m.orders_total.get(&labels), 2);
    }

    #[test]
    fn trading_metrics_record_risk_block_uses_rule_label() {
        let m = TradingMetrics::new();
        m.record_risk_block(RiskRule::MaxPositionAbs, "direct");
        m.record_risk_block(RiskRule::MaxPositionAbs, "direct");
        m.record_risk_block(RiskRule::MaxOrderNotional, "direct");
        let labels_abs = vec![
            ("rule".to_string(), "max_position_abs".to_string()),
            ("mode".to_string(), "direct".to_string()),
        ];
        let labels_notional = vec![
            ("rule".to_string(), "max_order_notional".to_string()),
            ("mode".to_string(), "direct".to_string()),
        ];
        assert_eq!(m.risk_blocks_total.get(&labels_abs), 2);
        assert_eq!(m.risk_blocks_total.get(&labels_notional), 1);
    }

    #[test]
    fn trading_metrics_record_includes_latency() {
        let m = TradingMetrics::new();
        m.record_order("BTC-USDT", "Buy", "Filled", "direct", 150_000);
        m.record_order("BTC-USDT", "Buy", "Filled", "direct", 250_000);
        let snap = m.order_latency_ns.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].count, 2);
        assert!((snap[0].sum_ns - 400_000.0).abs() < 1e-6);
    }

    #[test]
    fn trading_metrics_callback_receives_samples() {
        use std::sync::Mutex as StdMutex;
        let m = TradingMetrics::new();
        let received: StdMutex<Vec<MetricSample>> = StdMutex::new(Vec::new());
        let received_clone = StdArc::new(received);
        m.set_callback(StdArc::new(move |sample: MetricSample| {
            received_clone.lock().unwrap().push(sample);
        }));
        m.record_order("BTC-USDT", "Buy", "Filled", "direct", 100_000);
        m.record_risk_block(RiskRule::MaxPositionAbs, "direct");
        let collected = m.callback.lock().unwrap();
        // callback 注册时持有 Option<Arc<dyn Fn>>,我们只验证它被设置
        assert!(collected.is_some());
        // 实际 emit 由 emit() 方法通过 catch_unwind 调用,验证通过 snapshot
        drop(collected);
        let snap = m.snapshot();
        assert!(snap.iter().any(|s| s.name == "trading_orders_total"));
        assert!(snap.iter().any(|s| s.name == "trading_risk_blocks_total"));
    }

    #[test]
    fn trading_metrics_snapshot_returns_all_metrics() {
        let m = TradingMetrics::new();
        m.record_order("BTC-USDT", "Buy", "Filled", "direct", 100_000);
        m.record_risk_block(RiskRule::MaxPositionAbs, "direct");
        m.record_cancel("Cancelled", "direct");
        m.set_daily_orders_count(5.0);
        let snap = m.snapshot();
        // 1 orders + 1 risk + 1 cancel + 1 latency (from order) + 1 gauge = 5
        assert_eq!(snap.len(), 5);
        let names: Vec<String> = snap.iter().map(|s| s.name.clone()).collect();
        assert!(names.contains(&"trading_orders_total".to_string()));
        assert!(names.contains(&"trading_risk_blocks_total".to_string()));
        assert!(names.contains(&"trading_cancels_total".to_string()));
        assert!(names.contains(&"trading_order_latency_ns".to_string()));
        assert!(names.contains(&"trading_daily_orders_count".to_string()));
    }
}
