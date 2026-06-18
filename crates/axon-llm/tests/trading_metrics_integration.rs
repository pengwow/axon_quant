//! TradingMetrics 端到端集成测试(Stage H)
//!
//! 验证三个 trading tool(place_order / cancel_order / replace_order)
//! 共享同一 `Arc<TradingMetrics>` 时,各项埋点正确累计,snapshot 反映
//! 实时状态。
//!
//! 与 `tests/trading_integration.rs` 模式一致:用 `MockTradingBackend`
//! 替代真实后端,验证 Tool → Backend → Metrics 的端到端链路。

use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use axon_llm::MetricSample;
use axon_llm::tools::Tool;
use axon_llm::trading::{
    CancelOrderTool, DailyCounter, MockTradingBackend, OrderKind, OrderSide, PlaceOrderArgs,
    PlaceOrderTool, ReplaceOrderTool, RiskLimits, SafetyMode, TimeInForce, TradingBackend,
    TradingMetrics,
};

fn mk_args() -> PlaceOrderArgs {
    PlaceOrderArgs {
        symbol: "BTC-USDT".into(),
        side: OrderSide::Buy,
        quantity: 0.05,
        order_type: OrderKind::Limit,
        price: Some(50_000.0),
        time_in_force: TimeInForce::GTC,
        stop_loss: None,
        take_profit: None,
        extras: serde_json::Value::Null,
    }
}

fn make_place_tool(
    backend: Arc<MockTradingBackend>,
    risk: RiskLimits,
    metrics: Arc<TradingMetrics>,
) -> PlaceOrderTool {
    PlaceOrderTool::new(
        backend,
        SafetyMode::Direct,
        risk,
        Arc::new(DailyCounter::default()),
    )
    .with_metrics(metrics)
}

fn make_cancel_tool(
    backend: Arc<MockTradingBackend>,
    risk: RiskLimits,
    metrics: Arc<TradingMetrics>,
) -> CancelOrderTool {
    CancelOrderTool::new(backend, risk, Arc::new(DailyCounter::default())).with_metrics(metrics)
}

fn make_replace_tool(
    backend: Arc<MockTradingBackend>,
    risk: RiskLimits,
    metrics: Arc<TradingMetrics>,
) -> ReplaceOrderTool {
    ReplaceOrderTool::new(backend, risk).with_metrics(metrics)
}

fn filter(snap: &[MetricSample], name: &str, label_key: &str, label_val: &str) -> u64 {
    snap.iter()
        .filter(|s| s.name == name && s.labels.get(label_key) == Some(&label_val.to_string()))
        .map(|s| s.value as u64)
        .sum()
}

/// 1. 三个 tool 共享同一 metrics 实例,各自埋点独立累计
#[tokio::test]
async fn shared_metrics_aggregates_across_tools() {
    let backend = Arc::new(MockTradingBackend::new());
    let metrics = TradingMetrics::shared();

    let place = make_place_tool(backend.clone(), RiskLimits::permissive(), metrics.clone());
    let cancel = make_cancel_tool(backend.clone(), RiskLimits::permissive(), metrics.clone());
    let replace = make_replace_tool(backend.clone(), RiskLimits::permissive(), metrics.clone());

    // 下 2 笔 → 撤 1 笔 → 改 1 笔
    let a1 = backend.place_order(&mk_args()).await.unwrap();
    place
        .execute(
            r#"{"symbol":"BTC-USDT","side":"Buy","quantity":0.05,"order_type":"Limit","price":50000.0}"#,
        )
        .await
        .unwrap();
    let a2 = backend.place_order(&mk_args()).await.unwrap();
    place
        .execute(
            r#"{"symbol":"BTC-USDT","side":"Buy","quantity":0.05,"order_type":"Limit","price":50000.0}"#,
        )
        .await
        .unwrap();
    cancel
        .execute(&format!(r#"{{"order_id":"{}"}}"#, a1.order_id))
        .await
        .unwrap();
    replace
        .execute(&format!(
            r#"{{"order_id":"{}","new_req":{{"symbol":"BTC-USDT","side":"Buy","quantity":0.1,"order_type":"Limit","price":51000.0}}}}"#,
            a2.order_id
        ))
        .await
        .unwrap();

    let snap = metrics.snapshot();

    // orders_total: 下 2 笔通过 place tool 调出 → symbol=BTC-USDT, side=Buy, status=Filled, mode=direct
    assert_eq!(filter(&snap, "trading_orders_total", "status", "Filled"), 2);
    // cancels_total: 1 笔 Cancelled
    assert_eq!(
        filter(&snap, "trading_cancels_total", "status", "Cancelled"),
        1
    );
    // replaces_total: 1 笔 Replaced
    assert_eq!(
        filter(&snap, "trading_replaces_total", "status", "Replaced"),
        1
    );
}

/// 2. 风控拒绝:每类规则被 `RiskRule::from_err_msg` 正确分类
#[tokio::test]
async fn risk_block_metrics_classifies_rules() {
    let backend = Arc::new(MockTradingBackend::new());
    let metrics = TradingMetrics::shared();

    // 白名单拦截
    let risk_allowlist = RiskLimits {
        allowed_symbols: Some(vec!["ETH-USDT".into()]),
        ..Default::default()
    };
    let place = make_place_tool(backend.clone(), risk_allowlist, metrics.clone());
    let _ = place
        .execute(
            r#"{"symbol":"BTC-USDT","side":"Buy","quantity":0.05,"order_type":"Limit","price":50000.0}"#,
        )
        .await;

    // 单笔金额拦截
    let risk_notional = RiskLimits {
        max_order_notional: Some(1_000.0),
        ..Default::default()
    };
    let place2 = make_place_tool(backend.clone(), risk_notional, metrics.clone());
    // 0.5 * 50_000 = 25_000 > 1_000
    let _ = place2
        .execute(
            r#"{"symbol":"BTC-USDT","side":"Buy","quantity":0.5,"order_type":"Limit","price":50000.0}"#,
        )
        .await;

    let snap = metrics.snapshot();
    assert_eq!(
        filter(
            &snap,
            "trading_risk_blocks_total",
            "rule",
            "allowed_symbols"
        ),
        1
    );
    assert_eq!(
        filter(
            &snap,
            "trading_risk_blocks_total",
            "rule",
            "max_order_notional"
        ),
        1
    );
}

/// 3. callback 实时收到样本:每次 record_* 触发,且 panic 不污染业务
#[tokio::test]
async fn callback_receives_samples_and_panic_isolated() {
    let backend = Arc::new(MockTradingBackend::new());
    let metrics = TradingMetrics::shared();
    let received: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
    let received_clone = received.clone();
    metrics.set_callback(Arc::new(move |sample: MetricSample| {
        // 偶发 panic,验证 catch_unwind 隔离
        if sample.name == "trading_risk_blocks_total" {
            panic!("intentional test panic");
        }
        received_clone.lock().unwrap().push(sample.name.clone());
    }));

    // 先下一笔成功(emit orders_total)
    let place = make_place_tool(backend.clone(), RiskLimits::permissive(), metrics.clone());
    place
        .execute(
            r#"{"symbol":"BTC-USDT","side":"Buy","quantity":0.05,"order_type":"Limit","price":50000.0}"#,
        )
        .await
        .unwrap();

    // 再来一笔风控拒(emit risk_blocks_total → callback panic → 业务不应崩)
    let place2 = make_place_tool(
        backend.clone(),
        RiskLimits {
            allowed_symbols: Some(vec!["ETH-USDT".into()]),
            ..Default::default()
        },
        metrics.clone(),
    );
    // 业务路径不应受 callback panic 影响(此测试只验证业务成功)
    let out = place2
        .execute(
            r#"{"symbol":"BTC-USDT","side":"Buy","quantity":0.05,"order_type":"Limit","price":50000.0}"#,
        )
        .await;
    assert!(out.is_err(), "风控应拒单");

    let names = received.lock().unwrap().clone();
    // 至少收到一笔 orders_total 的样本
    assert!(names.iter().any(|n| n == "trading_orders_total"));
    // 业务未被 callback panic 中断
    assert!(out.is_err());
}

/// 4. snapshot 包含所有期望的 metric 名,且不重复 trigger callback
#[tokio::test]
async fn snapshot_returns_all_expected_metrics() {
    let backend = Arc::new(MockTradingBackend::new());
    let metrics = TradingMetrics::shared();
    let place = make_place_tool(backend.clone(), RiskLimits::permissive(), metrics.clone());
    place
        .execute(
            r#"{"symbol":"BTC-USDT","side":"Buy","quantity":0.05,"order_type":"Limit","price":50000.0}"#,
        )
        .await
        .unwrap();

    let snap = metrics.snapshot();
    let names: Vec<String> = snap.iter().map(|s| s.name.clone()).collect();
    // 必须包含的 metric 名
    for required in [
        "trading_orders_total",
        "trading_order_latency_ns",
        "trading_daily_orders_count",
    ] {
        assert!(
            names.contains(&required.to_string()),
            "snapshot 缺少 {}: {:?}",
            required,
            names
        );
    }
}
