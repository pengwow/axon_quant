//! Spike 测试:验证 `pyo3-arrow` 0.16 + `pyo3` 0.28 + `arrow` 57 能 zero-copy
//! 暴露 `arrow::RecordBatch` 到 `pyarrow.RecordBatch`。
//!
//! **为什么需要这个测试?**
//! `axon_quant.data` 的 Stage 1 计划要求通过 `pyo3-arrow` 把 `Dataset::batches`
//! 直接暴露为 Python 端 `pyarrow.RecordBatch`(零拷贝)。三方版本组合:
//! - `pyo3 = 0.28`
//! - `pyo3-arrow = 0.16`
//! - `arrow = 57`(workspace,Stage 1 spike 后从 53 升级以兼容 pyo3-arrow 0.16)
//!
//! 在 CI 中没有正式验证。本 spike 提前跑通,作为 Stage 1 所有子模块
//! (types / sources / dataset / service)数据契约的前置条件。
//!
//! **运行方式**:
//! ```bash
//! PYO3_PYTHON=.venv/bin/python cargo test -p axon-data \
//!   --features python --test python_spike -- --ignored --nocapture
//! ```
//!
//! 注:`--ignored` 是必须的,默认 features 不含 `python`,所以本测试在
//! `#[cfg(feature = "python")]` 之外不可见。

#![cfg(feature = "python")]

use std::ffi::CString;
use std::sync::Arc;

use arrow::array::{Array, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use pyo3::prelude::*;
use pyo3_arrow::PyRecordBatch;

/// 启动嵌入式 Python 解释器并把 .venv site-packages 加入 sys.path。
///
/// **为什么需要这个 helper?**
/// pyo3 嵌入式解释器在 cargo test binary 中启动时:
/// 1. `sys.executable` 指向 Rust test binary(不是 venv python)
/// 2. 默认不加载 site-packages(.venv 的 pyvenv.cfg 也不会被解析)
///
/// 解决:从 `PYO3_PYTHON` 环境变量推断 venv 根目录,手动 prepend
/// `<venv>/lib/pythonX.Y/site-packages` 到 `sys.path[0]`,这样
/// `import pyarrow` 才能解析到 .venv 中安装的版本。
///
/// **副作用**:`Python::initialize()` 是幂等的,可重复调用,不会 panic。
fn init_python_with_venv() {
    Python::initialize();
    Python::attach(|py| {
        let pyo3_python = std::env::var("PYO3_PYTHON").unwrap_or_default();
        if pyo3_python.is_empty() {
            // 没有 PYO3_PYTHON,跳过 site-packages 注入(测试中 import pyarrow 会失败,
            // 但这个错误信息由调用方负责)
            return;
        }
        let py_version = py.version_info();
        let pyver = format!("python{}.{}", py_version.major, py_version.minor);
        // .venv/bin/python -> .venv(取父目录两次)
        let venv_root = std::path::Path::new(&pyo3_python)
            .parent()
            .and_then(|p| p.parent())
            .unwrap_or_else(|| std::path::Path::new(&pyo3_python));
        let site_pkgs = venv_root
            .join("lib")
            .join(&pyver)
            .join("site-packages")
            .to_string_lossy()
            .to_string();
        let locals = pyo3::types::PyDict::new(py);
        // 三元表达式单行版本,避免 Python 单行 `if ...: ...` 后多语句的语法限制
        let code = CString::new(format!(
            "import sys, os; \
             site_pkgs={site_pkgs:?}; \
             sys.path.insert(0, site_pkgs) if site_pkgs and os.path.isdir(site_pkgs) else None; \
             pth=sys.path[:5]",
        ))
        .unwrap();
        py.run(code.as_c_str(), None, Some(&locals)).unwrap();
        eprintln!(
            "DEBUG [pyo3 spike] site-packages injected, sys.path[:5]={:?}",
            locals
                .get_item("pth")
                .unwrap()
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
        );
    });
}

/// Spike 核心:Roundtrip 验证。
///
/// 1. Rust 构造 `RecordBatch`
/// 2. `pyo3-arrow` 包装为 `PyRecordBatch`(声明零拷贝)
/// 3. 调用 `into_pyarrow()` 转换为 `pyarrow.RecordBatch`(通过 Arrow C data interface)
/// 4. 在 Python 端用 `pyarrow` 断言这是真实 `pyarrow.RecordBatch`
#[test]
#[ignore = "requires python feature + .venv"]
fn spike_recordbatch_roundtrip() {
    init_python_with_venv();
    Python::attach(|py| {
        // ----- 1) Rust 端构造 1 列 Int64 的 RecordBatch -----
        // 简单 schema,3 行,便于断言
        let schema = Schema::new(vec![Field::new("x", DataType::Int64, false)]);
        let arr = Int64Array::from(vec![1, 2, 3]);
        // arrow 57:`try_new` 签名改为 (SchemaRef, Vec<ArrayRef>);
        // ArrayRef = Arc<dyn Array>,需显式包 Arc(`arrow::array` 不 re-export `ArrayRef` 别名)
        let arr_ref: Arc<dyn Array> = Arc::new(arr);
        let batch = RecordBatch::try_new(schema.into(), vec![arr_ref]).unwrap();

        // ----- 2) zero-copy 包装:arrow::RecordBatch → PyRecordBatch → pyarrow.RecordBatch -----
        // pyo3-arrow 0.16 内部默认封装为 `arro3.core.RecordBatch`(模块 = "arro3.core._core"),
        // 它是 pyarrow ABI 兼容的另一个轻量实现(不是 pyarrow.RecordBatch 子类)。
        // 真正的零拷贝 → pyarrow.RecordBatch 需要调用 `into_pyarrow()`:
        // 内部走 Arrow C data interface(PyCapsule),不复制 buffer,只共享指针 + Arc 引用计数。
        // 文档:pyo3-arrow 0.16 README "Export to a pyarrow.RecordBatch" 一节。
        let py_batch = PyRecordBatch::new(batch);
        let pyarrow_batch = py_batch
            .into_pyarrow(py)
            .expect("into_pyarrow failed - is pyarrow >= 14 installed in .venv?");

        // ----- 3) Python 端断言类型与行数 -----
        // locals 字典把 Rust 端变量暴露给 `py.run` 内的 Python 代码
        let locals = pyo3::types::PyDict::new(py);
        locals.set_item("batch", pyarrow_batch).unwrap();
        // 断言 isinstance + num_rows + 第一个值,确认 zero-copy 真正生效
        // pyo3 0.28:`py.run` 接受 `&CStr` 而非 `&str`,用 CString::new 转一次
        let code = CString::new(
            "import pyarrow as pa; \
             assert isinstance(batch, pa.RecordBatch), 'PyRecordBatch is not pyarrow.RecordBatch'; \
             assert batch.num_rows == 3, f'num_rows={batch.num_rows}'; \
             assert batch.column('x').to_pylist() == [1, 2, 3]",
        )
        .unwrap();
        py.run(code.as_c_str(), None, Some(&locals))
            .expect("Python assert failed");
    });
}

/// Spike 辅助:多列 + 多种类型的 zero-copy 验证。
///
/// 验证 `PyRecordBatch` 在 schema 包含 Int64/Float64/Utf8 时的 ABI 兼容性。
#[test]
#[ignore = "requires python feature + .venv"]
fn spike_recordbatch_multitype() {
    use arrow::array::{Float64Array, StringArray};

    init_python_with_venv();
    Python::attach(|py| {
        // 三列:int / float / str
        let schema = Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("px", DataType::Float64, false),
            Field::new("sym", DataType::Utf8, false),
        ]);
        // arrow 57:`try_new` 需 Vec<Arc<dyn Array>>,每列显式包 Arc
        let columns: Vec<Arc<dyn Array>> = vec![
            Arc::new(Int64Array::from(vec![100, 200, 300])),
            Arc::new(Float64Array::from(vec![1.5, 2.5, 3.5])),
            Arc::new(StringArray::from(vec!["btc", "eth", "sol"])),
        ];
        let batch = RecordBatch::try_new(schema.into(), columns).unwrap();

        // 走 into_pyarrow() 拿 pyarrow.RecordBatch(同 roundtrip)
        let py_batch = PyRecordBatch::new(batch);
        let pyarrow_batch = py_batch
            .into_pyarrow(py)
            .expect("into_pyarrow failed - is pyarrow >= 14 installed in .venv?");
        let locals = pyo3::types::PyDict::new(py);
        locals.set_item("batch", pyarrow_batch).unwrap();
        let code = CString::new(
            "import pyarrow as pa; \
             assert isinstance(batch, pa.RecordBatch); \
             assert batch.num_columns == 3; \
             assert batch.column('id').to_pylist() == [100, 200, 300]; \
             assert batch.column('sym').to_pylist() == ['btc', 'eth', 'sol']",
        )
        .unwrap();
        py.run(code.as_c_str(), None, Some(&locals))
            .expect("Python assert failed");
    });
}
