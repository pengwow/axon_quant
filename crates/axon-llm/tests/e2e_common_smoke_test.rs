//! 验证 `tests/common/` 模块本身的正确性(smoke test)。
//!
//! 注意:本文件中所有 mutate env var 的 test 都依赖 `--test-threads=1` 才能稳定运行;
//! 默认 cargo test 是并行的,env var 修改会跨 test 污染。
//!
//! 推荐跑法:
//! ```bash
//! cargo test -p axon-llm --features "backends e2e" \
//!     --test e2e_common_smoke_test -- --test-threads=1
//! ```

#![cfg(feature = "e2e")]

mod common;

use axon_llm::types::TokenUsage;

#[test]
fn fixtures_dir_ends_with_e2e_common_fixtures() {
    let p = common::fixtures_dir();
    let s = p.to_string_lossy();
    assert!(s.ends_with("tests/e2e/common/fixtures"), "got: {s}");
}

#[test]
fn fixture_path_joins_test_model_and_id() {
    let p = common::fixture_path("simple_chat", "deepseek-chat", "b9d0dff0e795");
    let s = p.to_string_lossy();
    assert!(
        s.ends_with("simple_chat/deepseek-chat/b9d0dff0e795.json"),
        "got: {s}"
    );
}

#[test]
fn assert_cost_under_passes_for_small_usage() {
    let usage = TokenUsage {
        prompt_tokens: 100,
        completion_tokens: 50,
        total_tokens: 150,
    };
    // 100 input + 50 output ≈ $0.000028
    common::assert_cost_under(&usage, "deepseek-chat", 0.001);
}

#[test]
#[should_panic(expected = "exceeds")]
fn assert_cost_under_panics_when_over_budget() {
    let usage = TokenUsage {
        prompt_tokens: 1_000_000,
        completion_tokens: 1_000_000,
        total_tokens: 2_000_000,
    };
    // 1M input + 1M output ≈ $0.42,超过 $0.01
    common::assert_cost_under(&usage, "deepseek-chat", 0.01);
}

#[test]
#[should_panic(expected = "no pricing for")]
fn assert_cost_under_panics_for_unknown_model() {
    let usage = TokenUsage {
        prompt_tokens: 100,
        completion_tokens: 50,
        total_tokens: 150,
    };
    common::assert_cost_under(&usage, "unknown_model_xyz", 1.0);
}

// ─── env-var-mutating tests(需 --test-threads=1)────────────

#[test]
fn env_set_key_makes_has_key_or_fixture_true() {
    // Rust 2024 起,`std::env::set_var` / `remove_var` 已标记为 safe
    // (底层 setenv/unsetenv 在所有主要平台上都是线程安全的;详见
    // Rust 1.83 stabilization notes)。
    std::env::set_var("DEEPSEEK_API_KEY", "sk-test-smoke");
    assert!(common::has_key_or_fixture("__t__", "__m__"));
    std::env::remove_var("DEEPSEEK_API_KEY");
}

#[test]
fn env_unset_key_makes_has_key_or_fixture_false() {
    std::env::remove_var("DEEPSEEK_API_KEY");
    assert!(!common::has_key_or_fixture("__t__", "__m__"));
}

#[test]
fn env_unset_key_makes_deepseek_backend_none() {
    std::env::remove_var("DEEPSEEK_API_KEY");
    assert!(common::deepseek_backend().is_none());
}

#[test]
fn env_set_key_makes_deepseek_backend_some() {
    std::env::set_var("DEEPSEEK_API_KEY", "sk-test-smoke");
    let backend = common::deepseek_backend();
    assert!(backend.is_some());
    std::env::remove_var("DEEPSEEK_API_KEY");
}
