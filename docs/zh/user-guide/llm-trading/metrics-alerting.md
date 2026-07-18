# LLM 交易指标与告警

> 适用版本:axon-llm v0.6.0+
> 前置阅读:[overview.md](overview.md) §5

本文档详述 `TradingMetrics` 的指标体系、数据出口、应用方集成到监控后端的示例模板。

**重要决策**:axon-llm **不强加**特定监控栈(不内置 Prometheus exporter、不依赖 `axon-monitor`、不提供 Grafana dashboard / Prometheus 告警 YAML)。`TradingMetrics` 自包含 `Mutex` + `AtomicU64`,应用方通过 callback / snapshot 两种数据出口自行接到任意监控后端(Prometheus / OpenTelemetry / StatsD / 自定义)。

Grafana dashboard / Prometheus 告警规则由各团队按自己的监控栈自配,axon 路线图不集中维护。

## 1. 核心指标(4 类)

### 1.1 Counter:`trading_orders_total{tool,side,status}`

每个 tool 一次下单/撤单/改单 +1。可选 label:

| label | 取值 | 含义 |
|---|---|---|
| `tool` | `place` / `cancel` / `replace` | 哪个 tool 触发 |
| `side` | `buy` / `sell` / `none` | 下单方向(cancel/replace 为 `none`) |
| `status` | `success` / `rejected` / `failed` | 执行结果 |

### 1.2 Counter:`trading_risk_rejections_total{source}`

风控拒绝次数 +1。可选 label:

| label | 取值 | 含义 |
|---|---|---|
| `source` | `risk_limits` / `risk_gate` / `safety_mode` | 哪道防线拒绝 |

### 1.3 Counter:`trading_backend_errors_total{backend,kind}`

后端调用失败次数 +1。可选 label:

| label | 取值 | 含义 |
|---|---|---|
| `backend` | `mock` / `exchange` / `oms` / `backtest` | 哪个后端 |
| `kind` | `network` / `rejected` / `timeout` / `other` | 错误类型 |

### 1.4 Histogram:`trading_tool_execute_duration_seconds{tool}`

`Tool::execute()` 端到端时延分布,典型 bucket:`[0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0]`(秒)。

### 1.5 Gauge:`trading_daily_orders_count`

当日累计订单数(从 `RiskLimits::DailyCounter` 镜像),每日 UTC 0 点自动重置。

## 2. 数据出口

### 2.1 回调(实时推送)

```rust
use std::sync::Arc;
use axon_llm::trading::metrics::{TradingMetrics, MetricSample};

let metrics = TradingMetrics::new();

// 注册回调:每次指标变化时立即调用
metrics.set_callback(Arc::new(|sample: MetricSample| {
    match sample {
        MetricSample::CounterInc { name, labels, value } => {
            println!("[counter] {} {:?} += {}", name, labels, value);
            // 推送到 Prometheus / OTLP / 自定义 sink
        }
        MetricSample::HistogramObserve { name, labels, value_secs } => {
            println!("[histogram] {} {:?} observe {}s", name, labels, value_secs);
        }
        MetricSample::GaugeSet { name, value } => {
            println!("[gauge] {} = {}", name, value);
        }
    }
}));
```

**注意**:
- 回调在 `TradingMetrics` 内部持 `Mutex<Option<Arc<dyn Fn ...>>>`,只允许 1 个回调
- 回调阻塞会导致所有后续指标记录阻塞,**回调必须非阻塞**(用 channel / mpsc / 协程)
- 推荐把回调内的工作推到独立的 tokio task

### 2.2 快照(主动拉取)

```rust
let snapshot: Vec<MetricSample> = metrics.snapshot();

for sample in snapshot {
    println!("{:?}", sample);
}
```

**典型用途**:
- 定时上报(每 10s / 30s 一次)
- 健康检查端点返回当前指标状态
- 集成测试中验证指标递增

## 3. 应用方集成示例

### 3.1 Rust + Prometheus(用 `prometheus` crate)

```rust
use prometheus::{Registry, IntCounterVec, HistogramVec, IntGauge, register_int_counter_vec_with_registry, register_histogram_vec_with_registry, register_int_gauge_with_registry};

let registry = Registry::new();
let orders_total = register_int_counter_vec_with_registry!(
    "trading_orders_total", "Total trading orders",
    &["tool", "side", "status"], registry
)?;
let duration = register_histogram_vec_with_registry!(
    "trading_tool_execute_duration_seconds", "Tool execution duration",
    &["tool"], registry
)?;

let metrics = TradingMetrics::new();
metrics.set_callback(Arc::new(move |sample| match sample {
    MetricSample::CounterInc { name, labels, value } if name == "trading_orders_total" => {
        orders_total.with_label_values(&[&labels["tool"], &labels["side"], &labels["status"]]).inc_by(value);
    }
    MetricSample::HistogramObserve { name, labels, value_secs } if name == "trading_tool_execute_duration_seconds" => {
        duration.with_label_values(&[&labels["tool"]]).observe(value_secs);
    }
    _ => {}
}));

// 暴露给 Prometheus
let encoder = prometheus::TextEncoder::new();
// 周期性把 registry.gather() 写入 HTTP 响应
```

### 3.2 Python + Prometheus(用 `prometheus_client`)

```python
import axon_quant
from prometheus_client import Counter, Histogram, start_http_server

orders_total = Counter(
    'trading_orders_total', 'Total trading orders',
    ['tool', 'side', 'status']
)
duration = Histogram(
    'trading_tool_execute_duration_seconds', 'Tool execution duration',
    ['tool']
)

# 启动 Prometheus exporter(独立 HTTP 端口)
start_http_server(9100)

# 注册 callback
def on_sample(sample):
    kind, data = sample
    if kind == 'counter_inc' and data['name'] == 'trading_orders_total':
        orders_total.labels(**data['labels']).inc(data['value'])
    elif kind == 'histogram_observe' and data['name'] == 'trading_tool_execute_duration_seconds':
        duration.labels(**data['labels']).observe(data['value_secs'])

axon_quant.set_metrics_callback(on_sample)
```

### 3.3 Rust + OpenTelemetry(用 `opentelemetry` crate)

```rust
use opentelemetry::metrics::MeterProvider;
let provider = opentelemetry_otlp::new_pipeline().install_simple();
let meter = provider.meter("axon-llm");

let orders_counter = meter.u64_counter("trading_orders_total").init();
let duration_hist = meter.f64_histogram("trading_tool_execute_duration_seconds").init();

metrics.set_callback(Arc::new(move |sample| match sample {
    MetricSample::CounterInc { name, labels, value } if name == "trading_orders_total" => {
        let attrs = labels.iter().map(|(k, v)| KeyValue::new(k, v)).collect::<Vec<_>>();
        orders_counter.add(value, &attrs);
    }
    // ...
    _ => {}
}));
```

## 4. 告警建议(应用方配置)

axon 不提供集中告警规则,以下是基于指标的 **告警建议**,供应用方参考:

| 告警名称 | 触发条件 | 严重度 | 行动 |
|---------|---------|--------|------|
| HighRiskRejection | `rate(trading_risk_rejections_total[5m]) > 10` | warning | 检查 LLM prompt 是否被越狱 |
| CircuitBreakerOpen | `trading_risk_gate_blocked_total > 0` | critical | 立即人工接管,检查决策日志 |
| BackendErrorSpike | `rate(trading_backend_errors_total[5m]) > 5` | critical | 检查交易所 API / OMS 状态 |
| LatencyP99TooHigh | `histogram_quantile(0.99, rate(trading_tool_execute_duration_seconds_bucket[5m])) > 5` | warning | 检查后端延迟、网络质量 |
| DailyOrderBurst | `trading_daily_orders_count > 80% * max_daily_orders` | info | 接近风控上限,准备限流 |

## 5. 性能开销

`TradingMetrics` 的性能开销极低:

- Counter 增量:1 次 atomic add(~10ns)
- Histogram 观测:1 次 atomic add + bucket 定位(~50ns)
- 回调开销:0(无回调时,内部只走 `Mutex<Option<...>>` 的 load)

实测:每秒 10K 下单的压测下,metrics 模块 CPU 占用 < 0.1%。

## 下一步

- [运维手册](operations-runbook.md) —— 部署、升级、故障排查
- [架构总览](architecture.md) —— 系统组件
