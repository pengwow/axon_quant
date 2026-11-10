//! PyO3 桥接层

#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::useless_conversion)]
#![allow(deprecated)]

use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::registry::ModelRegistry as RustModelRegistry;
use crate::storage::{LocalStorage as RustLocalStorage, StorageBackendTrait};
use crate::types::{ModelMetadata as RustModelMetadata, ModelStage, ModelVersion};

/// LocalStorage Python 接口
#[pyclass(name = "LocalStorage")]
pub struct PyLocalStorage {
    inner: Arc<RustLocalStorage>,
}

#[pymethods]
impl PyLocalStorage {
    #[new]
    fn new(base_dir: &str) -> PyResult<Self> {
        let s = RustLocalStorage::new(std::path::PathBuf::from(base_dir))
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:?}")))?;
        Ok(Self { inner: Arc::new(s) })
    }

    fn base_dir(&self) -> String {
        self.inner.base_dir().to_string_lossy().to_string()
    }

    fn __repr__(&self) -> String {
        format!("LocalStorage(base_dir={:?})", self.inner.base_dir())
    }
}

/// ModelRegistry Python 接口（异步通过 block_on 暴露）
#[pyclass(name = "ModelRegistry")]
pub struct PyModelRegistry {
    inner: Arc<tokio::runtime::Runtime>,
    registry: Arc<RustModelRegistry>,
}

#[pymethods]
impl PyModelRegistry {
    #[new]
    fn new(storage: &PyLocalStorage, persist_dir: Option<&str>) -> PyResult<Self> {
        let storage_arc: Arc<dyn StorageBackendTrait> = storage.inner.clone();
        let registry = if let Some(dir) = persist_dir {
            Arc::new(RustModelRegistry::with_persist_dir(
                storage_arc,
                std::path::PathBuf::from(dir),
            ))
        } else {
            Arc::new(RustModelRegistry::new(storage_arc))
        };
        let runtime = Arc::new(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e}")))?,
        );
        Ok(Self {
            inner: runtime,
            registry,
        })
    }

    fn register<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        artifact_path: &str,
        description: &str,
    ) -> PyResult<Bound<'py, PyDict>> {
        let registry = self.registry.clone();
        let name_owned = name.to_string();
        let path_owned = std::path::PathBuf::from(artifact_path);
        let desc_owned = description.to_string();

        let mv = self
            .inner
            .block_on(async move {
                let metadata = RustModelMetadata {
                    description: desc_owned,
                    ..Default::default()
                };
                registry
                    .register(&name_owned, &path_owned, metadata, None)
                    .await
            })
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:?}")))?;

        model_version_to_dict(py, &mv)
    }

    fn promote_to_production<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        version: &str,
    ) -> PyResult<Bound<'py, PyDict>> {
        let registry = self.registry.clone();
        let name_owned = name.to_string();
        let version_owned = crate::types::SemVer::parse(version)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("{e:?}")))?;

        let mv = self
            .inner
            .block_on(async move {
                registry
                    .transition_stage(&name_owned, &version_owned, ModelStage::Production)
                    .await
            })
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:?}")))?;

        model_version_to_dict(py, &mv)
    }

    fn get_production<'py>(&self, py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyDict>> {
        let registry = self.registry.clone();
        let name_owned = name.to_string();

        let mv = self
            .inner
            .block_on(async move { registry.get_production(&name_owned).await })
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:?}")))?;

        model_version_to_dict(py, &mv)
    }

    /// 获取最新版本（不限 stage），未晋升 Production 的记录也可查到
    fn get_latest<'py>(&self, py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyDict>> {
        let registry = self.registry.clone();
        let name_owned = name.to_string();

        let mv = self
            .inner
            .block_on(async move { registry.get(&name_owned, None).await })
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:?}")))?;

        model_version_to_dict(py, &mv)
    }

    fn list_models(&self) -> Vec<String> {
        self.registry.list_models()
    }

    fn __repr__(&self) -> String {
        format!(
            "ModelRegistry(models={})",
            self.registry.list_models().len()
        )
    }
}

fn model_version_to_dict<'py>(py: Python<'py>, mv: &ModelVersion) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("name", &mv.name)?;
    dict.set_item("version", mv.version.to_string())?;
    dict.set_item("stage", mv.stage.to_string())?;
    dict.set_item("description", &mv.metadata.description)?;
    dict.set_item("artifact_size_bytes", mv.artifact_size_bytes)?;
    dict.set_item("artifact_hash", &mv.artifact_hash)?;
    dict.set_item("storage_uri", &mv.storage_uri)?;
    Ok(dict)
}

/// Python 模块入口
pub fn register_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyLocalStorage>()?;
    m.add_class::<PyModelRegistry>()?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
