//! AXON Python 统一入口
//!
//! 将各个 crate 的 Python 绑定封装到 `axon_quant` 模块中。

use pyo3::prelude::*;

/// AXON Quant Python 模块（原生扩展，由 __init__.py 导入并重新导出）
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", "0.1.0a1")?;

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

    Ok(())
}
