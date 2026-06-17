//! `live_trading_e2e` —— axon-llm 端到端交易 demo
//!
//! 演示完整的"LLM agent → PlaceOrderTool → TradingBackend"链路:
//! 1. 通过 `--config` 加载统一 `LLMConfig`(5 级 fallback)
//! 2. 构造 `OpenAICompatBackend`
//! 3. 选 `TradingBackend`:
//!    - `AXON_DEMO_DRY_RUN=1` → `MockTradingBackend`(无网络,CI / 离线体验)
//!    - 否则 → `ExchangeTradingBackend` + `BinanceAdapter` (testnet creds)
//! 4. 桥接 `axon-risk::CircuitBreaker` → axon-llm `RiskGate`
//! 5. 构造 `ReActAgent` + `PlaceOrderTool` (TwoPhase 模式) + `QueryPortfolioTool`
//! 6. 跑一次 ReAct 推理(LLM 看行情 → 决定下单 → 触发 TwoPhase 第一次 →
//!    立即 confirm → backend 真发)
//!
//! 运行:
//! ```bash
//! # DryRun 模式(默认安全,无网络):
//! cp crates/axon-llm/demo/bin/config.toml config.local.toml
//! # 编辑 config.local.toml,填入 api_key(任何 OpenAI 兼容服务)
//! AXON_DEMO_DRY_RUN=1 cargo run -p axon-llm --example live_trading_e2e \
//!   --features "demo,trading-exchange" -- --config config.local.toml
//!
//! # 真实 testnet 模式(需 Binance testnet 凭证):
//! export AXON_BINANCE_TESTNET_API_KEY=...
//! export AXON_BINANCE_TESTNET_API_SECRET=...
//! cargo run -p axon-llm --example live_trading_e2e \
//!   --features "demo,trading-exchange" -- --config config.local.toml
//! ```
//!
//! 也支持 `AXON_LLM_CONFIG=path/to/config.local.toml` 环境变量。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axon_llm::agent::AgentConfig;
use axon_llm::backends::{OpenAICompatBackend, OpenAICompatConfig};
use axon_llm::config::LLMConfig;
use axon_llm::react_agent::ReActAgent;
use axon_llm::trading::exchange::{ExchangeTradingBackend, SymbolMap};
use axon_llm::trading::mock::MockTradingBackend;
use axon_llm::trading::{
    DailyCounter, PlaceOrderTool, QueryPortfolioTool, RiskGate, RiskLimits, SafetyMode,
    TradingBackend,
};
use tracing::{info, warn};

fn main() {
    // 1. 解析 --config 参数(env var: AXON_LLM_CONFIG 兜底)
    let explicit_path = parse_config_arg();

    // 2. 5 级 fallback 解析 LLMConfig
    let cwd = std::env::current_dir().expect("cwd");
    let cfg = match LLMConfig::resolve_with_fallback(explicit_path.as_deref(), &cwd) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("❌ 加载 config 失败: {e}");
            eprintln!("   提示: cp crates/axon-llm/demo/bin/config.toml config.local.toml");
            eprintln!("   然后编辑 config.local.toml 填入 api_key");
            std::process::exit(1);
        }
    };

    // 3. 构造 OpenAICompat backend
    let compat = match OpenAICompatConfig::from_llm_config(&cfg, 0) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("❌ 构造 backend 失败: {e}");
            std::process::exit(1);
        }
    };
    let backend = OpenAICompatBackend::new(compat);
    println!(
        "▶ LLM backend 初始化完成: {} (model={})",
        cfg.backends[0].base_url, cfg.backends[0].model
    );

    // 4. 选 TradingBackend(DryRun / 真实 testnet)
    let trading: Arc<dyn TradingBackend> = match select_backend() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("❌ 构造 trading backend 失败: {e}");
            std::process::exit(1);
        }
    };
    println!("▶ Trading backend 初始化完成: {}", backend_label());

    // 5. 启 tokio runtime
    let rt = tokio::runtime::Runtime::new().expect("create tokio runtime");
    if let Err(e) = rt.block_on(run_demo(backend, trading)) {
        eprintln!("❌ demo 失败: {e}");
        std::process::exit(match e {
            DemoError::Config(_) => 1,
            DemoError::Backend(_) => 2,
            DemoError::Tool(_) => 3,
        });
    }
}

/// 解析 `--config` / `-c` 命令行参数,或回退到 `AXON_LLM_CONFIG` 环境变量
fn parse_config_arg() -> Option<PathBuf> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == "--config" || a == "-c")
        .and_then(|i| args.get(i + 1).map(PathBuf::from))
        .or_else(|| std::env::var("AXON_LLM_CONFIG").ok().map(PathBuf::from))
}

/// 选 backend:`AXON_DEMO_DRY_RUN=1` 走 Mock,否则走 Binance testnet
fn select_backend() -> Result<Arc<dyn TradingBackend>, String> {
    if std::env::var("AXON_DEMO_DRY_RUN").ok().as_deref() == Some("1") {
        info!("AXON_DEMO_DRY_RUN=1 → 走 MockTradingBackend");
        Ok(Arc::new(MockTradingBackend::new()))
    } else {
        let key = std::env::var("AXON_BINANCE_TESTNET_API_KEY").map_err(|_| {
            "未设置 AXON_BINANCE_TESTNET_API_KEY;若想离线体验请设 AXON_DEMO_DRY_RUN=1".to_string()
        })?;
        let secret = std::env::var("AXON_BINANCE_TESTNET_API_SECRET")
            .map_err(|_| "未设置 AXON_BINANCE_TESTNET_API_SECRET".to_string())?;
        let cfg = build_testnet_config(key, secret);
        // ExchangeTradingBackend::new 接收 `Box<dyn ExchangeAdapter>`,需要 Box 化
        let adapter = Box::new(axon_exchange::adapters::binance::BinanceAdapter::new(cfg));
        let mut symbol_map = SymbolMap::new();
        symbol_map.register("BTC-USDT", "BTCUSDT");
        info!("→ 走 ExchangeTradingBackend + Binance testnet");
        Ok(Arc::new(ExchangeTradingBackend::new(adapter, symbol_map)))
    }
}

/// 构造 Binance testnet 配置(参考 trading_exchange_testnet.rs)
fn build_testnet_config(key: String, secret: String) -> axon_exchange::ExchangeConfig {
    use axon_exchange::{ExchangeId, RateLimitConfig, ReconnectConfig};
    axon_exchange::ExchangeConfig {
        exchange_id: ExchangeId::Binance,
        api_key: key,
        api_secret: secret,
        passphrase: None,
        testnet: true,
        rest_base_url: std::env::var("AXON_BINANCE_TESTNET_REST_URL")
            .unwrap_or_else(|_| "https://testnet.binance.vision".into()),
        ws_url: "wss://testnet.binance.vision/ws".into(),
        rate_limit: RateLimitConfig {
            requests_per_second: 10,
            orders_per_minute: 60,
            ws_messages_per_second: 50,
        },
        reconnect: ReconnectConfig {
            max_retries: 3,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(10),
            backoff_multiplier: 2.0,
            circuit_breaker_threshold: 5,
            circuit_breaker_reset: Duration::from_secs(60),
        },
        proxy: None,
        position_endpoint: String::new(),
        fapi_base_url: None,
    }
}

/// 当前 backend 类型(打印用)
fn backend_label() -> &'static str {
    if std::env::var("AXON_DEMO_DRY_RUN").ok().as_deref() == Some("1") {
        "MockTradingBackend (DryRun, 无网络)"
    } else {
        "ExchangeTradingBackend (Binance testnet, 真实 HTTP)"
    }
}

// ── CircuitBreaker 桥接(Stage J 简化版)───────────────────

/// `RiskGate` 实现:桥接 `axon_risk::CircuitBreaker`
///
/// **不在 lib 暴露 `CircuitBreaker` 强依赖**:axon-llm 的 `RiskGate` 是
/// 内部 trait,demo 侧把 `axon_risk::CircuitBreaker` 适配进来。
struct CircuitBreakerGate {
    cb: Arc<axon_risk::circuit_breaker::CircuitBreaker>,
}

impl RiskGate for CircuitBreakerGate {
    fn is_blocked(&self) -> Option<String> {
        if self.cb.is_active() {
            Some("circuit breaker active (cooldown 未结束)".to_string())
        } else {
            None
        }
    }
}

// ── 主流程 ────────────────────────────────────────────────

#[derive(Debug)]
enum DemoError {
    /// 配置 / 环境错误
    ///
    /// **告警抑制决策**(按 workspace rule #4):本 variant 在主函数 `process::exit(1)`
    /// 路径下被 eprintln 打印后退出,未通过 `?` 透传构造;`match` arm 中被读取
    /// 用于决定退出码。保留为 demo 扩展预留(后续支持更多配置错误类型)。
    #[allow(dead_code)]
    Config(String),
    /// LLM / backend 错误
    Backend(String),
    /// 工具执行错误
    ///
    /// **告警抑制决策**(按 workspace rule #4):本 variant 当前在主函数 `match` arm
    /// (`=> 3`)和 `Display` impl 中被读取,但 demo 主体流程用 `?` 透传时构造。
    /// 保留为未来真实错误码扩展预留。
    #[allow(dead_code)]
    Tool(String),
}

impl std::fmt::Display for DemoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(s) => write!(f, "config error: {s}"),
            Self::Backend(s) => write!(f, "backend error: {s}"),
            Self::Tool(s) => write!(f, "tool error: {s}"),
        }
    }
}

impl std::error::Error for DemoError {}

async fn run_demo(
    backend: OpenAICompatBackend,
    trading: Arc<dyn TradingBackend>,
) -> Result<(), DemoError> {
    // 1. 风控闸门(可选用 CircuitBreaker)
    let cb = Arc::new(axon_risk::circuit_breaker::CircuitBreaker::new(
        /* daily_loss_limit = */ 50.0,
        /* cooldown = */ Duration::from_secs(60),
    ));
    let gate: Arc<dyn RiskGate> = Arc::new(CircuitBreakerGate { cb: cb.clone() });
    info!("▶ 闸门: CircuitBreaker (daily_loss_limit=50, cooldown=60s);DryRun 不触发,真发前检查");

    // 2. 风控规则
    let limits = RiskLimits {
        max_order_notional: Some(100.0), // 单笔 ≤ 100 USDT
        max_daily_orders: Some(20),
        allowed_symbols: Some(vec!["BTC-USDT".into()]),
        ..Default::default()
    };
    let daily = Arc::new(DailyCounter::default());

    // 3. 工具(TwoPhase 模式,需要二次 confirm 才真发)
    //
    // 注意:`add_tool` 接 `Box<dyn Tool>`,所以工具不能放进 Arc。
    // `trading` 在 `Arc<dyn TradingBackend>` 中共享给两个工具。
    let place =
        PlaceOrderTool::with_gate(trading.clone(), SafetyMode::TwoPhase, limits, daily, gate);
    let query = QueryPortfolioTool::new(trading.clone());

    // 4. ReAct agent(max_iterations=3:决策 → 工具 → 决策,够简单 demo 用)
    let mut agent = ReActAgent::new(Box::new(backend), AgentConfig::new().with_max_iterations(3));
    agent.add_tool(Box::new(place));
    agent.add_tool(Box::new(query));
    info!("▶ ReAct agent 初始化完成: max_iterations=3, tools=[place_order, query_portfolio]");

    // 5. 跑一次决策 + 下单
    let prompt = "当前 BTC 行情 $65,000,你有 $10,000 现金。决策:是否下 0.001 BTC 多单? \
                  若要下单,用 place_order 工具(symbol=BTC-USDT, side=Buy, quantity=0.001, \
                  order_type=Limit, price=50000 远低于市价避免成交),然后基于工具结果给出最终答复。";
    println!("\n=== ReAct 推理 ===");
    println!("user: {prompt}");

    let resp = agent
        .reason(prompt)
        .await
        .map_err(|e| DemoError::Backend(e.to_string()))?;

    println!("\n=== 决策结果 ===");
    println!("LLM 决策: {}", resp.answer);
    println!("迭代轮次: {}", resp.iterations);
    println!(
        "token 用量: prompt={} completion={} total={}",
        resp.token_usage.prompt_tokens,
        resp.token_usage.completion_tokens,
        resp.token_usage.total_tokens
    );
    println!("\n=== 推理链 ===");
    for step in &resp.reasoning_trace {
        println!("  step {}: thought={}", step.step, step.thought);
        if let Some(action) = &step.action {
            println!("    action: {}({})", action.function_name, action.arguments);
        }
        if let Some(obs) = &step.observation {
            // observation 是 tool 的 JSON 字符串,这里只打印前 200 字符
            let preview: String = obs.chars().take(200).collect();
            println!("    observation: {preview}...");
        }
    }

    // 6. 风控示例:模拟触发熔断器,演示闸门被激活
    println!("\n=== 风控演示:模拟触发 CircuitBreaker ===");
    cb.check_and_trigger(-100.0); // 模拟单日亏损 100 USDT
    warn!("CircuitBreaker 已被触发(daily_pnl=-100 超过 daily_loss_limit=50)");

    // 直接演示闸门被激活(不通过 tool,仅展示效果)
    let gate_after: Arc<dyn RiskGate> = Arc::new(CircuitBreakerGate { cb: cb.clone() });
    match gate_after.is_blocked() {
        Some(reason) => println!("✓ 闸门已激活,后续真发路径会被阻断: {reason}"),
        None => println!("✗ 闸门未激活(异常,应已触发)"),
    }
    Ok(())
}
