#!/usr/bin/env bash
# 本地开发数据库：PostgreSQL 18（Docker）。
# 注意：PG18 官方镜像的数据卷挂载点是 /var/lib/postgresql（不再是 .../data）。
set -euo pipefail

CONTAINER=blog-postgres
VOLUME=blog-pgdata
PORT="${BLOG_PG_PORT:-5432}"
WAIT_SECONDS="${BLOG_PG_WAIT_SECONDS:-60}"
if [[ ! "$WAIT_SECONDS" =~ ^[1-9][0-9]{0,3}$ ]] || (( WAIT_SECONDS > 3600 )); then
  echo 'BLOG_PG_WAIT_SECONDS 必须为 1–3600 的整数。' >&2
  exit 2
fi
command -v python3 >/dev/null || { echo '就绪超时检查需要 Python 3。' >&2; exit 1; }

if docker container inspect "$CONTAINER" >/dev/null 2>&1; then
  echo "容器 $CONTAINER 已存在，启动它。"
  docker start "$CONTAINER"
else
  docker run -d --name "$CONTAINER" \
    -e POSTGRES_USER=blog -e POSTGRES_PASSWORD=blog -e POSTGRES_DB=blog \
    -p "127.0.0.1:${PORT}:5432" \
    -v "$VOLUME:/var/lib/postgresql" \
    postgres:18-alpine@sha256:77f585114c32fbca283dc835b0596f4e52b51b4c6662d7810b2f4084f60a1873
fi

python3 - "$CONTAINER" "$WAIT_SECONDS" <<'PY'
import subprocess
import sys
import time

container, seconds = sys.argv[1], int(sys.argv[2])
deadline = time.monotonic() + seconds
failure = f"PostgreSQL 在 {seconds} 秒内未就绪。"
while time.monotonic() < deadline:
    try:
        state = subprocess.run(["docker", "inspect", "--format", "{{.State.Status}}", container],
                               capture_output=True, text=True, timeout=min(3, deadline - time.monotonic()))
        if state.returncode != 0 or state.stdout.strip() != "running":
            failure = "数据库容器已退出或无法读取状态。"
            break
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        ready = subprocess.run(["docker", "exec", container, "pg_isready", "-t", "2", "-U", "blog", "-d", "blog"],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=min(3, remaining))
        if ready.returncode == 0:
            sys.exit(0)
    except subprocess.TimeoutExpired:
        pass
    time.sleep(max(0, min(0.5, deadline - time.monotonic())))

print(failure, file=sys.stderr)
# Diagnostics are bounded too; a stalled Docker daemon must not hang this path.
for args in (["inspect", "--format", "{{.State.Status}} (exit={{.State.ExitCode}})", container],
             ["logs", "--tail", "30", container]):
    try:
        result = subprocess.run(["docker", *args], capture_output=True, text=True, timeout=3)
        print(result.stdout + result.stderr, file=sys.stderr, end="")
    except subprocess.TimeoutExpired:
        print("Docker 诊断请求超时。", file=sys.stderr)
sys.exit(1)
PY
echo "PostgreSQL 18 就绪：postgres://blog:blog@127.0.0.1:${PORT}/blog"
