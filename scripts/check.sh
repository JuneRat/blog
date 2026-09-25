#!/usr/bin/env bash
# 本地提交前检查：格式 + clippy(-D warnings) + 全量测试 + 后台 SPA 类型/测试 + 备份恢复演练。
# 与 CI（.github/workflows/ci.yml 的 check 与 web 两个 job）保持一致。
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> cargo fmt --check"
cargo fmt --all --check

echo "==> cargo clippy -D warnings"
cargo clippy --workspace --all-targets -- -D warnings

echo "==> cargo test --workspace"
cargo test --workspace

echo "==> admin SPA: tsc --noEmit + vitest"
(cd apps/admin && pnpm typecheck && pnpm test)

echo "==> backup/restore tool tests"
PYTHONPATH=scripts python3 -B -m unittest scripts/test_recovery.py

echo "全部通过。"
