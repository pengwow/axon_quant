# 交易所安全

> **完整示例**: [`examples/09_exchange/binance_demo.py`](../../../examples/09_exchange/binance_demo.py)
> Binance 测试网对接完整流程（REST + 签名 + K 线查询）。最佳实践

本文档介绍在生产环境使用 `axon_quant.exchange` Python 绑定时的安全最佳实践。**请把 API key 视为生产凭证**——泄漏可能直接造成资金损失。

## 1. 永远不要硬编码 API Key

**始终**从环境变量读取 API key,绝不从源码、配置文件或 notebook cell 中读取:

```python
# 错误 —— 源码硬编码
adapter = BinanceAdapter(ExchangeConfig(
    exchange_id=ExchangeId.Binance,
    api_key="abc123mysecretkey",
    api_secret="hunter2",
    testnet=False,
))

# 正确 —— 从环境变量读取
import os
os.environ["BINANCE_API_KEY"] = "..."        # 在 shell 中设置,不要写进源码
os.environ["BINANCE_API_SECRET"] = "..."
adapter = BinanceAdapter(binance_testnet_config())
```

`binance_testnet_config()` / `okx_testnet_config()` 工厂函数**只**从环境变量读取 key,缺一即抛 `ExchangeError`。

## 2. 环境变量命名

| 交易所 | API Key | Secret | Passphrase |
|--------|---------|--------|------------|
| Binance | `BINANCE_API_KEY` | `BINANCE_API_SECRET` | (无) |
| OKX | `OKX_API_KEY` | `OKX_API_SECRET` | `OKX_PASSPHRASE` |

启动 Python 前在 shell 中设置:

```bash
export BINANCE_API_KEY="..."
export BINANCE_API_SECRET="..."
python my_strategy.py
```

也可使用密钥管理服务(Vault / AWS Secrets Manager / GCP Secret Manager),在进程启动时把值注入环境变量。**绝不**把 `.env` 文件提交到 git。

## 3. 先 testnet,后 production

默认 `testnet=True` 是有意为之。生产模式(`testnet=False`)需要显式配置:

```python
# Stage 5 默认构造强制 testnet=True
cfg = ExchangeConfig(
    exchange_id=ExchangeId.Binance,
    api_key=os.environ["BINANCE_API_KEY"],
    api_secret=os.environ["BINANCE_API_SECRET"],
    rest_base_url="https://api.binance.com",  # 显式 production URL
    ws_url="wss://stream.binance.com:9443/ws",
    testnet=False,                            # 显式 production
)
```

策略在 testnet 验证前不要切生产;切生产前必须有完整回测 + 至少 1 周 testnet 实盘记录。

## 4. 自验 `__repr__` 不泄漏 secret

axon-quant 保证 `api_secret` / `passphrase` **永远不会**出现在 `repr()`。可在自己的环境中自验:

```python
cfg = binance_testnet_config()
r = repr(cfg)
assert os.environ["BINANCE_API_SECRET"] not in r, "API secret 泄漏到 repr!"
assert os.environ.get("OKX_PASSPHRASE", "") not in r, "passphrase 泄漏到 repr!"

adapter = BinanceAdapter(cfg)
r = repr(adapter)
assert "BinanceAdapter(...)" == r  # 完全匹配
```

Python E2E 测试套件(`python/tests/test_exchange_e2e.py`)中包含长期回归测试
`test_exchange_config_repr_does_not_leak_secret`,任何 adapter 改动后必须跑通。

## 5. 定期轮换 API Key

| 生命周期 | 建议 |
|----------|------|
| 只读 key | 90 天 |
| 交易 key | 30–60 天 |
| 怀疑泄漏后 | 立即轮换 + 撤销旧 key |

轮换流程:在交易所 UI **先签发新 key** → 部署新 env → **删除旧 key**;切勿让新旧 key 同时在线无协调运行。

## 6. 在交易所端启用 IP 白名单

Binance / OKX 等大多数交易所支持 IP 白名单。**生产 key 必须**开启:

- 限制 key 仅在生产交易服务器 IP 段内可用
- testnet key 使用独立白名单(或不开白名单)
- 若必须从动态 IP 访问,优先用 NAT 网关或 VPN 拿到稳定出口 IP

## 7. 优先使用只读 API Key

| 场景 | 所需权限 |
|------|----------|
| 行情采集 | 只读 |
| 账户 / 持仓查询 | 只读 |
| 下单 | 交易(读 + 写) |
| 提现 | **永不**为自动化系统开启 |

提现权限是最高风险设置。**不要**在任何 `axon_quant.exchange` 使用的 key 中包含提现权限。

## 8. 审计日志

`axon_quant.exchange` 本身不记录 secret,但你仍应:

- 把每次下单(symbol / side / quantity / price)写入 append-only 审计日志
- 记录 `ExchangeError` 错误码,做异常检测
- 配置告警:
  - `AuthenticationFailed` → key 需轮换
  - `RateLimited` → 算法太激进
  - `OrderRejected` → 合规 / 风控触发

## 9. 进程隔离

在最小权限进程中运行交易策略:

- 专用用户 / 容器,无多余网络出口
- 尽可能 read-only 文件系统
- 资源限制(CPU / 内存 / 文件描述符)防止 DoS
- 生产环境不开交互式 shell

## 10. 事件响应

怀疑 key 泄漏后:

1. **撤销** —— 在交易所 UI 立即吊销泄漏的 key
2. **轮换** —— 签发新 key(见 §5)
3. **审计** —— 查近期订单,确认无未授权活动
4. **复盘** —— 查日志与 git 历史,定位泄漏源
5. **更新** —— 把经验回写事件响应 runbook

## 总结清单

上线 production 前:

- [ ] API key 从环境变量读取(非源码 / 非配置文件)
- [ ] `repr(adapter)` 不含 secret(自动化测试已覆盖)
- [ ] 非生产环境一律 `testnet=True`
- [ ] 生产 key **不**含提现权限
- [ ] 交易所端开启 IP 白名单
- [ ] API key 30–60 天内轮换过
- [ ] 审计日志记录每个订单和错误
- [ ] 进程以最小权限运行(专用用户 / 容器)
- [ ] 事件响应 runbook 是最新版

## 相关文档

- `docs/zh/reference/python-bindings.md` —— Python API 参考
- `docs/zh/about/security.md` —— axon-quant 整体安全模型
- `.axon-internal/specs/2026-06-19-python-bindings-expansion-v1.1.md` —— 交易所 Python 绑定设计规范
