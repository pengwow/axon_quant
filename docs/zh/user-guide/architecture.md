# AXON 架构设计

> 适用版本：AXON v0.21.0+（统一交易内核上线）

## 系统总览

AXON 在 **v0.21.0** 引入 `axon-kernel` 统一交易内核，把回测 / paper / live 三条路径收敛到同一条 `Kernel` 主回路（`kernel_loop.rs` → `BarLifecycle::on_bar_closed` 九步编排）。模式差异只在**装配层**：数据泵（历史 `build_frames` 产 `GridFrame` vs 实时 `KernelPump`）与 `ExecutionSink`（`BacktestSink` 撮合 / `PaperSink` 模拟盘 / `LiveSink` 实盘下单）。主回路本身没有 mode 分支。

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│                           AXON 架构总览（v0.22.1）                            │
├──────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  ┌───────────────────────────── Python 驱动层 ───────────────────────────────┐│
│  │  LiveDriver / PaperDriver  │  ShadowDiff  │  LiveRecovery (journal 恢复) ││
│  └────────────┬───────────────┴──────┬───────┴───────────┬─────────────────┘│
│               │                       │                   │                  │
│  ┌────────────▼───────────────────────▼───────────────────▼─────────────────┐│
│  │                           axon-worker (策略编排 / Rust 原生运行器)         ││
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐ ││
│  │  │ PyKernel      │  │ BacktestRunner│  │ Reconcile    │  │ Exchange     │ ││
│  │  │ Python 宿主   │  │ legacy 回退  │  │ 三路对账     │  │ Gateway      │ ││
│  │  └──────┬───────┘  └──────┬───────┘  └──────────────┘  └──────┬───────┘ ││
│  └─────────┼─────────────────┼────────────────────────────────────┼─────────┘│
│            │                 │                                    │           │
│  ┌─────────▼─────────────────▼────────────────────────────────────▼─────────┐│
│  │                        axon-kernel（统一交易内核）                        ││
│  │  Kernel.run_history / Kernel.step / Kernel.run_realtime                  ││
│  │  BarLifecycle::on_bar_closed  九步 per-bar 编排                           ││
│  │  ┌─────────┐ ┌──────────┐ ┌────────────────┐ ┌───────────────────────┐   ││
│  │  │EventQueue│ │ Journal  │ │KernelCore(账务)│ │BarGridBarrier(多路屏障)│   ││
│  │  └─────────┘ └──────────┘ └────────────────┘ └───────────────────────┘   ││
│  │  ExecutionSink trait ──► BacktestSink / PaperSink / LiveSink             ││
│  │  数据泵：build_frames(历史) │ KernelPump(实时 Gap 补灌)                   ││
│  └──────────────────────────────────────────────────────────────────────────┘│
│               │                         │                         │           │
│  ┌────────────▼──────┐  ┌──────────────▼──────┐  ┌──────────────▼──────┐    │
│  │  axon-backtest    │  │    axon-exchange     │  │      axon-oms       │    │
│  │  BacktestSink     │  │  ExchangeGateway    │  │ LiveSink 订单状态机 │    │
│  │  撮合 + 账务      │  │  WS 行情 + user-data │  │ VenueUpdate 转换   │    │
│  │  逐仓保证金引擎   │  │  REST 下单           │  │                    │    │
│  └─────────┬─────────┘  └──────────────────────┘  └─────────────────────┘    │
│            │                                                                   │
│  ┌─────────▼────────────────────────────────────────────────────────────────┐ │
│  │                       axon-core（共享类型 / 账务基础 / 合约）              │ │
│  └──────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
│  ┌─────────────────────── 训练管线（与内核解耦） ──────────────────────────┐   │
│  │  axon-hpo  │ axon-rl  │ axon-walk-forward │ axon-tracker │ axon-registry│   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

**依赖方向（`Cargo.toml` 核实）**：

```text
axon-core ◄─────── axon-backtest ◄─────── axon-kernel ◄────────────┐
     ▲                    ▲                    ▲                    │
     │                    │                    │                    ▼
     └────────────────────┴───────────────── axon-worker ────── axon-exchange
                                             │
                                             ▼
                                          axon-oms
```

- `axon-kernel → axon-backtest`：`BacktestSink` 复用 `axon-backtest` 的撮合与账务逻辑（含 v0.21.0 逐仓保证金引擎）。
- `axon-worker → axon-kernel + axon-exchange + axon-backtest + axon-oms`：策略编排层持有 `LiveDriver / PaperDriver / PyKernel`，把外部行情接进内核、把内核的下单意图送到 `LiveSink`，同时保留 legacy `BacktestRunner` 作为回测回退路径。
- `axon-exchange → axon-core`：交易所对接独立成 crate，内核不感知 Binance / OKX 差异。

## 数据流

### 回测数据流

回测把历史 K 线/撮合行情压成 `GridFrame`（`build_frames`，按时间戳对齐多品种），直接喂进 `Kernel.run_history`，与实盘跑**完全相同**的 `BarLifecycle` 九步编排。

```text
历史数据 (CSV / Arrow / Parquet)
    │
    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  build_frames ──► GridFrame ──► Kernel.load_frames ──► Kernel.step 循环 │
│  (axon-kernel::pump)                                                    │
└────────────────────────────────────┬────────────────────────────────────┘
                                     │
                                     ▼
                 ┌─────────────────────────────────────┐
                 │  BarLifecycle::on_bar_closed        │
                 │  九步 per-bar 编排                    │
                 │                                     │
                 │  ① 时钟对齐 + 前向填充价格           │
                 │  ② 强平快照                           │
                 │  ③ seed 流动性 + 自动 rebalance       │
                 │     + funding 调度 + sample_bar_nav   │
                 │  ④ 逐 BarClosed 逐 Host 决策          │
                 │  ⑤ 组合资金约束 (budget 同比例缩减)    │
                 │  ⑥ sink.finish_bar 统一落地           │
                 │  ⑦ 资金费结算                         │
                 │  ⑧ 保证金检查与强平                    │
                 │  ⑨ sample_bar_nav (已在③内)           │
                 └──────────────┬──────────────────────┘
                                │
                                ▼
                  ┌────────────────────────────┐
                  │  BacktestSink               │
                  │  (axon-backtest 撮合引擎)   │
                  │  L1/L2/L3 撮合              │
                  │  延迟 / 费用 / 市场冲击     │
                  │  逐仓保证金引擎 (v0.21.0+)   │
                  └──────────────┬─────────────┘
                                 │
                                 ▼
                  ┌────────────────────────────┐
                  │  KernelResult (内核聚合)    │
                  │  ├ 账务 RunResult           │
                  │  │   (PnL / 成交 / NAV /    │
                  │  │    回撤 / 持仓)          │
                  │  ├ duration (墙钟耗时)      │
                  │  ├ bars_processed           │
                  │  └ orders_submitted         │
                  └────────────────────────────┘
                                 │
                                 ▼
                  ┌────────────────────────────┐
                  │  BacktestResult             │
                  │  (axon-worker 面向 Python)  │
                  │  legacy BacktestRunner     │
                  │  降级为回退路径              │
                  └────────────────────────────┘
```

### 实盘数据流（live / paper 共用同一主回路）

实盘的差异只在**数据泵 + sink 装配**：`ExchangeGateway` 把 WS 行情 + user-data stream 送进 `KernelPump`，`KernelPump` 自动处理 Gap（断开重连时的缺口补灌），再经 `Kernel.step` 轮询推进主回路；执行侧用 `LiveSink`（`RiskCheck` 风控内联 + 下单 + `VenueUpdate` 转换）或 `PaperSink`（模拟盘、不出网）。

```text
┌──────────────────────────────────── 实盘侧 (axon-worker) ────────────────────────────────────┐
│                                                                                               │
│  ExchangeGateway                                                                              │
│  ┌─────────────────┐   ┌──────────────────┐   ┌──────────────────┐                          │
│  │ 行情 WS (mark /  │   │ user-data stream  │   │ REST 下单接口     │                          │
│  │  kline / book)   │   │ (订单回报 / 成交) │   │                  │                          │
│  └────────┬────────┘   └────────┬─────────┘   └────────▲─────────┘                          │
│           │                      │                      │                                      │
│           ▼                      ▼                      │                                      │
│  ┌──────────────────────────────────────┐               │                                      │
│  │         KernelPump                   │               │                                      │
│  │  Gap 缺口补灌 ───► GridFrame         │               │                                      │
│  │  多品种行情对齐到统一网格时间戳      │               │                                      │
│  └──────────────────────┬───────────────┘               │                                      │
│                         │                               │                                      │
└─────────────────────────┼───────────────────────────────┼──────────────────────────────────────┘
                          ▼                               │
┌─────────────────────────────────────────────────────────┼──────────────────────────────────────┐
│                              axon-kernel 主回路 (同回测)                                         │
│                                                                                                 │
│  Kernel.step (轮询)                                                                            │
│       │                                                                                         │
│       ▼                                                                                         │
│  ┌─────────────────────────────────────┐                                                       │
│  │  BarLifecycle::on_bar_closed        │   九步 per-bar 编排（同回测）                          │
│  └──────────────┬──────────────────────┘                                                       │
│                 │                                                                               │
│                 ▼                                                                               │
│  ┌──────────────────────────────────────────────────────────────────┐                            │
│  │  ExecutionSink                                                 │                            │
│  │                                                                │                            │
│  │  LiveSink (实盘)                                               │                            │
│  │  ├─ RiskCheck 风控内联（订单意图级拒绝）                       │                            │
│  │  ├─ 下单意图 ──► axon-oms 订单状态机                           │                            │
│  │  ├─ VenueUpdate 转换：交易所回报 ──► Kernel 内部事件           │                            │
│  │  └─ 三路对账（行情 / 账户 / 订单簿）                            │                            │
│  │                                                                │                            │
│  │  PaperSink (模拟盘)                                            │                            │
│  │  └─ 同 LiveSink 链路，但不出网（内部撮合 + 滑点模拟）         │                            │
│  └──────────────────────────────┬─────────────────────────────────┘                            │
└─────────────────────────────────┼──────────────────────────────────────────────────────────────┘
                                  ▼
┌─────────────────────────────── 交易后 ──────────────────────────────────────────────────────┐
│                                                                                               │
│  ┌──────────────────────────────────────┐                                                   │
│  │  Journal (事件溯源)                   │                                                   │
│  │  Journal::create / append / resume   │                                                   │
│  │  Kernel.attach_journal(path)         │                                                   │
│  │  LiveDriver.start(journal_path)      │                                                   │
│  │  LiveRecovery 崩溃恢复：重演 journal │                                                   │
│  │  ──► Kernel 恢复到断点继续 step       │                                                   │
│  └──────────────────────────────────────┘                                                   │
│                                                                                               │
│  ┌──────────────────────────────────────┐                                                   │
│  │  三路对账 (Reconcile)                 │                                                   │
│  │  行情最新价 / 账户权益 / 订单簿快照 │                                                   │
│  └──────────────────────────────────────┘                                                   │
│                                                                                               │
└───────────────────────────────────────────────────────────────────────────────────────────────┘
```

**回测 vs 实盘 口径**：
- ✅ 共用：`Kernel`、`BarLifecycle::on_bar_closed` 九步编排、账务核心、Host 决策、事件路由、journal（回测也可落盘重演）
- ❌ 不共用：数据泵（`build_frames` 历史 vs `KernelPump` 实时）与 `ExecutionSink`（`BacktestSink` 撮合 vs `LiveSink` / `PaperSink`）
- ❌ 不共用（仅实盘侧）：`ExchangeGateway` WS 行情、user-data stream、Gap 补灌、三路对账、崩溃恢复、ExchangeGateway REST 下单

### 训练管线数据流

训练管线（`axon-hpo / axon-rl / axon-walk-forward / axon-tracker / axon-registry`）与内核化解耦，维持原有闭环。

```text
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│  axon-hpo   │────▶│  axon-rl    │────▶│  axon-      │
│  超参搜索   │     │  RL 环境    │     │  walk-      │
│             │     │             │     │  forward    │
└─────────────┘     └─────────────┘     └──────┬──────┘
                                               │
                                               ▼
                                        ┌─────────────┐
                                        │  axon-      │
                                        │  tracker    │
                                        │  实验追踪   │
                                        └──────┬──────┘
                                               │
                                               ▼
                                        ┌─────────────┐
                                        │  axon-      │
                                        │  registry   │
                                        │  模型注册   │
                                        └─────────────┘
```

## 模块依赖关系

```text
axon-core ◄──────────────────────────────────────────┐
    │                                                 │
    ├──► axon-backtest                                │
    │        │                                        │
    │        ▼                                        │
    ├──► axon-kernel                                  │
    │        │                                        │
    │        ▼                                        │
    ├──► axon-worker                                  │
    │        │                                        │
    │        ├──► axon-exchange ◄─────────────────────┤
    │        │         │                              │
    │        │         └──► axon-oms ◄───────────────┤
    │        │                                        │
    │        ▼                                        │
    ├──► axon-rl                                      │
    │        │                                        │
    │        ▼                                        │
    ├──► axon-hpo                                     │
    │        │                                        │
    │        ▼                                        │
    ├──► axon-walk-forward                            │
    │        │                                        │
    │        ▼                                        │
    ├──► axon-tracker                                 │
    │        │                                        │
    │        ▼                                        │
    ├──► axon-registry                                │
    │                                                 │
    ├──► axon-risk                                    │
    │                                                 │
    ├──► axon-inference                               │
    │                                                 │
    └──► axon-monitor ◄───────────────────────────────┘
```

**关键边说明**（`Cargo.toml` 核实）：
- `axon-kernel ──► axon-backtest`：`BacktestSink` 复用 `axon-backtest` 的撮合 + 账务。
- `axon-worker ──► axon-kernel`：策略编排层持有 `PyKernel`，驱动 `Kernel.run_history` / `Kernel.step`。
- `axon-worker ──► axon-exchange`：`ExchangeGateway` 与实盘 `KernelPump` 的数据源。
- `axon-worker ──► axon-oms`：`LiveSink` 的订单状态机与 `VenueUpdate` 转换。

## 事件系统

### 事件类型

```text
Event
├── MarketDataEvent
│   ├── Tick        (逐笔成交)
│   ├── Bar         (K 线)
│   └── OrderBook   (订单簿快照)
├── OrderEvent
│   ├── Submitted   (订单提交)
│   ├── Cancelled   (订单取消)
│   ├── Modified    (订单修改)
│   └── Rejected    (订单拒绝)
├── FillEvent
│   └── Trade       (成交记录)
└── SystemEvent
    ├── Heartbeat   (心跳)
    ├── SessionStart(会话开始)
    ├── SessionEnd  (会话结束)
    └── Error       (错误)
```

### 事件路由

`Kernel` 用 `EventQueue`（二叉堆）按时间戳驱动主回路；`BarLifecycle::on_bar_closed` 为每帧统一调度九步编排，不再为 live / backtest 维护两套路由。

```text
EventQueue (BinaryHeap)
    │
    ▼
Kernel
    ├── BarLifecycle::on_bar_closed
    │     └──► StrategyHost.on_event (逐 BarClosed)
    │           └──► ExecutionSink.submit(OrderIntent)
    │                 ├──► BacktestSink  (撮合 → FillEvent)
    │                 ├──► LiveSink      (RiskCheck → axon-oms → 交易所)
    │                 └──► PaperSink     (模拟撮合)
    ├── axon-risk 风控检查（LiveSink 内联，非独立循环）
    └── axon-monitor 指标 / 告警
```

## 并发模型

```text
┌─────────────────────────────────────────────────────────┐
│                Kernel 主回路 (单线程, tokio current-thread)│
│                                                         │
│  ┌─────────────┐     ┌─────────────┐     ┌───────────┐ │
│  │ EventQueue  │────▶│ BarLifecycle│────▶│ Execution │ │
│  │ (BinaryHeap) │     │ 九步编排    │     │ Sink      │ │
│  └─────────────┘     └─────────────┘     └───────────┘ │
└─────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────┐
│                    I/O 线程池 (tokio)                     │
│                                                         │
│  ┌─────────────┐     ┌─────────────┐     ┌───────────┐ │
│  │ Exchange     │     │ REST API    │     │ 文件监控  │ │
│  │ Gateway WS   │     │ (下单)      │     │ (notify)  │ │
│  └─────────────┘     └─────────────┘     └───────────┘ │
│                                                         │
│  KernelPump 跑在 I/O 池，GridFrame 推到 Kernel 同步队列  │
└─────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────┐
│                    计算线程池 (rayon)                      │
│                                                         │
│  ┌─────────────┐     ┌─────────────┐     ┌───────────┐ │
│  │ 批推理      │     │ VaR 计算    │     │ 数据处理  │ │
│  │ (并行)      │     │ (并行)      │     │ (并行)    │ │
│  └─────────────┘     └─────────────┘     └───────────┘ │
└─────────────────────────────────────────────────────────┘
```

## 性能关键路径

### 风控检查 (12ns)

`LiveSink` 内联执行，不再是独立循环的前置环节。

```text
check_order
    │
    ├─→ 熔断器检查 (AtomicBool, ~5ns)
    │       └─→ 不活跃时直接返回
    │
    ├─→ 订单大小检查 (~10ns)
    │       └─→ limit_price() × quantity
    │
    ├─→ 仓位限制检查 (~50ns)
    │       └─→ HashMap 查找
    │
    ├─→ 杠杆检查 (~20ns)
    │       └─→ nav / base_cash
    │
    └─→ 回撤检查 (~20ns)
            └─→ (peak - current) / peak
```

### 订单提交 (1.2µs)

```text
submit
    │
    ├─→ 幂等性检查 (HashMap 查找)
    │
    ├─→ UUID v7 生成
    │
    ├─→ 状态更新 (New → Submitted)
    │
    ├─→ 活跃订单插入 (HashMap)
    │
    └─→ 历史记录追加 (Vec)
```

## 数据存储

### Arrow/Parquet 列式存储

```text
┌─────────────────────────────────────────┐
│              Parquet 文件                │
├─────────────────────────────────────────┤
│  timestamp │ price │ quantity │ side    │
│  (i64)     │ (f64) │ (f64)    │ (u8)   │
├─────────────────────────────────────────┤
│  1000 行/批次                            │
│  压缩：Snappy / ZSTD                    │
│  读写延迟：< 15ms (1M ticks)             │
└─────────────────────────────────────────┘
```

### Journal（事件溯源）

```text
Journal 文件 (line-delimited JSON)
├── 首行 manifest: 内核指纹 (config / 格式版本 / run_id)
├── 其余每行 JournalEntry
│   ├── seq: u64              序号
│   ├── ev: Event             输入事件 (行情 / 交易所回报)
│   └── state: Option<Json>   当帧逐 bar state
├── Kernel.attach_journal(path)          截断创建 + 写 manifest
├── Kernel.enqueue_external(ev, state)   自动 append (重演期间禁止写)
├── Journal::resume(path, manifest)      崩溃恢复入口
└── LiveRecovery: 重演 journal → Kernel 恢复到断点
```

### 内存布局

```text
Tick (#[repr(C)], 32 bytes)
├── timestamp: i64    (8 bytes)
├── price: f64        (8 bytes)
├── quantity: f64     (8 bytes)
└── side: Side        (1 byte + padding)

Trade (#[repr(C)], 40 bytes)
├── timestamp: i64    (8 bytes)
├── price: f64        (8 bytes)
├── quantity: f64     (8 bytes)
├── buyer_id: u64     (8 bytes)
└── seller_id: u64    (8 bytes)
```
