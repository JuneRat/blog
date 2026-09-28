#!/usr/bin/env bash
# 本地提交前检查：依赖边界 + 格式 + clippy + 测试 + 后台 SPA + 验收工具。
# 与 CI（.github/workflows/ci.yml）保持一致；真实数据库演练在本地按需启用。
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> Immutable migrations and generated schema artifacts"
python3 -B scripts/schema_contract.py --base-ref "${SCHEMA_BASE_REF:-$(git rev-parse HEAD)}"
PYTHONPATH=scripts python3 -B -m unittest scripts/test_schema_contract.py

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

echo "==> recovery, media cleanup and acceptance tool tests"
PYTHONPATH=scripts python3 -B -m unittest scripts/test_recovery.py scripts/test_media_cleanup.py scripts/test_acceptance.py scripts/test_deployment_config.py scripts/test_compose_init.py scripts/test_compose_recovery.py
if [[ "${BLOG_RECOVERY_TEST:-}" == "1" || "${BLOG_ACCEPTANCE_TEST:-}" == "1" ]]; then
  cargo build -p server --bin blog
fi
if [[ "${BLOG_RECOVERY_TEST:-}" == "1" ]]; then
  PYTHONPATH=scripts python3 -B -m unittest scripts/test_recovery_postgres.py
fi
if [[ "${BLOG_ACCEPTANCE_TEST:-}" == "1" ]]; then
  pnpm --dir apps/admin build
  python3 -B scripts/acceptance.py
fi

echo "全部通过。"
