#!/usr/bin/env bash
# AXON 端到端测试脚本
# 用法: ./scripts/e2e_test.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_DIR"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

pass=0
fail=0
skip=0

run_test() {
    local name="$1"
    shift
    echo -n "  $name ... "
    if "$@" >/dev/null 2>&1; then
        echo -e "${GREEN}PASS${NC}"
        ((pass++))
    else
        echo -e "${RED}FAIL${NC}"
        ((fail++))
    fi
}

run_skip() {
    local name="$1"
    local reason="$2"
    echo -e "  $name ... ${YELLOW}SKIP${NC} ($reason)"
    ((skip++))
}

echo "============================================"
echo "AXON 全流程测试"
echo "============================================"
echo ""

# ============================================
# L0: 编译 + 静态检查
# ============================================
echo "--- L0: 编译 + 静态检查 ---"
run_test "cargo check" cargo check --workspace
run_test "cargo clippy" cargo clippy --workspace -- -D warnings
run_test "cargo fmt check" cargo fmt --all -- --check
echo ""

# ============================================
# L1: 单元测试（按 crate）
# ============================================
echo "--- L1: 单元测试 ---"
run_test "axon-core" cargo test -p axon-core --lib
run_test "axon-backtest" cargo test -p axon-backtest --lib
run_test "axon-rl" cargo test -p axon-rl --lib
run_test "axon-hpo" cargo test -p axon-hpo --lib
run_test "axon-walk-forward" cargo test -p axon-walk-forward --lib
run_test "axon-tracker" cargo test -p axon-tracker --lib
run_test "axon-registry" cargo test -p axon-registry --lib
run_test "axon-distributed" cargo test -p axon-distributed --lib
run_test "axon-python" cargo test -p axon-python --lib
echo ""

# ============================================
# L2: Python wheel 打包 + 安装
# ============================================
echo "--- L2: Python wheel ---"
run_test "maturin build" maturin build --release
run_test "pip install" .venv/bin/pip install target/wheels/axon_quant-*.whl --force-reinstall --no-deps --quiet
run_test "import axon_quant" .venv/bin/python3 -c "import axon_quant; assert axon_quant.__version__"
run_test "import rl" .venv/bin/python3 -c "import axon_quant; assert axon_quant.rl"
run_test "import hpo" .venv/bin/python3 -c "import axon_quant; assert axon_quant.hpo"
run_test "import walk_forward" .venv/bin/python3 -c "import axon_quant; assert axon_quant.walk_forward"
run_test "import tracker" .venv/bin/python3 -c "import axon_quant; assert axon_quant.tracker"
run_test "import registry" .venv/bin/python3 -c "import axon_quant; assert axon_quant.registry"
run_test "import distributed" .venv/bin/python3 -c "import axon_quant; assert axon_quant.distributed"
echo ""

# ============================================
# L3: Python 场景测试
# ============================================
echo "--- L3: Python 场景测试 ---"
if [ -f ".venv/bin/pytest" ]; then
    run_test "pytest scenarios" .venv/bin/pytest tests/python/ -v --tb=short
else
    run_skip "pytest scenarios" "pytest not installed"
fi
echo ""

# ============================================
# 汇总
# ============================================
echo "============================================"
echo -e "结果: ${GREEN}${pass} passed${NC}, ${RED}${fail} failed${NC}, ${YELLOW}${skip} skipped${NC}"
echo "============================================"

if [ "$fail" -gt 0 ]; then
    exit 1
fi
