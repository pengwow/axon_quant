# Python 绑定

> **完整可运行示例**: [`examples/17_python_bindings/python_bindings_demo.py`](https://github.com/pengwow/axon_quant/blob/main/examples/17_python_bindings/python_bindings_demo.py)
> 覆盖本文档全部 6 个模块（Backtest / Risk / OMS / Exchange / Inference / LLM Trading），一键执行。

> 适用版本:AXON v0.6.0+ Python 绑定(Stage K 交付)

AXON 通过 PyO3 把核心 Rust 类型暴露给 Python,提供 `axon_quant` 包。

## 安装

```bash
# 1. 准备 Python 3.14.6 虚拟环境(pyenv 管理)
pyenv install 3.14.6
pyenv virtualenv 3.14.6 axon_quant
pyenv local axon_quant
pyenv shell axon_quant

# 2. 编译并安装
make python-install

# 3. 验证
python -c "import axon_quant; print(axon_quant.__version__)"
```

## 核心模块

### `axon_quant.core`

- `Order` / `OrderSide` / `OrderType` / `TimeInForce`
- `MarketData` / `Tick` / `Bar`
- `Portfolio` / `Position` / `Cash`

### `axon_quant.backtest`(Stage 2 交付)

事件驱动回测引擎,含 L1/L2/L3 撮合引擎 + 市场冲击感知 + 事件回放主循环。

| 类 | 说明 |
|----|------|
| `L1MatchingEngine` | 价格-时间优先撮合(基础) |
| `L2MatchingEngine` | 进阶:`modify` / `from_entries` / `export_entries` / `volume_at_price` / `stats` / `location` |
| `MultiAssetMatchingEngine` | 多资产路由 + 暗池 + 批量拍卖 + 套利检测 |
| `ImpactedMatchingEngine` | 冲击感知撮合(支持 linear / power_law 模型 + Python 自定义模型) |
| `ImpactedMatchingEngineBuilder` | 链式构造冲击感知引擎 |
| `BacktestEngine` | 事件驱动回测主循环(`order_submitted` / `order_cancelled` / `order_modified` / `fill` 4 种事件) |
| `RunResult` / `RunStats` | 回测结果(events_processed / fills / PnL / drawdown / final_nav,以及 0.5.0 新增 `positions` / `leg_targets` / `marks` 三个 per-instrument dict) |
| `BacktestError` | 撮合异常(继承 `Exception`,**不**继承 `AxonError`,避免 cargo 循环) |
| `OrderBookEntry` | L2 订单簿条目(用于 `from_entries` 导入) |
| `DarkOrder` / `CrossPair` / `AuctionResult` / `ArbitrageOpportunity` | L3 暗池 / 跨资产 / 拍卖 / 套利数据结构 |
| `limit_order(id, instrument, side, price, quantity, tif="GTC")` | 工厂函数,返回限价单 dict(`0.5.0` 起 `instrument` dict 取代旧 `symbol` 字符串) |
| `market_order(id, instrument, side, quantity)` | 工厂函数,返回市价单 dict(tif 强制 IOC) |
| `spot_instrument(base, quote)` | 工厂函数,返回 spot instrument dict(`{"kind": "spot", "base": ..., "quote": ...}`) |
| `swap_instrument(base, quote, settle, contract_size)` | 工厂函数,返回 swap instrument dict(settle=`"usd_margin"`/`"coin_margin"`,contract_size 默认 1.0) |

#### 示例:基础撮合 + 冲击感知

```python
from axon_quant.backtest import (
    L1MatchingEngine, ImpactedMatchingEngineBuilder,
    BacktestEngine, limit_order, spot_instrument,
)

# 0.5.0 起:用 spot_instrument() 工厂构造 instrument dict 取代旧 "BTC-USDT" 字符串
btc_spot = spot_instrument("BTC", "USDT")

# 1) 基础撮合
engine = L1MatchingEngine()
engine.submit(limit_order(1, btc_spot, "Sell", 100.0, 1.0))
result = engine.submit(limit_order(2, btc_spot, "Buy", 100.0, 1.0))
print(result["is_filled"], len(result["fills"]))  # True, 1

# 2) 冲击感知(Builder 链式)
ie = (ImpactedMatchingEngineBuilder()
      .model_type("linear")
      .coefficient(0.1)
      .depth_levels(5)
      .build())
ie.submit(limit_order(3, btc_spot, "Buy", 100.0, 1.0))
print(ie.permanent_offset())  # 累计永久冲击偏移

# 3) 事件驱动回测
bt = BacktestEngine(initial_cash=100_000.0)
bt.push_event({
    "type": "order_submitted",
    "timestamp_ns": 1_000,
    "order": limit_order(1, btc_spot, "Sell", 100.0, 1.0),
})
bt.push_event({
    "type": "order_submitted",
    "timestamp_ns": 2_000,
    "order": limit_order(2, btc_spot, "Buy", 100.0, 1.0),
})
result = bt.run()
print(result.events_processed, result.fills, result.final_nav)
```

> 📖 **多 leg 回测(spot + perp delta-neutral 套利)**:
> 0.5.0 引入 `Instrument` 抽象 + 多 leg API,
> 0.6.0 收口全栈 Instrument 化 + 跨 leg 风险约束。
> 见 [multi-leg-backtest.md](multi-leg-backtest.md)。

#### 提交订单返回 dict 协议

所有 `submit` 调用统一返回:

```python
{
    "is_filled": bool,              # 是否全部成交
    "is_partially_filled": bool,    # 是否部分成交
    "remaining_quantity": float,    # 剩余未成交量
    "fills": [                      # 成交列表
        {
            "fill_id": int,
            "taker_order_id": int,
            "maker_order_id": int,
            "price": float,
            "quantity": float,
            "taker_side": "BUY" | "SELL",  # 全大写
        },
        ...
    ],
}
```

#### BacktestEngine 事件类型

| `type` 字段 | 必填字段 | 含义 |
|-------------|----------|------|
| `order_submitted` | `order: dict` | 提交订单 |
| `order_cancelled` | `order_id: int` | 撤销订单 |
| `order_modified` | `order_id: int` + `new_price` / `new_quantity` | 修改订单 |
| `fill` | `price` / `quantity` / `buyer_order_id` / `seller_order_id` | 外部成交(旁路撮合) |

#### `BacktestError` 异常体系

`BacktestError` **直接**继承 builtin `Exception`(`PyException` 子类),**不**继承 Stage 1 `AxonError` 基类。设计原因:`axon-backtest` 反向依赖 `axon-python::AxonError` 会造成 cargo 循环(`axon-python` 依赖 `axon-backtest`),所以 Rust 侧不硬依赖。Python 端 thin wrapper 走 `__bases__` 伪继承兜底:

```python
try:
    axon_quant.backtest.L1MatchingEngine().submit(bad_order)
except axon_quant.backtest.BacktestError as e:  # 实际是 Exception 子类
    code = e.args[0]    # e.g. "Matching"
    msg = e.args[1]     # e.g. "[Matching] invalid side: xxx"
```

| 错误码 | 含义 |
|--------|------|
| `Matching` | L1/L2 撮合错误(订单未找到 / 非法价格 / 非法数量) |
| `MatchingL3` | L3 多资产撮合错误(资产未注册 / 跨资产参数非法) |

### `axon_quant.risk`(Stage 3 交付)

预交易风控引擎,含 8 项风控阈值 + 独立熔断器 + 风险指标聚合 + 组合监控告警。

| 类 | 说明 |
|----|------|
| `DefaultRiskEngine` | 风控主类:`check_order` / `check_portfolio` / `update_daily_pnl` / `reset_daily` / `metrics` |
| `RiskConfig` | 8 项风控阈值配置(单标持仓 / 总敞口 / 单笔价值 / 杠杆 / 回撤 / 日内亏损 / 集中度 / 熔断冷却) |
| `CircuitBreaker` | 独立熔断器:`check_and_trigger` / `reset` / `is_active`(不依赖 `DefaultRiskEngine`) |
| `RiskMetrics` | 风险指标聚合(NAV / 杠杆 / 回撤 / 日内 PnL / VaR(95) / 集中度) |
| `RiskResult` | 检查结果(Allow / Reject(reason) / Warn(msg)),用 `kind` 标签模式(非 PyO3 enum) |
| `RiskReason` | 拒绝原因(8 个变体扁平化):`OrderTooLarge` / `PositionLimitExceeded` / `MaxLeverageExceeded` / `MaxDrawdownExceeded` / `DailyPnLLimit` / `CircuitBreakerActive` / `ConcentrationTooHigh` / `InsufficientMargin` |
| `RiskError` | 风控异常(继承 `Exception`,**不**继承 `AxonError`,避免 cargo 循环) |
| `make_order(...)` | 工厂函数,返回订单 dict(限价/市价) |
| `make_portfolio(...)` | 工厂函数,返回最简 portfolio dict(只填 base_currency / commission_rate) |
| `make_portfolio_with_positions(...)` | 工厂函数,返回含 cash + positions 的 portfolio dict |
| `make_risk_config(...)` | 工厂函数,返回 `RiskConfig` 实例 |
| `make_circuit_breaker(...)` | 工厂函数,返回 `CircuitBreaker` 实例 |

#### 示例:预交易风控 + 熔断 + 风险指标

```python
from axon_quant.risk import (
    DefaultRiskEngine, RiskConfig, CircuitBreaker,
    RiskResult, RiskReason, RiskMetrics, RiskError,
    make_order, make_portfolio, make_portfolio_with_positions,
    make_risk_config, make_circuit_breaker,
)

# 1) 构造风控引擎
engine = DefaultRiskEngine(make_risk_config(
    max_order_value=10_000.0,     # 单笔订单最大价值
    max_leverage=2.0,              # 最大杠杆倍数
    max_daily_loss=5_000.0,        # 日内最大亏损(触发熔断)
    max_concentration=0.30,        # 单一标的占组合最大比例
))

# 2) 构造订单 + 组合
order = make_order(
    id=1, symbol="BTC-USDT", side="Buy",
    type="limit", price=100.0, quantity=1.0,
)
portfolio = make_portfolio(
    base_currency="USD",
    commission_rate=0.001,
    cash={"USD": 100_000.0},
)

# 3) 预交易检查
result = engine.check_order(order, portfolio)
if result.is_allow:
    print("Order allowed")
elif result.is_reject:
    reason = result.reason
    print(f"Rejected: {reason.kind}")  # e.g. "OrderTooLarge"
else:
    print(f"Warning: {result.message}")
```

#### 累计日内 PnL 触发熔断

```python
# 触发日内亏损超阈值 → engine.check_order() 拒绝
engine.update_daily_pnl(2_000.0)    # 累计盈利
engine.update_daily_pnl(-7_500.0)   # 累计亏损超 5_000 → 熔断
r = engine.check_order(order, portfolio)
assert r.is_reject and r.reason.kind == "CircuitBreakerActive"

# 重置日内状态(不重置 VaR 历史窗口)
engine.reset_daily()
```

#### RiskReason 字段访问

```python
reason = RiskReason.from_dict({
    "kind": "OrderTooLarge",
    "max": 10_000.0,
    "actual": 20_000.0,
})
assert reason.kind == "OrderTooLarge"
assert reason.get("max") == 10_000.0
assert reason.get("actual") == 20_000.0
d = reason.to_dict()  # {"kind": "OrderTooLarge", "max": 10000.0, "actual": 20000.0}
```

#### RiskMetrics 独立类

```python
# 引擎内部产出
m_dict = engine.metrics(portfolio)
# {"total_exposure": 100000.0, "leverage": 1.5, "current_drawdown": 0.05,
#  "daily_realized_pnl": 500.0, "var_95": 1500.0,
#  "concentration": {"BTC-USDT": 0.45, "ETH-USDT": 0.20}}

# 独立构造(测试 / 序列化的反序列化场景)
m = RiskMetrics.from_dict(m_dict)
assert m.total_exposure == 100_000.0
assert m.leverage == 1.5
assert m.concentration["BTC-USDT"] == 0.45
```

#### 独立 CircuitBreaker(不依赖 engine)

```python
cb = make_circuit_breaker(daily_loss_limit=10_000.0, cooldown_seconds=3600)
cb.check_and_trigger(-5_000.0)   # 不到阈值,未激活
assert cb.is_active is False
cb.check_and_trigger(-15_000.0)  # 触发
assert cb.is_active is True
cb.reset()                       # 强制重置
assert cb.is_active is False
```

#### Portfolio dict 协议

```python
# 最简(只填必填字段)
make_portfolio(base_currency="USD", commission_rate=0.001)

# 含 cash
make_portfolio(
    base_currency="USD",
    commission_rate=0.001,
    cash={"USD": 100_000.0, "BTC": 1.5},
)

# 含 cash + positions
make_portfolio_with_positions(
    base_currency="USD",
    cash={"USD": 50_000.0},
    positions={
        "BTC-USDT": {
            "quantity": 1.0,
            "avg_cost": 50_000.0,
            "market_price": 55_000.0,  # 可选
        },
    },
    commission_rate=0.0,
)
```

#### `RiskError` 异常体系

`RiskError` **直接**继承 builtin `Exception`(`PyException` 子类),**不**继承 Stage 1 `AxonError` 基类。设计原因:`axon-risk` 反向依赖 `axon-python::AxonError` 会造成 cargo 循环(`axon-python` 依赖 `axon-risk`),所以 Rust 侧不硬依赖。

```python
try:
    engine.check_order(bad_order, portfolio)
except axon_quant.risk.RiskError as e:  # 实际是 Exception 子类
    code = e.args[0]   # e.g. "OrderRejected" / "CircuitBreakerActive"
    msg = e.args[1]    # e.g. "[OrderRejected] Order too large: ..."
```

| 错误码 | 含义 |
|--------|------|
| `CircuitBreakerActive` | 熔断器激活,订单被拒 |
| `OrderRejected` | 订单被风控拒绝 |
| `ConfigInvalid` | 风控配置非法 |
| `Overflow` | 数值溢出 |

### `axon_quant.oms`(Stage 4 交付)

订单管理系统(OMS),含订单生命周期管理(提交/撤销/状态机)、fill 事件处理、组合多币种现金 + 多 symbol 持仓、幂等键防重。

| 类 / 工厂 | 说明 |
|-----------|------|
| `OrderManager` | OMS 主类:`submit` / `cancel` / `update_status` / `get_order_status` / `batch_submit` / `add_fill` / `snapshot` / `snapshot_balance` / `snapshot_positions` / `active_count` / `history_count` / `deposit` / `withdraw` |
| `Order` | 订单对象:`symbol` / `side` / `order_type` / `quantity` / `price` / `idempotency_key` |
| `OrderStatus` | 订单状态(`kind` 标签模式):`New` / `Acknowledged` / `PartiallyFilled(filled_qty, avg_price)` / `Filled` / `Cancelled` / `Rejected(reason)` / `Expired`,带 `is_terminal()` 判定 |
| `Side` | 枚举:`Buy` / `Sell` |
| `OrderType` | 枚举:`Limit` / `Market` |
| `Portfolio` | 多币种现金 + 持仓容器:`deposit` / `withdraw` / `apply_fill` / `cash` / `positions` / `position_count` / `is_empty` / `to_dict` |
| `Position` | 单 symbol 持仓:`symbol` / `quantity` / `avg_price` / `realized_pnl` / `updated_at` / `to_dict` |
| `OmsError` | OMS 异常(继承 `Exception`,**不**继承 `AxonError`,避免 cargo 循环) |
| `limit_order(symbol, side, quantity, price, idempotency_key=None)` | 工厂函数,返回限价单 `Order` |
| `market_order(symbol, side, quantity, idempotency_key=None)` | 工厂函数,返回市价单 `Order`(taker 价待撮合确认) |
| `make_order_status(kind, filled_qty=None, avg_price=None, reason=None)` | 工厂函数,从 dict 构造 `OrderStatus` |

#### 示例:订单全生命周期 + 组合更新

```python
from axon_quant.oms import (
    OrderManager, Order, OrderStatus, Side, OrderType, Portfolio, Position,
    OmsError, limit_order, market_order, make_order_status,
)

# 1) 构造 OMS + 初始资金
mgr = OrderManager()
mgr.deposit("USDT", 100_000)

# 2) 提交订单 → 返回 order_id(UUID 36 字符)
oid = mgr.submit(limit_order(
    "BTC-USDT", "Buy", quantity=1, price=50_000,
    idempotency_key="my-bot-001",
))
print(oid, mgr.active_count())   # 1

# 3) 状态机推进
mgr.update_status(oid, make_order_status("Acknowledged"))

# 4) 推 fill 事件(部分成交)→ portfolio 自动更新
mgr.add_fill(
    order_id=oid, fill_id="f1", symbol="BTC-USDT",
    price=50_000, quantity=0.6, fee=0,
)
s = mgr.get_order_status(oid)
assert s.kind == "PartiallyFilled"
assert s.filled_qty == "0.6"

# 5) 查组合
snap = mgr.snapshot_balance()
assert snap["cash"]["USDT"] == "70000.0"
pos = snap["positions"]["BTC-USDT"]
assert pos.quantity == "0.6"
assert pos.avg_price == "50000"

# 6) 推满成 fill → 终态
mgr.add_fill(
    order_id=oid, fill_id="f2", symbol="BTC-USDT",
    price=51_000, quantity=0.4, fee=0,
)
assert mgr.get_order_status(oid) is None  # Filled 已从 active 移除
```

#### 批量下单 + 幂等键

```python
# 幂等键防重(同 key 重复 submit 会报 DuplicateIdempotencyKey)
oid_a = mgr.submit(limit_order("BTC-USDT", "Buy", 0.1, 50_000, idempotency_key="batch-1"))
try:
    oid_a2 = mgr.submit(limit_order("BTC-USDT", "Buy", 0.1, 50_000, idempotency_key="batch-1"))
except OmsError as e:
    assert e.args[0] == "DuplicateIdempotencyKey"

# 批量下单(循环 submit,失败时抛首个错误)
oids = mgr.batch_submit([
    limit_order("ETH-USDT", "Buy", 1, 3_000, idempotency_key="batch-2"),
    limit_order("SOL-USDT", "Buy", 10, 100, idempotency_key="batch-3"),
])
assert len(oids) == 2
```

#### OrderStatus 字段访问

```python
status = make_order_status("PartiallyFilled", filled_qty=0.6, avg_price=50_000)
assert status.kind == "PartiallyFilled"
assert status.filled_qty == "0.6"
assert status.avg_price == "50000"
assert status.is_terminal() is False

# 终态判定
filled = make_order_status("Filled", filled_qty=1, avg_price=50_000)
assert filled.is_terminal() is True
cancelled = make_order_status("Cancelled", reason="user_cancelled")
assert cancelled.is_terminal() is True
```

#### Portfolio 独立类

```python
# 不依赖 OrderManager 的轻量 portfolio(测试 / 序列化反序列化场景)
p = Portfolio()
p.deposit("USDT", 100_000)
p.deposit("BTC", 1.5)
p.apply_fill(
    fill_id="f1", symbol="BTC-USDT",
    price=50_000, quantity=0.6, fee=0,
)
assert p.cash["USDT"] == "70000.0"
assert p.positions["BTC-USDT"].quantity == "0.6"
assert p.position_count() == 1

# 出金(余额不足时抛 ValueError)
p.withdraw("USDT", 10_000)
assert p.cash["USDT"] == "60000.0"

# 序列化
d = p.to_dict()  # {"cash": {...}, "positions": {...}, "position_count": 1}
```

#### Decimal 桥(精度无损)

所有金额字段(`quantity` / `price` / `filled_qty` / `avg_price` / `realized_pnl` / `cash` 字典值)在 Python 端以 **字符串** 返回,通过 `decimal.Decimal` 构造。设计原因:`rust_decimal::Decimal` 128 位精度不可直接转 `float`,字符串往返零误差。

```python
from decimal import Decimal

o = limit_order("BTC-USDT", "Buy", Decimal("0.1"), Decimal("50000.5"))
# quantity / price 在 Python 端也是 Decimal 字符串
```

#### `OmsError` 异常体系

`OmsError` **直接**继承 builtin `Exception`(`PyException` 子类),**不**继承 Stage 1 `AxonError` 基类。设计原因:`axon-oms` 反向依赖 `axon-python::AxonError` 会造成 cargo 循环(`axon-python` 依赖 `axon-oms`),所以 Rust 侧不硬依赖。

```python
try:
    mgr.cancel("not-a-uuid")
except axon_quant.oms.OmsError as e:  # 实际是 Exception 子类
    code = e.args[0]    # e.g. "OrderNotFound"
    msg = e.args[1]     # e.g. "[OrderNotFound] order not found: xxx"
```

| 错误码 | 含义 |
|--------|------|
| `OrderNotFound` | 订单 ID 不存在 |
| `InvalidTransition` | 状态机非法转移(如 Filled → PartiallyFilled) |
| `DuplicateIdempotencyKey` | 重复幂等键 |
| `AlreadyTerminal` | 操作终态订单(Filled / Cancelled / Rejected) |
| `ExchangeRejected` | 交易所拒绝 |
| `NetworkError` | 网络错误 |
| `SerializationError` | 序列化错误 |
| `RecoveryFailed` | 状态恢复失败 |
| `Portfolio` | 组合错误(fill 数量与现金不一致等) |

### `axon_quant.exchange` 子模块(Stage 5)—— 真实交易所适配器

真实交易所适配器(Binance / OKX),支持 WebSocket 订阅、限流、订单生命周期管理与熔断器。**默认开启 testnet**,生产模式需显式配置;API key 通过环境变量读取(`BINANCE_API_KEY` / `BINANCE_API_SECRET` / `OKX_API_KEY` / `OKX_API_SECRET` / `OKX_PASSPHRASE`)。

| 类 / 函数 | 说明 |
|-----------|------|
| `ExchangeId` | 枚举:`Binance` / `Okx` |
| `ExchangeConfig` | 完整交易所配置(`api_key` / `api_secret` / `passphrase` / `rest_base_url` / `ws_url` / `testnet` / `rate_limit` / `reconnect`) |
| `RateLimitConfig` | 令牌桶限流(RPS / orders per minute / WS messages per second) |
| `ReconnectConfig` | 自动重连 + 熔断器配置(max_retries / backoff / 阈值) |
| `BinanceAdapter` | Binance 适配器(REST + WebSocket,testnet / production) |
| `OkxAdapter` | OKX 适配器(REST + WebSocket,testnet / production,需 `passphrase`) |
| `OrderLifecycleManager` | 订单状态机跟踪(Pending → Acknowledged → Filled / Rejected / Cancelled) |
| `TokenBucketRateLimiter` | 令牌桶限流器(同步 `try_acquire` + 状态查询) |
| `ExchangeError` | 交易所异常(继承 `Exception`,**不**继承 `AxonError` 以避免 cargo 循环) |
| `binance_testnet_config()` | 工厂:从环境变量读 Binance testnet API key |
| `okx_testnet_config()` | 工厂:从环境变量读 OKX testnet API key |

#### 示例:testnet 连接(env 读 key)

```python
import os
from axon_quant.exchange import BinanceAdapter, binance_testnet_config

# API key 自动从环境变量读取(BINANCE_API_KEY / BINANCE_API_SECRET)
# 缺一即抛 ExchangeError("BINANCE_API_KEY / BINANCE_API_SECRET not set in environment")
os.environ["BINANCE_API_KEY"] = "..."
os.environ["BINANCE_API_SECRET"] = "..."

adapter = BinanceAdapter(binance_testnet_config())
adapter.connect()  # 同步包装:内部 block_on 异步 connect
adapter.subscribe(symbols=["BTCUSDT"], kind="ticker")

# 下单:接受 dict,返回 order_id (UUID 字符串)
oid = adapter.place_order({
    "symbol": "BTCUSDT",
    "side": "buy",
    "type": "market",
    "quantity": "0.001",
    "tif": "IOC",
})

# 撤单
adapter.cancel_order(oid)

# 查询
balances = adapter.get_balance()
positions = adapter.get_positions()
```

#### 订单 dict 协议

订单构造用 Python dict(无需直接构造 axon-oms 类型):

| Key | 类型 | 必填 | 说明 |
|-----|------|------|------|
| `symbol` | str | ✓ | 交易对(Binance:`"BTCUSDT"`;OKX:`"BTC-USDT"`) |
| `side` | str | ✓ | `"buy"` / `"sell"` |
| `type` | str | ✓ | `"market"` / `"limit"` / `"stop_loss"` / `"stop_limit"` |
| `quantity` | str / Decimal | ✓ | 数量(字符串无损传输) |
| `tif` | str | ✓ | `"GTC"` / `"IOC"` / `"FOK"` |
| `price` | str / Decimal | (限价单) | 限价 |
| `client_order_id` | str | optional | 客户端订单 ID(UUID 字符串,缺省自动生成) |
| `meta` | dict | optional | 透传给交易所的元数据(如 Binance `newClientOrderId`) |

#### `ExchangeError` 异常体系

`ExchangeError` **直接**继承 builtin `Exception`(`PyException` 子类),**不**继承 Stage 1 `AxonError` 基类。设计原因:同 `BacktestError` / `RiskError` / `OmsError`,`axon-exchange` 反向依赖 `axon-python::AxonError` 会造成 cargo 循环。错误码取变体名,跨 release 稳定。

```python
from axon_quant.exchange import OrderLifecycleManager, ExchangeError

mgr = OrderLifecycleManager()
try:
    mgr.update_status(
        "00000000-0000-0000-0000-000000000000",
        {"status": "filled", "filled_qty": "0.1", "avg_price": "50000"},
    )
except ExchangeError as e:
    code = e.args[0]   # 例如 "OrderNotFound"
    msg  = e.args[1]   # 例如 "[OrderNotFound] order not found: ..."
```

| 错误码 | 含义 |
|--------|------|
| `ConnectionFailed` | REST / WebSocket 连接失败 |
| `WebSocketDisconnected` | WebSocket 意外断开 |
| `AuthenticationFailed` | API key 签名验证失败 |
| `OrderRejected` | 交易所拒绝订单(min notional 等) |
| `InsufficientBalance` | 余额不足 |
| `RateLimited` | 触发 API 限流(返回 `wait_ms`) |
| `OrderNotFound` | 订单 ID 不存在 |
| `ParseError` | 响应解析失败 |
| `ApiError` | 通用 API 错误(带 `code` + `message` 字段) |
| `WebSocket` | WebSocket 错误信息 |
| `CircuitBreakerOpen` | 熔断器打开(连续失败超阈值) |
| `Network` | 网络失败 |
| `Serialization` | (反)序列化失败 |

#### 安全:API key 永不暴露

`api_secret` / `passphrase` **永远不会**写入 `__repr__` / 不会打印 / 不会记录日志。可用 `repr(adapter)` 或 `repr(config)` 自验:

```python
adapter = BinanceAdapter(binance_testnet_config())
print(repr(adapter))   # "BinanceAdapter(...)"  —— 不含 secret
print(repr(config))    # "ExchangeConfig(Binance, testnet=True, rest=...)"  —— 不含 secret
```

完整安全清单见 `docs/zh/reference/exchange-security.md`。

### `axon_quant.inference`(Stage 6 交付)—— ONNX / Candle 推理引擎

`axon-inference` 的 PyO3 绑定,跨后端推理引擎 + 批推理管线 + 模型热更新(支持 Onnx / Candle / Tch 后端)。Stage 6 暴露 `Onnx` / `Candle` 给 Python;`Tch` 故意不暴露(避免 PyTorch C++ 链接)。

| 类 / 函数 | 用途 |
| --- | --- |
| `ModelConfig` | 模型配置:`path` / `backend` / `device` / `input_shape`(3 元 tuple)/ `output_dim` / `fp16` / `num_threads` |
| `InferenceBackend` | 枚举:`Onnx` / `Tch` / `Candle` |
| `Device` | 设备:`Device.cpu()` / `Device.cuda(device_id)` / `Device.metal()` |
| `Observation` | 输入观测:`symbol` / `timestamp_ns` / `features`(list[float]) |
| `ActionType` | 输出枚举:`Buy` / `Sell` / `Hold` / `ReduceLong` / `ReduceShort` |
| `Action` | 输出:`action_type`(枚举)/ `confidence`(f32)/ `target_position`(f32)/ `model_id` / `inference_time_us` |
| `BatchConfig` | 批管线:`max_batch_size` / `collect_timeout_us` / `num_workers` / `prealloc_buffer_size` / `collect_cpu_cores` / `collect_gpu_device_id` |
| `InferenceStats` | 统计:`total_inferences` / `total_batch_inferences` / `avg_latency_us` / `p99_latency_us` / `hot_reloads` / `errors` |
| `InferenceEngine` | 统一入口;`engine.load(path)` / `engine.infer(obs)` / `engine.infer_batch([obs])` / `engine.to_dict()` |
| `BatchInferencePipeline` | 简化批管线:`submit(obs)` / `collect()` / `pending()` / `stats()` |
| `ModelHotReloader` | Stage 6 占位:`__new__` 返回 `RuntimeError`(等待 `engine._config()` 内部访问器) |
| `create_onnx_engine(model_path, ...)` | 一步 ONNX 工厂(默认后端,无需额外 feature) |
| `create_candle_engine(model_path, ...)` | 一步 Candle 工厂(需 `candle-backend` feature) |
| `create_inference_engine(config, path=None)` | 底层工厂(亦可走 `axon_quant.create_inference_engine`) |

#### 示例:ONNX 单条推理

```python
from axon_quant.inference import (
    InferenceEngine, ModelConfig, Device, Observation, InferenceBackend,
    create_onnx_engine,
)

# 一步:创建 + 加载
engine = create_onnx_engine(
    model_path="model.onnx",
    input_shape=(1, 64, 128),
    output_dim=3,
)

# 单条推理
obs = Observation(symbol="BTC-USDT", timestamp_ns=1_000_000_000, features=[0.0] * 128)
action = engine.infer(obs)
print(action.action_type, action.confidence, action.target_position)
```

#### 示例:ONNX 批量推理

```python
from axon_quant.inference import create_onnx_engine, Observation

engine = create_onnx_engine(model_path="model.onnx", input_shape=(1, 64, 128), output_dim=3)
obs_list = [Observation(symbol="BTC-USDT", timestamp_ns=i * 1_000, features=[0.0] * 128) for i in range(32)]
actions = engine.infer_batch(obs_list)
assert len(actions) == 32
```

#### 示例:BatchInferencePipeline(缓冲式批推理)

```python
from axon_quant.inference import (
    BatchInferencePipeline, BatchConfig, create_onnx_engine, Observation,
)

engine = create_onnx_engine(model_path="model.onnx", input_shape=(1, 64, 128), output_dim=3)
bcfg = BatchConfig(max_batch_size=32, collect_timeout_us=500, num_workers=2)
pipe = BatchInferencePipeline(bcfg, engine)

# 缓冲 observation,然后触发一次批推理
for i in range(32):
    pipe.submit(Observation(symbol="BTC-USDT", timestamp_ns=i * 1_000, features=[0.0] * 128))
print(pipe.pending())  # 32
actions = pipe.collect()
print(len(actions), pipe.stats().total_inferences)  # 32 32
```

#### 后端选择(`Onnx` / `Candle`)

```python
from axon_quant.inference import InferenceEngine, ModelConfig, Device, InferenceBackend

# 默认 Onnx(Stage 6 默认 feature,无需额外编译 flag)
engine_onnx = InferenceEngine(ModelConfig(
    path="model.onnx", backend=InferenceBackend.Onnx, device=Device.cpu(),
    input_shape=(1, 64, 128), output_dim=3,
))

# Candle(纯 Rust,无 ONNX runtime 依赖)—— 需编译时启用 `candle-backend` feature:
# `cargo build -p axon-inference --features python --features candle-backend`。
# 若未编译,`__new__` 返回 `InferenceError("Candle backend not compiled: ...")`。
try:
    engine_candle = InferenceEngine(ModelConfig(
        path="model.safetensors", backend=InferenceBackend.Candle, device=Device.cpu(),
        input_shape=(1, 64, 128), output_dim=3,
    ))
except Exception as e:  # candle-backend feature 未启用
    print(f"skip: {e}")
```

#### `InferenceError` 异常体系

`InferenceError` **直接**继承 builtin `Exception`(`PyException` 子类),**不**继承 Stage 1 `AxonError` 基类。设计原因:同 `BacktestError` / `RiskError` / `OmsError` / `ExchangeError` —— `axon-inference` 反向依赖 `axon-python::AxonError` 会造成 cargo 循环,所以 Rust 侧不硬依赖。错误码取 Rust `Debug` 输出的变体名(如 `ModelNotFound` / `ModelLoadFailed` / `Onnx(...)` / `Candle(...)`),跨 release 稳定。

```python
from axon_quant.inference import InferenceEngine, ModelConfig, InferenceError, InferenceBackend, Device

cfg = ModelConfig(
    path="/nonexistent.onnx", backend=InferenceBackend.Onnx, device=Device.cpu(),
    input_shape=(1, 64, 128), output_dim=3,
)

try:
    engine = InferenceEngine(cfg)
    engine.load("/nonexistent.onnx")
except InferenceError as e:
    # e.args[0] 是稳定错误码(如 "ModelNotFound")
    # e.args[1] 是人类可读形式:"[ModelNotFound] model file not found: /nonexistent.onnx"
    print(e.args[0], e.args[1])
```

**Stage 6 限制 / 注意事项**:

- `Tch` 后端**不**暴露给 Python(避免 PyTorch C++ 链接);`InferenceEngine(InferenceBackend.Tch)` 返回 `InferenceError("Tch backend is not exposed to Python in Stage 6 ...")`。
- `ModelHotReloader.__new__` 返回 `RuntimeError`,因 `PyInferenceEngine` 不暴露底层 `ModelConfig`(等待 Stage 7+ 添加内部访问器)。Stage 6 期间用 `engine.infer_batch([...])` 做批量推理。
- `BatchInferencePipeline` 是简化版 Python 包装(无 tokio `batch_loop` task)。它在 `Vec` 中缓冲 `Observation`,`collect()` 时调 `engine.infer_batch`(内部已走 `par_iter` rayon 并行)。
- `Extension-module` PyO3 feature **默认关闭**(会破坏 `cargo test` 静态链接)。需走 `make python-develop` 编译 cdylib,而**不是** `cargo build --features python`。

### `axon_quant.compliance`(Stage 7 交付)—— 合规审计引擎

`axon-compliance` 的 PyO3 绑定。提供交易记录、不可变审计日志、报告生成和监管报送功能。Stage 7 把原本只在 Rust 端可用的合规模块完整暴露到 Python,内部状态使用 `Mutex` 包装保证线程安全。

| 类 / 函数 | 用途 |
| --- | --- |
| `ComplianceConfig` | 合规配置: `account_id` / `base_currency` / `large_trade_threshold` / `position_limit` / `max_portfolio_concentration` / `data_retention_years` / `regulators` |
| `ComplianceModule` | 合规模块主类: `record_trade(dict)` / `trade_count` / `audit_event_count` / `query_trades(filter)` / `get_trade_stats` / `generate_daily_report` / `generate_monthly_report` / `generate_annual_report` / `verify_audit_integrity` / `storage_path` / `config` |
| `TradeSide` | 枚举: `Buy` / `Sell`(`__str__` 返回 `buy` / `sell`) |
| `OrderType` | 枚举: `Market` / `Limit` / `StopLoss` / `TakeProfit` / `StopLimit` / `TrailingStop`(`__str__` 返回 `market` / `limit` / `stop_loss` / `take_profit` / `stop_limit` / `trailing_stop`) |
| `LiquidityType` | 枚举: `Maker` / `Taker` |
| `TradeStatus` | 枚举: `Pending` / `Filled` / `PartiallyFilled` / `Cancelled` / `Rejected` |
| `AuditEventType` | 17 种审计事件: `TradeExecuted` / `OrderPlaced` / `OrderCancelled` / `OrderModified` / `PositionOpened` / `PositionClosed` / `StrategyStarted` / `StrategyStopped` / `ConfigChanged` / `UserLogin` / `UserLogout` / `ApiKeyCreated` / `ApiKeyRevoked` / `ReportGenerated` / `DataExported` / `SystemError` / `ComplianceAlert` |
| `TradeRecord` | 辅助类: `required_fields()` / `optional_fields()` 静态方法返回 trade dict 必填 / 可选字段名(`__new__` 不可直接用,Python 端走 dict 协议) |
| `load_config_from_toml(path, storage_path=None)` | 从 TOML 配置文件一步创建 `ComplianceModule`(Stage 1 兼容入口) |

#### 示例:基础合规流程

```python
import tempfile
from axon_quant.compliance import ComplianceModule, ComplianceConfig

tmp = tempfile.mkdtemp()
cfg = ComplianceConfig(
    account_id="acc-001",
    base_currency="USDT",
    large_trade_threshold=100_000.0,
    position_limit=1_000_000.0,
    max_portfolio_concentration=0.4,
    data_retention_years=7,
    regulators=["SEC", "FINRA"],
)
cm = ComplianceModule(cfg, tmp)

# 记录交易(dict 协议,字符串枚举大小写不敏感)
cm.record_trade({
    "strategy_id": "strat-1",
    "symbol": "BTCUSDT",
    "side": "buy",
    "quantity": 1.0,
    "price": 50_000.0,
    "fee": 50.0,
    "fee_currency": "USDT",
    "exchange": "Binance",
})

print(cm.trade_count, cm.audit_event_count)  # 1 1
print(cm.verify_audit_integrity())  # True
```

#### 提交 trade dict 协议

`record_trade(dict)` 用 dict 协议接收交易,**降门槛**(用户不必 import 5 个枚举)。字段:

| 字段 | 必填 | 类型 | 说明 |
| --- | --- | --- | --- |
| `strategy_id` | ✓ | str | 策略 ID |
| `symbol` | ✓ | str | 交易对(如 `BTCUSDT`) |
| `side` | ✓ | str | `buy` / `sell`(大小写不敏感) |
| `quantity` | ✓ | float | 数量,> 0 |
| `price` | ✓ | float | 价格,> 0 |
| `fee` | ✓ | float | 手续费 |
| `fee_currency` | ✓ | str | 手续费币种 |
| `exchange` | ✓ | str | 交易所名 |
| `trade_id` | ✗ | str (UUID) | 缺省自动生成 |
| `order_id` | ✗ | str (UUID) | 缺省自动生成 |
| `execution_time` | ✗ | str (RFC3339) | 缺省用当前 UTC |
| `settlement_time` | ✗ | str (RFC3339) | None |
| `status` | ✗ | str | `pending` / `filled` / `partially_filled` / `cancelled` / `rejected`(默认 `filled`) |
| `order_type` | ✗ | str | `market` / `limit` / `stop_loss` / `take_profit` / `stop_limit` / `trailing_stop`(默认 `market`) |
| `exchange_trade_id` | ✗ | str | 交易所返回的 trade ID |
| `liquidity` | ✗ | str | `maker` / `taker`(默认 `taker`) |
| `realized_pnl` | ✗ | float | 已实现盈亏 |
| `funding_rate` | ✗ | float | 资金费率 |
| `slippage` | ✗ | float | 滑点 |

错误:
- `KeyError` —— 缺必填字段
- `ValueError` —— 字段类型错 / UUID 解析失败 / 状态字符串无效
- `ComplianceError` —— 数量/价格 ≤ 0 / notional 不匹配 / 审计失败

#### 查询与统计

```python
# 查询交易(过滤条件全部 optional)
btc_trades = cm.query_trades({
    "symbol": "BTCUSDT",
    "side": "buy",
    "min_notional": 10_000.0,
    "start_time": "2026-01-01T00:00:00Z",
    "end_time": "2026-12-31T23:59:59Z",
})

# 统计(dict 返回)
stats = cm.get_trade_stats("2026-01-01T00:00:00Z", "2026-12-31T23:59:59Z")
print(stats["total_trades"], stats["win_rate"], stats["avg_trade_size"])
```

#### 报告生成(日报 / 月报 / 年报)

```python
# 日报(date="YYYY-MM-DD", starting_balance)
daily = cm.generate_daily_report("2026-06-24", 100_000.0)
print(daily["account_id"], daily["net_pnl"])

# 月报(year, month)
monthly = cm.generate_monthly_report(2026, 6)

# 年报(year, initial_balance)
annual = cm.generate_annual_report(2026, 100_000.0)
```

#### `ComplianceError` 异常体系

`ComplianceError` **直接**继承 builtin `Exception`(`PyException` 子类),**不**继承 Stage 1 `AxonError` 基类。设计原因:同 `BacktestError` / `RiskError` / `OmsError` / `ExchangeError` / `InferenceError` —— `axon-compliance` 反向依赖 `axon-python::AxonError` 会造成 cargo 循环,所以 Rust 侧不硬依赖。

错误码取 Rust `Debug` 输出的变体名,跨 release 稳定:

| 错误码 | 触发场景 |
| --- | --- |
| `InvalidTradeData` | quantity / price ≤ 0、notional 不匹配 |
| `ConcentrationLimitBreached` | 持仓集中度超限 |
| `LargeTradeThresholdExceeded` | 单笔交易超过大额交易阈值 |
| `AuditIntegrityFailed` | 审计日志哈希链校验失败 |
| `StorageError` | 文件存储错误 |
| `SerializationError` | 序列化 / 反序列化错误 |
| `ReportError` | 报告生成错误 |
| `RegulatorFormatError` | 监管报送格式错误 |
| `ConfigError` | 配置解析 / 验证错误 |

```python
from axon_quant.compliance import ComplianceModule, ComplianceConfig, ComplianceError
import tempfile

cfg = ComplianceConfig(
    account_id="acc-001", base_currency="USDT",
    large_trade_threshold=100_000.0, position_limit=1_000_000.0,
    max_portfolio_concentration=0.4, data_retention_years=7, regulators=["SEC"],
)
cm = ComplianceModule(cfg, tempfile.mkdtemp())

try:
    cm.record_trade({
        "strategy_id": "x", "symbol": "BTCUSDT", "side": "buy",
        "quantity": -1.0,  # 触发 InvalidTradeData
        "price": 50_000.0, "fee": 50.0, "fee_currency": "USDT", "exchange": "Binance",
    })
except ComplianceError as e:
    # e.args[0] 是稳定错误码(如 "InvalidTradeData")
    # e.args[1] 是人类可读形式:"[InvalidTradeData] Invalid trade data: Quantity must be positive"
    print(e.args[0], e.args[1])
```

**Stage 7 限制 / 注意事项**:

- `ComplianceModule` 内部用 `Mutex<RustModule>` 保护,Python 端多线程调用安全(无锁退化风险)。
- `query_trades` / `get_trade_stats` / `generate_*_report` 全部**同步**返回(无 async),CPU 计算密集,不需要 `block_on` 包装。
- 报告(dict 返回)通过 `serde_json` round-trip 从 Rust 端 `DailyReport` / `MonthlyReport` / `AnnualReport` 直接序列化,**不**在 Python 端定义对应的 pyclass(避免 30+ 字段的 boilerplate)。
- `TradeRecord` **不**作为可构造 pyclass 暴露,Python 端走 dict 协议(`required_fields()` / `optional_fields()` 仅给元信息)。
- `AuditEvent` **不**暴露给 Python(只暴露 `audit_event_count` getter),内部字段由 `AuditLog` 链式管理。
- `load_config_from_toml(path, storage_path=None)` 是 Stage 1 兼容入口,推荐用 `ComplianceModule(cfg, storage_path)` 新接口。

### `axon_quant.rl`

- `AxonEnv` —— Gymnasium 兼容环境
- `VecEnv` —— 向量化环境

### `axon_quant.llm`

- `ReActAgent` / `Tool` 基类
- `LLMBackend` / `OpenAICompatBackend` / `MockLLMBackend`
- `LLMMessage` / `LLMToolCall` / `LLMToolResult`

### `axon_quant.llm.trading`(Stage K 交付)

交易相关类型:

| 类 | 说明 |
|----|------|
| `RiskLimits` | 静态风控规则(`max_order_notional` / `max_daily_orders` / `max_position_abs` / `allowed_symbols`) |
| `SafetyMode` | `DryRun` / `TwoPhase` / `Direct` 枚举 |
| `MockTradingBackend` | 无 feature flag,默认可用,用于测试 |
| `ExchangeTradingBackend` | feature = `trading-exchange`,对接 Binance/OKX |
| `OmsTradingBackend` | feature = `trading-oms`,对接 axon-oms |
| `BacktestTradingBackend` | feature = `trading-backtest`,对接 axon-backtest |
| `PlaceOrderTool` | 下单 tool |
| `QueryPortfolioTool` | 查询持仓 / 余额 tool |
| `CancelOrderTool` | 撤单 tool |
| `ReplaceOrderTool` | 改单 tool |
| `TradingMetrics` | 指标收集器(支持 callback + snapshot) |

## 示例:LLM 交易 Mock 端到端

```python
import axon_quant
from axon_quant.llm.trading import (
    RiskLimits, SafetyMode, MockTradingBackend,
    PlaceOrderTool, QueryPortfolioTool,
)

# 1. 配置风控
risk = RiskLimits(
    max_order_notional=50_000.0,
    max_daily_orders=100,
    max_position_abs=10.0,
    allowed_symbols={"BTC-USDT", "ETH-USDT"},
)

# 2. 选后端(Mock,无 feature flag)
backend = MockTradingBackend(initial_cash=100_000.0)

# 3. 构造 tool
place_order = PlaceOrderTool(
    backend=backend,
    risk=risk,
    safety_mode=SafetyMode.DryRun,  # DryRun,不真下单
)

# 4. 调 tool
result = place_order.execute({
    "symbol": "BTC-USDT",
    "side": "Buy",
    "quantity": 0.1,
    "price": 50_000.0,
})
print(result)  # JSON 字符串
```

## 示例:Backtest 后端回放

```python
import axon_quant
from axon_quant.llm.trading import BacktestTradingBackend, PlaceOrderTool, RiskLimits

# 1. 准备历史数据
market_data = axon_quant.MarketData.from_parquet("./data/btc_2024.parquet")

# 2. Backtest 后端(对接 axon-backtest L1)
backend = BacktestTradingBackend(
    market_data=market_data,
    engine="L1",
    impact_model="almgren_chriss",
    fee_model="taker_5bps",
)

# 3. 配置 + 下单
risk = RiskLimits(max_order_notional=100_000.0, max_position_abs=1.0)
place = PlaceOrderTool(backend=backend, risk=risk, safety_mode=SafetyMode.Direct)

# 跑 LLM 决策(伪代码)
for obs in market_data:
    decision = llm_agent.decide(obs)
    if decision == "buy":
        place.execute({"symbol": "BTC-USDT", "side": "Buy", "quantity": 0.01})

# 4. 查最终 PnL
print(backend.get_balances())
print(backend.get_positions())
```

## 示例:metrics 接入 Prometheus

```python
import axon_quant
from prometheus_client import Counter, Histogram, start_http_server

orders_total = Counter(
    'trading_orders_total', 'Total trading orders',
    ['tool', 'side', 'status']
)
duration = Histogram(
    'trading_tool_execute_duration_seconds', 'Tool execution duration',
    ['tool']
)

start_http_server(9100)

def on_sample(sample):
    kind, data = sample
    if kind == 'counter_inc' and data['name'] == 'trading_orders_total':
        orders_total.labels(**data['labels']).inc(data['value'])
    elif kind == 'histogram_observe' and data['name'] == 'trading_tool_execute_duration_seconds':
        duration.labels(**data['labels']).observe(data['value_secs'])

axon_quant.set_metrics_callback(on_sample)
```

详见 [指标与告警](../user-guide/llm-trading/metrics-alerting.md) §3。

## 异步转同步

axon_quant Python 绑定内部用 `tokio::Runtime::block_on` 把 Rust async 转为 Python 同步。Python 端不需要 `asyncio`,但需要理解:

- 一次 Python 调用 = 一次完整的 Rust async 任务
- 长耗时操作(如 Backtest 跑全历史数据)可能阻塞 Python 主线程
- 建议在 `threading.Thread` 或 `concurrent.futures` 中跑

```python
from concurrent.futures import ThreadPoolExecutor

def run_backtest():
    backend = BacktestTradingBackend(...)
    # 跑完所有 LLM 决策
    return backend.get_balances()

with ThreadPoolExecutor() as ex:
    future = ex.submit(run_backtest)
    result = future.result()  # 阻塞等完成
```

## 类型映射

| Rust | Python |
|------|--------|
| `OrderSide::Buy` | `OrderSide.Buy`(枚举类) |
| `f64` | `float` |
| `i64` | `int` |
| `String` | `str` |
| `Vec<T>` | `list[T]` |
| `HashMap<String, T>` | `dict[str, T]` |
| `Result<T, E>` | 抛出异常(自定义异常类) |
| `chrono::DateTime<Utc>` | `datetime.datetime`(带 tzinfo=UTC) |

## 异常处理

| Rust 错误 | Python 异常 |
|-----------|-------------|
| `TradingError::RiskLimitsViolation` | `ValueError` |
| `TradingError::RiskGateBlocked` | `RuntimeError` |
| `TradingError::BackendError::Network` | `ConnectionError` |
| `TradingError::BackendError::Rejected` | `RuntimeError` |
| `TradingError::BackendError::InsufficientFunds` | `ValueError` |
| `TradingError::ParseError` | `ValueError` |

## Agent Swarm 多智能体协作

axon_quant 支持多 Agent 协作框架，采用 Actor 模型实现专业分工和投票共识。

### 架构概览

```
┌─────────────────────────────────────────────────────────────┐
│                    SwarmOrchestrator                          │
│  - Agent 生命周期管理                                          │
│  - 消息路由                                                    │
│  - 投票协调                                                    │
└────────────────────┬────────────────────────────────────────┘
                     │ tokio::mpsc
         ┌───────────┼───────────┐
         ▼           ▼           ▼
    ┌──────────┐ ┌──────────┐ ┌──────────┐
    │ Market   │ │ Risk     │ │ Execution│
    │ Agent    │ │ Agent    │ │ Agent    │
    └──────────┘ └──────────┘ └──────────┘
```

### 核心组件

| 组件 | 说明 |
|------|------|
| `AgentId` | Agent 唯一标识 |
| `AgentRole` | Agent 角色（Market / Risk / Execution / Audit） |
| `AgentMessage` | Agent 间消息 |
| `MessageContent` | 消息内容（MarketSignal / RiskSignal / TradeOrder 等） |
| `VoteProposal` | 投票提案 |
| `VoteResult` | 投票结果 |
| `ConsensusManager` | 共识管理器 |
| `SwarmOrchestrator` | Swarm 编排器 |

### 使用示例

```python
# Agent Swarm 目前仅在 Rust 层实现
# Python 绑定将在后续版本中提供
```

### 设计文档

详细设计请参考 [Agent Swarm 架构设计](https://github.com/pengwow/axon_quant/blob/main/.axon-internal/specs/2026-06-21-agent-swarm-design.md)。

## DeFi 链上交易（实验性）

> **注意**：DeFi 功能为实验性质，正在积极开发中，API 可能会变化。

### 架构概览

```
┌─────────────────────────────────────────────────────────────────────────┐
│                              axon-defi                                  │
│                                                                         │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌────────────┐ │
│  │ EvmAdapter   │  │ UniswapV3    │  │ MevShare     │  │ BridgeMgr  │ │
│  │ (Exchange    │  │ Router       │  │ Client       │  │ (LayerZero)│ │
│  │  Adapter)    │  │              │  │              │  │            │ │
│  └──────────────┘  └──────────────┘  └──────────────┘  └────────────┘ │
│  ┌──────────────┐                                                     │
│  │ ContractRisk │                                                     │
│  │ Checker      │                                                     │
│  └──────────────┘                                                     │
└─────────────────────────────────────────────────────────────────────────┘
```

### 核心组件

| 组件 | 说明 |
|------|------|
| `EvmAdapter` | EVM 链适配器，实现 ExchangeAdapter trait |
| `UniswapRouter` | Uniswap V3 路由，最优路径执行 |
| `MevShareClient` | MEV-Share 客户端，防止三明治攻击 |
| `ContractRiskChecker` | 智能合约风控检查器 |
| `BridgeManager` | 跨链桥管理器，LayerZero 集成 |

### 支持的链

| 链 | Chain ID | LayerZero ID |
|----|----------|--------------|
| Ethereum | 1 | 101 |
| Arbitrum | 42161 | 110 |
| Optimism | 10 | 111 |
| Polygon | 137 | 109 |

### Python 类型

| 类 | 说明 |
|----|------|
| `Chain` | EVM 链枚举（Ethereum / Arbitrum / Optimism / Polygon） |
| `EvmConfig` | EVM 链配置（RPC、私钥、API Key） |
| `DefiOrder` | DeFi 订单（代币、金额、滑点） |
| `SwapRoute` | 交易路由（输入/输出代币、费率） |
| `RiskCheckResult` | 风控检查结果 |
| `UniswapV3Contracts` | Uniswap V3 合约地址 |
| `DefiError` | DeFi 异常 |

### 使用示例

```python
from axon_quant._native.defi import (
    Chain, EvmConfig, DefiOrder, SwapRoute, RiskCheckResult,
    UniswapV3Contracts, DefiError,
)

# 1. 获取链配置
chain = Chain.Ethereum
print(f"Chain: {chain.name}, ID: {chain.chain_id}")

# 2. 获取 Uniswap V3 合约地址
contracts = UniswapV3Contracts.for_chain(Chain.Ethereum)
print(f"Router: {contracts.router}")

# 3. 创建 EVM 配置
config = EvmConfig(
    chain_id=1,
    rpc_url="https://mainnet.infura.io/v3/xxx",
    private_key="0x...",
)

# 4. 创建 DeFi 订单
order = DefiOrder("0xtoken", "1000", 50000.0)
print(f"Order: {order}")
```

### 设计文档

详细设计请参考 [DeFi 链上交易架构设计](https://github.com/pengwow/axon_quant/blob/main/.axon-internal/specs/2026-06-21-defi-onchain-trading-design.md)。
