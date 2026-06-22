//! AXON Python 统一入口
//!
//! 将各个 crate 的 Python 绑定封装到 `axon_quant` 模块中。
//!
//! 默认禁用,需要时启用 `python` feature:
//! `cargo build -p axon-python --features python`
//! (需要本地 PYO3_PYTHON 与 Python 开发库)。

#![cfg(feature = "python")]

use pyo3::prelude::*;

// 公共异常基类 + 6 个子类的工厂入口(Stage 1-6 共享)。
// 必须在 `#[pymodule] _native` 顶部先调 `register_exceptions`,
// 确保 `axon-data::python::error::DataError` 等子类的 `create_exception!`
// 拿到已经存在的 `AxonError` 引用建立继承链。
mod error;

/// AXON Quant Python 模块(原生扩展,由 __init__.py 导入并重新导出)
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", "0.2.0")?;

    // 注册公共异常基类(必须先于各子模块的 `create_exception!`)
    error::register_exceptions(m)?;

    // axon-rl 子模块（使用 #[pymodule] 函数）
    let rl_module = PyModule::new(m.py(), "rl")?;
    axon_rl::python::axon_rl(&rl_module)?;
    m.add_submodule(&rl_module)?;

    // axon-tracker 子模块（使用 register_module 函数）
    let tracker_module = PyModule::new(m.py(), "tracker")?;
    axon_tracker::python::register_module(&tracker_module)?;
    m.add_submodule(&tracker_module)?;

    // axon-registry 子模块（使用 register_module 函数）
    let registry_module = PyModule::new(m.py(), "registry")?;
    axon_registry::python::register_module(&registry_module)?;
    m.add_submodule(&registry_module)?;

    // axon-hpo 子模块（使用 register_module 函数）
    let hpo_module = PyModule::new(m.py(), "hpo")?;
    axon_hpo::python::register_module(&hpo_module)?;
    m.add_submodule(&hpo_module)?;

    // axon-walk-forward 子模块（使用 register_module 函数）
    let wf_module = PyModule::new(m.py(), "walk_forward")?;
    axon_walk_forward::python::register_module(&wf_module)?;
    m.add_submodule(&wf_module)?;

    // axon-distributed 子模块（使用 register_module 函数）
    let dist_module = PyModule::new(m.py(), "distributed")?;
    axon_distributed::python::register_module(&dist_module)?;
    m.add_submodule(&dist_module)?;

    // axon-llm 子模块（OpenAI 兼容 LLM 后端的 PyO3 绑定）
    let llm_module = PyModule::new(m.py(), "llm")?;
    axon_llm::python::axon_llm(&llm_module)?;
    m.add_submodule(&llm_module)?;

    // axon-llm trading 子模块（Stage K:trading 工具的 PyO3 绑定）
    // 注:trading 是 llm 模块的子模块(由 `register_trading_module` 单独注册),
    //    在 `_native` 下单独暴露 trading,便于 Python 端 `from _native import trading`。
    let trading_module = PyModule::new(m.py(), "trading")?;
    axon_llm::python::trading::register_trading_module(&trading_module)?;
    m.add_submodule(&trading_module)?;

    // Stage 1:`axon-data` 子模块
    // 注:axon-data 内部已注册 error/types/sources/dataset/service 五个子模块,
    // 这里只调 `register_module` 把 `data` 挂到 `_native` 下。
    let data_module = PyModule::new(m.py(), "data")?;
    axon_data::python::register_module(&data_module)?;
    m.add_submodule(&data_module)?;

    // Stage 2:`axon-backtest` 子模块
    // 注:axon-backtest 内部已注册 error/types/matching_l1/matching_l2/
    // matching_l3/impact/engine 七个 Python 子模块,这里只调
    // `register_module` 把 `backtest` 挂到 `_native` 下。
    // 设计约束:axon-backtest 不依赖 axon-python(避免 cargo 循环),
    // BacktestError 继承 builtin PyException 而非 AxonError。
    let backtest_module = PyModule::new(m.py(), "backtest")?;
    axon_backtest::python::register_module(&backtest_module)?;
    m.add_submodule(&backtest_module)?;

    // Stage 3:`axon-risk` 子模块
    // 注:axon-risk 内部已注册 error/config/engine/circuit_breaker/metrics
    // 五个 Python 子模块,这里只调 `register_module` 把 `risk` 挂到
    // `_native` 下。设计约束同 backtest:RiskError 不继承 AxonError,
    // axon-risk 不依赖 axon-python(避免 cargo 循环)。
    let risk_module = PyModule::new(m.py(), "risk")?;
    axon_risk::python::register_module(&risk_module)?;
    m.add_submodule(&risk_module)?;

    // Stage 4:`axon-oms` 子模块
    // 注:axon-oms 内部已注册 error/types/manager/portfolio 四个 Python
    // 子模块,这里只调 `register_module` 把 `oms` 挂到 `_native` 下。
    // 设计约束同 backtest/risk:OmsError 不继承 AxonError,axon-oms 不
    // 依赖 axon-python(避免 cargo 循环)。
    let oms_module = PyModule::new(m.py(), "oms")?;
    axon_oms::python::register_module(&oms_module)?;
    m.add_submodule(&oms_module)?;

    // Stage 5:`axon-exchange` 子模块
    // 注:axon-exchange 内部已注册 error/config/binance/okx/lifecycle/
    // rate_limiter 六个 Python 子模块,这里只调 `register_module` 把
    // `exchange` 挂到 `_native` 下。设计约束同 backtest/risk/oms:
    // ExchangeError 继承 builtin PyException 而非 AxonError,
    // axon-exchange 不依赖 axon-python(避免 cargo 循环)。
    let exchange_module = PyModule::new(m.py(), "exchange")?;
    axon_exchange::python::register_module(&exchange_module)?;
    m.add_submodule(&exchange_module)?;

    // Stage 6:`axon-inference` 子模块
    // 注:axon-inference 内部已注册 error/config/engine/pipeline 四个
    // Python 子模块,这里只调 `register_module` 把 `inference` 挂到
    // `_native` 下。设计约束同 backtest/risk/oms/exchange:
    // InferenceError 继承 builtin PyException 而非 AxonError,
    // axon-inference 不依赖 axon-python(避免 cargo 循环)。
    let inference_module = PyModule::new(m.py(), "inference")?;
    axon_inference::python::register_module(&inference_module)?;
    m.add_submodule(&inference_module)?;

    // `axon-explain` 子模块
    // 注:axon-explain 内部已注册 error/types/shap/counterfactual/report
    // 五个 Python 子模块,这里只调 `register_module` 把 `explain` 挂到
    // `_native` 下。设计约束同 backtest/risk/oms/exchange/inference:
    // ExplainError 继承 builtin PyException 而非 AxonError,
    // axon-explain 不依赖 axon-python(避免 cargo 循环)。
    let explain_module = PyModule::new(m.py(), "explain")?;
    axon_explain::python::register_module(&explain_module)?;
    m.add_submodule(&explain_module)?;

    // `axon-ensemble` 子模块
    // 注:axon-ensemble 内部已注册 error/types/voting/manager/stacking
    // 五个 Python 子模块,这里只调 `register_module` 把 `ensemble` 挂到
    // `_native` 下。设计约束同 backtest/risk/oms/exchange/inference/explain:
    // EnsembleError 继承 builtin PyException 而非 AxonError,
    // axon-ensemble 不依赖 axon-python(避免 cargo 循环)。
    let ensemble_module = PyModule::new(m.py(), "ensemble")?;
    axon_ensemble::python::register_module(&ensemble_module)?;
    m.add_submodule(&ensemble_module)?;

    // `axon-compliance` 子模块
    // 注:axon-compliance 内部已注册 ComplianceModule,
    // 这里只调 `register_module` 把 `compliance` 挂到 `_native` 下。
    let compliance_module = PyModule::new(m.py(), "compliance")?;
    axon_compliance::python::register_module(&compliance_module)?;
    m.add_submodule(&compliance_module)?;

    // `axon-defi` 子模块
    // 注:axon-defi 内部已注册 error/types/chain/config 四个 Python 子模块,
    // 这里只调 `register_module` 把 `defi` 挂到 `_native` 下。
    // 设计约束同 backtest/risk/oms/exchange/inference/explain/ensemble:
    // DefiError 继承 builtin PyException 而非 AxonError,
    // axon-defi 不依赖 axon-python(避免 cargo 循环)。
    let defi_module = PyModule::new(m.py(), "defi")?;
    axon_defi::python::error::register(&defi_module)?;
    axon_defi::python::chain::register(&defi_module)?;
    axon_defi::python::types::register(&defi_module)?;
    axon_defi::python::config::register(&defi_module)?;
    m.add_submodule(&defi_module)?;

    Ok(())
}
