# `axon-inference` Model Hot Reload (0.6.0 P0 Stage 6 closure)

> Version: `axon-inference` v0.6.0+
> Status: **Implemented** (Stage 6 closure)
> Plan ref: `docs/superpowers/plans/2026-07-18-axon-quant-0.6.0.md` Workflow C

`ModelHotReloader` provides model weight hot-reload without interrupting inference. Goals:

- **Zero-downtime**: during `reload()`, the old session keeps serving inference requests. Only `replace_session` is a brief exclusive window (< 1ms).
- **Atomicity**: if the new session fails to build, the old session is left untouched.
- **Observability**: every reload fires `watch::Sender` + an optional Python callback with version + sha256 checksum.

## Design: Two-step atomic path

```text
reload()
  │
  ├─ ① compute_sha256(path)        — verify the new file
  │
  ├─ ② backend.read()              — acquire READ lock
  │   .build_session(path)         — pre-build new session in backend context
  │   ─→ on failure: return Err, old session untouched
  │
  ├─ ③ backend.write()             — acquire WRITE lock
  │   .replace_session(new)        — atomic swap
  │   ─→ on failure: return Err, old session still active
  │
  └─ ④ version.fetch_add(1)        — bump version + notify subscribers
```

The `build_session` step holds only a read lock, so the old session remains usable for concurrent inference. `replace_session` is `&mut self` and executes instantly (no I/O).

## Trait abstraction

```rust
pub trait InferenceEngine: Send + Sync {
    fn load(&mut self, path: &Path) -> Result<(), InferenceError>;
    fn infer(&self, observation: &Observation) -> Result<Action, InferenceError>;
    fn infer_batch(&self, observations: &[Observation]) -> Result<Vec<Action>, InferenceError>;

    /// Pre-build a new session in the backend context (does NOT swap)
    fn build_session(&self, path: &Path)
        -> Result<Box<dyn Any + Send + Sync>, InferenceError>;

    /// Atomically replace the current backend session
    fn replace_session(&mut self, new_session: Box<dyn Any + Send + Sync>)
        -> Result<(), InferenceError>;
}
```

Per-backend `build_session` / `replace_session`:

| Backend | `build_session` returns | `replace_session` downcast | Note |
|---|---|---|---|
| `OnnxBackend` | `Box<ort::session::Session>` (committed) | `Box<ort::session::Session>` | truly pre-built, IO done upfront |
| `TchBackend` | `Box<tch::CModule>` (loaded) | `Box<tch::CModule>` | same as Onnx |
| `CandleBackend` | `Box<CandleReloadState { path }>` | `Box<CandleReloadState>` → `self.load(&state.path)` | Candle models tightly bound to `self.device` / `self.config`; wraps the path and rebuilds on swap |

## Python bindings

### Construction

```python
from axon_quant import (
    InferenceEngine, ModelConfig, Device, InferenceBackend, ModelHotReloader,
)

cfg = ModelConfig(
    path="/models/policy_v1.onnx",
    backend=InferenceBackend.Onnx,
    device=Device.cpu(),
    input_shape=(1, 64, 128),
    output_dim=3,
)
eng = InferenceEngine(cfg)
eng.load("/models/policy_v1.onnx")  # load first, then pass to reloader

reloader = ModelHotReloader(eng)  # after Stage 6 closure: real impl, not RuntimeError
assert reloader.version() == 0
assert reloader.model_path() == "/models/policy_v1.onnx"
```

### Manual reload

```python
# Suppose the training process produced /models/policy_v2.onnx
# Business code atomically replaces it (use os.replace for atomicity)
import os
os.replace("/models/policy_v2.onnx", "/models/policy_v1.onnx")

# Trigger hot reload
new_version = reloader.reload()
assert new_version == 1
```

### Subscribe to reload events

```python
events = []
def on_reload(path, version):
    events.append((path, version))
    print(f"model {path} reloaded to v{version}")

reloader.subscribe(on_reload)
# ... trigger reload ...
assert len(events) == 1
assert events[0] == ("/models/policy_v1.onnx", 1)

reloader.unsubscribe()
```

### Rust-side `watch::Receiver` subscription

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

## Caveats

- **`build_session` failure is non-destructive**: sha256 / ONNX load failures return `Err` immediately; the old session is untouched.
- **`replace_session` type mismatch returns a backend error**: passing a `Box<dyn Any>` of the wrong type surfaces a clear `Onnx(...)` / `Tch(...)` / `Candle(...)` error containing the expected type name.
- **`CandleBackend` has the longest blocking window**: `replace_session` internally calls `self.load(&state.path)` to fully rebuild; for large models this can take 1–3 seconds. Onnx/Tch pre-construct, so the swap is instantaneous.
- **`build_session` / `replace_session` are NOT exposed to Python**: only `ModelHotReloader.reload` is, to prevent direct state mutation.
- **Real ONNX model loading is the user's responsibility**: `test_inference_e2e.py` deliberately avoids real model files (CI doesn't fetch them). Rust unit tests + Python API-contract tests cover the integration surface.

## Acceptance

- Rust unit tests: `cargo test -p axon-inference --features "onnx candle-backend python" --lib` — 57/57 passing
- Python E2E (`pytest python/tests/test_inference_e2e.py -v`) includes:
  - `test_reloader_new_succeeds_with_engine` — verifies `__new__` succeeds + `version() == 0` + non-empty `model_path`
  - `test_reloader_subscribe_and_unsubscribe` — verifies the `subscribe` / `unsubscribe` / `has_callback` trio
  - `test_reloader_model_path_matches_engine` — verifies path consistency
- The old Stage 6 stub test `test_reloader_new_returns_runtime_error_in_stage6` was removed; the Rust counterpart `reloader_new_returns_runtime_error` is replaced with `reloader_new_succeeds_with_engine`.
