//! 集成测试:验证 PyMatchingEngine trait 桥接能跑通真实回测
//!
//! 目的:验证 Stage 3 `with_matching_engine` 真替换默认 `L1MatchingEngine`,
//! Python 端自定义撮合引擎(stub)真接入 `BacktestEngine.submit` 路径,
//! 并能跑出 `fills > 0`。
//!
//! 注:本测试**不**依赖 `axon_quant.backtest` 的具体类(`L1MatchingEngine` 等),
//! 只验证 trait object 桥接的最小契约:`submit(dict) -> dict` 含
//! `is_filled` + `fills` + `is_partially_filled` + `remaining_quantity` 字段。
//!
//! 运行:`PYO3_PYTHON=<python-with-stdlib> cargo test -p axon-backtest --features python --test python_matching_engine_trait`

#![cfg(feature = "python")]

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule};

use axon_backtest::python::engine::register as register_engine;

/// 把 `BacktestEngine` / `RunResult` / `RunStats` 注册到 `parent` 模块
fn register_native(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    register_engine(parent)
}

/// 构造一个最小撮合引擎 stub:任何订单都完全成交 0.001 @ 100
///
/// 注:PyMatchingEngine 内部用 `py_engine.call_method1("submit", (order_dict,))` 调用,
/// 等价于 Python `engine.submit(order_dict)` —— single dict 作为位置参数。
/// stub 用 `def submit(self, order_dict)` 接收 dict 即可。
fn make_stub_engine(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    py.eval(
        c"type('PyImpactedStub', (), {
            'submit': lambda self, d: {
                'fills': [{
                    'fill_id': 1,
                    'taker_order_id': d['id'],
                    'maker_order_id': d['id'] + 1000,
                    'price': 100.0,
                    'quantity': 0.001,
                    'taker_side': 'buy' if d['side'] == 'buy' else 'sell',
                    'timestamp': 0,
                }],
                'is_filled': True,
                'is_partially_filled': False,
                'remaining_quantity': 0.0,
            },
            'active_order_count': lambda self: 0,
            'clear_book': lambda self: None,
        })",
        None,
        None,
    )
}

/// 测试 1: PyMatchingEngine 接受任意含 `submit` 方法的 Python 类
#[test]
fn py_matching_engine_accepts_python_class() {
    Python::attach(|py| {
        let module = PyModule::new(py, "test_backtest").unwrap();
        register_native(&module).unwrap();

        let cls = make_stub_engine(py).unwrap();
        // 创建实例,with_matching_engine 接收的是 instance 而非 class
        let instance = cls.call0().unwrap();

        let be_cls = module.getattr("BacktestEngine").unwrap();
        let engine_py = be_cls.call1((100_000.0_f64,)).unwrap();

        // with_matching_engine 真替换
        engine_py.call_method1("with_matching_engine", (instance,)).unwrap();

        // 推一个 buy 单
        let order_dict = PyDict::new(py);
        order_dict.set_item("id", 1u64).unwrap();
        order_dict.set_item("symbol", "BTCUSDT").unwrap();
        order_dict.set_item("side", "buy").unwrap();
        order_dict.set_item("type", "limit").unwrap();
        order_dict.set_item("price", 100.0_f64).unwrap();
        order_dict.set_item("quantity", 0.001_f64).unwrap();
        order_dict.set_item("tif", "GTC").unwrap();

        let event = PyDict::new(py);
        event.set_item("type", "order_submitted").unwrap();
        event.set_item("timestamp_ns", 1_000i64).unwrap();
        event.set_item("order", &order_dict).unwrap();
        engine_py.call_method1("push_event", (event,)).unwrap();

        // run 后 fills 应 > 0(因为 stub 返回 1 个 fill)
        let result = engine_py.call_method0("run").unwrap();
        let fills = result.getattr("fills").unwrap().extract::<u64>().unwrap();
        assert!(
            fills > 0,
            "PyMatchingEngine 真注入后应该成交,实际 fills={fills}"
        );
    });
}

/// 测试 2: Python 端抛异常时降级为 0 成交,不 panic
#[test]
fn py_matching_engine_python_exception_does_not_crash() {
    Python::attach(|py| {
        let module = PyModule::new(py, "test_backtest_2").unwrap();
        register_native(&module).unwrap();

        // 抛异常的 stub
        let cls = py
            .eval(
                c"type('Boom', (), {
                    'submit': lambda self, args: exec('raise ValueError(\"boom\")') or {},
                    'active_order_count': lambda self: 0,
                    'clear_book': lambda self: None,
                })",
                None,
                None,
            )
            .unwrap();

        let be_cls = module.getattr("BacktestEngine").unwrap();
        let engine_py = be_cls.call1((100_000.0_f64,)).unwrap();
        let instance = cls.call0().unwrap();
        engine_py.call_method1("with_matching_engine", (instance,)).unwrap();

        let order_dict = PyDict::new(py);
        order_dict.set_item("id", 1u64).unwrap();
        order_dict.set_item("symbol", "BTCUSDT").unwrap();
        order_dict.set_item("side", "buy").unwrap();
        order_dict.set_item("type", "limit").unwrap();
        order_dict.set_item("price", 100.0_f64).unwrap();
        order_dict.set_item("quantity", 0.001_f64).unwrap();
        order_dict.set_item("tif", "GTC").unwrap();

        let event = PyDict::new(py);
        event.set_item("type", "order_submitted").unwrap();
        event.set_item("timestamp_ns", 1_000i64).unwrap();
        event.set_item("order", &order_dict).unwrap();
        engine_py.call_method1("push_event", (event,)).unwrap();

        // 异常被降级为 0 成交,run 仍然返回(不 panic)
        let result = engine_py.call_method0("run").unwrap();
        let fills = result.getattr("fills").unwrap().extract::<u64>().unwrap();
        assert_eq!(fills, 0, "Python 异常应被降级为 0 成交");
    });
}

/// 测试 3: 自定义撮合引擎替换后,BacktestEngine 仍能跑出 1 笔成交 + 1 个 accepted order
#[test]
fn py_matching_engine_replace_default_l1() {
    Python::attach(|py| {
        let module = PyModule::new(py, "test_backtest_3").unwrap();
        register_native(&module).unwrap();

        // 简化 stub:任何订单立即成交 1 笔
        let cls = py
            .eval(
                c"type('L1Stub', (), {
                    'submit': lambda self, d: {
                        'fills': [{
                            'fill_id': 1,
                            'taker_order_id': d['id'],
                            'maker_order_id': d['id'] + 1000,
                            'price': 100.0,
                            'quantity': 0.001,
                            'taker_side': d['side'],
                            'timestamp': 0,
                        }],
                        'is_filled': True,
                        'is_partially_filled': False,
                        'remaining_quantity': 0.0,
                    },
                    'active_order_count': lambda self: 0,
                    'clear_book': lambda self: None,
                })",
                None,
                None,
            )
            .unwrap();

        let be_cls = module.getattr("BacktestEngine").unwrap();
        let engine_py = be_cls.call1((100_000.0_f64,)).unwrap();
        let instance = cls.call0().unwrap();
        engine_py.call_method1("with_matching_engine", (instance,)).unwrap();

        let order_dict = PyDict::new(py);
        order_dict.set_item("id", 1u64).unwrap();
        order_dict.set_item("symbol", "BTCUSDT").unwrap();
        order_dict.set_item("side", "buy").unwrap();
        order_dict.set_item("type", "limit").unwrap();
        order_dict.set_item("price", 100.0_f64).unwrap();
        order_dict.set_item("quantity", 0.001_f64).unwrap();
        order_dict.set_item("tif", "GTC").unwrap();

        let event = PyDict::new(py);
        event.set_item("type", "order_submitted").unwrap();
        event.set_item("timestamp_ns", 1_000i64).unwrap();
        event.set_item("order", &order_dict).unwrap();
        engine_py.call_method1("push_event", (event,)).unwrap();

        let result = engine_py.call_method0("run").unwrap();
        let fills = result.getattr("fills").unwrap().extract::<u64>().unwrap();
        assert!(fills > 0, "替换后应成交 1 笔,实际 fills={fills}");
        let orders_accepted = result
            .getattr("orders_accepted")
            .unwrap()
            .extract::<u64>()
            .unwrap();
        assert_eq!(orders_accepted, 1, "应接受 1 个订单");
    });
}
