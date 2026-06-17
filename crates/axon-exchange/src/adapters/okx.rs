use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures_util::stream::SplitStream;
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use reqwest::Client;
use rust_decimal::Decimal;
use sha2::Sha256;
use tokio::sync::{Mutex, Notify, mpsc};
use tokio::time::{Duration, interval};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::error::ExchangeError;
use crate::sign;
use crate::traits::ExchangeAdapter;
use crate::types::{
    AccountBalance, AccountInfo, DepthSnapshot, ExchangeConfig, ExchangeId, FundingRate,
    LeverageBracket, LongShortRatio, MarginType, OpenInterest, Order, OrderId, OrderStatus,
    Position, PositionMode, ReconnectConfig, Symbol, Ticker, WsMessage,
};

type HmacSha256 = Hmac<Sha256>;
type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;
type WsRead = SplitStream<WsStream>;

pub struct OkxAdapter {
    config: ExchangeConfig,
    client: Client,
    market_tx: mpsc::Sender<WsMessage>,
    market_rx: Mutex<Option<mpsc::Receiver<WsMessage>>>,
    depth_cache: Arc<Mutex<HashMap<String, DepthSnapshot>>>,
    ticker_cache: Arc<Mutex<HashMap<String, Ticker>>>,
    connected: Mutex<bool>,
    /// clOrdId -> instId 映射，用于撤单时获取正确的 instId
    order_inst_ids: Mutex<HashMap<String, String>>,
    /// 已订阅的 symbol 列表，重连时复用
    subscribed_symbols: Arc<Mutex<Vec<Symbol>>>,
    /// 共享 WebSocket 写入端，供 subscribe 发送 SUBSCRIBE 消息
    ws_writer: Arc<Mutex<Option<Arc<Mutex<WsSink>>>>>,
    /// 优雅关闭通知
    shutdown: Arc<Notify>,
}

impl OkxAdapter {
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
            connected: Mutex::new(false),
            order_inst_ids: Mutex::new(HashMap::new()),
            subscribed_symbols: Arc::new(Mutex::new(Vec::new())),
            ws_writer: Arc::new(Mutex::new(None)),
            shutdown: Arc::new(Notify::new()),
        }
    }

    /// OKX 签名：base64(HMAC-SHA256(timestamp + method + path + body, secret))
    fn sign(
        &self,
        timestamp: &str,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<String, ExchangeError> {
        let prehash = format!("{timestamp}{method}{path}{body}");
        let mut mac = HmacSha256::new_from_slice(self.config.api_secret.as_bytes())
            .map_err(|e| ExchangeError::AuthenticationFailed(e.to_string()))?;
        mac.update(prehash.as_bytes());
        Ok(BASE64.encode(mac.finalize().into_bytes()))
    }

    /// 构造请求头
    fn auth_headers(
        &self,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<Vec<(String, String)>, ExchangeError> {
        let timestamp = chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string();
        let sign = self.sign(&timestamp, method, path, body)?;
        let mut headers = vec![
            ("OK-ACCESS-KEY".into(), self.config.api_key.clone()),
            ("OK-ACCESS-SIGN".into(), sign),
            ("OK-ACCESS-TIMESTAMP".into(), timestamp),
            (
                "OK-ACCESS-PASSPHRASE".into(),
                self.config.passphrase.clone().unwrap_or_default(),
            ),
        ];
        // OKX 测试网（模拟盘）需要此 header
        if self.config.testnet {
            headers.push(("x-simulated-trading".into(), "1".into()));
        }
        Ok(headers)
    }

    /// REST GET 请求
    async fn rest_get(&self, path: &str) -> Result<serde_json::Value, ExchangeError> {
        let url = format!("{}{path}", self.config.rest_base_url);
        let headers = self.auth_headers("GET", path, "")?;
        let mut req = self.client.get(&url);
        for (k, v) in &headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req.send().await?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await?;
        if !status.is_success() || body["code"].as_str() != Some("0") {
            let code = body["code"].as_str().unwrap_or("-1");
            let msg = body["msg"].as_str().unwrap_or("unknown error");
            return Err(ExchangeError::ApiError {
                code: code.parse().unwrap_or(-1),
                message: msg.to_string(),
            });
        }
        Ok(body)
    }

    /// REST POST 请求
    async fn rest_post(&self, path: &str, body: &str) -> Result<serde_json::Value, ExchangeError> {
        let url = format!("{}{path}", self.config.rest_base_url);
        let headers = self.auth_headers("POST", path, body)?;
        let mut req = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body.to_string());
        for (k, v) in &headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req.send().await?;
        let status = resp.status();
        let resp_body: serde_json::Value = resp.json().await?;
        if !status.is_success() || resp_body["code"].as_str() != Some("0") {
            let code = resp_body["code"].as_str().unwrap_or("-1");
            let msg = resp_body["msg"].as_str().unwrap_or("unknown error");
            return Err(ExchangeError::ApiError {
                code: code.parse().unwrap_or(-1),
                message: msg.to_string(),
            });
        }
        Ok(resp_body)
    }

    /// REST POST 请求（下单）
    async fn rest_post_orders(&self, body: &str) -> Result<serde_json::Value, ExchangeError> {
        self.rest_post("/api/v5/trade/order", body).await
    }

    /// 启动 WebSocket 连接（监督任务：包含重连与重订阅）
    async fn start_ws(&self) -> Result<(), ExchangeError> {
        let ws_url = if self.config.testnet {
            "wss://wspap.okx.com:8443/ws/v5/public?brokerId=9999"
        } else {
            "wss://ws.okx.com:8443/ws/v5/public"
        };

        let reconnect_cfg = self.config.reconnect.clone();
        let tx = self.market_tx.clone();
        let depth_cache = self.depth_cache.clone();
        let ticker_cache = self.ticker_cache.clone();
        let subscribed = self.subscribed_symbols.clone();
        let ws_writer_slot = self.ws_writer.clone();
        let shutdown = self.shutdown.clone();
        let url = ws_url.to_string();

        tokio::spawn(async move {
            let mut backoff = reconnect_cfg.initial_backoff;
            let mut attempt: u32 = 0;
            loop {
                if attempt > 0 {
                    tokio::select! {
                        _ = shutdown.notified() => break,
                        _ = tokio::time::sleep(backoff) => {}
                    }
                }

                match tokio_tungstenite::connect_async(&url).await {
                    Ok((ws_stream, _)) => {
                        attempt = 0;
                        backoff = reconnect_cfg.initial_backoff;
                        let (ws_write, ws_read) = ws_stream.split();
                        let writer = Arc::new(Mutex::new(ws_write));
                        *ws_writer_slot.lock().await = Some(writer.clone());

                        // 连接建立后立即按已订阅列表重新发送 SUBSCRIBE（首次连接与重连均走此路径）
                        Self::send_subscribe_to_writer(&writer, &subscribed).await;

                        // 启动 Ping 保活（OKX 公共通道使用 "ping" 文本）
                        let writer_for_ping = writer.clone();
                        let shutdown_for_ping = shutdown.clone();
                        let ping_handle = tokio::spawn(async move {
                            let mut ticker = interval(Duration::from_secs(25));
                            loop {
                                tokio::select! {
                                    _ = shutdown_for_ping.notified() => break,
                                    _ = ticker.tick() => {
                                        let mut w = writer_for_ping.lock().await;
                                        if w.send(Message::Text("ping".into())).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                        });

                        // 读取循环（断线后返回）
                        Self::run_read_loop(
                            ws_read,
                            tx.clone(),
                            writer.clone(),
                            depth_cache.clone(),
                            ticker_cache.clone(),
                            shutdown.clone(),
                        )
                        .await;

                        // 清理
                        *ws_writer_slot.lock().await = None;
                        ping_handle.abort();
                        tracing::warn!("OKX WebSocket read loop exited, will reconnect");
                    }
                    Err(e) => {
                        attempt += 1;
                        if attempt > reconnect_cfg.max_retries {
                            tracing::error!(
                                "OKX WebSocket reconnect failed after {} attempts: {}",
                                reconnect_cfg.max_retries,
                                e
                            );
                            break;
                        }
                        tracing::warn!("OKX WebSocket connect failed (attempt {}): {}", attempt, e);
                        backoff = Self::next_backoff(backoff, &reconnect_cfg);
                    }
                }
            }
        });

        Ok(())
    }

    /// 读取循环：解析消息后更新缓存并通过 market_tx 转发
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
                            let mut w = writer.lock().await;
                            if w.send(Message::Pong(data)).await.is_err() {
                                return;
                            }
                        }
                        Ok(Message::Pong(_)) => {
                            let _ = tx.send(WsMessage::Pong).await;
                        }
                        Ok(Message::Close(_)) => return,
                        Err(e) => {
                            tracing::warn!("OKX WebSocket read error: {}", e);
                            return;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// 构造订阅消息并通过 writer 发送
    async fn send_subscribe_to_writer(
        writer: &Arc<Mutex<WsSink>>,
        subscribed: &Arc<Mutex<Vec<Symbol>>>,
    ) {
        let symbols = subscribed.lock().await.clone();
        if symbols.is_empty() {
            return;
        }
        let args: Vec<serde_json::Value> = symbols
            .iter()
            .flat_map(|s| {
                let inst_id = s.to_string();
                vec![
                    serde_json::json!({"channel": "tickers", "instId": inst_id}),
                    serde_json::json!({"channel": "books5", "instId": inst_id}),
                    serde_json::json!({"channel": "trades", "instId": inst_id}),
                ]
            })
            .collect();
        let payload = serde_json::json!({"op": "subscribe", "args": args}).to_string();
        let mut w = writer.lock().await;
        if let Err(e) = w.send(Message::Text(payload)).await {
            tracing::warn!("OKX subscribe send failed: {}", e);
        }
    }

    /// 指数退避重连
    fn next_backoff(current: Duration, cfg: &ReconnectConfig) -> Duration {
        let next = current.mul_f64(cfg.backoff_multiplier);
        if next > cfg.max_backoff {
            cfg.max_backoff
        } else {
            next
        }
    }

    // === 杠杆/合约 helper(Stage 4' D) ===
    // 这些是私有方法,不属于 ExchangeAdapter trait,放独立 impl 块

    /// OKX 签名 POST/GET 私有端点,返回 JSON
    pub async fn send_okx_signed(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<serde_json::Value, ExchangeError> {
        let body_str = body.unwrap_or("");
        let headers = sign::okx::build_headers(
            &self.config.api_key,
            &self.config.api_secret,
            self.config.passphrase.as_deref().unwrap_or(""),
            method,
            path,
            body_str,
        );
        let url = format!("{}{}", self.config.rest_base_url, path);
        let mut req = match method {
            "GET" => self.client.get(&url),
            "POST" => self.client.post(&url),
            other => {
                return Err(ExchangeError::OrderRejected {
                    reason: format!("unsupported HTTP method {other}"),
                });
            }
        };
        req = req
            .header("OK-ACCESS-KEY", &headers.ok_access_key)
            .header("OK-ACCESS-TIMESTAMP", &headers.ok_access_timestamp)
            .header("OK-ACCESS-SIGN", &headers.ok_access_sign)
            .header("OK-ACCESS-PASSPHRASE", &headers.ok_access_passphrase);
        if let Some(b) = body {
            req = req
                .header("Content-Type", "application/json")
                .body(b.to_string());
        }
        let resp = req.send().await?;
        self.parse_okx_response(resp).await
    }

    /// OKX 公开端点(无签名)
    pub async fn send_okx_public(&self, path: &str) -> Result<serde_json::Value, ExchangeError> {
        let url = format!("{}{}", self.config.rest_base_url, path);
        let resp = self.client.get(&url).send().await?;
        self.parse_okx_response(resp).await
    }

    /// 统一解析 OKX 响应:code != "0" / 401 / 429 / 5xx → 对应错误
    pub async fn parse_okx_response(
        &self,
        resp: reqwest::Response,
    ) -> Result<serde_json::Value, ExchangeError> {
        let status = resp.status();
        // 先判断 status,避免对 resp.json() 之后还要借用 resp
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
        if status.as_u16() == 401 {
            return Err(ExchangeError::AuthenticationFailed(
                "OKX 401 unauthorized".into(),
            ));
        }
        if status.is_server_error() {
            return Err(ExchangeError::ApiError {
                code: status.as_u16() as i32,
                message: status.to_string(),
            });
        }
        let json: serde_json::Value = resp.json().await?;
        if json["code"].as_str() != Some("0") {
            let code = json["code"].as_str().unwrap_or("-1");
            let msg = json["msg"].as_str().unwrap_or("unknown");
            return Err(ExchangeError::ApiError {
                code: code.parse().unwrap_or(-1),
                message: msg.to_string(),
            });
        }
        Ok(json)
    }
}

/// 解析 OKX WebSocket 推送消息
fn parse_ws_message(text: &str) -> Result<WsMessage, serde_json::Error> {
    let v: serde_json::Value = serde_json::from_str(text)?;

    // Ping/Pong
    if v.get("event").and_then(|e| e.as_str()) == Some("pong")
        || v.get("op").and_then(|o| o.as_str()) == Some("pong")
    {
        return Ok(WsMessage::Pong);
    }

    // 订阅确认等非数据消息
    if v.get("event").is_some() && v.get("data").is_none() {
        return Ok(WsMessage::Ping);
    }

    let arg = v.get("arg").unwrap_or(&serde_json::Value::Null);
    let channel = arg.get("channel").and_then(|c| c.as_str()).unwrap_or("");
    let inst_id = arg.get("instId").and_then(|i| i.as_str()).unwrap_or("");

    match channel {
        "tickers" => {
            let data = v
                .get("data")
                .and_then(|d| d.as_array())
                .and_then(|a| a.first());
            if let Some(d) = data {
                Ok(WsMessage::Ticker(Ticker {
                    symbol: Symbol::new(inst_id),
                    bid: d
                        .get("bidPx")
                        .and_then(|p| p.as_str())
                        .unwrap_or("0")
                        .parse()
                        .unwrap_or_default(),
                    ask: d
                        .get("askPx")
                        .and_then(|p| p.as_str())
                        .unwrap_or("0")
                        .parse()
                        .unwrap_or_default(),
                    last: d
                        .get("last")
                        .and_then(|p| p.as_str())
                        .unwrap_or("0")
                        .parse()
                        .unwrap_or_default(),
                    volume_24h: d
                        .get("vol24h")
                        .and_then(|p| p.as_str())
                        .unwrap_or("0")
                        .parse()
                        .unwrap_or_default(),
                    timestamp: chrono::Utc::now(),
                }))
            } else {
                Ok(WsMessage::Ping)
            }
        }
        "books5" => {
            let data = v
                .get("data")
                .and_then(|d| d.as_array())
                .and_then(|a| a.first());
            if let Some(d) = data {
                let bids = parse_okx_levels(d.get("bids"));
                let asks = parse_okx_levels(d.get("asks"));
                Ok(WsMessage::Depth(DepthSnapshot {
                    symbol: Symbol::new(inst_id),
                    bids,
                    asks,
                    timestamp: chrono::Utc::now(),
                }))
            } else {
                Ok(WsMessage::Ping)
            }
        }
        "trades" => {
            let data = v
                .get("data")
                .and_then(|d| d.as_array())
                .and_then(|a| a.first());
            if let Some(d) = data {
                let side_str = d.get("side").and_then(|s| s.as_str()).unwrap_or("buy");
                Ok(WsMessage::Trade(crate::types::Trade {
                    symbol: Symbol::new(inst_id),
                    price: d
                        .get("px")
                        .and_then(|p| p.as_str())
                        .unwrap_or("0")
                        .parse()
                        .unwrap_or_default(),
                    quantity: d
                        .get("sz")
                        .and_then(|p| p.as_str())
                        .unwrap_or("0")
                        .parse()
                        .unwrap_or_default(),
                    side: if side_str == "sell" {
                        crate::types::Side::Sell
                    } else {
                        crate::types::Side::Buy
                    },
                    timestamp: chrono::Utc::now(),
                }))
            } else {
                Ok(WsMessage::Ping)
            }
        }
        "orders" => {
            let data = v
                .get("data")
                .and_then(|d| d.as_array())
                .and_then(|a| a.first());
            if let Some(d) = data {
                let state = d.get("state").and_then(|s| s.as_str()).unwrap_or("");
                let filled_qty = d
                    .get("accFillSz")
                    .and_then(|s| s.as_str())
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or_default();
                let avg_price = d
                    .get("avgPx")
                    .and_then(|s| s.as_str())
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or_default();
                let status = match state {
                    "live" => OrderStatus::Acknowledged,
                    "partially_filled" => OrderStatus::PartiallyFilled {
                        filled_qty,
                        avg_price,
                    },
                    "filled" => OrderStatus::Filled {
                        filled_qty,
                        avg_price,
                    },
                    "canceled" => OrderStatus::Cancelled { filled_qty },
                    _ => OrderStatus::Pending,
                };
                Ok(WsMessage::OrderUpdate(crate::types::OrderUpdate {
                    order_id: d
                        .get("ordId")
                        .and_then(|o| o.as_str())
                        .unwrap_or("")
                        .to_string(),
                    client_order_id: OrderId(
                        uuid::Uuid::parse_str(
                            d.get("clOrdId")
                                .and_then(|c| c.as_str())
                                .unwrap_or("00000000-0000-0000-0000-000000000000"),
                        )
                        .unwrap_or_default(),
                    ),
                    status,
                    filled_qty,
                    avg_price: Some(avg_price),
                    timestamp: chrono::Utc::now(),
                }))
            } else {
                Ok(WsMessage::Ping)
            }
        }
        _ => Ok(WsMessage::Ping),
    }
}

/// 解析 OKX 价格层数组 `[["price","qty","0","1"], ...]`
fn parse_okx_levels(
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

/// 将 axon Order 转换为 OKX 下单 JSON
fn order_to_okx_json(order: &Order) -> String {
    let inst_id = order.symbol.to_string();
    let side = match order.side {
        crate::types::Side::Buy => "buy",
        crate::types::Side::Sell => "sell",
    };
    let ord_type = match order.order_type {
        crate::types::OrderType::Market => "market",
        crate::types::OrderType::Limit => "limit",
        _ => "limit",
    };
    let tif = match order.time_in_force {
        crate::types::TimeInForce::Gtc => "GTC",
        crate::types::TimeInForce::Ioc => "IOC",
        crate::types::TimeInForce::Fok => "FOK",
    };

    let mut map = serde_json::json!({
        "instId": inst_id,
        "side": side,
        "ordType": ord_type,
        "sz": order.quantity.to_string(),
        "tdMode": "cash",
        "clOrdId": order.client_order_id.to_string(),
    });

    if ord_type == "limit" {
        if let Some(price) = order.price {
            map["px"] = serde_json::json!(price.to_string());
        }
        map["tif"] = serde_json::json!(tif);
    }

    serde_json::to_string(&map).unwrap_or_default()
}

#[async_trait]
impl ExchangeAdapter for OkxAdapter {
    fn exchange_id(&self) -> ExchangeId {
        ExchangeId::Okx
    }

    async fn connect(&mut self) -> Result<(), ExchangeError> {
        // 验证 REST 连接
        let url = format!("{}/api/v5/public/time", self.config.rest_base_url);
        let resp = self.client.get(&url).send().await?;
        if !resp.status().is_success() {
            return Err(ExchangeError::ConnectionFailed(
                "OKX REST ping failed".into(),
            ));
        }

        // 启动 WebSocket
        self.start_ws().await?;
        tokio::time::sleep(Duration::from_millis(500)).await;

        *self.connected.lock().await = true;
        tracing::info!("OKX adapter connected");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ExchangeError> {
        *self.connected.lock().await = false;
        tracing::info!("OKX adapter disconnected");
        Ok(())
    }

    async fn subscribe(&mut self, symbols: &[Symbol]) -> Result<(), ExchangeError> {
        // 去重写入已订阅列表
        {
            let mut sub = self.subscribed_symbols.lock().await;
            for s in symbols {
                let s_str = s.to_string();
                if !sub.iter().any(|x| x.to_string() == s_str) {
                    sub.push(s.clone());
                }
            }
        }

        // 若 WebSocket 尚未就绪，仅记录订阅；连接建立后监督任务会按列表自动重订阅
        let writer_opt = self.ws_writer.lock().await.clone();
        match writer_opt {
            Some(writer) => Self::send_subscribe_to_writer(&writer, &self.subscribed_symbols).await,
            None => {
                tracing::info!(
                    "OKX WebSocket not ready; subscription will be applied on (re)connect"
                );
            }
        }

        tracing::info!("Subscribing to {} instruments", symbols.len());
        Ok(())
    }

    async fn send_order(&mut self, order: Order) -> Result<OrderId, ExchangeError> {
        if !*self.connected.lock().await {
            return Err(ExchangeError::ConnectionFailed("not connected".into()));
        }

        // 记录 clOrdId -> instId 映射，供后续撤单使用
        {
            let mut map = self.order_inst_ids.lock().await;
            map.insert(order.client_order_id.to_string(), order.symbol.to_string());
        }

        let body = order_to_okx_json(&order);
        let resp = self.rest_post_orders(&body).await?;

        let ord_data = resp["data"]
            .as_array()
            .and_then(|a| a.first())
            .ok_or_else(|| ExchangeError::ParseError("missing order data".into()))?;

        let ord_id = ord_data["ordId"]
            .as_str()
            .ok_or_else(|| ExchangeError::ParseError("missing ordId".into()))?;

        tracing::info!(
            "Order sent: client_id={}, ord_id={}",
            order.client_order_id,
            ord_id
        );
        Ok(order.client_order_id)
    }

    async fn cancel_order(&mut self, order_id: OrderId) -> Result<(), ExchangeError> {
        if !*self.connected.lock().await {
            return Err(ExchangeError::ConnectionFailed("not connected".into()));
        }

        // OKX 撤单需要 instId + ordId/clOrdId。从映射中获取正确的 instId
        let inst_id = self
            .order_inst_ids
            .lock()
            .await
            .get(&order_id.to_string())
            .cloned();

        let body = match inst_id {
            Some(sym) => serde_json::json!({
                "clOrdId": order_id.to_string(),
                "instId": sym,
            }),
            None => {
                tracing::warn!(
                    "cancel_order: no instId mapping for clOrdId={}, OKX API requires instId; \
                     consider providing it via order meta",
                    order_id
                );
                return Err(ExchangeError::OrderNotFound(order_id.to_string()));
            }
        };
        self.rest_post("/api/v5/trade/cancel-order", &body.to_string())
            .await?;
        self.order_inst_ids
            .lock()
            .await
            .remove(&order_id.to_string());

        tracing::info!("Order cancelled: {}", order_id);
        Ok(())
    }

    async fn get_balance(&self) -> Result<HashMap<String, AccountBalance>, ExchangeError> {
        let resp = self.rest_get("/api/v5/account/balance").await?;

        let details = resp["data"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|d| d.get("details"))
            .and_then(|d| d.as_array())
            .ok_or_else(|| ExchangeError::ParseError("missing balance details".into()))?;

        let balances = details
            .iter()
            .filter_map(|d| {
                let ccy = d.get("ccy")?.as_str()?;
                let avail = d.get("availBal")?.as_str()?.parse().ok()?;
                let frozen = d.get("frozenBal")?.as_str()?.parse().ok()?;
                Some((
                    ccy.to_string(),
                    AccountBalance {
                        currency: ccy.to_string(),
                        available: avail,
                        locked: frozen,
                    },
                ))
            })
            .collect();

        Ok(balances)
    }

    async fn get_positions(&self) -> Result<Vec<Position>, ExchangeError> {
        // 端点未配置时回退空（OKX 默认 /api/v5/account/positions，但允许覆盖）
        let endpoint = if self.config.position_endpoint.is_empty() {
            "/api/v5/account/positions".to_string()
        } else {
            self.config.position_endpoint.clone()
        };
        let resp = self.rest_get(&endpoint).await?;
        // OKX 标准响应：`{ "code": "0", "data": [...] }`
        let arr: Vec<serde_json::Value> = resp
            .get("data")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();
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
        if !(1..=125).contains(&leverage) {
            return Err(ExchangeError::OrderRejected {
                reason: format!("leverage {leverage} out of range 1..=125"),
            });
        }
        // mgnMode 默认 cross,允许 caller 通过 meta 覆盖(此处简化为 cross)
        let body = format!(r#"{{"instId":"{symbol}","lever":"{leverage}","mgnMode":"cross"}}"#);
        self.send_okx_signed("POST", "/api/v5/account/set-leverage", Some(&body))
            .await?;
        Ok(())
    }

    async fn set_margin_type(
        &self,
        symbol: &str,
        margin_type: MarginType,
    ) -> Result<(), ExchangeError> {
        let mgn = match margin_type {
            MarginType::Isolated => "isolated",
            MarginType::Cross => "cross",
        };
        let body = format!(r#"{{"instId":"{symbol}","mgnMode":"{mgn}"}}"#);
        self.send_okx_signed("POST", "/api/v5/account/set-margin-mode", Some(&body))
            .await?;
        Ok(())
    }

    async fn get_leverage_brackets(
        &self,
        symbol: &str,
    ) -> Result<Vec<LeverageBracket>, ExchangeError> {
        // 公开端点(无需签名)
        let path =
            format!("/api/v5/public/position-tiers?instType=SWAP&tdMode=cross&instId={symbol}");
        let json = self.send_okx_public(&path).await?;
        let arr = json
            .get("data")
            .and_then(|d| d.as_array())
            .ok_or_else(|| ExchangeError::ParseError("position-tiers: missing data".into()))?;
        let mut out = Vec::new();
        for entry in arr {
            // tier 是数组,每项含 maxLever / maxSz / mmr
            if let Some(tier_arr) = entry.get("tier").and_then(|t| t.as_array()) {
                for (i, tier) in tier_arr.iter().enumerate() {
                    // OKX API 的 maxLever 字段是字符串(如 "125"),需要 parse
                    let max_leverage: u8 = tier["maxLever"]
                        .as_str()
                        .and_then(|s| s.parse().ok())
                        .or_else(|| tier["maxLever"].as_u64())
                        .unwrap_or(1) as u8;
                    out.push(LeverageBracket {
                        bracket: (i + 1) as u32,
                        min_leverage: 1,
                        max_leverage,
                        max_notional: tier["maxSz"]
                            .as_str()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or_default(),
                        maint_margin_ratio: tier["mmr"]
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
        let pos_mode = if hedge_mode {
            "long_short_mode"
        } else {
            "net_mode"
        };
        let body = format!(r#"{{"posMode":"{pos_mode}"}}"#);
        self.send_okx_signed("POST", "/api/v5/account/set-position-mode", Some(&body))
            .await?;
        Ok(())
    }

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate, ExchangeError> {
        let path = format!("/api/v5/public/funding-rate?instId={symbol}");
        let json = self.send_okx_public(&path).await?;
        let entry = json
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|a| a.first())
            .ok_or_else(|| ExchangeError::ParseError("fundingRate: empty data".into()))?;
        Ok(FundingRate {
            symbol: symbol.to_string(),
            rate: entry["fundingRate"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or_default(),
            next_funding_ms: entry["nextFundingTime"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            mark_price: entry["markPx"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or_default(),
            index_price: entry["idxPx"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or_default(),
        })
    }

    async fn get_account_info(&self) -> Result<AccountInfo, ExchangeError> {
        // 1) 私有 balance 端点
        let balance_json = self
            .send_okx_signed("GET", "/api/v5/account/balance", None)
            .await?;
        let balance_entry = balance_json
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|a| a.first())
            .ok_or_else(|| ExchangeError::ParseError("balance: empty data".into()))?;
        let parse_dec = |k: &str| -> Decimal {
            balance_entry[k]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or_default()
        };
        let total_balance = parse_dec("totalEq");
        let initial_margin = parse_dec("imr");
        let maintenance_margin = parse_dec("mmr");
        let unrealized_pnl = parse_dec("upl");
        let margin_used = initial_margin;
        let available_balance = total_balance - margin_used;

        // 2) 私有 positions 端点(取 posMode)
        let positions_json = self
            .send_okx_signed("GET", "/api/v5/account/positions?instType=SWAP", None)
            .await?;
        let position_mode = positions_json
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|a| a.first())
            .and_then(|e| e.get("posMode").and_then(|p| p.as_str()))
            .map(|s| match s {
                "long_short_mode" => PositionMode::Hedge,
                _ => PositionMode::Net,
            })
            .unwrap_or(PositionMode::Net);

        Ok(AccountInfo {
            total_balance,
            available_balance,
            unrealized_pnl,
            margin_used,
            initial_margin,
            maintenance_margin,
            position_mode,
            as_of_ms: chrono::Utc::now().timestamp_millis(),
        })
    }

    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest, ExchangeError> {
        let path = format!("/api/v5/public/open-interest?instType=SWAP&instId={symbol}");
        let json = self.send_okx_public(&path).await?;
        let entry = json
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|a| a.first())
            .ok_or_else(|| ExchangeError::ParseError("openInterest: empty data".into()))?;
        Ok(OpenInterest {
            symbol: symbol.to_string(),
            contracts: entry["oi"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            notional: entry["oiCcy"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or_default(),
            timestamp_ms: entry["ts"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
        })
    }

    async fn get_long_short_ratio(&self, symbol: &str) -> Result<LongShortRatio, ExchangeError> {
        let path =
            format!("/api/v5/rubik/stat/contracts/long-short-account-ratio?ccy={symbol}&period=5m");
        let json = self.send_okx_public(&path).await?;
        let entry = json
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|a| a.first())
            .ok_or_else(|| ExchangeError::ParseError("longShortRatio: empty data".into()))?;
        // OKX API 的 longRatio / shortRatio 字段是字符串(如 "0.6"),需要 parse
        let parse_ratio = |k: &str| -> f64 {
            entry[k]
                .as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| entry[k].as_f64())
                .unwrap_or(0.5)
        };
        let long_ratio: f64 = parse_ratio("longRatio");
        let short_ratio: f64 = parse_ratio("shortRatio");
        Ok(LongShortRatio {
            symbol: symbol.to_string(),
            long_ratio,
            short_ratio,
            long_short_ratio: if short_ratio > 0.0 {
                long_ratio / short_ratio
            } else {
                1.0
            },
            timestamp_ms: entry["ts"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ExchangeConfig, RateLimitConfig, ReconnectConfig, TimeInForce};

    fn testnet_config() -> ExchangeConfig {
        ExchangeConfig {
            exchange_id: ExchangeId::Okx,
            api_key: std::env::var("OKX_TESTNET_API_KEY").unwrap_or_default(),
            api_secret: std::env::var("OKX_TESTNET_API_SECRET").unwrap_or_default(),
            passphrase: std::env::var("OKX_TESTNET_PASSPHRASE").ok(),
            testnet: true,
            rest_base_url: "https://www.okx.com".into(),
            ws_url: "wss://wspap.okx.com:8443/ws/v5/public?brokerId=9999".into(),
            rate_limit: RateLimitConfig {
                requests_per_second: 20,
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
            position_endpoint: "/api/v5/account/positions".into(),
            fapi_base_url: None,
        }
    }

    #[test]
    fn test_sign_returns_base64() {
        let config = testnet_config();
        let adapter = OkxAdapter::new(config);
        let sig = adapter.sign(
            "2024-01-01T00:00:00.000Z",
            "GET",
            "/api/v5/account/balance",
            "",
        );
        assert!(sig.is_ok());
        let b64 = sig.unwrap();
        assert!(!b64.is_empty());
        // base64 验证
        assert!(
            base64::engine::general_purpose::STANDARD
                .decode(&b64)
                .is_ok()
        );
    }

    #[test]
    fn test_auth_headers() {
        let config = testnet_config();
        let adapter = OkxAdapter::new(config);
        let headers = adapter
            .auth_headers("GET", "/api/v5/account/balance", "")
            .unwrap();
        // 4 标准头 + 1 测试网头
        assert_eq!(headers.len(), 5);
        assert_eq!(headers[0].0, "OK-ACCESS-KEY");
        assert_eq!(headers[1].0, "OK-ACCESS-SIGN");
        assert_eq!(headers[2].0, "OK-ACCESS-TIMESTAMP");
        assert_eq!(headers[3].0, "OK-ACCESS-PASSPHRASE");
        assert_eq!(headers[4].0, "x-simulated-trading");
        assert_eq!(headers[4].1, "1");
    }

    #[test]
    fn test_order_to_okx_json() {
        let order = Order {
            client_order_id: OrderId::new(),
            symbol: Symbol::new("BTC-USDT"),
            side: crate::types::Side::Buy,
            order_type: crate::types::OrderType::Limit,
            price: Some("50000.00".parse().unwrap()),
            quantity: "0.001".parse().unwrap(),
            time_in_force: TimeInForce::Gtc,
            exchange: ExchangeId::Okx,
            meta: HashMap::new(),
        };
        let json = order_to_okx_json(&order);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["instId"], "BTC-USDT");
        assert_eq!(v["side"], "buy");
        assert_eq!(v["ordType"], "limit");
        assert_eq!(v["sz"], "0.001");
        assert_eq!(v["px"], "50000.00");
    }

    #[test]
    fn test_parse_ticker_message() {
        let msg = r#"{"arg":{"channel":"tickers","instId":"BTC-USDT"},"data":[{"instId":"BTC-USDT","last":"50000","bidPx":"49999","askPx":"50001","vol24h":"100"}]}"#;
        let parsed = parse_ws_message(msg).unwrap();
        match parsed {
            WsMessage::Ticker(t) => {
                assert_eq!(t.symbol, Symbol::new("BTC-USDT"));
                assert_eq!(t.last, "50000".parse::<rust_decimal::Decimal>().unwrap());
                assert_eq!(t.bid, "49999".parse::<rust_decimal::Decimal>().unwrap());
                assert_eq!(t.ask, "50001".parse::<rust_decimal::Decimal>().unwrap());
            }
            _ => panic!("expected Ticker"),
        }
    }

    #[test]
    fn test_parse_depth_message() {
        let msg = r#"{"arg":{"channel":"books5","instId":"BTC-USDT"},"data":[{"bids":[["50000","1.0","0","1"],["49999","2.0","0","2"]],"asks":[["50001","0.5","0","1"]]}]}"#;
        let parsed = parse_ws_message(msg).unwrap();
        match parsed {
            WsMessage::Depth(d) => {
                assert_eq!(d.symbol, Symbol::new("BTC-USDT"));
                assert_eq!(d.bids.len(), 2);
                assert_eq!(d.asks.len(), 1);
            }
            _ => panic!("expected Depth"),
        }
    }

    #[test]
    fn test_parse_trade_message() {
        let msg = r#"{"arg":{"channel":"trades","instId":"BTC-USDT"},"data":[{"instId":"BTC-USDT","px":"50000","sz":"0.1","side":"buy"}]}"#;
        let parsed = parse_ws_message(msg).unwrap();
        match parsed {
            WsMessage::Trade(t) => {
                assert_eq!(t.symbol, Symbol::new("BTC-USDT"));
                assert_eq!(t.side, crate::types::Side::Buy);
            }
            _ => panic!("expected Trade"),
        }
    }

    #[test]
    fn test_parse_order_update_filled() {
        let msg = r#"{"arg":{"channel":"orders","instId":"BTC-USDT"},"data":[{"instId":"BTC-USDT","ordId":"12345","clOrdId":"my-order-1","state":"filled","accFillSz":"0.001","avgPx":"50000"}]}"#;
        let parsed = parse_ws_message(msg).unwrap();
        match parsed {
            WsMessage::OrderUpdate(u) => {
                assert_eq!(u.order_id, "12345");
                assert_eq!(
                    u.status,
                    OrderStatus::Filled {
                        filled_qty: "0.001".parse().unwrap(),
                        avg_price: "50000".parse().unwrap(),
                    }
                );
            }
            _ => panic!("expected OrderUpdate"),
        }
    }

    #[test]
    fn test_order_inst_ids_mapping_for_cancel() {
        // 验证 send_order 写入的 clOrdId -> instId 映射可被 cancel_order 读取
        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let adapter = OkxAdapter::new(testnet_config());
            let client_id = OrderId::new();
            adapter
                .order_inst_ids
                .lock()
                .await
                .insert(client_id.to_string(), "ETH-USDT".to_string());

            // 模拟 cancel_order 中查找 instId 的逻辑
            let inst_id = adapter
                .order_inst_ids
                .lock()
                .await
                .get(&client_id.to_string())
                .cloned();
            assert_eq!(inst_id, Some("ETH-USDT".to_string()));
        });
    }

    #[test]
    fn test_subscribe_records_symbols_for_resubscribe() {
        // WebSocket 未就绪时，subscribe 仍应记录到 subscribed_symbols
        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut adapter = OkxAdapter::new(testnet_config());
            let _ = adapter
                .subscribe(&[Symbol::new("BTC-USDT"), Symbol::new("ETH-USDT")])
                .await;
            let sub = adapter.subscribed_symbols.lock().await;
            assert_eq!(sub.len(), 2);
            assert!(sub.contains(&Symbol::new("BTC-USDT")));
            assert!(sub.contains(&Symbol::new("ETH-USDT")));
        });
    }

    #[test]
    fn test_subscribe_dedups_symbols() {
        // 重复订阅同一 symbol 不应产生重复条目
        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut adapter = OkxAdapter::new(testnet_config());
            let _ = adapter.subscribe(&[Symbol::new("BTC-USDT")]).await;
            let _ = adapter.subscribe(&[Symbol::new("BTC-USDT")]).await;
            let sub = adapter.subscribed_symbols.lock().await;
            assert_eq!(sub.len(), 1);
        });
    }

    #[test]
    fn test_cancel_without_inst_id_mapping_returns_error() {
        // 当没有 clOrdId -> instId 映射时，cancel_order 应返回 OrderNotFound
        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut adapter = OkxAdapter::new(testnet_config());
            // 标记为已连接，但未注册任何订单映射
            *adapter.connected.lock().await = true;

            let result = adapter.cancel_order(OrderId::new()).await;
            assert!(matches!(result, Err(ExchangeError::OrderNotFound(_))));
        });
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
        let b = OkxAdapter::next_backoff(cfg.initial_backoff, &cfg);
        assert_eq!(b, Duration::from_secs(30));
    }

    // ============== Stage 4' D: 杠杆/合约 wiremock 集成测试 ==============

    /// 构造指向 wiremock server 的 config
    fn wiremock_config(server_uri: &str) -> ExchangeConfig {
        ExchangeConfig {
            exchange_id: ExchangeId::Okx,
            api_key: "test_key".into(),
            api_secret: "test_secret".into(),
            passphrase: Some("test_pass".into()),
            testnet: true,
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
            position_endpoint: "/api/v5/account/positions".into(),
            fapi_base_url: None,
        }
    }

    /// OKX 标准成功响应包装(code="0")
    fn okx_ok(data: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "code": "0", "msg": "", "data": data })
    }

    #[test]
    fn okx_set_leverage_rejects_out_of_range() {
        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let adapter = OkxAdapter::new(wiremock_config("http://127.0.0.1:1"));
            let r = adapter.set_leverage("BTC-USDT-SWAP", 0).await;
            assert!(matches!(r, Err(ExchangeError::OrderRejected { .. })));
            let r = adapter.set_leverage("BTC-USDT-SWAP", 200).await;
            assert!(matches!(r, Err(ExchangeError::OrderRejected { .. })));
        });
    }

    #[test]
    fn okx_set_leverage_signed_endpoint_ok() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v5/account/set-leverage"))
                .and(header("OK-ACCESS-KEY", "test_key"))
                .and(header("OK-ACCESS-PASSPHRASE", "test_pass"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(okx_ok(serde_json::json!([
                        { "instId": "BTC-USDT-SWAP", "lever": "10", "mgnMode": "cross" }
                    ]))),
                )
                .expect(1)
                .mount(&server)
                .await;

            let adapter = OkxAdapter::new(wiremock_config(&server.uri()));
            let r = adapter.set_leverage("BTC-USDT-SWAP", 10).await;
            assert!(r.is_ok(), "set_leverage failed: {r:?}");
        });
    }

    #[test]
    fn okx_set_margin_type_ok() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v5/account/set-margin-mode"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(okx_ok(serde_json::json!([
                        { "instId": "BTC-USDT-SWAP", "mgnMode": "isolated" }
                    ]))),
                )
                .expect(1)
                .mount(&server)
                .await;

            let adapter = OkxAdapter::new(wiremock_config(&server.uri()));
            let r = adapter
                .set_margin_type("BTC-USDT-SWAP", MarginType::Isolated)
                .await;
            assert!(r.is_ok(), "set_margin_type failed: {r:?}");
        });
    }

    #[test]
    fn okx_set_position_mode_ok() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v5/account/set-position-mode"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(okx_ok(serde_json::json!([
                        { "posMode": "long_short_mode" }
                    ]))),
                )
                .expect(1)
                .mount(&server)
                .await;

            let adapter = OkxAdapter::new(wiremock_config(&server.uri()));
            let r = adapter.set_position_mode(true).await;
            assert!(r.is_ok(), "set_position_mode failed: {r:?}");
        });
    }

    #[test]
    fn okx_get_leverage_brackets_parses_tiers() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v5/public/position-tiers"))
                .and(query_param("instType", "SWAP"))
                .and(query_param("tdMode", "cross"))
                .and(query_param("instId", "BTC-USDT-SWAP"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(okx_ok(serde_json::json!([
                        {
                            "instId": "BTC-USDT-SWAP",
                            "tier": [
                                { "maxLever": "125", "maxSz": "50000", "mmr": "0.004" },
                                { "maxLever": "100", "maxSz": "250000", "mmr": "0.005" }
                            ]
                        }
                    ]))),
                )
                .expect(1)
                .mount(&server)
                .await;

            let adapter = OkxAdapter::new(wiremock_config(&server.uri()));
            let brackets = adapter
                .get_leverage_brackets("BTC-USDT-SWAP")
                .await
                .unwrap();
            assert_eq!(brackets.len(), 2);
            assert_eq!(brackets[0].max_leverage, 125);
            assert_eq!(brackets[0].max_notional, "50000".parse().unwrap());
            assert_eq!(brackets[0].maint_margin_ratio, "0.004".parse().unwrap());
            assert_eq!(brackets[1].max_leverage, 100);
        });
    }

    #[test]
    fn okx_get_funding_rate_parses_public_data() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v5/public/funding-rate"))
                .and(query_param("instId", "BTC-USDT-SWAP"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(okx_ok(serde_json::json!([
                        {
                            "instId": "BTC-USDT-SWAP",
                            "fundingRate": "0.0001",
                            "nextFundingTime": "1700000000000",
                            "markPx": "50000",
                            "idxPx": "49999"
                        }
                    ]))),
                )
                .expect(1)
                .mount(&server)
                .await;

            let adapter = OkxAdapter::new(wiremock_config(&server.uri()));
            let fr = adapter.get_funding_rate("BTC-USDT-SWAP").await.unwrap();
            assert_eq!(fr.symbol, "BTC-USDT-SWAP");
            assert_eq!(fr.rate, "0.0001".parse().unwrap());
            assert_eq!(fr.mark_price, "50000".parse().unwrap());
            assert_eq!(fr.index_price, "49999".parse().unwrap());
            assert_eq!(fr.next_funding_ms, 1_700_000_000_000);
        });
    }

    #[test]
    fn okx_get_account_info_combines_balance_and_positions() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = MockServer::start().await;
            // 第一次 GET /api/v5/account/balance -> 余额
            Mock::given(method("GET"))
                .and(path("/api/v5/account/balance"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(okx_ok(serde_json::json!([
                        {
                            "totalEq": "10000.50",
                            "imr": "2000.25",
                            "mmr": "150.00",
                            "upl": "120.00"
                        }
                    ]))),
                )
                .expect(1)
                .mount(&server)
                .await;
            // 第二次 GET /api/v5/account/positions?instType=SWAP -> 持仓模式
            Mock::given(method("GET"))
                .and(path("/api/v5/account/positions"))
                .and(query_param("instType", "SWAP"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(okx_ok(serde_json::json!([
                        { "instId": "BTC-USDT-SWAP", "posMode": "long_short_mode" }
                    ]))),
                )
                .expect(1)
                .mount(&server)
                .await;

            let adapter = OkxAdapter::new(wiremock_config(&server.uri()));
            let info = adapter.get_account_info().await.expect("get_account_info");
            assert_eq!(info.total_balance, "10000.50".parse().unwrap());
            assert_eq!(info.unrealized_pnl, "120.00".parse().unwrap());
            assert_eq!(info.initial_margin, "2000.25".parse().unwrap());
            assert_eq!(info.maintenance_margin, "150.00".parse().unwrap());
            assert_eq!(info.position_mode, PositionMode::Hedge);
            // available = total - imr
            let expected_avail = info.total_balance - info.initial_margin;
            assert_eq!(info.available_balance, expected_avail);
            assert!(info.as_of_ms > 0);
        });
    }

    #[test]
    fn okx_get_open_interest_parses_public_data() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v5/public/open-interest"))
                .and(query_param("instType", "SWAP"))
                .and(query_param("instId", "BTC-USDT-SWAP"))
                .respond_with(ResponseTemplate::new(200).set_body_json(okx_ok(serde_json::json!([
                    { "instId": "BTC-USDT-SWAP", "oi": "98765", "oiCcy": "9876500", "ts": "1700000000000" }
                ]))))
                .expect(1)
                .mount(&server)
                .await;

            let adapter = OkxAdapter::new(wiremock_config(&server.uri()));
            let oi = adapter.get_open_interest("BTC-USDT-SWAP").await.unwrap();
            assert_eq!(oi.contracts, 98765);
            assert_eq!(oi.notional, "9876500".parse().unwrap());
            assert_eq!(oi.timestamp_ms, 1_700_000_000_000);
        });
    }

    #[test]
    fn okx_get_long_short_ratio_parses_ratio() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path(
                    "/api/v5/rubik/stat/contracts/long-short-account-ratio",
                ))
                .and(query_param("ccy", "BTC"))
                .and(query_param("period", "5m"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(okx_ok(serde_json::json!([
                        { "longRatio": "0.6", "shortRatio": "0.4", "ts": "1700000000000" }
                    ]))),
                )
                .expect(1)
                .mount(&server)
                .await;

            let adapter = OkxAdapter::new(wiremock_config(&server.uri()));
            let r = adapter.get_long_short_ratio("BTC").await.unwrap();
            assert_eq!(r.symbol, "BTC");
            assert!((r.long_ratio - 0.6).abs() < 1e-9);
            assert!((r.short_ratio - 0.4).abs() < 1e-9);
            assert!((r.long_short_ratio - 1.5).abs() < 1e-9);
        });
    }

    #[test]
    fn okx_parse_response_handles_429() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v5/public/funding-rate"))
                .and(query_param("instId", "BTC-USDT-SWAP"))
                .respond_with(
                    ResponseTemplate::new(429)
                        .insert_header("Retry-After", "2")
                        .set_body_string("rate limited"),
                )
                .expect(1)
                .mount(&server)
                .await;

            let adapter = OkxAdapter::new(wiremock_config(&server.uri()));
            let r = adapter.get_funding_rate("BTC-USDT-SWAP").await;
            match r {
                Err(ExchangeError::RateLimited { wait_ms }) => assert_eq!(wait_ms, 2000),
                other => panic!("expected RateLimited {{ wait_ms: 2000 }}, got {other:?}"),
            }
        });
    }

    #[test]
    fn okx_parse_response_handles_401() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v5/account/balance"))
                .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
                .expect(1)
                .mount(&server)
                .await;

            let adapter = OkxAdapter::new(wiremock_config(&server.uri()));
            let r = adapter.get_account_info().await;
            assert!(matches!(r, Err(ExchangeError::AuthenticationFailed(_))));
        });
    }

    #[test]
    fn okx_parse_response_handles_api_error_code() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v5/account/set-leverage"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "code": "51000",
                    "msg": "Parameter lever error"
                })))
                .expect(1)
                .mount(&server)
                .await;

            let adapter = OkxAdapter::new(wiremock_config(&server.uri()));
            // 通过 set_leverage 内部用 10 触发,实际 mock 端点总是返回 code=51000
            let r = adapter.set_leverage("BTC-USDT-SWAP", 10).await;
            match r {
                Err(ExchangeError::ApiError { code, message }) => {
                    assert_eq!(code, 51000);
                    assert!(message.contains("Parameter"));
                }
                other => panic!("expected ApiError, got {other:?}"),
            }
        });
    }
}
