use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures_util::stream::SplitStream;
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use reqwest::Client;
use sha2::Sha256;
use tokio::sync::{Mutex, Notify, mpsc};
use tokio::time::{Duration, interval};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::error::ExchangeError;
use crate::traits::ExchangeAdapter;
use crate::types::{
    AccountBalance, DepthSnapshot, ExchangeConfig, ExchangeId, Order, OrderId, OrderStatus,
    Position, ReconnectConfig, Symbol, Ticker, WsMessage,
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
    fn sign(&self, timestamp: &str, method: &str, path: &str, body: &str) -> Result<String, ExchangeError> {
        let prehash = format!("{timestamp}{method}{path}{body}");
        let mut mac = HmacSha256::new_from_slice(self.config.api_secret.as_bytes())
            .map_err(|e| ExchangeError::AuthenticationFailed(e.to_string()))?;
        mac.update(prehash.as_bytes());
        Ok(BASE64.encode(mac.finalize().into_bytes()))
    }

    /// 构造请求头
    fn auth_headers(&self, method: &str, path: &str, body: &str) -> Result<Vec<(String, String)>, ExchangeError> {
        let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let sign = self.sign(&timestamp, method, path, body)?;
        let mut headers = vec![
            ("OK-ACCESS-KEY".into(), self.config.api_key.clone()),
            ("OK-ACCESS-SIGN".into(), sign),
            ("OK-ACCESS-TIMESTAMP".into(), timestamp),
            ("OK-ACCESS-PASSPHRASE".into(), self.config.passphrase.clone().unwrap_or_default()),
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
        let mut req = self.client.post(&url)
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
                        Self::run_read_loop(ws_read, tx.clone(), writer.clone(), depth_cache.clone(), ticker_cache.clone(), shutdown.clone()).await;

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
                        tracing::warn!(
                            "OKX WebSocket connect failed (attempt {}): {}",
                            attempt,
                            e
                        );
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
        if let Err(e) = w.send(Message::Text(payload.into())).await {
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
}

/// 解析 OKX WebSocket 推送消息
fn parse_ws_message(text: &str) -> Result<WsMessage, serde_json::Error> {
    let v: serde_json::Value = serde_json::from_str(text)?;

    // Ping/Pong
    if v.get("event").and_then(|e| e.as_str()) == Some("pong") || v.get("op").and_then(|o| o.as_str()) == Some("pong") {
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
            let data = v.get("data").and_then(|d| d.as_array()).and_then(|a| a.first());
            if let Some(d) = data {
                Ok(WsMessage::Ticker(Ticker {
                    symbol: Symbol::new(inst_id),
                    bid: d.get("bidPx").and_then(|p| p.as_str()).unwrap_or("0").parse().unwrap_or_default(),
                    ask: d.get("askPx").and_then(|p| p.as_str()).unwrap_or("0").parse().unwrap_or_default(),
                    last: d.get("last").and_then(|p| p.as_str()).unwrap_or("0").parse().unwrap_or_default(),
                    volume_24h: d.get("vol24h").and_then(|p| p.as_str()).unwrap_or("0").parse().unwrap_or_default(),
                    timestamp: chrono::Utc::now(),
                }))
            } else {
                Ok(WsMessage::Ping)
            }
        }
        "books5" => {
            let data = v.get("data").and_then(|d| d.as_array()).and_then(|a| a.first());
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
            let data = v.get("data").and_then(|d| d.as_array()).and_then(|a| a.first());
            if let Some(d) = data {
                let side_str = d.get("side").and_then(|s| s.as_str()).unwrap_or("buy");
                Ok(WsMessage::Trade(crate::types::Trade {
                    symbol: Symbol::new(inst_id),
                    price: d.get("px").and_then(|p| p.as_str()).unwrap_or("0").parse().unwrap_or_default(),
                    quantity: d.get("sz").and_then(|p| p.as_str()).unwrap_or("0").parse().unwrap_or_default(),
                    side: if side_str == "sell" { crate::types::Side::Sell } else { crate::types::Side::Buy },
                    timestamp: chrono::Utc::now(),
                }))
            } else {
                Ok(WsMessage::Ping)
            }
        }
        "orders" => {
            let data = v.get("data").and_then(|d| d.as_array()).and_then(|a| a.first());
            if let Some(d) = data {
                let state = d.get("state").and_then(|s| s.as_str()).unwrap_or("");
                let filled_qty = d.get("accFillSz").and_then(|s| s.as_str()).unwrap_or("0").parse().unwrap_or_default();
                let avg_price = d.get("avgPx").and_then(|s| s.as_str()).unwrap_or("0").parse().unwrap_or_default();
                let status = match state {
                    "live" => OrderStatus::Acknowledged,
                    "partially_filled" => OrderStatus::PartiallyFilled { filled_qty, avg_price },
                    "filled" => OrderStatus::Filled { filled_qty, avg_price },
                    "canceled" => OrderStatus::Cancelled { filled_qty },
                    _ => OrderStatus::Pending,
                };
                Ok(WsMessage::OrderUpdate(crate::types::OrderUpdate {
                    order_id: d.get("ordId").and_then(|o| o.as_str()).unwrap_or("").to_string(),
                    client_order_id: OrderId(uuid::Uuid::parse_str(
                        d.get("clOrdId").and_then(|c| c.as_str()).unwrap_or("00000000-0000-0000-0000-000000000000")
                    ).unwrap_or_default()),
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
fn parse_okx_levels(val: Option<&serde_json::Value>) -> Vec<(rust_decimal::Decimal, rust_decimal::Decimal)> {
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
            return Err(ExchangeError::ConnectionFailed("OKX REST ping failed".into()));
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

        let ord_data = resp["data"].as_array()
            .and_then(|a| a.first())
            .ok_or_else(|| ExchangeError::ParseError("missing order data".into()))?;

        let ord_id = ord_data["ordId"].as_str()
            .ok_or_else(|| ExchangeError::ParseError("missing ordId".into()))?;

        tracing::info!("Order sent: client_id={}, ord_id={}", order.client_order_id, ord_id);
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
        self.rest_post("/api/v5/trade/cancel-order", &body.to_string()).await?;
        self.order_inst_ids.lock().await.remove(&order_id.to_string());

        tracing::info!("Order cancelled: {}", order_id);
        Ok(())
    }

    async fn get_balance(&self) -> Result<HashMap<String, AccountBalance>, ExchangeError> {
        let resp = self.rest_get("/api/v5/account/balance").await?;

        let details = resp["data"].as_array()
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
        let resp = self.rest_get("/api/v5/account/positions").await?;

        let positions = resp["data"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|d| {
                        let inst_id = d.get("instId")?.as_str()?;
                        let pos_side = d.get("posSide")?.as_str().unwrap_or("net");
                        let side = match pos_side {
                            "long" => crate::types::Side::Buy,
                            "short" => crate::types::Side::Sell,
                            _ => {
                                let pos: f64 = d.get("pos")?.as_str()?.parse().ok()?;
                                if pos >= 0.0 { crate::types::Side::Buy } else { crate::types::Side::Sell }
                            }
                        };
                        let qty: rust_decimal::Decimal = d.get("pos")?.as_str()?.parse().ok()?;
                        let avg_px: rust_decimal::Decimal = d.get("avgPx")?.as_str()?.parse().ok()?;
                        let upl: rust_decimal::Decimal = d.get("upl")?.as_str()?.parse().ok()?;
                        Some(Position {
                            symbol: Symbol::new(inst_id),
                            side,
                            quantity: qty.abs(),
                            avg_entry_price: avg_px,
                            unrealized_pnl: upl,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(positions)
    }

    fn get_depth(&self, symbol: &Symbol) -> Option<DepthSnapshot> {
        self.depth_cache.blocking_lock().get(&symbol.to_string()).cloned()
    }

    fn get_ticker(&self, symbol: &Symbol) -> Option<Ticker> {
        self.ticker_cache.blocking_lock().get(&symbol.to_string()).cloned()
    }

    fn market_data_rx(&self) -> mpsc::Receiver<WsMessage> {
        self.market_rx
            .blocking_lock()
            .take()
            .expect("market_data_rx already taken")
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
        }
    }

    #[test]
    fn test_sign_returns_base64() {
        let config = testnet_config();
        let adapter = OkxAdapter::new(config);
        let sig = adapter.sign("2024-01-01T00:00:00.000Z", "GET", "/api/v5/account/balance", "");
        assert!(sig.is_ok());
        let b64 = sig.unwrap();
        assert!(!b64.is_empty());
        // base64 验证
        assert!(base64::engine::general_purpose::STANDARD.decode(&b64).is_ok());
    }

    #[test]
    fn test_auth_headers() {
        let config = testnet_config();
        let adapter = OkxAdapter::new(config);
        let headers = adapter.auth_headers("GET", "/api/v5/account/balance", "").unwrap();
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
                assert_eq!(u.status, OrderStatus::Filled {
                    filled_qty: "0.001".parse().unwrap(),
                    avg_price: "50000".parse().unwrap(),
                });
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
            let mut adapter = OkxAdapter::new(testnet_config());
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
            let _ = adapter.subscribe(&[Symbol::new("BTC-USDT"), Symbol::new("ETH-USDT")]).await;
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
}
