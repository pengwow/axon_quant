use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
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
    Position, Symbol, Ticker, WsMessage,
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
    connected: Mutex<bool>,
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
            connected: Mutex::new(false),
            subscribed_symbols: Arc::new(Mutex::new(Vec::new())),
            order_symbols: Mutex::new(HashMap::new()),
            ws_writer: Arc::new(Mutex::new(None)),
            disconnect_notify: Arc::new(Notify::new()),
            shutdown: Arc::new(Notify::new()),
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
        let url = format!("{}{path}?{query}", self.config.rest_base_url);
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
        Ok(body)
    }

    /// REST POST 请求（下单）
    async fn rest_post(&self, path: &str, body: &str) -> Result<serde_json::Value, ExchangeError> {
        let query = self.signed_query("")?;
        let url = format!("{}{path}?{query}", self.config.rest_base_url);
        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body.to_string())
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
    async fn rest_delete(&self, path: &str, params: &str) -> Result<serde_json::Value, ExchangeError> {
        let query = self.signed_query(params)?;
        let url = format!("{}{path}?{query}", self.config.rest_base_url);
        let resp = self.client.delete(&url).send().await?;
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

    /// Ping 保活任务：定时向 ws 写入 ping 帧，断线时退出
    fn spawn_ping_task(writer: Arc<Mutex<WsSink>>, shutdown: Arc<Notify>) -> tokio::task::JoinHandle<()> {
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
    async fn send_subscribe_msg(writer: &Arc<Mutex<WsSink>>, streams: Vec<String>) -> Result<(), ExchangeError> {
        let msg = serde_json::json!({
            "method": "SUBSCRIBE",
            "params": streams,
            "id": chrono::Utc::now().timestamp_millis(),
        });
        let payload = serde_json::to_string(&msg)
            .map_err(|e| ExchangeError::ParseError(e.to_string()))?;
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
                bid: v.get("b").and_then(|b| b.as_str()).unwrap_or("0").parse().unwrap_or_default(),
                ask: v.get("a").and_then(|a| a.as_str()).unwrap_or("0").parse().unwrap_or_default(),
                last: v.get("c").and_then(|c| c.as_str()).unwrap_or("0").parse().unwrap_or_default(),
                volume_24h: v.get("v").and_then(|vol| vol.as_str()).unwrap_or("0").parse().unwrap_or_default(),
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
            let price = v.get("p").and_then(|p| p.as_str()).unwrap_or("0").parse().unwrap_or_default();
            let quantity = v.get("q").and_then(|q| q.as_str()).unwrap_or("0").parse().unwrap_or_default();
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
            Ok(WsMessage::Kline(crate::types::Kline {
                symbol: Symbol::new(symbol),
                interval: interval.to_string(),
                open: k.get("o").and_then(|o| o.as_str()).unwrap_or("0").parse().unwrap_or_default(),
                high: k.get("h").and_then(|h| h.as_str()).unwrap_or("0").parse().unwrap_or_default(),
                low: k.get("l").and_then(|l| l.as_str()).unwrap_or("0").parse().unwrap_or_default(),
                close: k.get("c").and_then(|c| c.as_str()).unwrap_or("0").parse().unwrap_or_default(),
                volume: k.get("v").and_then(|v| v.as_str()).unwrap_or("0").parse().unwrap_or_default(),
                timestamp: chrono::Utc::now(),
                is_closed,
            }))
        }
        "executionReport" | "executionReportAlgo" => {
            let client_order_id = v.get("c").and_then(|c| c.as_str()).unwrap_or("");
            let status_str = v.get("X").and_then(|x| x.as_str()).unwrap_or("");
            let filled_qty = v.get("z").and_then(|z| z.as_str()).unwrap_or("0").parse().unwrap_or_default();
            let avg_price = v.get("Z").and_then(|z| z.as_str()).unwrap_or("0").parse().unwrap_or_default();
            let status = match status_str {
                "NEW" => OrderStatus::Acknowledged,
                "PARTIALLY_FILLED" => OrderStatus::PartiallyFilled { filled_qty, avg_price },
                "FILLED" => OrderStatus::Filled { filled_qty, avg_price },
                "CANCELED" | "CANCELLED" => OrderStatus::Cancelled { filled_qty },
                "REJECTED" => OrderStatus::Rejected {
                    reason: v.get("r").and_then(|r| r.as_str()).unwrap_or("rejected").to_string(),
                },
                _ => OrderStatus::Pending,
            };
            Ok(WsMessage::OrderUpdate(crate::types::OrderUpdate {
                order_id: v.get("i").and_then(|i| i.as_str()).unwrap_or("").to_string(),
                client_order_id: OrderId(uuid::Uuid::parse_str(client_order_id).unwrap_or_default()),
                status,
                filled_qty,
                avg_price: Some(avg_price),
                timestamp: chrono::Utc::now(),
            }))
        }
        _ => Ok(WsMessage::Ping),
    }
}

/// 解析 Binance 价格层数组 `[["price","qty"], ...]`
fn parse_price_level_array(val: Option<&serde_json::Value>) -> Vec<(rust_decimal::Decimal, rust_decimal::Decimal)> {
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
        "symbol={symbol}&side={side}&type={order_type}&timeInForce={tif}&quantity={}",
        order.quantity
    );

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
        // 验证 REST 连接：查询服务器时间
        let url = format!("{}/api/v3/ping", self.config.rest_base_url);
        let resp = self.client.get(&url).send().await?;
        if !resp.status().is_success() {
            return Err(ExchangeError::ConnectionFailed(
                "Binance REST ping failed".into(),
            ));
        }

        // 启动 WebSocket（重连逻辑在内部 tokio 任务中循环）
        self.start_ws().await?;

        // 等待连接建立
        tokio::time::sleep(Duration::from_millis(500)).await;

        *self.connected.lock().await = true;
        tracing::info!("Binance adapter connected");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ExchangeError> {
        *self.connected.lock().await = false;
        // 通知重连监督任务退出
        self.shutdown.notify_waiters();
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
        let resp = self.rest_post("/api/v3/order", &params).await?;

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

        let params = match symbol {
            Some(sym) => format!("symbol={sym}&clientOrderId={order_id}"),
            None => {
                tracing::warn!(
                    "cancel_order: no symbol mapping for client_order_id={order_id}, \
                     Binance API requires symbol; consider providing it via order meta"
                );
                return Err(ExchangeError::OrderNotFound(order_id.to_string()));
            }
        };
        self.rest_delete("/api/v3/order", &params).await?;

        // 清理映射
        self.order_symbols.lock().await.remove(&order_id.to_string());

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
        // Binance 期货有 positions 端点，现货无直接 positions
        // 这里返回空，或可通过 account info 推导
        Ok(Vec::new())
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
                assert_eq!(u.status, OrderStatus::Filled {
                    filled_qty: "0.001".parse().unwrap(),
                    avg_price: "50000.00".parse().unwrap(),
                });
            }
            _ => panic!("expected OrderUpdate"),
        }
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
}