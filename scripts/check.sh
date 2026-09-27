#!/usr/bin/env bash
# 本地提交前检查：依赖边界 + 格式 + clippy + 测试 + 后台 SPA + 备份恢复。
# 与 CI（.github/workflows/ci.yml 的 check 与 web 两个 job）保持一致。
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> Cargo dependency boundaries"
python3 -B scripts/check_dependencies.py
PYTHONPATH=scripts python3 -B -m unittest scripts/test_check_dependencies.py

echo "==> cargo fmt --check"
cargo fmt --all --check

echo "==> cargo clippy -D warnings"
cargo clippy --workspace --all-targets -- -D warnings

echo "==> cargo test --workspace"
cargo test --workspace

echo "==> admin SPA: tsc --noEmit + vitest"
(cd apps/admin && pnpm typecheck && pnpm test)

echo "==> backup/restore and media cleanup tool tests"
PYTHONPATH=scripts python3 -B -m unittest scripts/test_recovery.py scripts/test_media_cleanup.py
if [[ "${BLOG_RECOVERY_TEST:-}" == "1" ]]; then
  cargo build -p server --bin blog
  PYTHONPATH=scripts python3 -B -m unittest scripts/test_recovery_postgres.py
fi

echo "全部通过。"
