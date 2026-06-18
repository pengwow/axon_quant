# PyPI 发布指南

## 当前状态

项目已经配置了基本的 PyPI 发布流程，但需要一些调整才能正确工作。

## 编译流程

### 本地编译

```bash
# 使用 Makefile（推荐）
make python-build

# 或直接使用 maturin
PYO3_PYTHON=.venv/bin/python .venv/bin/maturin build --release
```

### 输出位置

wheel 文件会生成在 `target/wheels/` 目录下，文件名格式：
```
axon_quant-0.1.0a1-cp312-cp312-manylinux_2_17_x86_64.manylinux2014_x86_64.whl
```

## 发布到 PyPI

### 前置条件

1. **PyPI 账户**：需要在 https://pypi.org 创建账户
2. **API Token**：在 PyPI 账户设置中生成 API Token
3. **GitHub Secrets**：在仓库设置中添加以下 secrets：
   - `PYPI_API_TOKEN`：PyPI API Token
   - `TESTPYPI_API_TOKEN`：TestPyPI API Token（可选，用于测试）

### 手动发布

```bash
# 1. 构建 wheel
make python-build

# 2. 安装 twine
pip install twine

# 3. 上传到 TestPyPI（测试）
twine upload --repository testpypi target/wheels/*

# 4. 上传到 PyPI（正式）
twine upload target/wheels/*
```

### 自动发布（GitHub Actions）

项目已配置 `.github/workflows/release.yml`，当推送 `v*` 标签时自动触发：

```bash
# 创建标签
git tag v0.1.0a1
git push origin v0.1.0a1
```

**注意**：当前的 release.yml 需要更新以正确使用 maturin。

## 版本管理

### 版本号格式

项目使用语义化版本：
- `0.1.0a1`：Alpha 版本
- `0.1.0b1`：Beta 版本
- `0.1.0rc1`：Release Candidate
- `0.1.0`：正式版本

### 更新版本

1. **Cargo.toml**：`version = "0.1.0-alpha.1"`
2. **pyproject.toml**：`version = "0.1.0a1"`

**注意**：Python 使用 `a1` 格式，Rust 使用 `0.1.0-alpha.1` 格式。

## 平台支持

当前支持的平台：
- **Linux**：`manylinux_2_17_x86_64`
- **macOS**：`macosx_11_0_arm64`（Apple Silicon）
- **Windows**：`win_amd64`

## 故障排除

### 编译失败

```bash
# 确保使用正确的 Python 环境
PYO3_PYTHON=.venv/bin/python maturin build --release

# 检查 Rust 工具链
rustup show

# 清理重新编译
cargo clean
make python-build
```

### 导入失败

```bash
# 检查 wheel 是否正确安装
pip show axon-quant

# 检查扩展模块
python -c "import axon_quant; print(axon_quant.__version__)"
```

## 下一步

1. **更新 release.yml**：使用 maturin 替代 `python -m build`
2. **添加 CI 检查**：在 PR 中验证 wheel 可以正确构建
3. **多平台构建**：确保在所有目标平台构建 wheel
4. **文档更新**：更新 README 和用户指南中的安装说明
