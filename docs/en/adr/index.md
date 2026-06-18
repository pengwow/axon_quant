# Architecture Decision Records

This directory contains Architecture Decision Records (ADRs) for the AXON project.

## What is an ADR?

An ADR captures an important architectural decision along with its context and consequences.

## ADR Format

Each ADR follows this format:

```markdown
# ADR {number}: {title}

## Status

{Proposed | Accepted | Deprecated | Superseded}

## Context

{Describe the context and problem statement}

## Decision

{Describe the decision that was made}

## Consequences

### Positive

- {Positive consequence 1}
- {Positive consequence 2}

### Negative

- {Negative consequence 1}
- {Negative consequence 2}

### Neutral

- {Neutral consequence 1}
```

## ADR Index

| ADR | Title | Status |
|-----|-------|--------|
| [0001](0001-rust-as-core-language.md) | Rust as Core Language | Accepted |
| [0002](0002-cargo-workspace-layout.md) | Cargo Workspace Layout | Accepted |
| [0003](0003-license-dual-mit-apache.md) | Apache-2.0 License | Accepted |

## Creating New ADRs

1. Create a new file: `docs/adr/{number}-{title-slug}.md`
2. Use the template above
3. Submit PR for review
4. Update this index after approval

## References

- [ADR GitHub Repository](https://github.com/joelparkerhenderson/architecture-decision-record)
- [Michael Nygard's ADR Article](https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions)
