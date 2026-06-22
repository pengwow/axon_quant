//! 集成测试入口
//!
//! 启用 axon-integration-tests crate 内的所有集成测试模块

#![allow(clippy::needless_range_loop)]

use axon_integration_tests::contract;
use axon_integration_tests::distributed_flow;
use axon_integration_tests::e2e_pipeline;
use axon_integration_tests::error_recovery_and_concurrency;
use axon_integration_tests::hpo_flow;
use axon_integration_tests::hpo_tracker;
use axon_integration_tests::matching_flow;
use axon_integration_tests::multi_objective;
use axon_integration_tests::phase4_e2e;
use axon_integration_tests::tracker_registry;
use axon_integration_tests::tracker_registry_flow;
use axon_integration_tests::walkforward_flow;
use axon_integration_tests::walkforward_registry;

// HPO + Tracker 集成测试
#[test]
fn hpo_tracker_trial_tracking() {
    hpo_tracker::run_hpo_trial_tracking();
}

#[test]
fn hpo_tracker_config_simulation() {
    hpo_tracker::run_hpo_config_simulation();
}

#[test]
fn hpo_tracker_batch_param_logging() {
    hpo_tracker::run_hpo_batch_param_logging();
}

// Walk-forward + Registry 集成测试
#[tokio::test]
async fn walkforward_registry_basic_flow() {
    walkforward_registry::test_walkforward_best_fold_registered().await;
}

#[tokio::test]
async fn walkforward_registry_window_combination() {
    walkforward_registry::test_walkforward_window_type_combination().await;
}

#[tokio::test]
async fn walkforward_registry_iterative_registration() {
    walkforward_registry::test_walkforward_iterative_registration().await;
}

// Tracker + Registry 集成测试
#[tokio::test]
async fn tracker_registry_metrics_drive_promotion() {
    tracker_registry::test_tracker_metrics_drive_promotion().await;
}

#[tokio::test]
async fn tracker_registry_metadata_consistency() {
    tracker_registry::test_tracker_registry_metadata_consistency().await;
}

#[tokio::test]
async fn tracker_registry_flush_independence() {
    tracker_registry::test_tracker_flush_independent_from_registry().await;
}

// 多目标 HPO + Pareto + Tracker
#[tokio::test]
async fn multi_objective_pareto_tracker() {
    multi_objective::test_multi_objective_with_pareto_and_tracker().await;
}

#[test]
fn multi_objective_dominance_transitivity() {
    multi_objective::test_pareto_dominance_transitivity();
}

#[test]
fn multi_objective_hpo_config() {
    multi_objective::test_hpo_multi_objective_config();
}

// 端到端训练管线
#[tokio::test]
async fn e2e_pipeline_full() {
    e2e_pipeline::test_end_to_end_training_pipeline().await;
}

#[tokio::test]
async fn e2e_pipeline_train_register_rollback() {
    e2e_pipeline::test_e2e_train_register_rollback().await;
}

#[tokio::test]
async fn e2e_pipeline_window_type_tracker() {
    e2e_pipeline::test_window_type_with_tracker_reporting().await;
}

// 错误恢复与并发场景
#[tokio::test]
async fn hpo_failure_does_not_pollute_registry() {
    error_recovery_and_concurrency::test_hpo_failure_does_not_pollute_registry().await;
}

#[tokio::test]
async fn concurrent_registry_registrations() {
    error_recovery_and_concurrency::test_concurrent_registry_registrations().await;
}

#[tokio::test]
async fn tracker_registry_data_consistency() {
    error_recovery_and_concurrency::test_tracker_registry_data_consistency().await;
}

#[tokio::test]
async fn purged_walkforward_registration() {
    error_recovery_and_concurrency::test_purged_walkforward_registration().await;
}

#[test]
fn config_serialization_roundtrip() {
    error_recovery_and_concurrency::test_config_serialization_roundtrip();
}

#[tokio::test]
async fn aggregate_oos_then_register() {
    error_recovery_and_concurrency::test_aggregate_oos_then_register().await;
}

// ── 契约测试（直接调用模块函数） ──
#[test]
fn contract_semver_basics() {
    contract::contract_semver_roundtrip_serde();
    contract::contract_semver_parse_display_roundtrip();
    contract::contract_semver_bump_invariant();
    contract::contract_semver_ordering();
}

#[test]
fn contract_enum_stability() {
    contract::contract_model_stage_serde_stable();
    contract::contract_model_stage_string_mapping_locked();
    contract::contract_trial_state_serde_stable();
    contract::contract_trial_state_predicates();
    contract::contract_study_direction_serde_stable();
    contract::contract_window_type_serde_stable();
    contract::contract_window_type_default();
    contract::contract_run_status_serde_stable();
}

#[test]
fn contract_data_serde_roundtrip() {
    contract::contract_trial_result_serde_stable();
    contract::contract_trial_result_backward_compat_missing_intermediate();
    contract::contract_walkforward_config_backward_compat();
    contract::contract_sampler_type_aliases();
    contract::contract_sampler_type_tpe_with_defaults();
    contract::contract_study_config_full_roundtrip();
    contract::contract_param_value_all_variants();
    contract::contract_metric_value_scalar_roundtrip();
    contract::contract_metric_value_histogram_roundtrip();
    contract::contract_metrics_roundtrip();
    contract::contract_metrics_default_zero();
}

#[test]
fn contract_external_string_mappings() {
    contract::contract_study_direction_optuna_string();
    contract::contract_run_status_mlflow_string();
}

#[test]
fn contract_breaking_change_detection() {
    contract::contract_hpo_result_required_fields();
    contract::contract_hpo_config_required_fields();
    contract::contract_f64_precision_preserved_is_metrics();
}

// ── 模糊测试（proptest）入口 ──
//
// proptest 测试在 `#[test]` 内部由 `proptest!` 宏展开，
// 这里仅作占位以便 cargo test 看到集成测试入口。
#[test]
fn fuzz_module_compiles() {
    // 实际 proptest 用例由 `axon-integration-tests::fuzz` 模块内部驱动。
    // 这里通过引用模块名来确保它被链接进测试二进制。
    let _ = std::any::type_name::<axon_integration_tests::fuzz::FuzzMarker>();
}

// ── Phase 4 契约测试 ──

#[test]
fn contract_phase4_risk_config() {
    contract::contract_risk_config_defaults();
    contract::contract_risk_result_serde();
}

#[test]
fn contract_phase4_oms_order_status() {
    contract::contract_oms_order_status_transitions();
    contract::contract_oms_order_snapshot_roundtrip();
}

#[test]
fn contract_phase4_inference_config() {
    contract::contract_inference_config_serde();
    contract::contract_inference_action_types();
}

#[test]
fn contract_phase4_monitor_metrics() {
    contract::contract_monitor_counter_inc_get();
    contract::contract_monitor_histogram_quantiles();
}

#[test]
fn contract_phase4_exchange_status() {
    contract::contract_exchange_order_status_terminal();
}

// ── Phase 4 端到端集成测试 ──

#[test]
fn phase4_e2e_full_trading_flow() {
    phase4_e2e::run_full_trading_flow();
}

#[test]
fn phase4_e2e_risk_rejection_flow() {
    phase4_e2e::run_risk_rejection_flow();
}

#[test]
fn phase4_e2e_circuit_breaker_flow() {
    phase4_e2e::run_circuit_breaker_flow();
}

#[test]
fn phase4_e2e_batch_trading_stats() {
    phase4_e2e::run_batch_trading_stats();
}

// ── 场景 1：回测引擎撮合全流程 ──

#[test]
fn matching_flow_ohlcv_data() {
    matching_flow::run_ohlcv_data_construction();
}

#[test]
fn matching_flow_engine_with_strategy() {
    matching_flow::run_engine_with_strategy_orders();
}

#[test]
fn matching_flow_fills_verification() {
    matching_flow::run_matching_and_verify_fills();
}

#[test]
fn matching_flow_order_state_machine() {
    matching_flow::run_order_state_machine();
}

#[test]
fn matching_flow_fee_verification() {
    matching_flow::run_fee_verification();
}

#[test]
fn matching_flow_market_impact() {
    matching_flow::run_market_impact_verification();
}

#[test]
fn matching_flow_l2_depth() {
    matching_flow::run_l2_depth_matching();
}

#[test]
fn matching_flow_l2_snapshot() {
    matching_flow::run_l2_depth_snapshot();
}

// ── 压力场景测试 ──

#[test]
fn matching_flow_flash_crash() {
    matching_flow::run_flash_crash_scenario();
}

#[test]
fn matching_flow_zero_liquidity() {
    matching_flow::run_zero_liquidity_rejection();
}

#[test]
fn matching_flow_large_order_impact() {
    matching_flow::run_large_order_impact();
}

#[test]
fn matching_flow_partial_fill() {
    matching_flow::run_partial_fill_update();
}

#[test]
fn matching_flow_rapid_churn() {
    matching_flow::run_rapid_order_churn();
}

// ── L3 多资产引擎测试 ──

#[test]
fn matching_flow_l3_multi_asset_routing() {
    matching_flow::run_l3_multi_asset_routing();
}

#[test]
fn matching_flow_l3_cross_pair_arbitrage() {
    matching_flow::run_l3_cross_pair_arbitrage();
}

#[test]
fn matching_flow_l3_snapshot_restore() {
    matching_flow::run_l3_snapshot_restore();
}

// ── 场景 3：HPO 超参数优化全流程 ──

#[test]
fn hpo_flow_mock_trials() {
    hpo_flow::run_hpo_with_mock_trials();
}

#[test]
fn hpo_flow_pareto_front() {
    hpo_flow::run_pareto_front_single_objective();
}

#[test]
fn hpo_flow_hypervolume() {
    hpo_flow::run_hypervolume_verification();
}

#[test]
fn hpo_flow_multi_objective() {
    hpo_flow::run_multi_objective_pareto();
}

#[test]
fn hpo_flow_empty_trials() {
    hpo_flow::run_empty_trials();
}

// ── 场景 4：Walk-Forward 验证全流程 ──

#[test]
fn walkforward_flow_config_creation() {
    walkforward_flow::run_walkforward_config_creation();
}

#[test]
fn walkforward_flow_splits_config() {
    walkforward_flow::run_splits_purge_embargo_config();
}

#[test]
fn walkforward_flow_folds() {
    walkforward_flow::run_forward_validation_folds();
}

#[test]
fn walkforward_flow_leakage() {
    walkforward_flow::run_leakage_detection();
}

#[test]
fn walkforward_flow_embargo() {
    walkforward_flow::run_embargo_exclusion();
}

#[test]
fn walkforward_flow_purge() {
    walkforward_flow::run_purge_overlapping_labels();
}

#[test]
fn walkforward_flow_deflated_sharpe() {
    walkforward_flow::run_deflated_sharpe();
}

#[test]
fn walkforward_flow_window_types() {
    walkforward_flow::run_window_type_difference();
}

// ── 场景 5：实验追踪全流程 ──

#[test]
fn tracker_registry_flow_creation() {
    tracker_registry_flow::run_tracker_creation();
}

#[test]
fn tracker_registry_flow_logging() {
    tracker_registry_flow::run_param_metric_logging();
}

#[test]
fn tracker_registry_flow_query() {
    tracker_registry_flow::run_metrics_query();
}

#[test]
fn tracker_registry_flow_multi_param() {
    tracker_registry_flow::run_multi_param_logging();
}

#[test]
fn tracker_registry_flow_status() {
    tracker_registry_flow::run_status_management();
}

// ── 场景 6：分布式训练全流程 ──

#[test]
fn distributed_flow_metrics() {
    distributed_flow::run_metrics_serialization();
}

#[test]
fn distributed_flow_checkpoint() {
    distributed_flow::run_checkpoint_save_load();
}

#[test]
fn distributed_flow_config() {
    distributed_flow::run_config_validation();
}

#[test]
fn distributed_flow_invalid_config() {
    distributed_flow::run_invalid_config_rejected();
}
