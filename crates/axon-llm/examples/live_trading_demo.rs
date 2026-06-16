//! `live_trading_demo` —— axon-llm 真实 LLM 端到端 demo
//!
//! 演示:
//! 1. 通过 `--config` 参数 + 5 级 fallback 加载统一 `LlmConfig`
//! 2. 用 `OpenAICompatConfig::from_llm_config` 构造 backend
//! 3. 真实调 DeepSeek / OpenAI / 任意 OpenAI 兼容 API
//! 4. 跑一次"工具调用 → 解析"循环
//!
//! 运行:
//! ```bash
//! cp crates/axon-llm/demo/bin/config.toml config.local.toml
//! # 编辑 config.local.toml,在 [[backends]] 填入真实 api_key
//! cargo run -p axon-llm --example live_trading_demo \
//!   --features demo -- --config config.local.toml
//! ```
//!
//! 也支持环境变量 `AXON_LLM_CONFIG=path/to/config.local.toml`。
//!
//! 退出码:
//!  0 — 成功
//!  1 — 配置 / 环境错误(缺 API key、config 解析失败)
//!  2 — backend 错误(网络 / 限流 / 解析)
//!  3 — 工具执行错误

use std::path::PathBuf;

use axon_llm::backend::{LLMBackend, LLMError, ToolDefinition};
use axon_llm::backends::{OpenAICompatBackend, OpenAICompatConfig};
use axon_llm::config::LlmConfig;
use axon_llm::types::Message;

fn main() {
    // 1. 解析 --config 参数(env var: AXON_LLM_CONFIG 兜底)
    let explicit_path = parse_config_arg();

    // 2. 5 级 fallback 解析 LlmConfig
    let cwd = std::env::current_dir().expect("cwd");
    let cfg = match LlmConfig::resolve_with_fallback(explicit_path.as_deref(), &cwd) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("❌ 加载 config 失败: {e}");
            eprintln!("   提示: cp crates/axon-llm/demo/bin/config.toml config.local.toml");
            eprintln!("   然后编辑 config.local.toml 填入 api_key");
            std::process::exit(1);
        }
    };

    // 3. 构造 backend
    let compat = match OpenAICompatConfig::from_llm_config(&cfg, 0) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("❌ 构造 backend 失败: {e}");
            std::process::exit(1);
        }
    };
    let backend = OpenAICompatBackend::new(compat);
    println!(
        "▶ backend 初始化完成: {} (model={})",
        cfg.backends[0].base_url, cfg.backends[0].model
    );

    // 4. 启 tokio runtime 跑异步
    let rt = tokio::runtime::Runtime::new().expect("create tokio runtime");
    if let Err(e) = rt.block_on(run_demo(backend)) {
        eprintln!("❌ demo 失败: {e}");
        std::process::exit(match e {
            DemoError::Backend(_) => 2,
            DemoError::Tool(_) => 3,
        });
    }
}

/// 解析命令行 `--config` / `-c` 参数,或环境变量 `AXON_LLM_CONFIG`
fn parse_config_arg() -> Option<PathBuf> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == "--config" || a == "-c")
        .and_then(|i| args.get(i + 1).map(PathBuf::from))
        .or_else(|| std::env::var("AXON_LLM_CONFIG").ok().map(PathBuf::from))
}

async fn run_demo(backend: OpenAICompatBackend) -> Result<(), DemoError> {
    // 阶段 1: 简单对话
    println!("\n=== 阶段 1: 简单对话 ===");
    let intro_query = "Hi! Please introduce yourself in one short paragraph.";
    println!("user: {intro_query}");
    let msgs = vec![Message::user(intro_query)];
    let resp = backend.complete(&msgs).await.map_err(DemoError::Backend)?;
    println!("assistant: {}", resp.content.unwrap_or_default());
    println!(
        "token usage: prompt={} completion={} total={}",
        resp.token_usage.prompt_tokens,
        resp.token_usage.completion_tokens,
        resp.token_usage.total_tokens
    );

    // 阶段 2: 工具调用
    println!("\n=== 阶段 2: 工具调用 ===");
    let tools = vec![ToolDefinition {
        name: "get_quote".into(),
        description: "Get the latest quote for a stock symbol".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "symbol": {"type": "string", "description": "Stock ticker, e.g. AAPL"}
            },
            "required": ["symbol"]
        }),
    }];

    let tool_query = "What's the current price of AAPL? Use the get_quote tool.";
    println!("user: {tool_query}");
    let resp2 = backend
        .complete_with_tools(&[Message::user(tool_query)], &tools)
        .await
        .map_err(DemoError::Backend)?;

    if resp2.has_tool_calls() {
        let tc = &resp2.tool_calls.expect("tool_calls")[0];
        println!(
            "assistant 决定调用工具: {}({})",
            tc.function_name, tc.arguments
        );
        // 真实场景:这里会执行 broker API;demo 直接 mock 返回
        let mock_result = r#"{"symbol":"AAPL","price":178.42,"note":"mock result from demo (no real broker call)"}"#;
        println!("tool result: {mock_result}");

        // 5. 把 tool result 喂回 LLM,获得自然语言答复
        let follow_up = vec![
            Message::user(tool_query),
            Message::assistant(""),
            axon_llm::types::Message {
                role: axon_llm::types::Role::Assistant,
                content: String::new(),
                tool_call_id: None,
                tool_calls: Some(vec![tc.clone()]),
            },
            Message::tool_result(&tc.id, mock_result),
        ];
        let resp3 = backend
            .complete(&follow_up)
            .await
            .map_err(DemoError::Backend)?;
        println!(
            "\nassistant(基于工具结果): {}",
            resp3.content.unwrap_or_default()
        );
    } else {
        println!(
            "assistant(未调用工具): {}",
            resp2.content.unwrap_or_default()
        );
    }

    Ok(())
}

#[derive(Debug)]
enum DemoError {
    Backend(LLMError),
    /// 本地 tool 执行错误变体。
    ///
    /// **告警抑制决策**(按 workspace rule #4):`Tool` variant 当前只在 `match` arm
    /// (主函数 `=> 3`)和 `Display` impl 中被读取,但没有构造点(本 demo 暂未接入
    /// 真实 broker tool,仅在 LLM 工具调用循环里 mock 返回)。rustc dead_code
    /// lint 仍会警告"variant never constructed"。
    ///
    /// 保留该 variant 是为未来接入真实 broker API(任务 Task 5 `integrated_trading_demo`
    /// 或后续 broker 适配)时无需反复改动 demo 错误类型。`#[allow(dead_code)]` 是
    /// **必须**保留的抑制项。
    #[allow(dead_code)]
    Tool(String),
}

impl std::fmt::Display for DemoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backend(e) => write!(f, "backend error: {e}"),
            Self::Tool(s) => write!(f, "tool error: {s}"),
        }
    }
}

impl std::error::Error for DemoError {}
