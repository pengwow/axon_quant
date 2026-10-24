//! testnet 真网冒烟：`ExchangeGateway`（async adapter → sync gateway）+ `LiveSink` 全链路
//!
//! 默认 `#[ignore]`，只有显式 `--ignored` 才会发真网请求：
//!
//! ```bash
//! BINANCE_API_KEY=... BINANCE_API_SECRET=... \
//!   cargo test -p axon-worker --test testnet_smoke -- --ignored --nocapture
//! ```
//!
//! 覆盖：连接 → 订阅行情 → 远离市价的限价挂单 → 撤单 → 市价单成交 → 对账报告 →
//! 收尾（撤销未成交单 + disconnect）。
//!
//! # 两个必须在报告里说明的对账缺口
//!
//! - `axon-exchange` 的 Binance adapter 只订阅行情流，**没有 user data stream**，
//!   `executionReport` 不会到网关，`LiveSink::poll` 拿不到成交回报 —— 所以本测试的
//!   成交 / 撤单终态一律用签名 REST 查询确认（adapter 未暴露「按订单号查询」接口，
//!   这里用同一份配置直接发请求，不新增依赖）；
//! - 因此 OMS 影子账本里没有成交记录，对账报告的持仓 / 现金 break 是**预期**的
//!   （真实数据在交易所侧），报告只用于验证 `reconcile` 能吃真网快照。

use std::collections::{BTreeMap, BTreeSet};
use std::thread::sleep;
use std::time::{Duration, Instant};

use axon_core::market::Side as CoreSide;
use axon_core::types::{Instrument, Price, Quantity, SpotInstrument, Symbol};
use axon_core::{Event, OrderAction};
use axon_exchange::adapters::binance::BinanceAdapter;
use axon_exchange::types::{
    ExchangeConfig, ExchangeId, RateLimitConfig, ReconnectConfig, Symbol as ExchangeSymbol, Ticker,
};
use axon_exchange::{build_http_client, sign_binance};
use axon_kernel::{ExecutionSink, HostCtx, KernelConfig, KernelCore, OrderIntent, OrderSpec};
use axon_oms::OrderId as OmsOrderId;
use axon_worker::reconcile::{LocalSnapshot, ReconcileConfig, VenueSnapshot, reconcile};
use axon_worker::{ExchangeGateway, LiveSink};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde_json::Value;
use tokio::runtime::Runtime;

const SYMBOL: &str = "BTCUSDT";
const BASE: &str = "BTC";
const QUOTE: &str = "USDT";
/// 最小可成交量（BTCUSDT stepSize = 0.00001）
const SMOKE_QTY: f64 = 0.0001;
/// 市价单等待成交的上限
const FILL_TIMEOUT: Duration = Duration::from_secs(10);
/// 等 WS ticker 的上限
const TICKER_TIMEOUT: Duration = Duration::from_secs(15);

#[test]
#[ignore = "需要 testnet 网络与 BINANCE_API_KEY / BINANCE_API_SECRET"]
fn testnet_smoke_limit_cancel_market_reconcile() {
    let Ok(api_key) = std::env::var("BINANCE_API_KEY") else {
        panic!("缺少 BINANCE_API_KEY / BINANCE_API_SECRET，请先设置环境变量再跑冒烟测试");
    };
    let Ok(api_secret) = std::env::var("BINANCE_API_SECRET") else {
        panic!("缺少 BINANCE_API_KEY / BINANCE_API_SECRET，请先设置环境变量再跑冒烟测试");
    };
    assert!(
        !api_key.is_empty() && !api_secret.is_empty(),
        "BINANCE_API_KEY / BINANCE_API_SECRET 不能为空"
    );

    // connect 是 async 的，用独立运行时驱动；必须 multi-thread，否则 block_on 返回后
    // adapter 的 WS 监督任务不会被继续轮询。
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("构建冒烟测试运行时失败");
    let config = testnet_config(api_key, api_secret);

    // 构造网关必须在 runtime 之外（内部取走 adapter 的单消费者行情通道）。
    let mut gateway = ExchangeGateway::new(Box::new(BinanceAdapter::new(config.clone())))
        .expect("构造 ExchangeGateway 失败");
    runtime
        .block_on(gateway.connect())
        .expect("testnet 连接失败（检查 key / 网络 / proxy）");
    println!("[smoke] testnet 已连接");

    // ── 行情：WS ticker + M4 的 fetch_ohlcv ──────────────────────────────
    gateway
        .subscribe(&[ExchangeSymbol::new(SYMBOL)])
        .expect("订阅 BTCUSDT 行情失败");
    let ticker = wait_ticker(&gateway, TICKER_TIMEOUT);
    println!(
        "[smoke] {} 最新价 {}（bid {} / ask {}）",
        SYMBOL, ticker.last, ticker.bid, ticker.ask
    );
    let bars = gateway
        .fetch_ohlcv(&ExchangeSymbol::new(SYMBOL), "1m", 5)
        .expect("fetch_ohlcv 失败");
    assert_eq!(bars.len(), 5, "应返回 5 根 1m K 线");
    println!(
        "[smoke] 5 根 1m K 线，最后一根 close = {}",
        bars.last().expect("应有 K 线").close
    );

    // ── 影子账本起点：交易所真实 USDT 余额 ──────────────────────────────
    let opening_cash = account_balances(&runtime, &config)
        .get(QUOTE)
        .copied()
        .unwrap_or(Decimal::ZERO);
    println!("[smoke] 账户 {QUOTE} 余额 {opening_cash}");
    let mut sink = LiveSink::new(Box::new(gateway), opening_cash);
    let mut core = KernelCore::new(&KernelConfig {
        initial_cash: opening_cash.to_f64().unwrap_or(0.0),
        ..KernelConfig::default()
    });

    // ── 1) 远离市价的限价买单 → 应被接受 ────────────────────────────────
    let limit_price = limit_price_below(ticker.last);
    sink.submit(
        OrderIntent::Submit(OrderSpec {
            instrument: btc(),
            side: CoreSide::Buy,
            quantity: Quantity::from_f64(SMOKE_QTY),
            price: Some(Price::from_f64(limit_price.to_f64().expect("限价可转 f64"))),
        }),
        &mut core,
        &HostCtx::default(),
    )
    .expect("限价挂单应被 testnet 接受");
    let limit_id = latest_order_id(&sink);
    println!(
        "[smoke] 限价买单已受理 client=…{} 数量 {SMOKE_QTY} 价格 {limit_price}",
        tail6(&limit_id.to_string())
    );

    // ── 2) poll 若干次：远离市价不应产生成交 ───────────────────────────
    for round in 0..3 {
        let events = sink.poll().expect("poll 不应失败");
        assert!(events.is_empty(), "远离市价的限价单不应有成交回报");
        sleep(Duration::from_millis(200));
        println!("[smoke] poll 第 {} 次：0 条回报", round + 1);
    }
    let (status, executed) = order_status(&runtime, &config, &limit_id);
    assert_eq!(status, "NEW", "限价单应停在 NEW（实际 {status}）");
    assert_eq!(executed, Decimal::ZERO, "挂单不应有成交量");

    // ── 3) 撤单 → 确认终态 ─────────────────────────────────────────────
    // LiveSink 的内核订单号从 1 单调递增，此时只有这一笔挂单。
    let events = sink
        .submit(OrderIntent::Cancel(1), &mut core, &HostCtx::default())
        .expect("撤单应成功");
    assert!(
        matches!(
            events.as_slice(),
            [Event::Order(e)] if matches!(e.action, OrderAction::Cancelled(_))
        ),
        "撤单必须回流一条 Cancelled 事件: {events:?}"
    );
    let (status, executed) = order_status(&runtime, &config, &limit_id);
    assert_eq!(status, "CANCELED", "撤单后应终态 CANCELED（实际 {status}）");
    assert_eq!(executed, Decimal::ZERO);
    println!("[smoke] 限价单已撤销（CANCELED）");

    // ── 4) 市价买单 → 轮询直到 FILLED 或超时 ────────────────────────────
    sink.submit(
        OrderIntent::Submit(OrderSpec {
            instrument: btc(),
            side: CoreSide::Buy,
            quantity: Quantity::from_f64(SMOKE_QTY),
            price: None,
        }),
        &mut core,
        &HostCtx::default(),
    )
    .expect("市价单应被 testnet 接受");
    let market_id = latest_order_id(&sink);
    println!(
        "[smoke] 市价买单已受理 client=…{} 数量 {SMOKE_QTY}",
        tail6(&market_id.to_string())
    );

    let deadline = Instant::now() + FILL_TIMEOUT;
    let mut executed = Decimal::ZERO;
    let mut filled = false;
    while Instant::now() < deadline {
        // 网关 WS 通道：adapter 没有 user data stream，这里通常是 0 条回报。
        let _ = sink.poll().expect("poll 不应失败");
        let (status, qty) = order_status(&runtime, &config, &market_id);
        if status == "FILLED" {
            executed = qty;
            filled = true;
            break;
        }
        if matches!(status.as_str(), "CANCELED" | "REJECTED" | "EXPIRED") {
            panic!("市价单未成交就进入终态 {status}");
        }
        sleep(Duration::from_millis(500));
    }
    assert!(filled, "市价单 {}s 内未成交", FILL_TIMEOUT.as_secs());
    assert!(executed > Decimal::ZERO, "市价单成交量应为正");
    println!("[smoke] 市价单已成交 executedQty = {executed}");

    // ── 5) 对账：本地 OMS 影子账本 vs 交易所快照 ─────────────────────────
    let local = local_snapshot(&sink);
    let venue = venue_snapshot(&runtime, &config);
    let report = reconcile(&local, &venue, &ReconcileConfig::default());
    println!("[smoke] LocalSnapshot = {local:?}");
    println!("[smoke] VenueSnapshot = {venue:?}");
    println!("[smoke] ReconcileReport = {report:#?}");
    // 现货成交没有进 OMS（无 user data stream），交易所有持仓就必须被冻结。
    if venue.positions.values().any(|qty| *qty > 0.0) {
        let instrument = format!("{BASE}/{QUOTE}");
        assert!(
            report.frozen_instruments.contains(&instrument),
            "本地未记账的现货持仓必须进 frozen_instruments: {report:?}"
        );
    }

    // ── 6) 收尾：留一笔未成交挂单，验证 shutdown 会撤单 + disconnect ─────
    sink.submit(
        OrderIntent::Submit(OrderSpec {
            instrument: btc(),
            side: CoreSide::Buy,
            quantity: Quantity::from_f64(SMOKE_QTY),
            price: Some(Price::from_f64(limit_price.to_f64().expect("限价可转 f64"))),
        }),
        &mut core,
        &HostCtx::default(),
    )
    .expect("收尾前的挂单应被接受");
    let resting_id = latest_order_id(&sink);
    let shutdown = sink.shutdown();
    match &shutdown {
        Ok(()) => println!("[smoke] 收尾完成：未成交单已撤销，连接已关闭"),
        // 市价单在 OMS 里仍是 Acknowledged（没有 user data stream 回报），交易所侧
        // 早已 FILLED，撤它会返回「订单不存在」。这是已知缺口，不是收尾逻辑的错。
        Err(err) if err.contains(&market_id.to_string()) => {
            println!("[smoke] 收尾告警（已知缺口，预期）：已成交市价单在 OMS 里仍是活动单 → {err}");
        }
        Err(err) => panic!("收尾（撤单 + disconnect）失败: {err}"),
    }
    let (status, _) = order_status(&runtime, &config, &resting_id);
    assert_eq!(
        status, "CANCELED",
        "shutdown 必须撤掉未成交单（实际 {status}）"
    );
}

// ── 配置 ─────────────────────────────────────────────────────────────

fn testnet_config(api_key: String, api_secret: String) -> ExchangeConfig {
    ExchangeConfig {
        exchange_id: ExchangeId::Binance,
        api_key,
        api_secret,
        passphrase: None,
        testnet: true,
        rest_base_url: "https://testnet.binance.vision".into(),
        // 与 Python 侧 `binance_testnet_config()` 保持一致（市场流专用端点）。
        ws_url: "wss://stream.testnet.binance.vision/ws".into(),
        rate_limit: RateLimitConfig {
            requests_per_second: 10,
            orders_per_minute: 60,
            ws_messages_per_second: 50,
        },
        reconnect: ReconnectConfig {
            max_retries: 5,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(10),
            backoff_multiplier: 2.0,
            circuit_breaker_threshold: 5,
            circuit_breaker_reset: Duration::from_secs(60),
        },
        proxy: None,
        // 现货没有持仓端点：留空让 adapter 直接返回空 Vec，不发注定 404 的请求。
        position_endpoint: String::new(),
        fapi_base_url: None,
    }
}

// ── REST 查询（adapter 未暴露订单 / 账户查询，测试直接用同一份配置发签名请求）──

fn signed_get(runtime: &Runtime, config: &ExchangeConfig, path: &str, query: &str) -> Value {
    let client = build_http_client(config);
    let timestamp_ms = chrono::Utc::now().timestamp_millis();
    let signed = sign_binance::signed_query(query, &config.api_secret, timestamp_ms);
    let url = format!("{}{path}?{signed}", config.rest_base_url);
    let response = runtime
        .block_on(async {
            client
                .get(url)
                .header("X-MBX-APIKEY", config.api_key.as_str())
                .send()
                .await
        })
        .unwrap_or_else(|e| panic!("testnet REST {path} 请求失败: {e}"));
    let status = response.status();
    let body: Value = runtime
        .block_on(response.json::<Value>())
        .unwrap_or_else(|e| panic!("testnet REST {path} 响应不是 JSON: {e}"));
    if !status.is_success() {
        panic!(
            "testnet REST {path} 返回 {}: code={} msg={}",
            status.as_u16(),
            body["code"],
            body["msg"]
        );
    }
    body
}

/// 账户余额（asset → free + locked）
fn account_balances(runtime: &Runtime, config: &ExchangeConfig) -> BTreeMap<String, Decimal> {
    let body = signed_get(runtime, config, "/api/v3/account", "");
    let entries = body["balances"]
        .as_array()
        .unwrap_or_else(|| panic!("账户响应缺 balances 字段: {body}"));
    entries
        .iter()
        .filter_map(|entry| {
            let asset = entry["asset"].as_str()?;
            let free: Decimal = entry["free"].as_str()?.parse().ok()?;
            let locked: Decimal = entry["locked"].as_str()?.parse().ok()?;
            Some((asset.to_string(), free + locked))
        })
        .collect()
}

/// 按 client order id 查订单状态与累计成交量
fn order_status(
    runtime: &Runtime,
    config: &ExchangeConfig,
    client_order_id: &OmsOrderId,
) -> (String, Decimal) {
    let query = format!("symbol={SYMBOL}&origClientOrderId={client_order_id}");
    let body = signed_get(runtime, config, "/api/v3/order", &query);
    let status = body["status"].as_str().unwrap_or_default().to_string();
    let executed: Decimal = body["executedQty"]
        .as_str()
        .unwrap_or("0")
        .parse()
        .unwrap_or_default();
    (status, executed)
}

// ── 小工具 ───────────────────────────────────────────────────────────

fn wait_ticker(gateway: &ExchangeGateway, timeout: Duration) -> Ticker {
    let symbol = ExchangeSymbol::new(SYMBOL);
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(ticker) = gateway.ticker(&symbol)
            && ticker.last > Decimal::ZERO
        {
            return ticker;
        }
        if Instant::now() >= deadline {
            panic!(
                "{}s 内未收到 {} 的 WS ticker，检查 testnet WS 是否可达",
                timeout.as_secs(),
                SYMBOL
            );
        }
        sleep(Duration::from_millis(250));
    }
}

/// 现价的一半，按 BTCUSDT 的 tick size（0.01）取整
fn limit_price_below(last: Decimal) -> Decimal {
    (last / Decimal::from(2)).round_dp(2)
}

fn btc() -> Instrument {
    Instrument::Spot(SpotInstrument {
        base: Symbol::from(BASE),
        quote: Symbol::from(QUOTE),
    })
}

fn latest_order_id(sink: &LiveSink) -> OmsOrderId {
    sink.oms()
        .snapshot()
        .order_history
        .last()
        .expect("OMS 里应有刚提交的订单")
        .order
        .id
}

fn local_snapshot(sink: &LiveSink) -> LocalSnapshot {
    let balance = sink.oms().snapshot_balance();
    let positions = sink
        .oms()
        .snapshot_positions()
        .into_iter()
        .filter_map(|position| {
            let quantity = position.quantity.to_f64()?;
            (quantity.abs() > f64::EPSILON).then_some((position.symbol, quantity))
        })
        .collect();
    let fill_ids = sink
        .oms()
        .snapshot()
        .order_history
        .iter()
        .flat_map(|record| record.fills.iter().map(|fill| fill.fill_id.clone()))
        .collect();
    LocalSnapshot {
        positions,
        cash: balance
            .cash
            .get(QUOTE)
            .and_then(|v| v.to_f64())
            .unwrap_or(0.0),
        fill_ids,
    }
}

fn venue_snapshot(runtime: &Runtime, config: &ExchangeConfig) -> VenueSnapshot {
    let balances = account_balances(runtime, config);
    let mut positions = BTreeMap::new();
    // 现货没有持仓端点：用余额反推（free + locked 都已计入）。
    if let Some(quantity) = balances.get(BASE).and_then(|v| v.to_f64())
        && quantity > 0.0
    {
        positions.insert(format!("{BASE}/{QUOTE}"), quantity);
    }
    VenueSnapshot {
        positions,
        cash: balances.get(QUOTE).and_then(|v| v.to_f64()).unwrap_or(0.0),
        // adapter 没有 fetch_my_trades，交易所侧成交编号留空。
        fill_ids: BTreeSet::new(),
    }
}

fn tail6(id: &str) -> &str {
    &id[id.len().saturating_sub(6)..]
}
