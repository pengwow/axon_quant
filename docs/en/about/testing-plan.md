# Testing Plan

AXON uses a comprehensive testing strategy to ensure code quality and reliability.

## Test Types

### Unit Tests

Unit tests verify individual functions and modules in isolation.

```bash
# Run all unit tests
cargo test --workspace

# Run tests for specific crate
cargo test -p axon-rl
cargo test -p axon-llm
```

### Integration Tests

Integration tests verify interactions between modules.

```bash
# Run integration tests
cargo test --test integration_tests

# Run specific integration test
cargo test --test integration_tests test_exchange_adapter
```

### Property-Based Tests

AXON uses proptest for property-based testing to verify invariants.

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn test_portfolio_invariants(capital in 1000.0..1_000_000.0) {
        let portfolio = Portfolio::new(capital);
        prop_assert!(portfolio.total_value() >= 0.0);
        prop_assert!(portfolio.cash() <= portfolio.total_value());
    }
}
```

### Benchmarks

Benchmarks verify performance requirements.

```bash
# Run benchmarks
cargo bench --workspace

# Run specific benchmark
cargo bench -p axon-core --bench order_book
```

## Test Coverage

AXON aims for high test coverage:

- **Unit tests**: > 90% line coverage
- **Integration tests**: All critical paths covered
- **Property tests**: All public APIs verified

## CI/CD

Tests run automatically in CI:

1. **Format check**: `cargo fmt --all -- --check`
2. **Lint**: `cargo clippy --workspace --all-targets -- -D warnings`
3. **Test**: `cargo test --workspace`
4. **Build**: `cargo build --workspace --release`
5. **MSRV**: Verify minimum supported Rust version

## Test Data

Test data is generated programmatically:

```rust
fn generate_test_market_data(n: usize) -> Vec<MarketBar> {
    let mut rng = StdRng::seed_from_u64(42);
    (0..n)
        .map(|i| MarketBar {
            timestamp: Timestamp::from_millis(i as i64),
            open: Price::from_f64(100.0 + rng.gen::<f64>() * 10.0),
            high: Price::from_f64(105.0 + rng.gen::<f64>() * 5.0),
            low: Price::from_f64(95.0 + rng.gen::<f64>() * 5.0),
            close: Price::from_f64(100.0 + rng.gen::<f64>() * 10.0),
            volume: Quantity::from_f64(1000.0 + rng.gen::<f64>() * 500.0),
        })
        .collect()
}
```

## Mocking

AXON uses mockall for mocking external dependencies:

```rust
use mockall::automock;

#[automock]
trait ExchangeAdapter {
    async fn place_order(&self, order: Order) -> Result<OrderId, ExchangeError>;
    async fn get_balance(&self) -> Result<Balance, ExchangeError>;
}

#[tokio::test]
async fn test_order_placement() {
    let mut mock_adapter = MockExchangeAdapter::new();
    mock_adapter
        .expect_place_order()
        .returning(|_| Ok(OrderId::new("test-123")));
    
    let order = Order::spot(1, "BTC", "USDT", Side::Buy, OrderType::Market, Quantity::from_f64(0.001), TimeInForce::GTC);
    let result = mock_adapter.place_order(order).await;
    
    assert!(result.is_ok());
}
```

## Performance Testing

Performance tests verify latency and throughput requirements:

```rust
#[bench]
fn bench_order_matching(b: &mut Bencher) {
    let mut engine = L1MatchingEngine::new();
    // Setup order book
    // ...
    
    b.iter(|| {
        black_box(engine.process_order(Order::market("BTCUSDT", Side::Buy, Quantity::from_f64(0.001))));
    });
}
```

## Chaos Testing

AXON includes chaos testing for resilience:

- Network failures
- Exchange outages
- Memory pressure
- Concurrent access

## Next Steps

- [Contributing Guide](contributing.md) — How to contribute tests
- [API Reference](../reference/api-reference.md) — API documentation
