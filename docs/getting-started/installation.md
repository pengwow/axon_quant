# 安装

> 适用版本:AXON v0.1.0+

本文档描述 AXON 的安装方法。

## 1. 前置依赖

### 1.1 必需

| 工具 | 版本 | 说明 |
|------|------|------|
| **Rust** | 1.96.0+ | 由 `rust-toolchain.toml` 强制 |
| **Git** | 2.30+ | 拉取源码 |

### 1.2 可选

| 工具 | 用途 |
|------|------|
| **Python 3.14.6** | PyO3 绑定(axon-rl、axon-hpo、axon-walk-forward、axon-distributed、axon-llm) |
| **CUDA Toolkit** | GPU 加速(axon-inference feature = `cuda`) |
| **Docker** | 容器化部署 |

## 2. 安装 Rust 工具链

如果使用 `rustup`,直接进入仓库根目录即可触发 `rust-toolchain.toml` 自动安装:

```bash
cd axon_quant
rustup show  # 自动下载 1.96.0
```

## 3. 克隆并构建

```bash
git clone https://github.com/pengwow/axon_quant.git
cd axon_quant

# 调试构建
cargo build --workspace

# Release 构建(LTO,1 codegen-unit)
cargo build --workspace --release
```

## 4. 安装 CLI

```bash
cargo install --path crates/axon-cli --locked
```

验证:

```bash
axon --version
```

## 5. Python 绑定(可选)

```bash
# 创建虚拟环境(必须使用 pyenv 的 3.14.6)
pyenv install 3.14.6
pyenv virtualenv 3.14.6 axon_quant
pyenv local axon_quant
pyenv shell axon_quant

# 编译并安装
make python-install

# 验证
python -c "import axon_quant; print(axon_quant.__version__)"
```

## 6. 验证安装

```bash
# 运行全部单元测试
cargo test --workspace

# 运行 lint + 格式检查
make verify
```

## 下一步

- [第一个回测](quickstart.md) —— 5 分钟跑通
- [架构总览](../user-guide/architecture.md) —— 了解系统组成
