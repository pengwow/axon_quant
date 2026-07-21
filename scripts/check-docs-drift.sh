#!/usr/bin/env bash
# 检查 docs/zh/ 与 docs/en/ 的结构漂移(structural drift)
#
# 0.8.0 Phase 1.2:CI 加 docs-en-vs-zh check(检测 drift)
# https://github.com/pengwow/axon_quant/blob/main/docs/superpowers/plans/2026-07-19-axon-quant-0.8.0.md
#
# 检查项:
# 1. docs/zh/ 和 docs/en/ 下 .md 文件列表必须完全一致
# 2. 每个子目录结构必须对齐
# 3. mkdocs.yml / mkdocs-en.yml nav 引用的所有文件必须存在
#
# 翻译内容差异(`diff -q` 显示 "Files differ")是预期行为,不视为 drift。
# 脚本在以下情况失败:
#   - 某文件只存在于 docs/zh/(或 docs/en/)
#   - 子目录结构不对称(如 zh 有 llm-trading/ 而 en 没有)
#   - nav 引用了不存在的 .md 文件

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

ZH_DIR="docs/zh"
EN_DIR="docs/en"

if [[ ! -d "$ZH_DIR" ]] || [[ ! -d "$EN_DIR" ]]; then
    echo "ERROR: $ZH_DIR 或 $EN_DIR 不存在" >&2
    exit 1
fi

echo "==> 检查 docs/zh/ 与 docs/en/ 结构对齐"

# 1. 收集所有 .md 相对路径
ZH_FILES=$(cd "$ZH_DIR" && find . -type f -name '*.md' | sort)
EN_FILES=$(cd "$EN_DIR" && find . -type f -name '*.md' | sort)

# 2. 检查 only-in-one-side
ONLY_ZH=$(comm -23 <(echo "$ZH_FILES") <(echo "$EN_FILES") || true)
ONLY_EN=$(comm -13 <(echo "$ZH_FILES") <(echo "$EN_FILES") || true)

if [[ -n "$ONLY_ZH" ]]; then
    echo "ERROR: 以下 .md 文件只存在于 docs/zh/(需在 docs/en/ 同步):" >&2
    echo "$ONLY_ZH" >&2
    exit 1
fi

if [[ -n "$ONLY_EN" ]]; then
    echo "ERROR: 以下 .md 文件只存在于 docs/en/(需在 docs/zh/ 同步):" >&2
    echo "$ONLY_EN" >&2
    exit 1
fi

# 3. 检查子目录对齐
ZH_DIRS=$(cd "$ZH_DIR" && find . -type d | sort)
EN_DIRS=$(cd "$EN_DIR" && find . -type d | sort)

ONLY_ZH_DIR=$(comm -23 <(echo "$ZH_DIRS") <(echo "$EN_DIRS") || true)
ONLY_EN_DIR=$(comm -13 <(echo "$ZH_DIRS") <(echo "$EN_DIRS") || true)

if [[ -n "$ONLY_ZH_DIR" ]]; then
    echo "ERROR: 以下子目录只存在于 docs/zh/:" >&2
    echo "$ONLY_ZH_DIR" >&2
    exit 1
fi

if [[ -n "$ONLY_EN_DIR" ]]; then
    echo "ERROR: 以下子目录只存在于 docs/en/:" >&2
    echo "$ONLY_EN_DIR" >&2
    exit 1
fi

# 4. 检查 mkdocs nav 引用的文件是否存在(中英都检查)
# 注:mkdocs nav 中每行的 .md 引用可能有多种形式:
#   - "Home: index.md"
#   - "  - Architecture: user-guide/architecture.md"
# 这里用 grep 提取所有以 .md 结尾的 token,再做存在性校验。
check_nav_files() {
    local config=$1
    local docs_dir=$2
    local label=$3
    local missing=0

    while IFS= read -r ref; do
        if [[ -z "$ref" ]]; then
            continue
        fi
        if [[ ! -f "$docs_dir/$ref" ]]; then
            echo "ERROR: $label nav 引用 '$ref' 但文件不存在 ($docs_dir/$ref)" >&2
            missing=1
        fi
    done < <(grep -oE '[A-Za-z0-9_./-]+\.md' "$config" | sort -u)

    return $missing
}

if ! check_nav_files "mkdocs.yml" "$ZH_DIR" "中文(zh)"; then
    exit 1
fi

if ! check_nav_files "mkdocs-en.yml" "$EN_DIR" "英文(en)"; then
    exit 1
fi

ZH_COUNT=$(echo "$ZH_FILES" | wc -l | tr -d ' ')
ZH_DIR_COUNT=$(echo "$ZH_DIRS" | wc -l | tr -d ' ')

echo "✅ 结构对齐:zh 与 en 均有 $ZH_COUNT 个 .md 文件 + $ZH_DIR_COUNT 个子目录"
echo "✅ mkdocs.yml / mkdocs-en.yml nav 引用全部命中"
echo ""
echo "注:翻译内容差异('Files differ')是预期行为,本脚本只检测结构漂移。"
