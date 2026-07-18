# `axon-inference` 模型热更新(0.6.0 P0 Stage 6 收口)

> 适用版本:`axon-inference` v0.6.0+
> 状态:**已实现**(Stage 6 收口)
> 上游 plan:`docs/superpowers/plans/2026-07-18-axon-quant-0.6.0.md` 工作流 C

`ModelHotReloader` 提供不中断推理服务的模型权重热更新能力,核心目标:

- **零停机**:reload 期间旧 session 仍可服务推理请求,只在 `replace_session` 瞬间阻塞(< 1ms)
- **原子性**:新 session 构建失败时,旧 session 保持不变
- **可观测**:每次 reload 触发 `watch::Sender` + Python 端可选回调,记录版本号 + sha256 校验和

## 设计核心:两步原子路径

```text
reload()
  │
  ├─ ① compute_sha256(path)        — 算新文件校验和(预校验)
  │
  ├─ ② backend.read()              — 拿**只读**锁
  │   .build_session(path)         — 在 backend 上下文中预构造新 session
  │   ─→ 失败:立刻返回 Err,旧 session 不动
  │
  ├─ ③ backend.write()             — 拿**写**锁
  │   .replace_session(new)        — 原子替换
  │   ─→ 失败:返回 Err,旧 session 仍生效
  │
  └─ ④ version.fetch_add(1)        — 版本号 +1 + 通知所有订阅者
```

**关键约束**:`build_session` 阶段持有只读锁,旧 session 仍可并发推理;`replace_session` 是 `&mut self` 瞬间操作,只阻塞当前写者(无 IO)。

## Trait 抽象

```rust
pub trait InferenceEngine: Send + Sync {
    fn load(&mut self, path: &Path) -> Result<(), InferenceError>;
    fn infer(&self, observation: &Observation) -> Result<Action, InferenceError>;
    fn infer_batch(&self, observations: &[Observation]) -> Result<Vec<Action>, InferenceError>;

    /// 在 backend 上下文中预构造新 session(不替换当前 session)
    fn build_session(&self, path: &Path)
        -> Result<Box<dyn Any + Send + Sync>, InferenceError>;

    /// 原子替换当前 backend 的 session
    fn replace_session(&mut self, new_session: Box<dyn Any + Send + Sync>)
        -> Result<(), InferenceError>;
}
```

各 backend `build_session` / `replace_session` 实现:

| Backend | build_session 返回 | replace_session downcast | 备注 |
|---|---|---|---|
| `OnnxBackend` | `Box<ort::session::Session>`(已 commit) | `Box<ort::session::Session>` | 真预构造,可独立完成 IO |
| `TchBackend` | `Box<tch::CModule>`(已 load) | `Box<tch::CModule>` | 同上 |
| `CandleBackend` | `Box<CandleReloadState { path }>` | `Box<CandleReloadState>` → `self.load(&state.path)` | Candle 模型与 `self.device` / `self.config` 紧绑定,无法独立构造;走"包 path,replace 时再 load" 模式 |

## Python 绑定

### 构造

```python
from axon_quant import InferenceEngine, ModelConfig, Device, InferenceBackend, ModelHotReloader

cfg = ModelConfig(
    path="/models/policy_v1.onnx",
    backend=InferenceBackend.Onnx,
    device=Device.cpu(),
    input_shape=(1, 64, 128),
    output_dim=3,
)
eng = InferenceEngine(cfg)
eng.load("/models/policy_v1.onnx")  # 先 load,再传 reloader

reloader = ModelHotReloader(eng)  # Stage 6 收口后**真实现**,不再 RuntimeError
assert reloader.version() == 0
assert reloader.model_path() == "/models/policy_v1.onnx"
```

### 手动 reload

```python
# 假设训练进程产出了新权重 /models/policy_v2.onnx
# 业务侧复制覆盖 /models/policy_v1.onnx(用 `os.replace` 原子替换)
import os
os.replace("/models/policy_v2.onnx", "/models/policy_v1.onnx")

# 触发热更新
new_version = reloader.reload()
assert new_version == 1
```

### 订阅回调

```python
events = []
def on_reload(path, version):
    events.append((path, version))
    print(f"model {path} reloaded to v{version}")

reloader.subscribe(on_reload)
# ... 触发 reload ...
assert len(events) == 1
assert events[0] == ("/models/policy_v1.onnx", 1)

reloader.unsubscribe()  # 取消订阅
```

### `watch::Receiver` Rust 端订阅

```rust
use axon_inference::hot_reload::ModelHotReloader;
let mut rx = reloader.subscribe();  // watch::Receiver<u64>
tokio::spawn(async move {
    while rx.changed().await.is_ok() {
        let v = *rx.borrow();
        tracing::info!(version = v, "model hot reloaded");
    }
});
```

## 注意事项

- **`build_session` 失败不污染状态**:算 sha256 / 加载 ONNX 失败 → 立刻 `Err`,旧 session 不动。
- **`replace_session` 类型不匹配返回 `Backend` 错误**:传错 `Box<dyn Any>` 类型 → 后端返回明确的 `Onnx(...)` / `Tch(...)` / `Candle(...)` 错误信息(包含期望的类型名),便于排查。
- **`CandleBackend` 阻塞时间最长**:因为 `replace_session` 内部走 `self.load(&state.path)` 完整重建,大模型可能 1-3 秒;Onnx/Tch 因预构造完成,替换瞬间。
- **Python 端不暴露 `build_session` / `replace_session`**:仅 `ModelHotReloader.reload` 触发整条流程,避免误用导致状态不一致。
- **真实 ONNX 模型加载需自行准备**:`test_inference_e2e.py` 默认不测真模型(避免 CI 拉模型),仅验证 Rust 单元测试覆盖 + Python API 协议(版本号、回调、路径一致)。

## 验收

- Rust 单元测试:`cargo test -p axon-inference --features "onnx candle-backend python" --lib` 57/57 通过
- Python E2E:`pytest python/tests/test_inference_e2e.py -v` 包含:
  - `test_reloader_new_succeeds_with_engine`:验证 `__new__` 成功 + `version() == 0` + `model_path` 非空
  - `test_reloader_subscribe_and_unsubscribe`:验证 `subscribe/unsubscribe/has_callback` 三件套
  - `test_reloader_model_path_matches_engine`:验证 path 一致
- 旧 Stage 6 stub 测试 `test_reloader_new_returns_runtime_error_in_stage6` 已删除(对应 Rust 侧 `reloader_new_returns_runtime_error` 改为 `reloader_new_succeeds_with_engine`)
