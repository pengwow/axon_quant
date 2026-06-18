# PyPI Publishing Guide

This guide explains how to publish AXON packages to PyPI.

## Prerequisites

1. **PyPI Account**: Create an account at https://pypi.org
2. **API Token**: Generate an API token in PyPI account settings
3. **Build Tools**: Install maturin and twine

```bash
pip install maturin twine
```

## Building Packages

### Build Python Wheel

```bash
# Build release wheel
maturin build --release

# Build for specific Python version
maturin build --release --interpreter python3.14

# Build universal wheel (multiple Python versions)
maturin build --release --universal
```

### Build Source Distribution

```bash
maturin sdist
```

### Build Output

Wheels are generated in `target/wheels/`:

```
target/wheels/
├── axon_quant-0.1.0-cp312-cp312-macosx_11_0_arm64.whl
├── axon_quant-0.1.0-cp312-cp312-manylinux_2_17_x86_64.whl
└── axon_quant-0.1.0.tar.gz
```

## Publishing to TestPyPI

Test packages on TestPyPI before production:

```bash
# Upload to TestPyPI
twine upload --repository testpypi target/wheels/*

# Install from TestPyPI
pip install --index-url https://test.pypi.org/simple/ axon-quant
```

## Publishing to PyPI

```bash
# Upload to production PyPI
twine upload target/wheels/*
```

## Version Management

Update version in `pyproject.toml`:

```toml
[project]
version = "0.1.0"
```

AXON follows Semantic Versioning:
- MAJOR: Breaking changes
- MINOR: New features (backwards compatible)
- PATCH: Bug fixes

## CI/CD Publishing

AXON uses GitHub Actions for automated publishing:

1. Push to `main` branch triggers build
2. Create git tag for release: `git tag v0.1.0`
3. Push tag: `git push origin v0.1.0`
4. GitHub Actions builds and publishes to PyPI

## Troubleshooting

### "File already exists" error

Package version already exists. Bump version number.

### Authentication failure

Verify API token is correct and has upload permissions.

### Build fails

Check Rust toolchain version and Python version compatibility.

## Next Steps

- [Installation](../getting-started/installation.md) — Install AXON
- [Python Bindings](../reference/python-bindings.md) — Python API documentation
