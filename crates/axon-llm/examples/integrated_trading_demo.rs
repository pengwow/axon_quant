//! `integrated_trading_demo` —— axon-llm 集成示例
//!
//! 演示三阶段:
//!   阶段 1: 多 backend 简单对话(每个 backend 独立问同一 query)
//!   阶段 2: 多 backend ensemble voting(基于 YES/NO 答案的 hard vote)
//!   阶段 3: 用 axon-explain `ReportGenerator` 生成决策报告(Markdown)
//!
//! 运行:
//!   cp crates/axon-llm/demo/bin/config.toml config.local.toml
//!   # 编辑 config.local.toml,在 [[backends]] 中填入真实 api_key(可填 1..3 个)
//!   cargo run -p axon-llm --example integrated_trading_demo \
//!     --features demo -- --config config.local.toml
//!
//! 也支持环境变量 `AXON_LLM_CONFIG=path/to/config.local.toml`。
//!
//! 注意:本 demo 需要 `[[backends]]` 数组(支持 1..N 个)。单 backend 会自动退化为
//! 单一票,ensemble 退化为单一决策。

use std::path::PathBuf;

use axon_llm::backend::LLMBackend;
use axon_llm::backends::{OpenAICompatBackend, OpenAICompatConfig};
use axon_llm::config::LLMConfig;
use axon_llm::types::Message;

use axon_ensemble::traits::VotingStrategy;
use axon_ensemble::types::{Action, ActionProbabilities, ActionType, ModelPrediction, ModelType};
use axon_ensemble::voting::HardVoteStrategy;

use axon_explain::report::ReportGenerator;
use axon_explain::types::{
    ActionSnapshot, AttentionWeights, ContributionDirection, DecisionReport, Explanation,
    FeatureContribution,
};

fn main() {
    // 1. 解析 --config 参数(env var: AXON_LLM_CONFIG 兜底)
    let explicit_path = parse_config_arg();
    let cwd = std::env::current_dir().expect("cwd");

    // 2. 5 级 fallback 解析 LLMConfig
    let cfg = match LLMConfig::resolve_with_fallback(explicit_path.as_deref(), &cwd) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("❌ 加载 config 失败: {e}");
            eprintln!("   提示: cp crates/axon-llm/demo/bin/config.toml config.local.toml");
            eprintln!("   然后编辑 config.local.toml 填入 api_key(可填 1..3 个 backend)");
            std::process::exit(1);
        }
    };

    if cfg.backends.is_empty() {
        eprintln!("❌ 配置中没有 backend,退出");
        std::process::exit(1);
    }
    println!(
        "▶ 已加载 {} 个 backend,backend[0] = {} (model={})",
        cfg.backends.len(),
        cfg.backends[0].base_url,
        cfg.backends[0].model
    );

    // 3. 启 tokio runtime 跑异步
    let rt = tokio::runtime::Runtime::new().expect("create tokio runtime");
    if let Err(e) = rt.block_on(run_all_phases(&cfg)) {
        eprintln!("❌ demo 失败: {e}");
        std::process::exit(1);
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

#[derive(Debug)]
enum DemoError {
    /// LLM backend 错误占位变体(本 demo 阶段 1 把错误吞掉,转写入 `raw_answer = "ERROR: ..."`
    /// 以便 ensemble 阶段仍能处理)。**告警抑制决策**(按 workspace rule #4):当前阶段未
    /// 显式构造(后续接入真实 broker / RL 集成时可能用到)。`#[allow(dead_code)]` 是
    /// **必须**保留的抑制项。
    #[allow(dead_code)]
    Backend(String),
    /// 初始化错误(配置解析 / backend 构造失败)
    Init(String),
}

impl std::fmt::Display for DemoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backend(s) => write!(f, "backend error: {s}"),
            Self::Init(s) => write!(f, "init error: {s}"),
        }
    }
}

impl std::error::Error for DemoError {}

/// 顺序执行三阶段 demo
async fn run_all_phases(cfg: &LLMConfig) -> Result<(), DemoError> {
    // 阶段 1: 多 backend 简单对话
    println!("\n=== 阶段 1: 多 backend 简单对话 ===");
    let responses = phase1_parallel_collect(cfg).await?;

    // 阶段 2: ensemble voting
    println!("\n=== 阶段 2: Ensemble voting(HardVoteStrategy) ===");
    let decision = phase2_ensemble_vote(&responses);

    // 阶段 3: explain 报告
    println!("\n=== 阶段 3: Explain 报告(DecisionReport Markdown) ===");
    phase3_explain_report(&responses, &decision, cfg)?;

    Ok(())
}

/// 阶段 1:每个 backend 问同样的 query,串行收集响应
///
/// 串行而非并行的考虑:不同厂商 rate limit 不同,并发可能触发限流;
/// demo 更关注可读性,串行输出更清晰。
async fn phase1_parallel_collect(cfg: &LLMConfig) -> Result<Vec<BackendResponse>, DemoError> {
    let query = "Should I buy BTC at $65000 right now? Answer YES or NO in one word.";
    let mut out = Vec::with_capacity(cfg.backends.len());
    for (i, b) in cfg.backends.iter().enumerate() {
        let compat = OpenAICompatConfig::from_llm_config(cfg, i)
            .map_err(|e| DemoError::Init(e.to_string()))?;
        let backend = OpenAICompatBackend::new(compat);
        let msgs = vec![Message::user(query)];
        match backend.complete(&msgs).await {
            Ok(resp) => {
                let answer = resp.content.unwrap_or_default();
                println!(
                    "  backend[{i}] ({}, model={}) → {}",
                    b.name, b.model, answer
                );
                out.push(BackendResponse {
                    backend_index: i,
                    backend_name: b.name.clone(),
                    model: b.model.clone(),
                    raw_answer: answer,
                });
            }
            Err(e) => {
                eprintln!(
                    "  backend[{i}] ({}, model={}) → ERROR: {e}",
                    b.name, b.model
                );
                // 错误也收集,让 ensemble 阶段决定如何处理
                out.push(BackendResponse {
                    backend_index: i,
                    backend_name: b.name.clone(),
                    model: b.model.clone(),
                    raw_answer: format!("ERROR: {e}"),
                });
            }
        }
    }
    Ok(out)
}

/// 阶段 2:把 YES/NO 答案转成 `ModelPrediction`,用 `HardVoteStrategy` 投票
fn phase2_ensemble_vote(responses: &[BackendResponse]) -> EnsembleDecision {
    let strategy = HardVoteStrategy;
    let predictions: Vec<ModelPrediction> = responses.iter().map(parse_to_prediction).collect();

    let action: Action = strategy.combine(&predictions);
    let decision = EnsembleDecision {
        action: action.action_type,
        confidence: action.confidence,
        buy: predictions
            .iter()
            .filter(|p| p.action.action_type == ActionType::Buy)
            .count(),
        sell: predictions
            .iter()
            .filter(|p| p.action.action_type == ActionType::Sell)
            .count(),
        hold: predictions
            .iter()
            .filter(|p| p.action.action_type == ActionType::Hold)
            .count(),
        total: predictions.len(),
    };

    println!(
        "  → ensemble 决策: {:?} (confidence={:.2}); 票数 buy={} sell={} hold={} / total={}",
        decision.action,
        decision.confidence,
        decision.buy,
        decision.sell,
        decision.hold,
        decision.total
    );
    decision
}

/// 阶段 3:把 ensemble 决策构造为 `DecisionReport`,渲染 Markdown
///
/// 注意:此阶段不调 LLM(避免重复计费);输入是阶段 1/2 的内存数据。
/// 真实集成场景下,`ReportGenerator` 的输入是 RL/规则策略生成的 Explanation 列表,
/// demo 简化为:每个 backend 的回答视为一个"决策"特征。
fn phase3_explain_report(
    responses: &[BackendResponse],
    decision: &EnsembleDecision,
    cfg: &LLMConfig,
) -> Result<(), DemoError> {
    // 构造 Explanation 列表:每个 backend 回答对应一条 Explanation
    let explanations: Vec<Explanation> = responses
        .iter()
        .map(|r| {
            // 特征重要性:用 backend name / 答案长度 / 是否含 YES 等简单启发式
            let contains_yes = r.raw_answer.to_uppercase().contains("YES");
            let contains_no = r.raw_answer.to_uppercase().contains("NO");
            let buy_signal = if contains_yes && !contains_no {
                0.8
            } else {
                0.0
            };
            let sell_signal = if contains_no && !contains_yes {
                0.8
            } else {
                0.0
            };
            let len_norm = (r.raw_answer.len() as f64 / 100.0).clamp(0.0, 1.0);

            Explanation {
                id: format!("exp-{}", r.backend_index),
                observation_id: format!("obs-{}", r.backend_index),
                action: ActionSnapshot {
                    position_size: 0.0,
                    entry_price: 65000.0,
                    stop_loss: 0.0,
                    take_profit: 0.0,
                    order_type: "MARKET".to_string(),
                },
                feature_importance: [
                    ("buy_signal".to_string(), buy_signal),
                    ("sell_signal".to_string(), sell_signal),
                    ("response_length".to_string(), len_norm),
                ]
                .into_iter()
                .collect(),
                action_attributions: Vec::new(),
                attention_weights: Some(Vec::<AttentionWeights>::new()),
                counterfactuals: Vec::new(),
                summary: format!(
                    "backend[{}] ({}, model={}) 回答: {}",
                    r.backend_index, r.backend_name, r.model, r.raw_answer
                ),
                confidence: 0.5,
                generated_at: chrono::Utc::now(),
            }
        })
        .collect();

    // 报告期间:用 UTC 当前时间窗
    let now = chrono::Utc::now();
    let report: DecisionReport = ReportGenerator::generate_decision_report(
        "integrated_trading_demo_report",
        explanations,
        now - chrono::Duration::seconds(60),
        now,
    );

    let md = report.markdown_content.clone().unwrap_or_default();
    println!("\n--- DecisionReport (Markdown) ---");
    println!("{md}");
    println!("--- 结束 ---\n");

    if cfg.explain.record_decisions {
        // 若用户在 config 中开启 record_decisions,则持久化到 store_path
        let path = cfg
            .explain
            .store_path
            .clone()
            .unwrap_or_else(|| "./explain_decisions.jsonl".to_string());
        println!("  决策将持久化到: {path} (本 demo 不实际写文件,留给 ReAct 集成阶段使用)");
    } else {
        println!("  explain.record_decisions = false,跳过持久化");
    }

    // 输出 ensemble 决策概要(供脚本捕获)
    println!(
        "FINAL: action={:?} confidence={:.2}",
        decision.action, decision.confidence
    );
    // 引用 report 变量避免 unused 警告
    let _ = report.html_content.as_ref().map(|h| h.len()).unwrap_or(0);
    Ok(())
}

/// 单个 backend 的响应
struct BackendResponse {
    backend_index: usize,
    backend_name: String,
    model: String,
    raw_answer: String,
}

/// ensemble 阶段输出的最终决策
struct EnsembleDecision {
    action: ActionType,
    confidence: f64,
    buy: usize,
    sell: usize,
    hold: usize,
    total: usize,
}

/// 把 backend 响应解析为 `ModelPrediction`(用文本启发式推断 action_type)
fn parse_to_prediction(r: &BackendResponse) -> ModelPrediction {
    let upper = r.raw_answer.to_uppercase();
    // 错误信息(以 "ERROR:" 开头)视为 Hold
    let (action_type, buy_p, sell_p, hold_p) = if upper.starts_with("ERROR") {
        (ActionType::Hold, 0.0, 0.0, 1.0)
    } else if upper.contains("YES") && !upper.contains("NO") {
        (ActionType::Buy, 0.8, 0.1, 0.1)
    } else if upper.contains("NO") && !upper.contains("YES") {
        (ActionType::Sell, 0.1, 0.8, 0.1)
    } else {
        (ActionType::Hold, 0.33, 0.33, 0.34)
    };

    ModelPrediction {
        model_name: format!("{}/{}", r.backend_name, r.model),
        model_type: ModelType::RuleBased,
        action: Action {
            action_type,
            symbol: Some("BTC".to_string()),
            quantity: Some(0.0),
            confidence: 0.5,
        },
        confidence: 0.5,
        action_probs: ActionProbabilities::new(buy_p, sell_p, hold_p),
    }
}

// 静默 unused 警告(FeatureContribution / ContributionDirection 通过
// Explanation.feature_importance 间接使用,无需在 demo 中显式调用)。
#[allow(dead_code)]
fn _silence_unused() {
    let _: FeatureContribution = FeatureContribution {
        feature_name: String::new(),
        shap_value: 0.0,
        feature_value: 0.0,
        direction: ContributionDirection::Neutral,
    };
}
