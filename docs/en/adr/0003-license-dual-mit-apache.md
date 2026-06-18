# ADR 0003: Apache-2.0 License

## Status

Accepted

## Context

AXON is an open-source quantitative trading framework. Need a license that:
- Permits commercial use
- Allows modification and distribution
- Provides patent protection
- Is widely recognized and understood

## Decision

Use Apache License 2.0 for the entire project.

### Rationale

1. **Commercial Friendly**: Permits use in commercial products without copyleft requirements
2. **Patent Protection**: Includes explicit patent grant from contributors
3. **Industry Standard**: Widely used in open-source projects (Kubernetes, TensorFlow, etc.)
4. **Compatibility**: Compatible with most other open-source licenses
5. **Contributor Friendly**: Clear terms encourage contributions

## Consequences

### Positive

- Can be used commercially without restrictions
- Patent protection for users
- Clear contribution terms
- Wide ecosystem compatibility

### Negative

- Requires copyright notice and license copy in distributions
- More verbose than MIT license
- Some organizations may have license policies requiring approval

### Neutral

- All source files include Apache-2.0 header
- LICENSE file included in repository root
- Third-party dependency licenses documented
