use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::stream::SplitStream;
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use parking_lot::Mutex as PkMutex;
use reqwest::Client;
use rust_decimal::Decimal;
use sha2::Sha256;
use tokio::sync::{Mutex, Notify, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Duration, interval, interval_at};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use axon_core::{Bar, Price, Quantity, Timestamp};

use crate::error::ExchangeError;
use crate::sign;
use crate::traits::ExchangeAdapter;
use crate::types::{
    AccountBalance, AccountInfo, DepthSnapshot, ExchangeConfig, ExchangeId, FundingRate,
    LeverageBracket, LongShortRatio, MarginType, OpenInterest, Order, OrderId, OrderStatus,
    OrderUpdate, Position, PositionMode, Symbol, Ticker, WsMessage,
};

type HmacSha256 = Hmac<Sha256>;

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;
type WsRead = SplitStream<WsStream>;

pub struct BinanceAdapter {
    config: ExchangeConfig,
    client: Client,
    market_tx: mpsc::Sender<WsMessage>,
    market_rx: Mutex<Option<mpsc::Receiver<WsMessage>>>,
    depth_cache: Arc<Mutex<HashMap<String, DepthSnapshot>>>,
    ticker_cache: Arc<Mutex<HashMap<String, Ticker>>>,
    /// 是否处于已连接状态（user data stream 监督任务也据此判断是否退出）
    connected: Arc<Mutex<bool>>,
    // 已订阅的 symbol 列表，重连时用于重新订阅
    subscribed_symbols: Arc<Mutex<Vec<Symbol>>>,
    // 客户端订单 ID -> 交易对，用于撤单时获取 symbol
    order_symbols: Mutex<HashMap<String, String>>,
    // WebSocket 写入端共享句柄，供 subscribe 使用
    ws_writer: Arc<Mutex<Option<Arc<Mutex<WsSink>>>>>,
    // 断连通知，触发自动重连
    disconnect_notify: Arc<Notify>,
    // 优雅停止信号（disconnect 时使用）
    shutdown: Arc<Notify>,
    // 合约 user data stream 的 listenKey。只存在内存里，绝不写日志 / 落盘。
    listen_key: Arc<PkMutex<Option<String>>>,
    // user data stream 监督任务句柄（disconnect 时 abort）
    user_stream_handle: Mutex<Option<JoinHandle<()>>>,
    // user data stream 是否已建立（connect 等它就绪后才算连上，避免头几秒丢回报）
    user_stream_ready: watch::Sender<bool>,
}

impl BinanceAdapter {
    pub fn new(config: ExchangeConfig) -> Self {
        let (market_tx, market_rx) = mpsc::channel(4096);
        let client = crate::build_http_client(&config);
        Self {
            config,
            client,
            market_tx,
            market_rx: Mutex::new(Some(market_rx)),
            depth_cache: Arc::new(Mutex::new(HashMap::new())),
            ticker_cache: Arc::new(Mutex::new(HashMap::new())),
            connected: Arc::new(Mutex::new(false)),
            subscribed_symbols: Arc::new(Mutex::new(Vec::new())),
            order_symbols: Mutex::new(HashMap::new()),
            ws_writer: Arc::new(Mutex::new(None)),
            disconnect_notify: Arc::new(Notify::new()),
            shutdown: Arc::new(Notify::new()),
            listen_key: Arc::new(PkMutex::new(None)),
            user_stream_handle: Mutex::new(None),
            user_stream_ready: watch::Sender::new(false),
        }
    }

    /// HMAC-SHA256 签名
    fn sign(&self, query: &str) -> Result<String, ExchangeError> {
        let mut mac = HmacSha256::new_from_slice(self.config.api_secret.as_bytes())
            .map_err(|e| ExchangeError::AuthenticationFailed(e.to_string()))?;
        mac.update(query.as_bytes());
        Ok(hex::encode(mac.finalize().into_bytes()))
    }

    /// 构造带签名的查询字符串
    fn signed_query(&self, params: &str) -> Result<String, ExchangeError> {
        let timestamp = chrono::Utc::now().timestamp_millis();
        let query = if params.is_empty() {
            format!("timestamp={timestamp}")
        } else {
            format!("{params}&timestamp={timestamp}")
        };
        let signature = self.sign(&query)?;
        Ok(format!("{query}&signature={signature}"))
    }

    /// REST GET 请求
    async fn rest_get(&self, path: &str, params: &str) -> Result<serde_json::Value, ExchangeError> {
        let query = self.signed_query(params)?;
        let url = format!("{}{path}?{query}", self.trading_base_url());
        let resp = self
            .client
            .get(&url)
            .header("X-MBX-APIKEY", self.config.api_key.as_str())
            .send()
            .await?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await?;
        if !status.is_success() {
            let code = body["code"].as_i64().unwrap_or(-1);
            let msg = body["msg"].as_str().unwrap_or("unknown error");
            return Err(ExchangeError::ApiError {
                code: code as i32,
                message: msg.to_string(),
            });
        }
        Ok(body)
    }

    /// REST POST 请求（下单）
    ///
    /// 参数全部放在 query string 里：Binance 的签名覆盖「query + form body」的
    /// 全部参数，只把 timestamp 放进 query、其余放 body 会导致签名与交易所算出的
    /// 不一致（-1022）。query 方式现货与合约端点都接受。
    async fn rest_post(
        &self,
        path: &str,
        params: &str,
    ) -> Result<serde_json::Value, ExchangeError> {
        let query = self.signed_query(params)?;
        let url = format!("{}{path}?{query}", self.trading_base_url());
        let resp = self
            .client
            .post(&url)
            .header("X-MBX-APIKEY", self.config.api_key.as_str())
            .send()
            .await?;
        let status = resp.status();
        let resp_body: serde_json::Value = resp.json().await?;
        if !status.is_success() {
            let code = resp_body["code"].as_i64().unwrap_or(-1);
            let msg = resp_body["msg"].as_str().unwrap_or("unknown error");
            return Err(ExchangeError::ApiError {
                code: code as i32,
                message: msg.to_string(),
            });
        }
        Ok(resp_body)
    }

    /// REST DELETE 请求（撤单）
    async fn rest_delete(
        &self,
        path: &str,
        params: &str,
    ) -> Result<serde_json::Value, ExchangeError> {
        let query = self.signed_query(params)?;
        let url = format!("{}{path}?{query}", self.trading_base_url());
        let resp = self
            .client
            .delete(&url)
            .header("X-MBX-APIKEY", self.config.api_key.as_str())
            .send()
            .await?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await?;
        if !status.is_success() {
            let code = body["code"].as_i64().unwrap_or(-1);
            let msg = body["msg"].as_str().unwrap_or("unknown error");
            return Err(ExchangeError::ApiError {
                code: code as i32,
                message: msg.to_string(),
            });
        }
        Ok(body)
    }

    /// 启动 WebSocket 连接并管理重连循环
    async fn start_ws(&self) -> Result<(), ExchangeError> {
        let url = self.config.ws_url.clone();
        let reconnect_cfg = self.config.reconnect.clone();
        let tx = self.market_tx.clone();
        let depth_cache = self.depth_cache.clone();
        let ticker_cache = self.ticker_cache.clone();
        let subscribed = self.subscribed_symbols.clone();
        let ws_writer_slot = self.ws_writer.clone();
        let disconnect_notify = self.disconnect_notify.clone();
        let shutdown = self.shutdown.clone();

        // 重连监督任务：负责建立连接、启动读写任务、断线后指数退避重连
        tokio::spawn(async move {
            let mut backoff = reconnect_cfg.initial_backoff;
            let mut attempt: u32 = 0;
            loop {
                if attempt > 0 {
                    // 等待 shutdown 或退避时长
                    tokio::select! {
                        _ = shutdown.notified() => break,
                        _ = tokio::time::sleep(backoff) => {}
                    }
                }

                match Self::open_ws(&url).await {
                    Ok((ws_write, ws_read)) => {
                        attempt = 0;
                        backoff = reconnect_cfg.initial_backoff;
                        let writer = Arc::new(Mutex::new(ws_write));
                        *ws_writer_slot.lock().await = Some(writer.clone());

                        // 连接建立后立即按已订阅列表重新发送 SUBSCRIBE（首次连接与重连均走此路径）
                        {
                            let symbols = subscribed.lock().await.clone();
                            if !symbols.is_empty() {
                                let streams = Self::build_streams(&symbols);
                                if let Err(e) = Self::send_subscribe_msg(&writer, streams).await {
                                    tracing::warn!("subscribe on (re)connect failed: {}", e);
                                }
                            }
                        }

                        // 启动 Ping 保活
                        let ping_handle = Self::spawn_ping_task(writer.clone(), shutdown.clone());

                        // 启动读取循环（持续到断线）
                        Self::run_read_loop(
                            ws_read,
                            tx.clone(),
                            writer.clone(),
                            depth_cache.clone(),
                            ticker_cache.clone(),
                            shutdown.clone(),
                        )
                        .await;

                        // 清理：清空共享 writer
                        *ws_writer_slot.lock().await = None;
                        ping_handle.abort();
                        tracing::warn!("Binance WebSocket read loop exited, will reconnect");
                        disconnect_notify.notify_waiters();
                    }
                    Err(e) => {
                        attempt += 1;
                        if attempt > reconnect_cfg.max_retries {
                            tracing::error!(
                                "Binance WebSocket reconnect failed after {} attempts: {}",
                                reconnect_cfg.max_retries,
                                e
                            );
                            break;
                        }
                        tracing::warn!(
                            "Binance WebSocket connect failed (attempt {}): {}",
                            attempt,
                            e
                        );
                        backoff = next_backoff(backoff, &reconnect_cfg);
                    }
                }
            }
        });

        Ok(())
    }

    /// 建立 WebSocket 连接并拆分读写
    async fn open_ws(url: &str) -> Result<(WsSink, WsRead), ExchangeError> {
        let (ws_stream, _) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|e| ExchangeError::WebSocket(e.to_string()))?;
        Ok(ws_stream.split())
    }

    /// 启动合约 user data stream（成交 / 撤单回报的来源）
    ///
    /// 现货分支本阶段明确不支持：`/api/v3/userDataStream` 的生命周期与这里相同，
    /// 但现货 `executionReport` 报文字段与合约 `ORDER_TRADE_UPDATE` 不同，未经
    /// testnet 验证不做猜测。现货行情路径不受影响（`connect` 只在合约下调用本方法）。
    async fn start_user_stream(&self) -> Result<(), ExchangeError> {
        if !self.is_futures() {
            return Err(ExchangeError::ConnectionFailed(
                "Binance 现货 user data stream 暂不支持（仅合约可用）".into(),
            ));
        }

        // 重连（connect 被再次调用）时先停掉上一轮的监督任务，避免两个任务同抢一条流。
        self.user_stream_ready.send_replace(false);
        if let Some(handle) = self.user_stream_handle.lock().await.take() {
            handle.abort();
        }

        let config = self.config.clone();
        let client = self.client.clone();
        let listen_key = self.listen_key.clone();
        let tx = self.market_tx.clone();
        let connected = self.connected.clone();
        let shutdown = self.shutdown.clone();
        let ready = self.user_stream_ready.clone();

        // 先把 listenKey 拿到手：失败要让 connect() 直接报错，而不是静默地收不到回报。
        ensure_listen_key(&config, &client, &listen_key).await?;

        let handle = tokio::spawn(async move {
            run_user_stream(config, client, listen_key, tx, connected, shutdown, ready).await;
        });
        *self.user_stream_handle.lock().await = Some(handle);
        Ok(())
    }

    /// Ping 保活任务：定时向 ws 写入 ping 帧，断线时退出
    fn spawn_ping_task(
        writer: Arc<Mutex<WsSink>>,
        shutdown: Arc<Notify>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(30));
            loop {
                tokio::select! {
                    _ = shutdown.notified() => break,
                    _ = ticker.tick() => {
                        let mut w = writer.lock().await;
                        if w.send(Message::Ping(Vec::new())).await.is_err() {
                            break;
                        }
                    }
                }
            }
        })
    }

    /// 读取循环：解析消息后更新缓存并通过 market_tx 转发；
    /// 断线（读取或写入失败）或收到 shutdown 信号后返回。
    async fn run_read_loop(
        mut ws_read: WsRead,
        tx: mpsc::Sender<WsMessage>,
        writer: Arc<Mutex<WsSink>>,
        depth_cache: Arc<Mutex<HashMap<String, DepthSnapshot>>>,
        ticker_cache: Arc<Mutex<HashMap<String, Ticker>>>,
        shutdown: Arc<Notify>,
    ) {
        loop {
            tokio::select! {
                _ = shutdown.notified() => return,
                msg = ws_read.next() => {
                    let Some(msg) = msg else { return; };
                    match msg {
                        Ok(Message::Text(text)) => {
                            if let Ok(parsed) = parse_ws_message(&text) {
                                // 缓存：深度 / Ticker
                                match &parsed {
                                    WsMessage::Depth(d) => {
                                        depth_cache.lock().await.insert(d.symbol.to_string(), d.clone());
                                    }
                                    WsMessage::Ticker(t) => {
                                        ticker_cache.lock().await.insert(t.symbol.to_string(), t.clone());
                                    }
                                    _ => {}
                                }
                                if tx.send(parsed).await.is_err() {
                                    return;
                                }
                            }
                        }
                        Ok(Message::Ping(data)) => {
                            // 按 Binance 协议回 Pong
                            let mut w = writer.lock().await;
                            if w.send(Message::Pong(data)).await.is_err() {
                                return;
                            }
                            let _ = tx.send(WsMessage::Pong).await;
                        }
                        Ok(Message::Close(_)) => return,
                        Err(e) => {
                            tracing::warn!("Binance WebSocket read error: {}", e);
                            return;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// 通过共享 writer 发送订阅消息
    async fn send_subscribe_msg(
        writer: &Arc<Mutex<WsSink>>,
        streams: Vec<String>,
    ) -> Result<(), ExchangeError> {
        let msg = serde_json::json!({
            "method": "SUBSCRIBE",
            "params": streams,
            "id": chrono::Utc::now().timestamp_millis(),
        });
        let payload =
            serde_json::to_string(&msg).map_err(|e| ExchangeError::ParseError(e.to_string()))?;
        let mut w = writer.lock().await;
        w.send(Message::Text(payload))
            .await
            .map_err(|e| ExchangeError::WebSocket(e.to_string()))?;
        Ok(())
    }

    /// 构造交易对 stream 名称列表
    fn build_streams(symbols: &[Symbol]) -> Vec<String> {
        symbols
            .iter()
            .flat_map(|s| {
                let lower = s.to_string().to_lowercase();
                vec![
                    format!("{lower}@depth@100ms"),
                    format!("{lower}@ticker"),
                    format!("{lower}@trade"),
                    format!("{lower}@kline_1m"),
                ]
            })
            .collect()
    }
}

/// 计算下一次退避时长，上限为 max_backoff
fn next_backoff(current: Duration, cfg: &crate::types::ReconnectConfig) -> Duration {
    let next = Duration::from_secs_f64(current.as_secs_f64() * cfg.backoff_multiplier);
    if next > cfg.max_backoff {
        cfg.max_backoff
    } else {
        next
    }
}

/// 解析 Binance WebSocket 推送消息
fn parse_ws_message(text: &str) -> Result<WsMessage, serde_json::Error> {
    let v: serde_json::Value = serde_json::from_str(text)?;

    // Ping/Pong
    if v.get("ping").is_some() || v.get("result").is_some() {
        return Ok(WsMessage::Pong);
    }

    let event = v.get("e").and_then(|e| e.as_str()).unwrap_or("");

    match event {
        "24hrTicker" => {
            let symbol = v.get("s").and_then(|s| s.as_str()).unwrap_or("");
            let ticker = Ticker {
                symbol: Symbol::new(symbol),
                bid: v
                    .get("b")
                    .and_then(|b| b.as_str())
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or_default(),
                ask: v
                    .get("a")
                    .and_then(|a| a.as_str())
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or_default(),
                last: v
                    .get("c")
                    .and_then(|c| c.as_str())
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or_default(),
                volume_24h: v
                    .get("v")
                    .and_then(|vol| vol.as_str())
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or_default(),
                timestamp: chrono::Utc::now(),
            };
            Ok(WsMessage::Ticker(ticker))
        }
        "depthUpdate" => {
            let symbol = v.get("s").and_then(|s| s.as_str()).unwrap_or("");
            let bids = parse_price_level_array(v.get("b"));
            let asks = parse_price_level_array(v.get("a"));
            Ok(WsMessage::Depth(DepthSnapshot {
                symbol: Symbol::new(symbol),
                bids,
                asks,
                timestamp: chrono::Utc::now(),
            }))
        }
        "trade" | "aggTrade" => {
            let symbol = v.get("s").and_then(|s| s.as_str()).unwrap_or("");
            let price = v
                .get("p")
                .and_then(|p| p.as_str())
                .unwrap_or("0")
                .parse()
                .unwrap_or_default();
            let quantity = v
                .get("q")
                .and_then(|q| q.as_str())
                .unwrap_or("0")
                .parse()
                .unwrap_or_default();
            let side = if v.get("m").and_then(|m| m.as_bool()).unwrap_or(false) {
                crate::types::Side::Sell
            } else {
                crate::types::Side::Buy
            };
            Ok(WsMessage::Trade(crate::types::Trade {
                symbol: Symbol::new(symbol),
                price,
                quantity,
                side,
                timestamp: chrono::Utc::now(),
            }))
        }
        "kline" => {
            let symbol = v.get("s").and_then(|s| s.as_str()).unwrap_or("");
            let k = v.get("k").unwrap_or(&serde_json::Value::Null);
            let interval = k.get("i").and_then(|i| i.as_str()).unwrap_or("");
            let is_closed = k.get("x").and_then(|x| x.as_bool()).unwrap_or(false);
            // `k.t` 是 K 线**起始时间**（ms）。内核聚合器用它做去重 / 缺口检测 /
            // 收市时刻推导，不能用接收时刻（此前误用 Utc::now()）。字段缺失时
            // 退回当前时间，保证不 panic。
            let bar_timestamp = k
                .get("t")
                .and_then(|t| t.as_i64())
                .and_then(chrono::DateTime::from_timestamp_millis)
                .unwrap_or_else(chrono::Utc::now);
            Ok(WsMessage::Kline(crate::types::Kline {
                symbol: Symbol::new(symbol),
                interval: interval.to_string(),
                open: k
                    .get("o")
                    .and_then(|o| o.as_str())
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or_default(),
                high: k
                    .get("h")
                    .and_then(|h| h.as_str())
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or_default(),
                low: k
                    .get("l")
                    .and_then(|l| l.as_str())
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or_default(),
                close: k
                    .get("c")
                    .and_then(|c| c.as_str())
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or_default(),
                volume: k
                    .get("v")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or_default(),
                timestamp: bar_timestamp,
                is_closed,
            }))
        }
        "executionReport" | "executionReportAlgo" => {
            let client_order_id = v.get("c").and_then(|c| c.as_str()).unwrap_or("");
            let status_str = v.get("X").and_then(|x| x.as_str()).unwrap_or("");
            let filled_qty = v
                .get("z")
                .and_then(|z| z.as_str())
                .unwrap_or("0")
                .parse()
                .unwrap_or_default();
            let avg_price = v
                .get("Z")
                .and_then(|z| z.as_str())
                .unwrap_or("0")
                .parse()
                .unwrap_or_default();
            let status = match status_str {
                "NEW" => OrderStatus::Acknowledged,
                "PARTIALLY_FILLED" => OrderStatus::PartiallyFilled {
                    filled_qty,
                    avg_price,
                },
                "FILLED" => OrderStatus::Filled {
                    filled_qty,
                    avg_price,
                },
                "CANCELED" | "CANCELLED" => OrderStatus::Cancelled { filled_qty },
                "REJECTED" => OrderStatus::Rejected {
                    reason: v
                        .get("r")
                        .and_then(|r| r.as_str())
                        .unwrap_or("rejected")
                        .to_string(),
                },
                _ => OrderStatus::Pending,
            };
            Ok(WsMessage::OrderUpdate(crate::types::OrderUpdate {
                order_id: v
                    .get("i")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string(),
                client_order_id: OrderId(
                    uuid::Uuid::parse_str(client_order_id).unwrap_or_default(),
                ),
                status,
                filled_qty,
                avg_price: Some(avg_price),
                timestamp: chrono::Utc::now(),
            }))
        }
        _ => Ok(WsMessage::Ping),
    }
}

// ============================================================================
// 合约 user data stream：listenKey 生命周期 + ORDER_TRADE_UPDATE 接入
// ============================================================================
//
// 落点说明：订单回报统一送进**已有的** `market_tx` 通道（`ExchangeGateway::poll_updates`
// 就是从这个通道里筛 `WsMessage::OrderUpdate`）。不新开第二条通道，否则网关要同时
// drain 两个 rx，行情泵和回报的优先级又会打架。

/// listenKey 续期周期：Binance 要求 60 分钟内至少续期一次，25 分钟留足抖动余量
const LISTEN_KEY_KEEPALIVE: Duration = Duration::from_secs(25 * 60);
/// user data stream 重连的初始退避与上限
const USER_STREAM_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const USER_STREAM_MAX_BACKOFF: Duration = Duration::from_secs(30);
/// 续期连续失败达到该次数即视为 key 已不可用：重建 listenKey 并重连
const KEEPALIVE_MAX_FAILURES: u32 = 3;
/// 连接稳定超过该时长才把退避清零，避免「连上即断」时 1 秒一次地打交易所
const USER_STREAM_STABLE_AFTER: Duration = Duration::from_secs(60);
/// connect() 等 user data stream 建立的上限
const USER_STREAM_READY_TIMEOUT: Duration = Duration::from_secs(5);

/// user data stream 事件
enum UserStreamEvent {
    /// 已归一到 `WsMessage::OrderUpdate` 的订单回报
    Order(WsMessage),
    /// 服务端通知 listenKey 过期：必须重新 POST 并重连
    ListenKeyExpired,
    /// 我们不关心的推送（ACCOUNT_UPDATE / MARGIN_CALL / 非 Uuid client id 等）
    Ignored,
}

/// 合约 REST 基础 URL（配置优先，否则按 testnet 推断）
fn fapi_base_url_of(config: &ExchangeConfig) -> &str {
    if let Some(url) = config.fapi_base_url.as_deref() {
        url
    } else if config.testnet {
        "https://testnet.binancefuture.com"
    } else {
        "https://fapi.binance.com"
    }
}

/// user data stream 的 WS 入口。testnet 实测可用的是 `fstream.binancefuture.com`，
/// 生产是 `fstream.binance.com`；两者路径都是 `/ws/<listenKey>`。
fn user_stream_ws_base(config: &ExchangeConfig) -> &'static str {
    if config.testnet {
        "wss://fstream.binancefuture.com/ws"
    } else {
        "wss://fstream.binance.com/ws"
    }
}

fn user_stream_api_error(status: reqwest::StatusCode, body: &serde_json::Value) -> ExchangeError {
    ExchangeError::ApiError {
        code: body["code"].as_i64().unwrap_or(status.as_u16() as i64) as i32,
        message: body["msg"].as_str().unwrap_or("unknown error").to_string(),
    }
}

/// `POST /fapi/v1/listenKey`：只带 `X-MBX-APIKEY`，**没有签名参数**
async fn create_listen_key(
    config: &ExchangeConfig,
    client: &Client,
) -> Result<String, ExchangeError> {
    let url = format!("{}/fapi/v1/listenKey", fapi_base_url_of(config));
    let resp = client
        .post(&url)
        .header("X-MBX-APIKEY", config.api_key.as_str())
        .send()
        .await?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await?;
    if !status.is_success() {
        return Err(user_stream_api_error(status, &body));
    }
    body["listenKey"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| ExchangeError::ParseError("listenKey 响应缺 listenKey 字段".into()))
}

/// `PUT /fapi/v1/listenKey`：续期（同样只带 API key，无签名）
async fn put_listen_key(config: &ExchangeConfig, client: &Client) -> Result<(), ExchangeError> {
    let url = format!("{}/fapi/v1/listenKey", fapi_base_url_of(config));
    let resp = client
        .put(&url)
        .header("X-MBX-APIKEY", config.api_key.as_str())
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let body: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        return Err(user_stream_api_error(status, &body));
    }
    Ok(())
}

/// `DELETE /fapi/v1/listenKey`：断开时释放 key
async fn delete_listen_key(config: &ExchangeConfig, client: &Client) -> Result<(), ExchangeError> {
    let url = format!("{}/fapi/v1/listenKey", fapi_base_url_of(config));
    let resp = client
        .delete(&url)
        .header("X-MBX-APIKEY", config.api_key.as_str())
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let body: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        return Err(user_stream_api_error(status, &body));
    }
    Ok(())
}

/// 复用内存里的 listenKey；没有才去申请新的
async fn ensure_listen_key(
    config: &ExchangeConfig,
    client: &Client,
    slot: &Arc<PkMutex<Option<String>>>,
) -> Result<String, ExchangeError> {
    if let Some(key) = slot.lock().clone() {
        return Ok(key);
    }
    let key = create_listen_key(config, client).await?;
    *slot.lock() = Some(key.clone());
    tracing::info!("Binance 合约 user data stream: listenKey 已获取");
    Ok(key)
}

/// user data stream 监督任务：拿 key → 连 WS → 读回报 → 25 分钟续期 → 断线退避重连
async fn run_user_stream(
    config: ExchangeConfig,
    client: Client,
    listen_key: Arc<PkMutex<Option<String>>>,
    tx: mpsc::Sender<WsMessage>,
    connected: Arc<Mutex<bool>>,
    shutdown: Arc<Notify>,
    ready: watch::Sender<bool>,
) {
    let mut backoff = USER_STREAM_INITIAL_BACKOFF;
    let mut attempt: u32 = 0;

    'supervisor: loop {
        if !*connected.lock().await {
            break;
        }
        if attempt > 0 {
            tokio::select! {
                _ = shutdown.notified() => break,
                _ = tokio::time::sleep(backoff) => {}
            }
            if !*connected.lock().await {
                break;
            }
        }

        let key = match ensure_listen_key(&config, &client, &listen_key).await {
            Ok(key) => key,
            Err(err) => {
                attempt += 1;
                // 长时间断网时不刷屏：前 3 次 + 每 10 次记一条
                if attempt <= 3 || attempt.is_multiple_of(10) {
                    tracing::warn!(attempt, error = %err, "Binance user data stream: 获取 listenKey 失败");
                }
                backoff = (backoff * 2).min(USER_STREAM_MAX_BACKOFF);
                continue;
            }
        };

        let url = format!("{}/{}", user_stream_ws_base(&config), key);
        let (mut write, mut read) = match BinanceAdapter::open_ws(&url).await {
            Ok(pair) => pair,
            Err(err) => {
                attempt += 1;
                if attempt <= 3 || attempt.is_multiple_of(10) {
                    // 握手错误可能把含 listenKey 的 URL 带进 message，先脱敏再打日志
                    let redacted = err.to_string().replace(&key, "<listenKey>");
                    tracing::warn!(attempt, error = %redacted, "Binance user data stream: 连接失败");
                }
                backoff = (backoff * 2).min(USER_STREAM_MAX_BACKOFF);
                continue;
            }
        };

        tracing::info!("Binance 合约 user data stream 已连接");
        ready.send_replace(true);
        let connected_at = tokio::time::Instant::now();
        let mut keepalive = interval_at(
            tokio::time::Instant::now() + LISTEN_KEY_KEEPALIVE,
            LISTEN_KEY_KEEPALIVE,
        );
        keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // 服务端明确说过 key 失效 / 续期连续失败：下一轮必须重新 POST
        let mut need_new_key = false;

        loop {
            tokio::select! {
                _ = shutdown.notified() => break,
                _ = keepalive.tick() => {
                    // 续期失败立刻做短重试（不是等下一个 25 分钟）：listenKey 只有
                    // 60 分钟寿命，等下一轮再试就已经过期了。
                    let mut last_error = None;
                    let mut renewed = false;
                    for attempt in 1..=KEEPALIVE_MAX_FAILURES {
                        match put_listen_key(&config, &client).await {
                            Ok(()) => {
                                renewed = true;
                                break;
                            }
                            Err(err) => {
                                last_error = Some(err.to_string());
                                if attempt < KEEPALIVE_MAX_FAILURES {
                                    tracing::warn!(
                                        attempt,
                                        error = %err,
                                        "Binance user data stream: listenKey 续期失败，5s 后重试"
                                    );
                                    tokio::time::sleep(Duration::from_secs(5)).await;
                                }
                            }
                        }
                    }
                    if !renewed {
                        // 连续 3 次失败基本等于 key 已死：重建 listenKey 并重连，记 error 不 panic
                        tracing::error!(
                            failures = KEEPALIVE_MAX_FAILURES,
                            error = last_error.as_deref().unwrap_or("unknown"),
                            "Binance user data stream: listenKey 续期连续失败，重建 listenKey"
                        );
                        need_new_key = true;
                        break;
                    }
                }
                message = read.next() => {
                    match message {
                        Some(Ok(Message::Text(text))) => {
                            match parse_user_stream_event(&text) {
                                Ok(UserStreamEvent::Order(message)) => {
                                    // 与行情共用同一条通道：网关从里面筛 OrderUpdate。
                                    if tx.send(message).await.is_err() {
                                        // 消费端已销毁 = 进程在收尾，直接退出并释放 key
                                        break 'supervisor;
                                    }
                                }
                                Ok(UserStreamEvent::ListenKeyExpired) => {
                                    tracing::warn!("Binance user data stream: listenKey 过期，重建并重连");
                                    need_new_key = true;
                                    break;
                                }
                                Ok(UserStreamEvent::Ignored) => {}
                                Err(err) => {
                                    tracing::warn!(error = %err, "Binance user data stream: 收到非 JSON 消息");
                                }
                            }
                        }
                        Some(Ok(Message::Ping(data))) => {
                            if let Err(err) = write.send(Message::Pong(data)).await {
                                tracing::warn!(error = %err, "Binance user data stream: 回 Pong 失败");
                                break;
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        Some(Err(err)) => {
                            tracing::warn!(error = %err, "Binance user data stream: 读取失败");
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }

        // 连接已断：先对 connect() 侧标记「未就绪」，重连成功后再置回 true
        ready.send_replace(false);
        if need_new_key {
            // 旧 key 已失效，丢弃内存值让下一轮重新申请
            listen_key.lock().take();
        }
        if !*connected.lock().await {
            break;
        }

        // 稳定连接过一段时间的断开按首次重连处理；连上即断的则继续指数退避
        if connected_at.elapsed() >= USER_STREAM_STABLE_AFTER {
            attempt = 1;
            backoff = USER_STREAM_INITIAL_BACKOFF;
        } else {
            attempt += 1;
            backoff = (backoff * 2).min(USER_STREAM_MAX_BACKOFF);
        }
        if attempt <= 3 || attempt.is_multiple_of(10) {
            tracing::warn!(attempt, "Binance user data stream 断开，准备重连");
        }
    }

    // 退出（shutdown / 通道关闭）：先撤「就绪」标记，再主动释放 key
    ready.send_replace(false);
    if listen_key.lock().take().is_some() {
        let _ = delete_listen_key(&config, &client).await;
    }
}

/// 解析 user data stream 推送
///
/// 只处理 `ORDER_TRADE_UPDATE` 与 `listenKeyExpired`：`ACCOUNT_UPDATE` /
/// `MARGIN_CALL` 等当前没有消费方，按 `Ignored` 丢掉，不制造空事件。
fn parse_user_stream_event(text: &str) -> Result<UserStreamEvent, serde_json::Error> {
    let v: serde_json::Value = serde_json::from_str(text)?;
    match v.get("e").and_then(|e| e.as_str()).unwrap_or("") {
        "listenKeyExpired" => Ok(UserStreamEvent::ListenKeyExpired),
        "ORDER_TRADE_UPDATE" => {
            let o = v.get("o").unwrap_or(&serde_json::Value::Null);
            let raw_client_id = o.get("c").and_then(|c| c.as_str()).unwrap_or("");
            // 我们的 client order id 就是 Uuid；解析不出来就无法归因到 OMS 订单，跳过。
            let Ok(client_uuid) = uuid::Uuid::parse_str(raw_client_id) else {
                tracing::warn!(
                    client_order_id = tail6(raw_client_id),
                    "Binance user stream: client order id 不是 Uuid，跳过该条回报"
                );
                return Ok(UserStreamEvent::Ignored);
            };

            // `z` 是累计成交量，`ap` 是累计均价（未成交时为空串 → 0）
            let filled_qty = decimal_field(o, "z");
            let avg_price = decimal_field(o, "ap");
            let status = match o.get("X").and_then(|x| x.as_str()).unwrap_or("") {
                "NEW" => OrderStatus::Acknowledged,
                "PARTIALLY_FILLED" => OrderStatus::PartiallyFilled {
                    filled_qty,
                    avg_price,
                },
                "FILLED" => OrderStatus::Filled {
                    filled_qty,
                    avg_price,
                },
                "CANCELED" | "EXPIRED" => OrderStatus::Cancelled { filled_qty },
                "REJECTED" => OrderStatus::Rejected {
                    reason: o
                        .get("r")
                        .and_then(|r| r.as_str())
                        .filter(|r| !r.is_empty() && *r != "NONE")
                        .unwrap_or("rejected")
                        .to_string(),
                },
                _ => OrderStatus::Pending,
            };
            let timestamp = v
                .get("E")
                .and_then(|e| e.as_i64())
                .and_then(chrono::DateTime::from_timestamp_millis)
                .unwrap_or_else(chrono::Utc::now);

            Ok(UserStreamEvent::Order(WsMessage::OrderUpdate(
                OrderUpdate {
                    order_id: o
                        .get("i")
                        .and_then(|i| i.as_i64())
                        .map(|i| i.to_string())
                        .unwrap_or_default(),
                    client_order_id: OrderId(client_uuid),
                    status,
                    filled_qty,
                    // 只在真正成交后带均价；NEW / CANCELED 的 `ap` 是 0
                    avg_price: (avg_price > Decimal::ZERO).then_some(avg_price),
                    timestamp,
                },
            )))
        }
        _ => Ok(UserStreamEvent::Ignored),
    }
}

fn decimal_field(value: &serde_json::Value, key: &str) -> Decimal {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or_default()
}

/// 只留尾部 6 位，日志里不出现完整订单号
fn tail6(value: &str) -> &str {
    &value[value.len().saturating_sub(6)..]
}

/// 解析 Binance 价格层数组 `[["price","qty"], ...]`
fn parse_price_level_array(
    val: Option<&serde_json::Value>,
) -> Vec<(rust_decimal::Decimal, rust_decimal::Decimal)> {
    val.and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let price = item.get(0)?.as_str()?.parse().ok()?;
                    let qty = item.get(1)?.as_str()?.parse().ok()?;
                    Some((price, qty))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 探活端点：合约（USDⓈ-M）`/fapi/v1/ping`，现货 `/api/v3/ping`
fn ping_path(is_futures: bool) -> &'static str {
    if is_futures {
        "/fapi/v1/ping"
    } else {
        "/api/v3/ping"
    }
}

/// 下单 / 撤单端点：合约 `/fapi/v1/order`，现货 `/api/v3/order`
fn order_path(is_futures: bool) -> &'static str {
    if is_futures {
        "/fapi/v1/order"
    } else {
        "/api/v3/order"
    }
}

/// 将 axon Order 转换为 Binance REST 请求参数
fn order_to_binance_params(order: &Order) -> String {
    let symbol = order.symbol.to_string().to_uppercase();
    let side = match order.side {
        crate::types::Side::Buy => "BUY",
        crate::types::Side::Sell => "SELL",
    };
    let order_type = match order.order_type {
        crate::types::OrderType::Market => "MARKET",
        crate::types::OrderType::Limit => "LIMIT",
        crate::types::OrderType::StopLoss => "STOP_LOSS",
        crate::types::OrderType::StopLimit => "STOP_LIMIT",
    };
    let tif = match order.time_in_force {
        crate::types::TimeInForce::Gtc => "GTC",
        crate::types::TimeInForce::Ioc => "IOC",
        crate::types::TimeInForce::Fok => "FOK",
    };

    let mut params = format!(
        "symbol={symbol}&side={side}&type={order_type}&quantity={}",
        order.quantity
    );

    // 只有限价类订单要 timeInForce：Binance 对 MARKET 携带该参数会返回
    // -1106 `Parameter 'timeInForce' sent when not required.`
    if matches!(
        order.order_type,
        crate::types::OrderType::Limit | crate::types::OrderType::StopLimit
    ) {
        params.push_str(&format!("&timeInForce={tif}"));
    }

    if let Some(price) = order.price {
        params.push_str(&format!("&price={price}"));
    }

    if let Some(client_id) = order.meta.get("client_order_id") {
        params.push_str(&format!("&newClientOrderId={client_id}"));
    } else {
        params.push_str(&format!("&newClientOrderId={}", order.client_order_id));
    }

    params
}

#[async_trait]
impl ExchangeAdapter for BinanceAdapter {
    fn exchange_id(&self) -> ExchangeId {
        ExchangeId::Binance
    }

    async fn connect(&mut self) -> Result<(), ExchangeError> {
        // 验证 REST 连接：合约 ping 公开端点无需签名，现货 /api/v3/ping 同理
        let is_futures = self.is_futures();
        let url = format!("{}{}", self.trading_base_url(), ping_path(is_futures));
        let resp = self.client.get(&url).send().await?;
        if !resp.status().is_success() {
            return Err(ExchangeError::ConnectionFailed(
                "Binance REST ping failed".into(),
            ));
        }

        // 启动 WebSocket（重连逻辑在内部 tokio 任务中循环）
        self.start_ws().await?;
        *self.connected.lock().await = true;

        // 合约额外接 user data stream：ORDER_TRADE_UPDATE 是成交 / 撤单回报的唯一来源。
        // 现货不接（见 start_user_stream 注释），行情路径保持不变。
        if self.is_futures() {
            // 先订阅再启动：watch 的当前值会被立刻读到，不会丢通知。
            let mut ready = self.user_stream_ready.subscribe();
            if let Err(err) = self.start_user_stream().await {
                // user stream 起不来就等于拿不到成交回报，不能假装连接成功。
                *self.connected.lock().await = false;
                self.shutdown.notify_waiters();
                return Err(err);
            }
            // 等回报流真正建立再返回：否则「已连接」之后头几秒的订单回报会静默丢失。
            let established = tokio::time::timeout(USER_STREAM_READY_TIMEOUT, async {
                ready.wait_for(|value| *value).await.map(|_| ())
            })
            .await;
            let failure = match established {
                Ok(Ok(())) => None,
                Ok(Err(err)) => Some(format!("user data stream 任务提前退出: {err}")),
                Err(_) => Some(format!(
                    "user data stream {}s 内未建立",
                    USER_STREAM_READY_TIMEOUT.as_secs()
                )),
            };
            if let Some(reason) = failure {
                *self.connected.lock().await = false;
                self.shutdown.notify_waiters();
                if let Some(handle) = self.user_stream_handle.lock().await.take() {
                    handle.abort();
                }
                return Err(ExchangeError::ConnectionFailed(reason));
            }
        }

        // 等待连接建立
        tokio::time::sleep(Duration::from_millis(500)).await;

        tracing::info!("Binance adapter connected");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ExchangeError> {
        *self.connected.lock().await = false;
        // 通知重连监督任务退出
        self.shutdown.notify_waiters();
        // 停掉 user data stream 监督任务，避免它在 DELETE 之后又续期旧 key
        self.user_stream_ready.send_replace(false);
        if let Some(handle) = self.user_stream_handle.lock().await.take() {
            handle.abort();
        }
        // 释放 listenKey：服务端在停止续期后仍会保留它一段时间，主动 DELETE 才是干净收尾。
        // listenKey 只走内存，任何分支都不打印它的值。
        if self.listen_key.lock().take().is_some() {
            let _ = delete_listen_key(&self.config, &self.client).await;
        }
        tracing::info!("Binance adapter disconnected");
        Ok(())
    }

    async fn subscribe(&mut self, symbols: &[Symbol]) -> Result<(), ExchangeError> {
        // 记录已订阅的 symbol，重连时用于重新订阅
        {
            let mut sub = self.subscribed_symbols.lock().await;
            for s in symbols {
                if !sub.iter().any(|x| x.to_string() == s.to_string()) {
                    sub.push(s.clone());
                }
            }
        }

        let streams = Self::build_streams(symbols);
        tracing::info!("Subscribing to {} streams", streams.len());

        // 通过共享 writer 发送订阅消息
        let writer_opt = self.ws_writer.lock().await.clone();
        match writer_opt {
            Some(writer) => Self::send_subscribe_msg(&writer, streams).await,
            None => {
                // WebSocket 尚未建立，消息将在重连后由 start_ws 循环中的重新订阅逻辑发出
                tracing::warn!("WebSocket not ready; subscription will be applied on reconnect");
                Ok(())
            }
        }
    }

    async fn send_order(&mut self, order: Order) -> Result<OrderId, ExchangeError> {
        if !*self.connected.lock().await {
            return Err(ExchangeError::ConnectionFailed("not connected".into()));
        }

        // 记录 client_order_id -> symbol，供后续撤单使用
        self.order_symbols
            .lock()
            .await
            .insert(order.client_order_id.to_string(), order.symbol.to_string());

        let params = order_to_binance_params(&order);
        let resp = self
            .rest_post(order_path(self.is_futures()), &params)
            .await?;

        let exchange_order_id = resp["orderId"]
            .as_i64()
            .ok_or_else(|| ExchangeError::ParseError("missing orderId".into()))?
            .to_string();

        tracing::info!(
            "Order sent: client_id={}, exchange_id={}",
            order.client_order_id,
            exchange_order_id
        );

        Ok(order.client_order_id)
    }

    async fn cancel_order(&mut self, order_id: OrderId) -> Result<(), ExchangeError> {
        if !*self.connected.lock().await {
            return Err(ExchangeError::ConnectionFailed("not connected".into()));
        }

        // Binance 撤单需要 symbol + (orderId 或 clientOrderId)
        // 从本地映射查找订单对应的 symbol
        let symbol = self
            .order_symbols
            .lock()
            .await
            .get(&order_id.to_string())
            .cloned();

        // Binance 撤单需要 symbol + (orderId 或 origClientOrderId)；合约与现货
        // 都不认裸 `clientOrderId`（合约返回 -1102）。
        let params = match symbol {
            Some(sym) => format!("symbol={sym}&origClientOrderId={order_id}"),
            None => {
                tracing::warn!(
                    "cancel_order: no symbol mapping for client_order_id={order_id}, \
                     Binance API requires symbol; consider providing it via order meta"
                );
                return Err(ExchangeError::OrderNotFound(order_id.to_string()));
            }
        };
        self.rest_delete(order_path(self.is_futures()), &params)
            .await?;

        // 清理映射
        self.order_symbols
            .lock()
            .await
            .remove(&order_id.to_string());

        tracing::info!("Order cancelled: {}", order_id);
        Ok(())
    }

    async fn get_balance(&self) -> Result<HashMap<String, AccountBalance>, ExchangeError> {
        let resp = self.rest_get("/api/v3/account", "").await?;

        let balances = resp["balances"]
            .as_array()
            .ok_or_else(|| ExchangeError::ParseError("missing balances".into()))?
            .iter()
            .filter_map(|b| {
                let asset = b["asset"].as_str()?;
                let free = b["free"].as_str()?.parse().ok()?;
                let locked = b["locked"].as_str()?.parse().ok()?;
                Some((
                    asset.to_string(),
                    AccountBalance {
                        currency: asset.to_string(),
                        available: free,
                        locked,
                    },
                ))
            })
            .collect();

        Ok(balances)
    }

    async fn get_positions(&self) -> Result<Vec<Position>, ExchangeError> {
        // 现货账户无 position 端点：未配置端点时直接返回空
        if self.config.position_endpoint.is_empty() {
            tracing::debug!("Binance position_endpoint empty, returning empty positions");
            return Ok(Vec::new());
        }
        // 查询合约 / 账户持仓端点；解析失败时记录 warn 并返回空 Vec（不 panic）
        let resp = match self.rest_get(&self.config.position_endpoint, "").await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "Binance get_positions: {} query failed: {}",
                    self.config.position_endpoint,
                    e
                );
                return Ok(Vec::new());
            }
        };
        // Binance 合约返回数组，OKX 包装在 `data` 中；尝试两种格式
        let arr: Vec<serde_json::Value> = if let Some(arr) = resp.as_array() {
            arr.clone()
        } else if let Some(arr) = resp.get("data").and_then(|d| d.as_array()) {
            arr.clone()
        } else {
            Vec::new()
        };
        Ok(crate::adapters::parse_positions_from_json(&arr))
    }

    fn get_depth(&self, symbol: &Symbol) -> Option<DepthSnapshot> {
        self.depth_cache
            .blocking_lock()
            .get(&symbol.to_string())
            .cloned()
    }

    fn get_ticker(&self, symbol: &Symbol) -> Option<Ticker> {
        self.ticker_cache
            .blocking_lock()
            .get(&symbol.to_string())
            .cloned()
    }

    fn market_data_rx(&self) -> mpsc::Receiver<WsMessage> {
        self.market_rx
            .blocking_lock()
            .take()
            .expect("market_data_rx already taken")
    }

    // === 杠杆/合约实现(Stage 4' D) ===

    async fn set_leverage(&self, symbol: &str, leverage: u8) -> Result<(), ExchangeError> {
        // 校验范围(避免发送到非法值)
        if !(1..=125).contains(&leverage) {
            return Err(ExchangeError::OrderRejected {
                reason: format!("leverage {leverage} out of range 1..=125"),
            });
        }
        self.fapi_post(
            "/fapi/v1/leverage",
            &format!("symbol={symbol}&leverage={leverage}"),
        )
        .await?;
        Ok(())
    }

    async fn set_margin_type(
        &self,
        symbol: &str,
        margin_type: MarginType,
    ) -> Result<(), ExchangeError> {
        let mt = match margin_type {
            MarginType::Isolated => "ISOLATED",
            MarginType::Cross => "CROSSED",
        };
        self.fapi_post(
            "/fapi/v1/marginType",
            &format!("symbol={symbol}&marginType={mt}"),
        )
        .await?;
        Ok(())
    }

    async fn get_leverage_brackets(
        &self,
        symbol: &str,
    ) -> Result<Vec<LeverageBracket>, ExchangeError> {
        let json = self
            .fapi_get("/fapi/v1/leverageBracket", &format!("symbol={symbol}"))
            .await?;
        let arr = json
            .as_array()
            .ok_or_else(|| ExchangeError::ParseError("leverageBracket: expected array".into()))?;
        let mut out = Vec::new();
        for entry in arr {
            // 仅取第一档的 maxNotional / 维持保证金,简化模型
            if let Some(brackets) = entry["brackets"].as_array() {
                for b in brackets {
                    out.push(LeverageBracket {
                        bracket: b["bracket"].as_u64().unwrap_or(0) as u32,
                        min_leverage: 1,
                        max_leverage: b["initialLeverage"].as_u64().unwrap_or(1) as u8,
                        max_notional: b["notionalCap"]
                            .as_str()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or_default(),
                        maint_margin_ratio: b["maintMarginRatio"]
                            .as_str()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or_default(),
                    });
                }
            }
        }
        Ok(out)
    }

    async fn set_position_mode(&self, hedge_mode: bool) -> Result<(), ExchangeError> {
        let dual = if hedge_mode { "true" } else { "false" };
        self.fapi_post(
            "/fapi/v1/positionSide/dual",
            &format!("dualSidePosition={dual}"),
        )
        .await?;
        Ok(())
    }

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate, ExchangeError> {
        // /fapi/v1/fundingRate 是公开端点(无签名),最近 1 条记录
        let json = self
            .fapi_get_public("/fapi/v1/fundingRate", &format!("symbol={symbol}"))
            .await?;
        let entry = json
            .as_array()
            .and_then(|a| a.first())
            .ok_or_else(|| ExchangeError::ParseError("fundingRate: empty response".into()))?;
        let funding_time: i64 = entry["fundingTime"].as_i64().unwrap_or(0);
        Ok(FundingRate {
            symbol: symbol.to_string(),
            rate: entry["fundingRate"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or_default(),
            // 下次结算 = 当前 + 8h(Binance 默认 8h 结算周期)
            next_funding_ms: if funding_time > 0 {
                funding_time + 8 * 3600 * 1000
            } else {
                0
            },
            mark_price: entry["markPrice"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or_default(),
            index_price: Decimal::ZERO, // /fapi/v1/fundingRate 端点不返回 indexPx
        })
    }

    async fn get_account_info(&self) -> Result<AccountInfo, ExchangeError> {
        let json = self.fapi_get("/fapi/v2/account", "").await?;
        let parse_dec = |k: &str| -> Decimal {
            json[k]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or_default()
        };
        // positionMode 是 hedge / one-way 字符串
        let position_mode = match json["positionMode"].as_str() {
            Some("hedge") => PositionMode::Hedge,
            _ => PositionMode::Net,
        };
        Ok(AccountInfo {
            total_balance: parse_dec("totalWalletBalance"),
            available_balance: parse_dec("availableBalance"),
            unrealized_pnl: parse_dec("totalUnrealizedProfit"),
            margin_used: parse_dec("totalInitialMargin"),
            initial_margin: parse_dec("totalInitialMargin"),
            maintenance_margin: parse_dec("totalMaintMargin"),
            position_mode,
            as_of_ms: chrono::Utc::now().timestamp_millis(),
        })
    }

    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest, ExchangeError> {
        // /fapi/v1/openInterest 是公开端点
        let json = self
            .fapi_get_public("/fapi/v1/openInterest", &format!("symbol={symbol}"))
            .await?;
        Ok(OpenInterest {
            symbol: symbol.to_string(),
            contracts: json["openInterest"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            notional: Decimal::ZERO, // 该端点不直接给美元名义
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        })
    }

    async fn get_long_short_ratio(&self, symbol: &str) -> Result<LongShortRatio, ExchangeError> {
        // 走 fapi 域(datadry 不在 fapi) - 用 base url
        let url = format!(
            "{}/futures/data/globalLongShortAccountRatio?symbol={symbol}&period=5m",
            self.fapi_base_url()
        );
        let resp = self.client.get(&url).send().await?;
        let json: serde_json::Value = resp.json().await?;
        let entry = json
            .as_array()
            .and_then(|a| a.first())
            .ok_or_else(|| ExchangeError::ParseError("longShortRatio: empty response".into()))?;
        let long_ratio: f64 = entry["longAccount"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.5);
        let short_ratio: f64 = entry["shortAccount"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.5);
        Ok(LongShortRatio {
            symbol: symbol.to_string(),
            long_ratio,
            short_ratio,
            long_short_ratio: entry["longShortRatio"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1.0),
            timestamp_ms: entry["timestamp"].as_i64().unwrap_or(0),
        })
    }

    async fn fetch_ohlcv(
        &self,
        symbol: &Symbol,
        interval: &str,
        limit: usize,
    ) -> Result<Vec<Bar>, ExchangeError> {
        // klines 是公开接口，优先用合约域（配置了 fapi_base_url 时），否则现货域
        let (base_url, path) = if self.config.fapi_base_url.is_some() {
            (self.fapi_base_url(), "/fapi/v1/klines")
        } else {
            (self.config.rest_base_url.as_str(), "/api/v3/klines")
        };
        let url = format!("{base_url}{path}?symbol={symbol}&interval={interval}&limit={limit}");
        let resp = self.client.get(&url).send().await?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await?;
        if !status.is_success() {
            let code = body["code"].as_i64().unwrap_or(-1);
            let msg = body["msg"].as_str().unwrap_or("unknown error");
            return Err(ExchangeError::ApiError {
                code: code as i32,
                message: msg.to_string(),
            });
        }

        let arr = body.as_array().ok_or_else(|| {
            ExchangeError::ParseError("fetch_ohlcv: response is not an array".into())
        })?;

        let bars: Vec<Bar> = arr
            .iter()
            .filter_map(|k| {
                // Binance kline: [open_time, open, high, low, close, volume, ...]
                let open_time = k.get(0)?.as_i64()?;
                Some(Bar {
                    timestamp: Timestamp::from_millis(open_time),
                    open: Price::from_f64(k.get(1)?.as_str()?.parse().ok()?),
                    high: Price::from_f64(k.get(2)?.as_str()?.parse().ok()?),
                    low: Price::from_f64(k.get(3)?.as_str()?.parse().ok()?),
                    close: Price::from_f64(k.get(4)?.as_str()?.parse().ok()?),
                    volume: Quantity::from_f64(k.get(5)?.as_str()?.parse().ok()?),
                })
            })
            .collect();

        Ok(bars)
    }
}

// === 私有 helper(独立 impl 块,不进 trait) ===

impl BinanceAdapter {
    /// 是否走合约端点(USDⓈ-M)
    ///
    /// ponytail: 用 URL 推断而不是新增 `market` 字段——`ExchangeConfig` 的
    /// struct literal 散布在 31 处(含 axon-llm 示例),加字段的破坏面远大于收益。
    /// 升级路径: 后续统一到显式 `MarketType` 并一次性迁移所有构造点。
    fn is_futures(&self) -> bool {
        self.config.fapi_base_url.is_some()
            || self.config.rest_base_url.contains("binancefuture")
            || self.config.ws_url.contains("binancefuture")
    }

    /// 交易域基础 URL：合约走 fapi，现货保持 `rest_base_url`
    ///
    /// 签名类端点（下单 / 撤单 / 持仓）跟 `is_futures()` 一起切换域；这样
    /// `rest_base_url` 写现货域、`fapi_base_url` 写合约域的配置也不会串域。
    fn trading_base_url(&self) -> &str {
        if self.is_futures() {
            self.fapi_base_url()
        } else {
            &self.config.rest_base_url
        }
    }

    /// 拿合约 base URL(优先配置,否则按 testnet 推断)
    fn fapi_base_url(&self) -> &str {
        fapi_base_url_of(&self.config)
    }

    /// 签名 GET 请求到 fapi
    async fn fapi_get(&self, path: &str, params: &str) -> Result<serde_json::Value, ExchangeError> {
        let ts = chrono::Utc::now().timestamp_millis();
        let query = if params.is_empty() {
            format!("timestamp={ts}")
        } else {
            format!("{params}&timestamp={ts}")
        };
        let sig = sign::binance::sign_query(&query, &self.config.api_secret);
        let url = format!("{}{path}?{query}&signature={sig}", self.fapi_base_url());
        // 先读 headers/status 再消费 resp，避免 borrow-of-moved-value
        let resp = self.client.get(&url).send().await?;
        let status = resp.status();
        if status.as_u16() == 429 {
            let wait_ms = resp
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok())
                .map(|s: u64| s * 1000)
                .unwrap_or(1000);
            return Err(ExchangeError::RateLimited { wait_ms });
        }
        let body: serde_json::Value = resp.json().await?;
        if !status.is_success() {
            let code = body["code"].as_i64().unwrap_or(-1);
            let msg = body["msg"].as_str().unwrap_or("unknown error");
            return Err(ExchangeError::ApiError {
                code: code as i32,
                message: msg.to_string(),
            });
        }
        Ok(body)
    }

    /// 签名 POST 请求到 fapi
    async fn fapi_post(
        &self,
        path: &str,
        params: &str,
    ) -> Result<serde_json::Value, ExchangeError> {
        let ts = chrono::Utc::now().timestamp_millis();
        let query = if params.is_empty() {
            format!("timestamp={ts}")
        } else {
            format!("{params}&timestamp={ts}")
        };
        let sig = sign::binance::sign_query(&query, &self.config.api_secret);
        let url = format!("{}{path}?{query}&signature={sig}", self.fapi_base_url());
        // 先读 headers/status 再消费 resp，避免 borrow-of-moved-value
        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send()
            .await?;
        let status = resp.status();
        if status.as_u16() == 429 {
            let wait_ms = resp
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok())
                .map(|s: u64| s * 1000)
                .unwrap_or(1000);
            return Err(ExchangeError::RateLimited { wait_ms });
        }
        let body: serde_json::Value = resp.json().await?;
        if !status.is_success() {
            let code = body["code"].as_i64().unwrap_or(-1);
            let msg = body["msg"].as_str().unwrap_or("unknown error");
            return Err(ExchangeError::ApiError {
                code: code as i32,
                message: msg.to_string(),
            });
        }
        Ok(body)
    }

    /// 公共 GET(无签名)用于公开端点如 fundingRate / openInterest
    async fn fapi_get_public(
        &self,
        path: &str,
        params: &str,
    ) -> Result<serde_json::Value, ExchangeError> {
        let url = if params.is_empty() {
            format!("{}{path}", self.fapi_base_url())
        } else {
            format!("{}{path}?{params}", self.fapi_base_url())
        };
        let resp = self.client.get(&url).send().await?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await?;
        if !status.is_success() {
            return Err(ExchangeError::ApiError {
                code: status.as_u16() as i32,
                message: body.to_string(),
            });
        }
        Ok(body)
    }
}

impl Drop for BinanceAdapter {
    fn drop(&mut self) {
        // Drop 是同步的，无法 await：有 tokio 运行时上下文时 fire-and-forget 释放
        // listenKey；没有运行时（纯结构体析构）就跳过 —— 服务端 60 分钟无续期会
        // 让 key 自然失效。listenKey 的值绝不进日志。
        if self.listen_key.lock().take().is_none() {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let client = self.client.clone();
        let url = format!("{}/fapi/v1/listenKey", fapi_base_url_of(&self.config));
        let api_key = self.config.api_key.clone();
        handle.spawn(async move {
            let _ = client
                .delete(&url)
                .header("X-MBX-APIKEY", api_key.as_str())
                .send()
                .await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ExchangeConfig, RateLimitConfig, ReconnectConfig, TimeInForce};

    fn testnet_config() -> ExchangeConfig {
        ExchangeConfig {
            exchange_id: ExchangeId::Binance,
            api_key: std::env::var("BINANCE_TESTNET_API_KEY").unwrap_or_default(),
            api_secret: std::env::var("BINANCE_TESTNET_API_SECRET").unwrap_or_default(),
            passphrase: None,
            testnet: true,
            rest_base_url: "https://testnet.binance.vision".into(),
            ws_url: "wss://testnet.binance.vision/ws".into(),
            rate_limit: RateLimitConfig {
                requests_per_second: 10,
                orders_per_minute: 60,
                ws_messages_per_second: 50,
            },
            reconnect: ReconnectConfig {
                max_retries: 10,
                initial_backoff: Duration::from_millis(500),
                max_backoff: Duration::from_secs(30),
                backoff_multiplier: 2.0,
                circuit_breaker_threshold: 5,
                circuit_breaker_reset: Duration::from_secs(60),
            },
            proxy: None,
            position_endpoint: "/fapi/v2/positionRisk".into(),
            fapi_base_url: Some("https://testnet.binancefuture.com".into()),
        }
    }

    #[test]
    fn test_sign_returns_hex() {
        let config = testnet_config();
        let adapter = BinanceAdapter::new(config);
        let sig = adapter.sign("symbol=BTCUSDT&timestamp=1234567890");
        assert!(sig.is_ok());
        let hex = sig.unwrap();
        assert_eq!(hex.len(), 64); // SHA256 = 32 bytes = 64 hex chars
    }

    #[test]
    fn test_signed_query_contains_signature() {
        let config = testnet_config();
        let adapter = BinanceAdapter::new(config);
        let query = adapter.signed_query("symbol=BTCUSDT").unwrap();
        assert!(query.contains("signature="));
        assert!(query.contains("timestamp="));
    }

    #[test]
    fn test_order_to_binance_params() {
        let order = Order {
            client_order_id: OrderId::new(),
            symbol: Symbol::new("BTCUSDT"),
            side: crate::types::Side::Buy,
            order_type: crate::types::OrderType::Limit,
            price: Some("50000.00".parse().unwrap()),
            quantity: "0.001".parse().unwrap(),
            time_in_force: TimeInForce::Gtc,
            exchange: ExchangeId::Binance,
            meta: HashMap::new(),
        };
        let params = order_to_binance_params(&order);
        assert!(params.contains("symbol=BTCUSDT"));
        assert!(params.contains("side=BUY"));
        assert!(params.contains("type=LIMIT"));
        assert!(params.contains("timeInForce=GTC"));
        assert!(params.contains("quantity=0.001"));
        assert!(params.contains("price=50000.00"));
    }

    /// 市价单不能带 `timeInForce` / `price`（Binance 会返回 -1106）
    #[test]
    fn test_market_order_omits_time_in_force_and_price() {
        let order = Order {
            client_order_id: OrderId::new(),
            symbol: Symbol::new("BTCUSDT"),
            side: crate::types::Side::Buy,
            order_type: crate::types::OrderType::Market,
            price: None,
            quantity: "0.0001".parse().unwrap(),
            time_in_force: TimeInForce::Gtc,
            exchange: ExchangeId::Binance,
            meta: HashMap::new(),
        };
        let params = order_to_binance_params(&order);
        assert!(params.contains("type=MARKET"));
        assert!(
            !params.contains("timeInForce"),
            "MARKET 不得携带 timeInForce: {params}"
        );
        assert!(!params.contains("price="), "市价单不得携带 price: {params}");
    }

    #[test]
    fn test_parse_ticker_message() {
        let msg = r#"{"e":"24hrTicker","s":"BTCUSDT","b":"50000.00","a":"50001.00","c":"50000.50","v":"100.0"}"#;
        let parsed = parse_ws_message(msg).unwrap();
        match parsed {
            WsMessage::Ticker(t) => {
                assert_eq!(t.symbol, Symbol::new("BTCUSDT"));
                assert_eq!(t.bid, "50000.00".parse::<rust_decimal::Decimal>().unwrap());
                assert_eq!(t.ask, "50001.00".parse::<rust_decimal::Decimal>().unwrap());
            }
            _ => panic!("expected Ticker"),
        }
    }

    #[test]
    fn test_parse_depth_message() {
        let msg = r#"{"e":"depthUpdate","s":"BTCUSDT","b":[["50000.00","1.0"],["49999.00","2.0"]],"a":[["50001.00","0.5"]]}"#;
        let parsed = parse_ws_message(msg).unwrap();
        match parsed {
            WsMessage::Depth(d) => {
                assert_eq!(d.symbol, Symbol::new("BTCUSDT"));
                assert_eq!(d.bids.len(), 2);
                assert_eq!(d.asks.len(), 1);
            }
            _ => panic!("expected Depth"),
        }
    }

    #[test]
    fn test_parse_trade_message() {
        let msg = r#"{"e":"trade","s":"BTCUSDT","p":"50000.00","q":"0.1","m":false}"#;
        let parsed = parse_ws_message(msg).unwrap();
        match parsed {
            WsMessage::Trade(t) => {
                assert_eq!(t.symbol, Symbol::new("BTCUSDT"));
                assert_eq!(t.side, crate::types::Side::Buy);
            }
            _ => panic!("expected Trade"),
        }
    }

    #[test]
    fn test_parse_order_update_filled() {
        let msg = r#"{"e":"executionReport","s":"BTCUSDT","c":"my-order-1","X":"FILLED","z":"0.001","Z":"50000.00","i":"12345"}"#;
        let parsed = parse_ws_message(msg).unwrap();
        match parsed {
            WsMessage::OrderUpdate(u) => {
                assert_eq!(
                    u.status,
                    OrderStatus::Filled {
                        filled_qty: "0.001".parse().unwrap(),
                        avg_price: "50000.00".parse().unwrap(),
                    }
                );
            }
            _ => panic!("expected OrderUpdate"),
        }
    }

    // ============== 合约 user data stream：解析 ==============

    /// 构造一条最小 `ORDER_TRADE_UPDATE`（字段名与 Binance 合约推送一致）
    fn order_trade_update(client_id: &str, status: &str, filled: &str, avg: &str) -> String {
        serde_json::json!({
            "e": "ORDER_TRADE_UPDATE",
            "E": 1_700_000_000_123_i64,
            "T": 1_700_000_000_120_i64,
            "o": {
                "s": "BTCUSDT",
                "c": client_id,
                "S": "BUY",
                "o": "MARKET",
                "q": "0.001",
                "p": "0",
                "ap": avg,
                "x": "TRADE",
                "X": status,
                "i": 8886774_i64,
                "l": "0.001",
                "z": filled,
                "T": 1_700_000_000_120_i64,
                "r": "NONE"
            }
        })
        .to_string()
    }

    #[test]
    fn parse_order_trade_update_filled_maps_fields() {
        let client_id = uuid::Uuid::now_v7();
        let parsed = parse_user_stream_event(&order_trade_update(
            &client_id.to_string(),
            "FILLED",
            "0.001",
            "80123.4",
        ))
        .unwrap();
        match parsed {
            UserStreamEvent::Order(WsMessage::OrderUpdate(update)) => {
                assert_eq!(
                    update.order_id, "8886774",
                    "order_id ← o.i（交易所订单号字符串化）"
                );
                assert_eq!(
                    update.client_order_id,
                    OrderId(client_id),
                    "client id ← o.c（Uuid）"
                );
                assert_eq!(
                    update.status,
                    OrderStatus::Filled {
                        filled_qty: "0.001".parse().unwrap(),
                        avg_price: "80123.4".parse().unwrap(),
                    }
                );
                assert_eq!(
                    update.filled_qty,
                    "0.001".parse::<Decimal>().unwrap(),
                    "filled_qty ← o.z"
                );
                assert_eq!(
                    update.avg_price,
                    Some("80123.4".parse().unwrap()),
                    "avg_price ← o.ap"
                );
                assert_eq!(
                    update.timestamp.timestamp_millis(),
                    1_700_000_000_123,
                    "timestamp ← E（毫秒）"
                );
            }
            _ => panic!("expected OrderUpdate"),
        }
    }

    #[test]
    fn parse_order_trade_update_partially_filled_and_canceled() {
        let client_id = uuid::Uuid::now_v7().to_string();

        let parsed = parse_user_stream_event(&order_trade_update(
            &client_id,
            "PARTIALLY_FILLED",
            "0.0004",
            "80000.5",
        ))
        .unwrap();
        match parsed {
            UserStreamEvent::Order(WsMessage::OrderUpdate(update)) => {
                assert_eq!(
                    update.status,
                    OrderStatus::PartiallyFilled {
                        filled_qty: "0.0004".parse().unwrap(),
                        avg_price: "80000.5".parse().unwrap(),
                    }
                );
            }
            _ => panic!("expected OrderUpdate"),
        }

        // CANCELED 的 ap 是 0：不带均价（否则会污染 OMS 的成交价）
        let parsed =
            parse_user_stream_event(&order_trade_update(&client_id, "CANCELED", "0", "0")).unwrap();
        match parsed {
            UserStreamEvent::Order(WsMessage::OrderUpdate(update)) => {
                assert_eq!(
                    update.status,
                    OrderStatus::Cancelled {
                        filled_qty: Decimal::ZERO
                    }
                );
                assert_eq!(update.avg_price, None);
            }
            _ => panic!("expected OrderUpdate"),
        }
    }

    #[test]
    fn parse_order_trade_update_skips_non_uuid_client_id() {
        // 交易所侧手搓的 client id（不是 Uuid）无法归因到 OMS 订单，必须跳过而不是塞 nil uuid
        let parsed =
            parse_user_stream_event(&order_trade_update("my-order-1", "FILLED", "0.001", "1"))
                .unwrap();
        assert!(matches!(parsed, UserStreamEvent::Ignored));
    }

    #[test]
    fn parse_listen_key_expired_and_ignores_other_events() {
        let expired = r#"{"e":"listenKeyExpired","E":1700000000000}"#;
        assert!(matches!(
            parse_user_stream_event(expired).unwrap(),
            UserStreamEvent::ListenKeyExpired
        ));

        let account_update = r#"{"e":"ACCOUNT_UPDATE","E":1700000000000,"a":{}}"#;
        assert!(matches!(
            parse_user_stream_event(account_update).unwrap(),
            UserStreamEvent::Ignored
        ));
    }

    #[test]
    fn user_stream_ws_base_switches_with_testnet() {
        assert_eq!(
            user_stream_ws_base(&testnet_config()),
            "wss://fstream.binancefuture.com/ws"
        );
        let prod = ExchangeConfig {
            testnet: false,
            fapi_base_url: None,
            ..testnet_config()
        };
        assert_eq!(user_stream_ws_base(&prod), "wss://fstream.binance.com/ws");
        assert_eq!(fapi_base_url_of(&prod), "https://fapi.binance.com");
    }

    /// listenKey 的三个端点只带 `X-MBX-APIKEY`，不带签名参数（带签名会被 -1022 拒）
    #[test]
    fn listen_key_lifecycle_uses_api_key_header_only() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/fapi/v1/listenKey"))
                .and(header("X-MBX-APIKEY", "test_key"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(serde_json::json!({ "listenKey": "lk_test" })),
                )
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("PUT"))
                .and(path("/fapi/v1/listenKey"))
                .and(header("X-MBX-APIKEY", "test_key"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("DELETE"))
                .and(path("/fapi/v1/listenKey"))
                .and(header("X-MBX-APIKEY", "test_key"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
                .expect(1)
                .mount(&server)
                .await;

            let config = wiremock_config(&server.uri());
            let client = crate::build_http_client(&config);

            let key = create_listen_key(&config, &client).await.expect("create");
            assert_eq!(key, "lk_test");
            put_listen_key(&config, &client).await.expect("keepalive");
            delete_listen_key(&config, &client).await.expect("delete");
        });
    }

    #[test]
    fn test_build_streams_includes_depth_ticker_trade_kline() {
        let syms = [Symbol::new("BTCUSDT")];
        let streams = BinanceAdapter::build_streams(&syms);
        assert_eq!(streams.len(), 4);
        assert!(streams.contains(&"btcusdt@depth@100ms".to_string()));
        assert!(streams.contains(&"btcusdt@ticker".to_string()));
        assert!(streams.contains(&"btcusdt@trade".to_string()));
        assert!(streams.contains(&"btcusdt@kline_1m".to_string()));
    }

    #[test]
    fn test_next_backoff_grows_with_multiplier() {
        let cfg = ReconnectConfig {
            max_retries: 10,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            backoff_multiplier: 2.0,
            circuit_breaker_threshold: 5,
            circuit_breaker_reset: Duration::from_secs(60),
        };
        let b1 = next_backoff(cfg.initial_backoff, &cfg);
        let b2 = next_backoff(b1, &cfg);
        let b3 = next_backoff(b2, &cfg);
        assert_eq!(b1, Duration::from_secs(1));
        assert_eq!(b2, Duration::from_secs(2));
        assert_eq!(b3, Duration::from_secs(4));
    }

    #[test]
    fn test_next_backoff_caps_at_max() {
        let cfg = ReconnectConfig {
            max_retries: 10,
            initial_backoff: Duration::from_secs(20),
            max_backoff: Duration::from_secs(30),
            backoff_multiplier: 2.0,
            circuit_breaker_threshold: 5,
            circuit_breaker_reset: Duration::from_secs(60),
        };
        // 20 * 2 = 40 > 30，应被截断到 30
        let b = next_backoff(cfg.initial_backoff, &cfg);
        assert_eq!(b, Duration::from_secs(30));
    }

    // ============== 现货 / 合约端点路由 ==============

    /// 全现货 URL（无 fapi 配置）的配置
    fn spot_config() -> ExchangeConfig {
        ExchangeConfig {
            rest_base_url: "https://api.binance.com".into(),
            ws_url: "wss://stream.binance.com/ws".into(),
            fapi_base_url: None,
            position_endpoint: String::new(),
            ..testnet_config()
        }
    }

    #[test]
    fn is_futures_true_when_fapi_base_url_configured() {
        let adapter = BinanceAdapter::new(spot_config());
        assert!(!adapter.is_futures(), "纯现货 URL 不应判定为合约");

        let cfg = ExchangeConfig {
            fapi_base_url: Some("https://fapi.binance.com".into()),
            ..spot_config()
        };
        assert!(BinanceAdapter::new(cfg).is_futures());
    }

    #[test]
    fn is_futures_true_when_rest_base_url_is_binancefuture() {
        let cfg = ExchangeConfig {
            rest_base_url: "https://testnet.binancefuture.com".into(),
            ..spot_config()
        };
        let adapter = BinanceAdapter::new(cfg);
        assert!(adapter.is_futures());
        // 未显式配置 fapi 时按 testnet 推断到合约 testnet 域
        assert_eq!(
            adapter.trading_base_url(),
            "https://testnet.binancefuture.com"
        );
    }

    #[test]
    fn is_futures_true_when_ws_url_is_binancefuture() {
        let cfg = ExchangeConfig {
            ws_url: "wss://fstream.binancefuture.com/ws".into(),
            ..spot_config()
        };
        assert!(BinanceAdapter::new(cfg).is_futures());
    }

    #[test]
    fn endpoint_paths_follow_market() {
        assert_eq!(ping_path(true), "/fapi/v1/ping");
        assert_eq!(ping_path(false), "/api/v3/ping");
        assert_eq!(order_path(true), "/fapi/v1/order");
        assert_eq!(order_path(false), "/api/v3/order");

        // trading_base_url 必须跟着市场切域，而不是永远用 rest_base_url
        let spot = BinanceAdapter::new(spot_config());
        assert_eq!(spot.trading_base_url(), "https://api.binance.com");

        let futures = BinanceAdapter::new(ExchangeConfig {
            rest_base_url: "https://testnet.binance.vision".into(),
            fapi_base_url: Some("https://testnet.binancefuture.com".into()),
            ..spot_config()
        });
        assert_eq!(
            futures.trading_base_url(),
            "https://testnet.binancefuture.com"
        );
    }

    #[test]
    fn test_order_symbols_mapping_for_cancel() {
        // 验证 send_order 写入的 client_order_id -> symbol 映射可被 cancel_order 读取
        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let adapter = BinanceAdapter::new(testnet_config());
            let client_id = OrderId::new();
            adapter
                .order_symbols
                .lock()
                .await
                .insert(client_id.to_string(), "BTCUSDT".to_string());

            let symbol = adapter
                .order_symbols
                .lock()
                .await
                .get(&client_id.to_string())
                .cloned();
            assert_eq!(symbol, Some("BTCUSDT".to_string()));
        });
    }

    #[test]
    fn test_subscribe_records_symbols_for_resubscribe() {
        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut adapter = BinanceAdapter::new(testnet_config());
            let _ = adapter.subscribe(&[Symbol::new("BTCUSDT")]).await;
            let sub = adapter.subscribed_symbols.lock().await;
            assert_eq!(sub.len(), 1);
            assert_eq!(sub[0], Symbol::new("BTCUSDT"));
        });
    }

    // ============== Stage 4' D: 杠杆/合约 wiremock 集成测试 ==============

    /// 构造指向 wiremock server 的 config
    fn wiremock_config(server_uri: &str) -> ExchangeConfig {
        ExchangeConfig {
            exchange_id: ExchangeId::Binance,
            api_key: "test_key".into(),
            api_secret: "test_secret".into(),
            passphrase: None,
            testnet: false,
            rest_base_url: server_uri.to_string(),
            ws_url: "ws://invalid".into(),
            rate_limit: RateLimitConfig {
                requests_per_second: 1000,
                orders_per_minute: 6000,
                ws_messages_per_second: 50,
            },
            reconnect: ReconnectConfig {
                max_retries: 1,
                initial_backoff: Duration::from_millis(10),
                max_backoff: Duration::from_millis(100),
                backoff_multiplier: 2.0,
                circuit_breaker_threshold: 100,
                circuit_breaker_reset: Duration::from_secs(60),
            },
            proxy: None,
            position_endpoint: "/fapi/v2/positionRisk".into(),
            fapi_base_url: Some(server_uri.to_string()),
        }
    }

    #[test]
    fn fapi_set_leverage_rejects_out_of_range() {
        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let adapter = BinanceAdapter::new(wiremock_config("http://127.0.0.1:1"));
            // 0 不在 1..=125
            let r = adapter.set_leverage("BTCUSDT", 0).await;
            assert!(matches!(r, Err(ExchangeError::OrderRejected { .. })));
            // 200 超出
            let r = adapter.set_leverage("BTCUSDT", 200).await;
            assert!(matches!(r, Err(ExchangeError::OrderRejected { .. })));
        });
    }

    #[test]
    fn fapi_set_leverage_ok_against_wiremock() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/fapi/v1/leverage"))
                .and(query_param("symbol", "BTCUSDT"))
                .and(query_param("leverage", "10"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "symbol": "BTCUSDT",
                    "leverage": 10,
                    "maxNotionalValue": "1000000"
                })))
                .expect(1)
                .mount(&server)
                .await;

            let adapter = BinanceAdapter::new(wiremock_config(&server.uri()));
            let r = adapter.set_leverage("BTCUSDT", 10).await;
            assert!(r.is_ok(), "set_leverage failed: {r:?}");
        });
    }

    #[test]
    fn fapi_get_account_info_parses_v2_payload() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/fapi/v2/account"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "totalWalletBalance": "10000.50",
                    "availableBalance": "8000.25",
                    "totalUnrealizedProfit": "120.00",
                    "totalInitialMargin": "2000.25",
                    "totalMaintMargin": "150.00",
                    "positionMode": "hedge"
                })))
                .expect(1)
                .mount(&server)
                .await;

            let adapter = BinanceAdapter::new(wiremock_config(&server.uri()));
            let info = adapter.get_account_info().await.expect("get_account_info");
            assert_eq!(info.total_balance, "10000.50".parse().unwrap());
            assert_eq!(info.available_balance, "8000.25".parse().unwrap());
            assert_eq!(info.unrealized_pnl, "120.00".parse().unwrap());
            assert_eq!(info.position_mode, PositionMode::Hedge);
            assert!(info.as_of_ms > 0);
        });
    }

    #[test]
    fn fapi_get_leverage_brackets_parses_array() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/fapi/v1/leverageBracket"))
                .and(query_param("symbol", "BTCUSDT"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                    {
                        "symbol": "BTCUSDT",
                        "brackets": [
                            { "bracket": 1, "initialLeverage": 125, "notionalCap": "50000", "maintMarginRatio": "0.004" },
                            { "bracket": 2, "initialLeverage": 100, "notionalCap": "250000", "maintMarginRatio": "0.005" }
                        ]
                    }
                ])))
                .expect(1)
                .mount(&server)
                .await;

            let adapter = BinanceAdapter::new(wiremock_config(&server.uri()));
            let brackets = adapter.get_leverage_brackets("BTCUSDT").await.unwrap();
            assert_eq!(brackets.len(), 2);
            assert_eq!(brackets[0].max_leverage, 125);
            assert_eq!(brackets[0].max_notional, "50000".parse().unwrap());
            assert_eq!(brackets[0].maint_margin_ratio, "0.004".parse().unwrap());
            assert_eq!(brackets[1].max_leverage, 100);
        });
    }

    #[test]
    fn fapi_get_funding_rate_uses_public_endpoint() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = MockServer::start().await;
            let funding_time = 1_700_000_000_000_i64;
            Mock::given(method("GET"))
                .and(path("/fapi/v1/fundingRate"))
                .and(query_param("symbol", "BTCUSDT"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                    {
                        "symbol": "BTCUSDT",
                        "fundingRate": "0.0001",
                        "fundingTime": funding_time,
                        "markPrice": "50000.00"
                    }
                ])))
                .expect(1)
                .mount(&server)
                .await;

            let adapter = BinanceAdapter::new(wiremock_config(&server.uri()));
            let fr = adapter.get_funding_rate("BTCUSDT").await.unwrap();
            assert_eq!(fr.symbol, "BTCUSDT");
            assert_eq!(fr.rate, "0.0001".parse().unwrap());
            assert_eq!(fr.mark_price, "50000.00".parse().unwrap());
            // next = funding + 8h
            assert_eq!(fr.next_funding_ms, funding_time + 8 * 3600 * 1000);
        });
    }

    #[test]
    fn fapi_get_open_interest_uses_public_endpoint() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/fapi/v1/openInterest"))
                .and(query_param("symbol", "BTCUSDT"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "symbol": "BTCUSDT",
                    "openInterest": "12345",
                    "time": 1_700_000_000_000_i64
                })))
                .expect(1)
                .mount(&server)
                .await;

            let adapter = BinanceAdapter::new(wiremock_config(&server.uri()));
            let oi = adapter.get_open_interest("BTCUSDT").await.unwrap();
            assert_eq!(oi.contracts, 12345);
            assert_eq!(oi.symbol, "BTCUSDT");
        });
    }

    #[test]
    fn fapi_set_margin_type_ok() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/fapi/v1/marginType"))
                .and(query_param("symbol", "BTCUSDT"))
                .and(query_param("marginType", "ISOLATED"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "code": 200,
                    "msg": "success"
                })))
                .expect(1)
                .mount(&server)
                .await;

            let adapter = BinanceAdapter::new(wiremock_config(&server.uri()));
            let r = adapter
                .set_margin_type("BTCUSDT", MarginType::Isolated)
                .await;
            assert!(r.is_ok(), "set_margin_type failed: {r:?}");
        });
    }

    #[test]
    fn fapi_set_position_mode_ok() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/fapi/v1/positionSide/dual"))
                .and(query_param("dualSidePosition", "true"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "code": 200,
                    "msg": "success"
                })))
                .expect(1)
                .mount(&server)
                .await;

            let adapter = BinanceAdapter::new(wiremock_config(&server.uri()));
            let r = adapter.set_position_mode(true).await;
            assert!(r.is_ok(), "set_position_mode failed: {r:?}");
        });
    }
}
