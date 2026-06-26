# Exchange Security Best Practices

> **Full example**: [`examples/09_exchange/binance_demo.py`](https://github.com/pengwow/axon_quant/blob/main/examples/09_exchange/binance_demo.py)
> Complete Binance testnet integration (REST + HMAC signing + K-line queries).

This document outlines the security best practices for using the `axon_quant.exchange` Python bindings
in production. **Treat API keys as production credentials**; a leak can lead to direct financial loss.

## 1. Never Hard-Code API Keys

**Always** read API keys from environment variables, never from source code, config files, or
notebook cells:

```python
# BAD — hard-coded in source
adapter = BinanceAdapter(ExchangeConfig(
    exchange_id=ExchangeId.Binance,
    api_key="abc123mysecretkey",
    api_secret="hunter2",
    testnet=False,
))

# GOOD — read from environment
import os
os.environ["BINANCE_API_KEY"] = "..."        # set in shell, not source
os.environ["BINANCE_API_SECRET"] = "..."
adapter = BinanceAdapter(binance_testnet_config())
```

The `binance_testnet_config()` and `okx_testnet_config()` factories read keys exclusively from
environment variables and raise `ExchangeError` if any are missing.

## 2. Environment Variable Naming

| Exchange | API Key | Secret | Passphrase |
|----------|---------|--------|------------|
| Binance  | `BINANCE_API_KEY` | `BINANCE_API_SECRET` | (n/a) |
| OKX      | `OKX_API_KEY` | `OKX_API_SECRET` | `OKX_PASSPHRASE` |

Set them via shell before launching Python:

```bash
export BINANCE_API_KEY="..."
export BINANCE_API_SECRET="..."
python my_strategy.py
```

Or use a secrets manager (Vault, AWS Secrets Manager, GCP Secret Manager) and inject the values
into the process environment at startup. **Never** commit `.env` files to git.

## 3. Testnet First, Production Last

The default `testnet=True` is intentional. Production mode (`testnet=False`) requires explicit
configuration:

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

Always smoke-test on testnet first; only flip to production after the strategy has been validated.

## 4. Verify `__repr__` Does Not Leak Secrets

axon-quant guarantees that `api_secret` and `passphrase` **never** appear in `repr()`. Verify this
in your own setup:

```python
cfg = binance_testnet_config()
r = repr(cfg)
assert os.environ["BINANCE_API_SECRET"] not in r, "API secret leaked in repr!"
assert os.environ.get("OKX_PASSPHRASE", "") not in r, "Passphrase leaked in repr!"

adapter = BinanceAdapter(cfg)
r = repr(adapter)
assert "BinanceAdapter(...)" == r  # exact match
```

The Python E2E test suite (`python/tests/test_exchange_e2e.py`) includes a permanent
`test_exchange_config_repr_does_not_leak_secret` regression test. Run it after any adapter change.

## 5. Rotate API Keys Periodically

| Lifecycle | Recommendation |
|-----------|----------------|
| Read-only keys | Rotate every 90 days |
| Trading keys | Rotate every 30–60 days |
| After any suspected leak | Rotate immediately + revoke old key |

When rotating, use the exchange's UI to **issue a new key first**, deploy the new env vars, then
**delete the old key** — never overlap without coordination.

## 6. Use IP Whitelist on the Exchange Side

Most exchanges (Binance, OKX) support IP whitelisting. **Always** enable it for production keys:

- Restrict the key to the IP range of your production trading servers
- Use a different (or no) whitelist for testnet keys
- If you must access from dynamic IPs, prefer a stable egress IP via NAT gateway or VPN

## 7. Prefer Read-Only API Keys When Possible

| Use Case | Required Permissions |
|----------|----------------------|
| Market data collection | Read-only |
| Account / position query | Read-only |
| Order submission | Trade (read + write) |
| Withdrawal | **NEVER** enable for automated systems |

Withdrawal permission is the highest-risk setting. Do **not** include it in any key used by
`axon_quant.exchange`.

## 8. Audit Logging

`axon_quant.exchange` does not log API secrets, but you should still:

- Log every order submission (symbol / side / quantity / price) to an append-only audit log
- Record `ExchangeError` codes for anomaly detection
- Set up alerts for: `AuthenticationFailed` (key rotation needed), `RateLimited` (algorithm too
  aggressive), `OrderRejected` (compliance / risk control triggered)

## 9. Process Isolation

Run trading strategies in a process with minimal privileges:

- Dedicated user / container with no unnecessary network egress
- Read-only filesystem where possible
- Resource limits (CPU / memory / file descriptors) to prevent DoS
- No interactive shells in production

## 10. Incident Response

If you suspect a key leak:

1. **Revoke** the leaked key in the exchange UI immediately
2. **Rotate** to a new key (see §5)
3. **Audit** recent orders for unauthorized activity
4. **Review** logs and version control history for the leak source
5. **Update** incident runbook based on findings

## Summary Checklist

Before going to production:

- [ ] API keys read from environment variables (not source / not config files)
- [ ] `repr(adapter)` does not contain secrets (verified by automated test)
- [ ] `testnet=True` for all non-production environments
- [ ] Production keys have **no** withdrawal permission
- [ ] IP whitelist enabled on the exchange side
- [ ] API keys rotated within the last 30–60 days
- [ ] Audit log captures every order and error
- [ ] Process runs with minimal privileges (dedicated user / container)
- [ ] Incident response runbook is up to date

## Related Documentation

- `docs/en/reference/python-bindings.md` — Python API reference
- `docs/en/about/security.md` — General axon-quant security model
- `.axon-internal/specs/2026-06-19-python-bindings-expansion-v1.1.md` — Design spec for the
  exchange Python bindings
