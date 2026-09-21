#!/usr/bin/env bash
# 本地提交前检查：格式 + clippy(-D warnings) + 全量测试。
# 与 CI（.github/workflows/ci.yml）保持一致。
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> cargo fmt --check"
cargo fmt --all --check

echo "==> cargo clippy -D warnings"
cargo clippy --workspace --all-targets -- -D warnings

echo "==> cargo test --workspace"
cargo test --workspace

echo "全部通过。"
