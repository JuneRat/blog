#!/usr/bin/env bash
# 本地开发数据库：PostgreSQL 18（Docker）。
# 注意：PG18 官方镜像的数据卷挂载点是 /var/lib/postgresql（不再是 .../data）。
set -euo pipefail

CONTAINER=blog-postgres
VOLUME=blog-pgdata
PORT="${BLOG_PG_PORT:-5432}"

if docker container inspect "$CONTAINER" >/dev/null 2>&1; then
  echo "容器 $CONTAINER 已存在，启动它。"
  docker start "$CONTAINER"
else
  docker run -d --name "$CONTAINER" \
    -e POSTGRES_USER=blog -e POSTGRES_PASSWORD=blog -e POSTGRES_DB=blog \
    -p "127.0.0.1:${PORT}:5432" \
    -v "$VOLUME:/var/lib/postgresql" \
    postgres:18-alpine
fi

until docker exec "$CONTAINER" pg_isready -U blog -d blog >/dev/null 2>&1; do
  sleep 0.5
done
echo "PostgreSQL 18 就绪：postgres://blog:blog@127.0.0.1:${PORT}/blog"
