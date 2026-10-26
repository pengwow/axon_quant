//! tracing → Python logging 桥接（仅 WARN 及以上）
//!
//! live 链路的排障线索（live_sink / exchange_gateway 等十余处 warn!）只
//! 存在于 tracing 系统，Python 侧完全看不到，生产排障没有抓手。零新依赖：
//! 自定义 tracing Layer 把 WARN+ 事件转发给 `logging.getLogger("axon_quant.rust")`。
//!
//! 只桥 WARN+ 而非全量：回测热路径每 bar 有 debug!（1m K 线一年百万级
//! 事件），全桥接会让 Python logging 拖慢热路径一个量级；WARN+ 量小且
//! 是排障唯一线索。Python 侧可用 `logging.getLogger("axon_quant.rust")
//! .setLevel(logging.ERROR)` 自行再过滤（`Logger.log` 内部自检
//! `isEnabledFor`，Rust 侧无需感知 Python 侧级别）。

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use pyo3::prelude::*;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Context, Layer as LayerTrait, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;

/// 重入保护：桥转发期间若 Python logging handler 又触发 Rust warn!（如
/// handler 内调回 Rust），on_event 会再次 attach 形成无限递归。转发期间
/// 的新事件直接丢弃——反馈循环场景里的日志本就没有观察意义。
static IN_BRIDGE: AtomicBool = AtomicBool::new(false);

/// RAII 兜底释放重入标记（Python 侧 panic / 转发失败也不能卡死后续转发）
struct ResetGuard;

impl Drop for ResetGuard {
    fn drop(&mut self) {
        IN_BRIDGE.store(false, Ordering::SeqCst);
    }
}

/// 提取事件消息与结构化字段。tracing 的 typed 字段（i64/f64/str...）默认
/// 都委托到 `record_debug`，只重写这一个方法即可全量捕获。
struct MessageVisitor {
    message: Option<String>,
    fields: Vec<(String, String)>,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        } else {
            self.fields
                .push((field.name().to_string(), format!("{value:?}")));
        }
    }
}

/// WARN+ 转发 Layer：同步拿 GIL 调 Python `logger.log`。
struct PyLogBridge;

impl<S: Subscriber> LayerTrait<S> for PyLogBridge {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        // per-layer LevelFilter::WARN 已挡 INFO 及以下；这里 match 是自明双保险
        let meta = event.metadata();
        let py_level = match *meta.level() {
            Level::ERROR => logging::ERROR,
            Level::WARN => logging::WARNING,
            _ => return,
        };

        let mut visitor = MessageVisitor {
            message: None,
            fields: Vec::new(),
        };
        event.record(&mut visitor);
        let mut msg = visitor.message.unwrap_or_default();
        for (key, value) in &visitor.fields {
            msg.push_str(&format!(" {key}={value}"));
        }
        msg.push_str(&format!(
            " [rust_target={}, rust_file={}, rust_line={}]",
            meta.target(),
            meta.file().unwrap_or("<unknown>"),
            meta.line().unwrap_or(0),
        ));

        if IN_BRIDGE
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let _guard = ResetGuard;

        // 同线程重入安全（GIL 线程内可重入）；跨线程窗口见模块顶部说明
        let _ = Python::attach(|py| -> PyResult<()> {
            let logging_mod = py.import("logging")?;
            let logger = logging_mod.call_method1("getLogger", ("axon_quant.rust",))?;
            logger.call_method1("log", (py_level, msg))?;
            Ok(())
        });
        // 转发失败静默：桥的职责是尽力投递，不能反过来把 Rust 调用方炸掉；
        // 且 Python logging 自身不可用时（极少见），也没有更好的上报通道。
    }
}

/// 安装 Rust → Python logging 桥（幂等）。
///
/// 全局 tracing subscriber 只能装一次：首次安装成功返回 True，已存在
/// （重复调用）返回 False。装上后 Rust 侧 WARN+ 会以对应级别（WARNING/
/// ERROR）投递给 `logging.getLogger("axon_quant.rust")`，消息尾部附带
/// `rust_target / rust_file / rust_line` 定位信息。
#[pyfunction]
pub fn install_rust_logging() -> bool {
    tracing_subscriber::registry()
        .with(PyLogBridge.with_filter(LevelFilter::WARN))
        .try_init()
        .is_ok()
}

mod logging {
    // Python logging 模块的级别常量数值，与官方 slog 标准保持一致：
    // WARNING=30、ERROR=40（DEBUG=10 / INFO=20 用不到，桥只转发 WARN+）
    pub const WARNING: i64 = 30;
    pub const ERROR: i64 = 40;
}
