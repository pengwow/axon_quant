# Contributing Guide

Welcome to the AXON project! This document describes the contribution process and conventions.

## Code of Conduct

- Friendly, inclusive, and professional communication
- Prefer discussing design in issues before submitting large PRs
- Code quality first; do not accept "merge first, optimize later" PRs

## Contribution Process

1. **Issue First**: Any non-trivial change should start with an issue for design discussion
2. **Fork & Branch**: Create feature branch from `main` (`feat/xxx` / `fix/xxx` / `docs/xxx`)
3. **TDD**: Write tests first for new features (see [Testing Plan](testing-plan.md))
4. **Lint & Test**: Run `make verify` locally, all checks must pass
5. **PR Description**: Link issue + change description + test screenshots / benchmark results
6. **Code Review**: At least 1 maintainer approval before merge
7. **Squash Merge**: Use squash merge to keep main clean

## Development Setup

```bash
# Clone repository
git clone https://github.com/pengwow/axon_quant.git
cd axon_quant

# Install dependencies
make setup

# Run verification
make verify
```

## Code Style

### Rust

- Follow `rustfmt` formatting (enforced in CI)
- Use `clippy` lints with `-D warnings`
- Document public APIs with `///` doc comments
- Write unit tests for all public functions

```rust
/// Calculates the Sharpe ratio from a series of returns.
///
/// # Arguments
/// * `returns` - Slice of periodic returns
/// * `risk_free_rate` - Annual risk-free rate
///
/// # Returns
/// Annualized Sharpe ratio
pub fn sharpe_ratio(returns: &[f64], risk_free_rate: f64) -> f64 {
    // Implementation
}
```

### Python

- Follow PEP 8 style
- Use type hints for all function signatures
- Write docstrings for public functions
- Use `ruff` for linting (enforced in CI)

```python
def calculate_sharpe(returns: list[float], risk_free_rate: float = 0.02) -> float:
    """Calculate annualized Sharpe ratio.
    
    Args:
        returns: List of periodic returns
        risk_free_rate: Annual risk-free rate
        
    Returns:
        Annualized Sharpe ratio
    """
    # Implementation
```

## Commit Messages

Use conventional commits:

```
feat(rl): add new reward function
fix(exchange): handle network timeout
docs: update installation guide
test(llm): add integration tests
chore: update dependencies
```

## Testing

### Unit Tests

Run all unit tests:

```bash
cargo test --workspace
```

### Integration Tests

Run integration tests:

```bash
cargo test --test integration_tests
```

### Property-Based Tests

Use proptest for property-based testing:

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn test_sharpe_ratio_bounds(returns in prop::collection::vec(-0.5..0.5, 1..100)) {
        let sr = sharpe_ratio(&returns, 0.0);
        // Sharpe ratio should be finite
        prop_assert!(sr.is_finite());
    }
}
```

## Documentation

- Update documentation for any user-facing changes
- Add examples for new features
- Keep README.md up to date

## Release Process

1. Update version in `Cargo.toml` and `pyproject.toml`
2. Update `CHANGELOG.md`
3. Create release PR
4. After merge, create git tag: `git tag v0.1.0`
5. Push tag: `git push origin v0.1.0`

## Questions?

Open a GitHub Issue or reach out on the project's discussion board.
