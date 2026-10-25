//! testnet 真网冒烟：Binance **USDⓈ-M 合约 testnet**（`testnet.binancefuture.com`）
//!
//! `ExchangeGateway`（async adapter → sync gateway）+ `LiveSink` 全链路。
//!
//! 默认 `#[ignore]`，只有显式 `--ignored` 才会发真网请求：
//!
//! ```bash
//! BINANCE_API_KEY=... BINANCE_API_SECRET=... \
//!   cargo test -p axon-worker --test testnet_smoke -- --ignored --nocapture
//! ```
//!
//! 凭证必须是**合约 testnet** 的 key（现货 `testnet.binance.vision` 会返回 -2015）。
//!
//! 覆盖：连接（`/fapi/v1/ping`）→ 订阅行情 → 把账户切成单向持仓 → 标记价 97% 的
//! 限价挂单 → 撤单 → 市价单成交 → 持仓 / 账户对账 → 收尾（撤销未成交单 +
//! disconnect）→ 平掉冒烟开的仓。
//!
//! # 与现货版的差异
//!
//! - 数量 0.001 BTCUSDT（合约 testnet 的 LOT_SIZE 是 0.0001 步长，MIN_NOTIONAL 50 USDT）；
//! - 限价单不能再用「现价 × 0.5」：实测会 `-4164 Order's notional must be no smaller
//!   than 50`（0.001 × 半价 ≈ 40 USDT），另外合约还有 `PERCENT_PRICE` 0.95~1.05 的
//!   限制，所以改用标记价 × 0.97（远离盘口、又能过 notional）；
//! - 合约账户默认可能处于**双向持仓**（`dualSidePosition=true`），此时下单必须带
//!   `positionSide`，否则 `-4061`。adapter 不传 `positionSide`（单向模式约定），
//!   所以本测试先把账户切成单向持仓，收尾再平仓，保证可重复运行；
//! - 对账的持仓来自 `/fapi/v2/positionRisk` 的 `positionAmt`；现金用
//!   `/fapi/v2/account` 的 **`totalWalletBalance`**（钱包余额）。
//!
//! # 合约钱包口径（`AccountingMode::Wallet`）
//!
//! 合约开仓不扣名义价值、只占用保证金，钱包余额只受**手续费**与**已实现盈亏**
//! 影响。因此影子账本必须切到 [`axon_oms::AccountingMode::Wallet`]，现金起点与
//! 对账基准都用 `totalWalletBalance`；若沿用现货口径（`Cash`），本地会多扣一整笔
//! 名义价值（实测约 75 USDT），现金对账必然偏差。
//!
//! # 成交 / 撤单终态只认 OMS 账本（即 user data stream 链路）
//!
//! 本测试的关键断言全部落在 `LiveSink` 的 OMS 上：市价单 / 撤单的终态必须是
//! **WS 回报驱动**出来的，而不是测试自己用 REST 兜出来的。REST 查询只做交叉
//! 校验（交易所侧确实成交 / 确实撤销）。
//!
//! 现金对账要求**归零**：容差只留 f64 往返的相对误差（`1e-6 × max(1, |cash|)`），
//! 不按保证金 / 名义价值放宽。若仍有差，把 `totalWalletBalance` /
//! `totalUnrealizedProfit` / `availableBalance` 与 OMS cash 一起打印出来。

use std::collections::{BTreeMap, BTreeSet};
use std::thread::sleep;
use std::time::{Duration, Instant};

use axon_core::market::Side as CoreSide;
use axon_core::types::{Instrument, Price, Quantity, SpotInstrument, Symbol};
use axon_core::{Event, OrderAction};
use axon_exchange::ExchangeAdapter;
use axon_exchange::adapters::binance::BinanceAdapter;
use axon_exchange::types::{
    ExchangeConfig, ExchangeId, RateLimitConfig, ReconnectConfig, Symbol as ExchangeSymbol, Ticker,
};
use axon_exchange::{build_http_client, sign_binance};
use axon_kernel::{ExecutionSink, HostCtx, KernelConfig, KernelCore, OrderIntent, OrderSpec};
use axon_oms::{AccountingMode, OrderId as OmsOrderId, OrderStatus as OmsOrderStatus};
use axon_worker::reconcile::{LocalSnapshot, ReconcileConfig, VenueSnapshot, reconcile};
use axon_worker::{ExchangeGateway, LiveSink};
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use serde_json::Value;
use tokio::runtime::Runtime;

const SYMBOL: &str = "BTCUSDT";
const BASE: &str = "BTC";
const QUOTE: &str = "USDT";
/// 冒烟数量：0.001 BTCUSDT（合约 stepSize 0.0001，币价 ≈ 8 万时 notional 远高于 50 USDT）
const SMOKE_QTY: f64 = 0.001;
/// 限价单挂在标记价的 97%：合约 PERCENT_PRICE 只允许 0.95~1.05 倍，
/// 而 0.5 倍会先撞 MIN_NOTIONAL（实测 -4164）
const LIMIT_PRICE_RATIO: f64 = 0.97;
/// 市价单 / 平仓单等待成交的上限
const FILL_TIMEOUT: Duration = Duration::from_secs(15);
/// 等 user data stream 回报（成交 / 撤单终态）的上限
const STREAM_TIMEOUT: Duration = Duration::from_secs(10);
/// 等 WS ticker 的上限
const TICKER_TIMEOUT: Duration = Duration::from_secs(20);
/// 合约 BTCUSDT 的 tickSize 是 0.10
const PRICE_DP: u32 = 1;

#[test]
#[ignore = "需要合约 testnet 网络与 BINANCE_API_KEY / BINANCE_API_SECRET"]
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

    // ── 账户前置：清遗留挂单 → 单向持仓 → 空仓 ──────────────────────────
    cancel_stale_open_orders(&runtime, &config);
    ensure_one_way_mode(&runtime, &config);
    let leftover = position_amount(&runtime, &config);
    if leftover != Decimal::ZERO {
        println!("[smoke] 发现遗留持仓 {leftover}，先平掉再跑");
        flatten_position(&runtime, &config);
    }

    // 构造网关必须在 runtime 之外（内部取走 adapter 的单消费者行情通道）。
    let mut gateway = ExchangeGateway::new(Box::new(BinanceAdapter::new(config.clone())))
        .expect("构造 ExchangeGateway 失败");
    // connect 里的 REST ping 也可能被 testnet 丢连接，重试几次（失败时 WS 还没起，重试安全）
    let mut connected = false;
    let mut connect_err = String::new();
    for attempt in 1..=REST_ATTEMPTS {
        match runtime.block_on(gateway.connect()) {
            Ok(()) => {
                connected = true;
                break;
            }
            Err(err) => {
                connect_err = err;
                println!("[smoke] 连接第 {attempt} 次失败，重试: {connect_err}");
                sleep(Duration::from_millis(500 * attempt as u64));
            }
        }
    }
    assert!(
        connected,
        "合约 testnet 连接失败（检查 key / 网络 / proxy）: {connect_err}"
    );
    println!("[smoke] 合约 testnet 已连接（REST ping /fapi/v1/ping）");

    // ── 行情：WS ticker + 合约 klines ────────────────────────────────────
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

    // ── 影子账本起点：交易所 USDT 钱包余额（totalWalletBalance）────────────
    // 合约钱包口径：开仓不扣名义价值，钱包余额只受手续费 / 已实现盈亏影响。
    let opening = account_balances(&runtime, &config);
    let opening_cash = opening.wallet;
    // fills 对账窗口起点：只覆盖本 run 的成交。前置的平遗留仓（会产生交易所
    // 成交但本地 OMS 看不到）已被排除在窗口外；收尾挂单不会成交、平仓发生在
    // 对账之后，均不污染窗口。
    let trade_window_start = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时钟不应早于 1970")
        .as_millis() as i64;
    println!(
        "[smoke] 账户 totalWalletBalance {} / totalUnrealizedProfit {} / availableBalance {} {QUOTE}",
        opening.wallet, opening.unrealized, opening.available
    );
    assert!(
        opening_cash > Decimal::ZERO,
        "合约 testnet 账户没有可用保证金，先领测试币"
    );
    let mut sink =
        LiveSink::new(Box::new(gateway), opening_cash).with_accounting_mode(AccountingMode::Wallet);
    let mut core = KernelCore::new(&KernelConfig {
        initial_cash: opening_cash.to_f64().unwrap_or(0.0),
        ..KernelConfig::default()
    });

    // ── 1) 标记价 97% 的限价买单 → 应被接受且不成交 ────────────────────
    let mark = mark_price(&runtime, &config);
    let limit_price = limit_price_below(mark);
    assert!(
        limit_price < ticker.last,
        "限价必须低于盘口才不会立即成交（limit {limit_price} / last {}）",
        ticker.last
    );
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
    .expect("限价挂单应被合约 testnet 接受");
    let limit_id = latest_order_id(&sink);
    println!(
        "[smoke] 限价买单已受理 client=…{} 数量 {SMOKE_QTY} 价格 {limit_price}",
        tail6(&limit_id.to_string())
    );

    // ── 2) poll 若干次：远离盘口不应产生成交 ───────────────────────────
    for round in 0..3 {
        let events = sink.poll().expect("poll 不应失败");
        assert!(events.is_empty(), "远离盘口的限价单不应有成交回报");
        sleep(Duration::from_millis(200));
        println!("[smoke] poll 第 {} 次：0 条回报", round + 1);
    }
    let (status, executed) = order_status(&runtime, &config, &limit_id);
    assert_eq!(status, "NEW", "限价单应停在 NEW（实际 {status}）");
    assert_eq!(executed, Decimal::ZERO, "挂单不应有成交量");

    // ── 3) 撤单：REST 撤单，终态必须由 user data stream 回报驱动 OMS ────
    signed_delete(
        &runtime,
        &config,
        "/fapi/v1/order",
        &format!("symbol={SYMBOL}&origClientOrderId={limit_id}"),
    );
    let (cancel_events, cancel_status) = wait_oms_terminal(&mut sink, &limit_id, STREAM_TIMEOUT);
    assert!(
        matches!(
            cancel_status,
            Some(OmsOrderStatus::Cancelled { filled_qty }) if filled_qty == Decimal::ZERO
        ),
        "OMS 未在 {}s 内收到 Cancelled 回报（当前 {:?}）",
        STREAM_TIMEOUT.as_secs(),
        cancel_status
    );
    assert!(
        cancel_events.iter().any(
            |e| matches!(e, Event::Order(oe) if matches!(oe.action, OrderAction::Cancelled(_)))
        ),
        "撤单回报必须回流内核事件: {cancel_events:?}"
    );
    let (status, executed) = order_status(&runtime, &config, &limit_id);
    assert_eq!(
        status, "CANCELED",
        "交易所侧应终态 CANCELED（实际 {status}）"
    );
    assert_eq!(executed, Decimal::ZERO);
    println!("[smoke] 限价单已撤销，OMS 收到 Cancelled（WS 回报）");

    // ── 4) 市价买单 → 轮询 LiveSink 直到 OMS 收到 Filled ─────────────────
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
    .expect("市价单应被合约 testnet 接受");
    let market_id = latest_order_id(&sink);
    println!(
        "[smoke] 市价买单已受理 client=…{} 数量 {SMOKE_QTY}",
        tail6(&market_id.to_string())
    );

    let (market_events, market_status) = wait_oms_terminal(&mut sink, &market_id, STREAM_TIMEOUT);
    let oms_filled = matches!(
        market_status,
        Some(OmsOrderStatus::Filled {
            filled_qty,
            ..
        }) if filled_qty == Decimal::from_f64(SMOKE_QTY).expect("SMOKE_QTY 可转 Decimal")
    );
    assert!(
        oms_filled,
        "OMS 未在 {}s 内收到该市价单的 Filled（当前 {:?}）",
        STREAM_TIMEOUT.as_secs(),
        market_status
    );
    assert!(
        market_events.iter().any(|e| matches!(e, Event::Fill(_))),
        "成交回报必须回流 Fill 事件: {market_events:?}"
    );
    if let Some(OmsOrderStatus::Filled {
        filled_qty,
        avg_price,
    }) = market_status
    {
        println!("[smoke] OMS 收到 Filled：成交量 {filled_qty} 均价 {avg_price}");
    }

    // REST 交叉校验：交易所侧确实成交（不是 OMS 凭空记的）
    let (venue_status, executed) = order_status(&runtime, &config, &market_id);
    assert_eq!(
        venue_status, "FILLED",
        "交易所侧应 FILLED（实际 {venue_status}）"
    );
    assert!(executed > Decimal::ZERO, "市价单成交量应为正");
    println!("[smoke] 交易所侧 executedQty = {executed}");

    // 合约特有：成交必须体现在 positionRisk 上（现货是查余额，合约是查持仓）
    let position = position_amount(&runtime, &config);
    assert!(
        position > Decimal::ZERO,
        "市价买入后应有正持仓，实际 {position}"
    );
    assert_eq!(
        position, executed,
        "positionRisk 的 positionAmt 应等于成交量"
    );
    println!("[smoke] positionRisk 持仓 positionAmt = {position}");

    // ── 5) 对账：本地 OMS 影子账本 vs 交易所快照 ─────────────────────────
    let local = local_snapshot(&sink);
    let venue_balances = account_balances(&runtime, &config);
    let venue = venue_snapshot(&runtime, &config, &venue_balances, trade_window_start);
    let report = reconcile(&local, &venue, &ReconcileConfig::default());
    println!("[smoke] LocalSnapshot = {local:?}");
    println!("[smoke] VenueSnapshot = {venue:?}");
    println!("[smoke] ReconcileReport = {report:#?}");
    // 成交已由 user data stream 落进 OMS：持仓对账必须干净，不放宽容差。
    assert!(
        report.position_breaks.is_empty(),
        "OMS 持仓与 positionRisk 不一致: {report:#?}"
    );
    assert!(
        report.frozen_instruments.is_empty(),
        "持仓干净时不应冻结任何品种: {report:#?}"
    );
    // 现金必须归零：容差只留 f64 往返的相对误差（1e-6 × max(1, |cash|)），
    // 不按保证金 / 名义价值放宽。
    let cash_diff = report.cash_break.as_ref().map_or(0.0, |b| b.diff);
    let cash_tolerance = 1e-6 * local.cash.abs().max(1.0);
    if let Some(cash_break) = &report.cash_break {
        println!(
            "[smoke] 现金差异 local {} / venue {} / diff {}（容差 {cash_tolerance}）",
            cash_break.local, cash_break.venue, cash_break.diff
        );
        println!(
            "[smoke] 交易所账户 totalWalletBalance {} / totalUnrealizedProfit {} / availableBalance {}",
            venue_balances.wallet, venue_balances.unrealized, venue_balances.available
        );
    } else {
        println!(
            "[smoke] 现金对账归零：local {} / venue {}",
            local.cash, venue.cash
        );
    }
    assert!(
        cash_diff.abs() <= cash_tolerance,
        "现金对账未归零: diff {cash_diff} > 容差 {cash_tolerance}（local {} / venue {}）",
        local.cash,
        venue.cash
    );
    // 三路对账的最后一路：fills 交叉比对。两侧同源于撮合成交号
    // （WS `o.t` ⇄ REST userTrades `id`），任何漏记 / 多记都会在这里爆出。
    assert!(
        report.missing_fills.is_empty() && report.extra_fills.is_empty(),
        "fills 对账未闭合: missing={:?} extra={:?}",
        report.missing_fills,
        report.extra_fills
    );

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
    // 市价单已由回报落为 Filled（终态），shutdown 只撤未完成订单，不会再撞 -2011。
    sink.shutdown().expect("shutdown 必须成功");
    println!("[smoke] 收尾完成：未成交单已撤销，连接已关闭");
    let (status, _) = order_status(&runtime, &config, &resting_id);
    assert_eq!(
        status, "CANCELED",
        "shutdown 必须撤掉未成交单（实际 {status}）"
    );
    // shutdown 之后交易所侧不应残留任何活动单
    let residual = open_orders(&runtime, &config);
    assert!(residual.is_empty(), "shutdown 后仍残留活动单: {residual:?}");
    println!("[smoke] 交易所侧活动单数 = 0");

    // ── 7) 平掉冒烟开的仓，恢复空仓（下一次运行才能继续切持仓模式）────────
    flatten_position(&runtime, &config);
    let final_position = position_amount(&runtime, &config);
    assert_eq!(
        final_position,
        Decimal::ZERO,
        "冒烟结束应恢复空仓，实际 {final_position}"
    );
    println!("[smoke] 已平仓，账户恢复空仓");
}

// ── 配置 ─────────────────────────────────────────────────────────────

fn testnet_config(api_key: String, api_secret: String) -> ExchangeConfig {
    ExchangeConfig {
        exchange_id: ExchangeId::Binance,
        api_key,
        api_secret,
        passphrase: None,
        testnet: true,
        // 合约 testnet 的 REST 域同时提供 /fapi/* 与 /api/v3/*（spot 域是
        // testnet.binance.vision，合约 key 在那边过不了鉴权）
        rest_base_url: "https://testnet.binancefuture.com".into(),
        // 实测 `wss://testnet.binancefuture.com/ws` 不接受握手，合约 testnet 的
        // 官方行情流是 fstream 域
        ws_url: "wss://fstream.binancefuture.com/ws".into(),
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
        position_endpoint: "/fapi/v2/positionRisk".into(),
        fapi_base_url: Some("https://testnet.binancefuture.com".into()),
    }
}

// ── REST 查询（adapter 未暴露订单 / 账户查询，测试直接用同一份配置发签名请求）──

/// 合约 testnet（CloudFront）偶发丢连接 / 握手超时，测试侧对**传输层**错误重试；
/// 交易所返回的业务错误（非 2xx）不重试 —— 那才是要观察的结果。
const REST_ATTEMPTS: usize = 4;

/// 签名 query 统一带上 recvWindow：testnet 偶发时钟抖动会报 -1021
fn signed_with_window(params: &str) -> String {
    if params.is_empty() {
        "recvWindow=60000".into()
    } else {
        format!("{params}&recvWindow=60000")
    }
}

/// 发一次请求，返回 (HTTP 状态, JSON body)；传输层错误返回 `Err(错误描述)`
fn try_send(
    runtime: &Runtime,
    config: &ExchangeConfig,
    method: &str,
    url: String,
    api_key: Option<&str>,
) -> Result<(u16, Value), String> {
    let client = build_http_client(config);
    // 请求（含 reqwest 的超时 timer）必须在 runtime 上下文里构造：`send()` 的
    // future 会 eager 地创建 `tokio::time::Sleep`，在 block_on 外面构造会 panic。
    let response = runtime
        .block_on(async move {
            let mut request = match method {
                "POST" => client.post(&url),
                "DELETE" => client.delete(&url),
                _ => client.get(&url),
            };
            if let Some(key) = api_key {
                request = request.header("X-MBX-APIKEY", key);
            }
            request.send().await
        })
        .map_err(|e| format!("{e}"))?;
    let status = response.status().as_u16();
    let body = runtime
        .block_on(response.json::<Value>())
        .map_err(|e| format!("响应不是 JSON: {e}"))?;
    Ok((status, body))
}

fn check_status(path: &str, status: u16, body: &Value) -> Value {
    if !(200..300).contains(&status) {
        panic!(
            "testnet REST {path} 返回 {status}: code={} msg={}",
            body["code"], body["msg"]
        );
    }
    body.clone()
}

/// 签名请求；每次重试都重新生成 timestamp 与签名
fn signed_request(
    runtime: &Runtime,
    config: &ExchangeConfig,
    path: &str,
    params: &str,
    method: &str,
) -> Value {
    let mut last_err = String::new();
    for attempt in 1..=REST_ATTEMPTS {
        let timestamp_ms = chrono::Utc::now().timestamp_millis();
        let signed = sign_binance::signed_query(
            &signed_with_window(params),
            &config.api_secret,
            timestamp_ms,
        );
        let url = format!("{}{path}?{signed}", config.rest_base_url);
        match try_send(runtime, config, method, url, Some(&config.api_key)) {
            Ok((status, body)) => return check_status(path, status, &body),
            Err(err) => {
                last_err = err;
                println!("[smoke] {path} 第 {attempt} 次传输失败，重试: {last_err}");
                sleep(Duration::from_millis(500 * attempt as u64));
            }
        }
    }
    panic!("testnet REST {path} {REST_ATTEMPTS} 次传输均失败: {last_err}");
}

fn signed_get(runtime: &Runtime, config: &ExchangeConfig, path: &str, query: &str) -> Value {
    signed_request(runtime, config, path, query, "GET")
}

fn signed_post(runtime: &Runtime, config: &ExchangeConfig, path: &str, params: &str) -> Value {
    signed_request(runtime, config, path, params, "POST")
}

fn signed_delete(runtime: &Runtime, config: &ExchangeConfig, path: &str, params: &str) -> Value {
    signed_request(runtime, config, path, params, "DELETE")
}

/// 公开 GET（无签名），用于标记价
fn public_get(runtime: &Runtime, config: &ExchangeConfig, path: &str, query: &str) -> Value {
    let url = format!("{}{path}?{query}", config.rest_base_url);
    let mut last_err = String::new();
    for attempt in 1..=REST_ATTEMPTS {
        match try_send(runtime, config, "GET", url.clone(), None) {
            Ok((status, body)) => return check_status(path, status, &body),
            Err(err) => {
                last_err = err;
                println!("[smoke] {path} 第 {attempt} 次传输失败，重试: {last_err}");
                sleep(Duration::from_millis(500 * attempt as u64));
            }
        }
    }
    panic!("testnet REST {path} {REST_ATTEMPTS} 次传输均失败: {last_err}");
}

/// `/fapi/v2/account` 中与现金对账相关的三项 USDT 余额
#[derive(Debug, Clone, Copy)]
struct AccountBalances {
    /// 钱包余额：只受手续费 / 已实现盈亏 / 划转影响（不含未实现盈亏）
    wallet: Decimal,
    /// 未实现盈亏
    unrealized: Decimal,
    /// 可用保证金（已扣初始保证金与手续费）
    available: Decimal,
}

fn account_balances(runtime: &Runtime, config: &ExchangeConfig) -> AccountBalances {
    let body = signed_get(runtime, config, "/fapi/v2/account", "");
    let field = |key: &str| -> Decimal {
        body[key]
            .as_str()
            .unwrap_or_else(|| panic!("账户响应缺 {key}: {body}"))
            .parse()
            .expect("账户余额不是合法小数")
    };
    AccountBalances {
        wallet: field("totalWalletBalance"),
        unrealized: field("totalUnrealizedProfit"),
        available: field("availableBalance"),
    }
}

/// 标记价（公开端点，限价单要避开盘口又要过 PERCENT_PRICE）
fn mark_price(runtime: &Runtime, config: &ExchangeConfig) -> Decimal {
    let body = public_get(
        runtime,
        config,
        "/fapi/v1/premiumIndex",
        &format!("symbol={SYMBOL}"),
    );
    body["markPrice"]
        .as_str()
        .unwrap_or_else(|| panic!("premiumIndex 缺 markPrice: {body}"))
        .parse()
        .expect("markPrice 不是合法小数")
}

/// 当前持仓数量（`positionRisk` 的 `positionAmt`，净持仓口径）
fn position_amount(runtime: &Runtime, config: &ExchangeConfig) -> Decimal {
    let body = signed_get(
        runtime,
        config,
        "/fapi/v2/positionRisk",
        &format!("symbol={SYMBOL}"),
    );
    body.as_array()
        .unwrap_or_else(|| panic!("positionRisk 响应不是数组: {body}"))
        .iter()
        .find(|entry| entry["symbol"].as_str() == Some(SYMBOL))
        .and_then(|entry| entry["positionAmt"].as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or_default()
}

/// 交易所侧活动单（`/fapi/v1/openOrders`），shutdown 后必须为空
fn open_orders(runtime: &Runtime, config: &ExchangeConfig) -> Vec<Value> {
    let body = signed_get(
        runtime,
        config,
        "/fapi/v1/openOrders",
        &format!("symbol={SYMBOL}"),
    );
    body.as_array()
        .unwrap_or_else(|| panic!("openOrders 响应不是数组: {body}"))
        .to_vec()
}

/// 撤销交易所侧所有遗留挂单
///
/// 上一次运行中途崩溃可能留下挂单，会让「shutdown 后无活动单」的断言误报。
fn cancel_stale_open_orders(runtime: &Runtime, config: &ExchangeConfig) {
    let stale = open_orders(runtime, config);
    if stale.is_empty() {
        return;
    }
    println!("[smoke] 发现 {} 笔遗留挂单，先撤销", stale.len());
    for order in stale {
        let Some(order_id) = order["orderId"].as_i64() else {
            continue;
        };
        let client_order_id = order["clientOrderId"].as_str().unwrap_or_default();
        signed_delete(
            runtime,
            config,
            "/fapi/v1/order",
            &format!("symbol={SYMBOL}&orderId={order_id}"),
        );
        println!(
            "[smoke] 已撤销遗留挂单 client=…{} 数量 {}",
            tail6(client_order_id),
            order["origQty"]
        );
    }
    assert!(open_orders(runtime, config).is_empty(), "遗留挂单未清干净");
}

/// 把账户切成单向持仓（adapter 下单不带 positionSide）
///
/// 已经是单向时 Binance 返回 -4059 `No need to change position side`，视为成功。
fn ensure_one_way_mode(runtime: &Runtime, config: &ExchangeConfig) {
    let dual = signed_get(runtime, config, "/fapi/v1/positionSide/dual", "");
    if dual["dualSidePosition"].as_bool() == Some(false) {
        println!("[smoke] 账户已是单向持仓模式");
        return;
    }
    let body = signed_post(
        runtime,
        config,
        "/fapi/v1/positionSide/dual",
        "dualSidePosition=false",
    );
    // signed_post 对非 2xx 直接 panic，这里只可能是 200 / -4059 两条路
    println!("[smoke] 持仓模式已切换为单向: {body}");
}

/// 用市价单把净持仓平掉（开多则卖出、开空则买入），并等到持仓归零
fn flatten_position(runtime: &Runtime, config: &ExchangeConfig) {
    let position = position_amount(runtime, config);
    if position == Decimal::ZERO {
        return;
    }
    let (side, quantity) = if position > Decimal::ZERO {
        ("SELL", position)
    } else {
        ("BUY", -position)
    };
    let body = signed_post(
        runtime,
        config,
        "/fapi/v1/order",
        &format!("symbol={SYMBOL}&side={side}&type=MARKET&quantity={quantity}"),
    );
    let order_id = body["orderId"].as_i64().unwrap_or_default().to_string();
    println!(
        "[smoke] 平仓市价单已受理 orderId=…{} 数量 {quantity}",
        tail6(&order_id)
    );

    let deadline = Instant::now() + FILL_TIMEOUT;
    while Instant::now() < deadline {
        if position_amount(runtime, config) == Decimal::ZERO {
            return;
        }
        sleep(Duration::from_millis(300));
    }
    panic!("平仓后 {}s 内持仓未归零", FILL_TIMEOUT.as_secs());
}

/// 按 client order id 查订单状态与累计成交量（合约 `/fapi/v1/order`）
fn order_status(
    runtime: &Runtime,
    config: &ExchangeConfig,
    client_order_id: &OmsOrderId,
) -> (String, Decimal) {
    let query = format!("symbol={SYMBOL}&origClientOrderId={client_order_id}");
    let body = signed_get(runtime, config, "/fapi/v1/order", &query);
    let status = body["status"].as_str().unwrap_or_default().to_string();
    let executed: Decimal = body["executedQty"]
        .as_str()
        .unwrap_or("0")
        .parse()
        .unwrap_or_default();
    (status, executed)
}

// ── 小工具 ───────────────────────────────────────────────────────────

/// 轮询 `LiveSink`，直到 OMS 里该订单进入终态（或超时）
///
/// 只靠 `sink.poll()` 推动：终态必须由交易所回报驱动，测试不自己改 OMS 状态。
fn wait_oms_terminal(
    sink: &mut LiveSink,
    id: &OmsOrderId,
    timeout: Duration,
) -> (Vec<Event>, Option<OmsOrderStatus>) {
    let deadline = Instant::now() + timeout;
    let mut events = Vec::new();
    loop {
        events.extend(sink.poll().expect("poll 不应失败"));
        let status = oms_order_status(sink, id);
        if status.as_ref().is_some_and(|s| s.is_terminal()) || Instant::now() >= deadline {
            return (events, status);
        }
        sleep(Duration::from_millis(200));
    }
}

/// 从 OMS 订单历史里读某笔订单的当前状态
fn oms_order_status(sink: &LiveSink, id: &OmsOrderId) -> Option<OmsOrderStatus> {
    sink.oms()
        .snapshot()
        .order_history
        .iter()
        .rev()
        .find(|record| record.order.id == *id)
        .map(|record| record.order.status.clone())
}

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
                "{}s 内未收到 {} 的 WS ticker，检查合约 testnet WS 是否可达",
                timeout.as_secs(),
                SYMBOL
            );
        }
        sleep(Duration::from_millis(250));
    }
}

/// 标记价的 97%，按合约 tickSize（0.10）取整
fn limit_price_below(mark: Decimal) -> Decimal {
    let ratio = Decimal::from_f64(LIMIT_PRICE_RATIO).expect("ratio 可转 Decimal");
    (mark * ratio).round_dp(PRICE_DP)
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

fn venue_snapshot(
    runtime: &Runtime,
    config: &ExchangeConfig,
    balances: &AccountBalances,
    fills_since_ms: i64,
) -> VenueSnapshot {
    let mut positions = BTreeMap::new();
    // 合约持仓看 positionRisk；positionAmt 为净持仓，空头是负数
    let quantity = position_amount(runtime, config);
    if let Some(quantity) = quantity.to_f64()
        && quantity.abs() > f64::EPSILON
    {
        positions.insert(format!("{BASE}/{QUOTE}"), quantity);
    }
    // 交易所侧成交：REST /fapi/v1/userTrades，与 user stream 互为独立数据源。
    // trade_id 是 adapter 归一号（binfut:{symbol}:{id}），与 WS 回报同源，
    // 独立 adapter 实例即可查询（签名端点不依赖 connect）。
    let trade_adapter = BinanceAdapter::new(config.clone());
    let venue_fills = runtime
        .block_on(trade_adapter.fetch_my_trades(
            &ExchangeSymbol::new(SYMBOL),
            Some(fills_since_ms),
            1000,
        ))
        .expect("fetch_my_trades 不应失败");
    let fill_ids: BTreeSet<String> = venue_fills.iter().map(|t| t.trade_id.clone()).collect();
    println!(
        "[smoke] userTrades（窗口内）{} 笔：{fill_ids:?}",
        venue_fills.len()
    );
    VenueSnapshot {
        positions,
        // 合约钱包口径：与 OMS `AccountingMode::Wallet` 的 cash 对齐
        cash: balances.wallet.to_f64().unwrap_or(0.0),
        fill_ids,
    }
}

fn tail6(id: &str) -> &str {
    &id[id.len().saturating_sub(6)..]
}
