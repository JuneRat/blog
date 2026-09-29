# Docker Compose 部署

单机常驻服务为 `blog` 和 `db`；备份恢复和媒体清理使用按需启动的 `ops` 服务，保留期维护使用独立的 `maintenance` 服务。主机只需 Docker Engine/Desktop 与 Docker Compose 2.24 或更新版本；构建及备份所需工具均在镜像内。运行镜像包含 release 二进制、后台生产资源、两套主题和 PostgreSQL 迁移，全部资源使用容器内绝对路径。

## 从源码构建并首次安装

在仓库根目录操作：

```sh
sh scripts/compose-init.sh
```

初始化脚本复用已有 `.env`，不存在时从 `.env.example` 创建；只为缺失或空的 `BLOG_POSTGRES_PASSWORD` / `BLOG_OWNER_PASSWORD` 生成随机密码，并自动设置文件为仅所有者可读写。重复运行不会更换已有密码，也不会覆盖其他配置。`600` 用于保护文件内的密码，不是 Docker 的运行要求，用户无需手动执行 chmod。

`BLOG_POSTGRES_PASSWORD` 仅用于 PostgreSQL 集群管理，`BLOG_OWNER_PASSWORD` 对应非超级用户 `blog_owner`，负责站点结构初始化。首次安装时不要设置 `DATABASE_URL`，否则会按已有部署启动。

```sh
docker compose up -d --build
docker compose logs blog
```

访问 <http://127.0.0.1:8080/install>，填写日志中的安装码、站点地址、管理员用户名/密码，以及：

```text
postgres://blog_owner:<BLOG_OWNER_PASSWORD 的值>@db:5432/blog
```

数据库主机是 Compose 服务名 `db`，不是 `localhost`。数据库密码如果包含 URL 特殊字符，须先进行百分号编码。远程服务器可通过 SSH 端口转发访问默认只绑定 loopback 的安装页；生产站点地址应填写最终的 HTTPS 域名。

安装完成后访问 `/admin/` 登录。服务在同一进程中切换到正常模式，部署配置保存在配置卷内，权限为 600。安装恢复日志也在该卷内，成功后自动清理。

## 镜像交付与验证

`container` workflow 构建匹配的应用与 ops 镜像，再验证安装、媒体/页面持久化、备份、加密仓库存取、失败恢复和独立项目恢复。`v*` 标签或手动触发产出 `blog-compose-linux-amd64-<commit>` artifact，包含应用、ops 和 PostgreSQL 镜像归档、部署脚本、定时任务单元、`IMAGE` / `OPS_IMAGE` 和 `SHA256SUMS`。迁移与恢复工具随匹配镜像交付，宿主机无需再保留重复副本。当前不自动推送镜像仓库，也不创建 GitHub Release；artifact 保留 90 天，正式发布应将它与对应备份保存到长期存储。

下载并解压 artifact 后，在解压目录执行：

```sh
sha256sum -c SHA256SUMS
docker load -i blog-linux-amd64.tar.gz
sh scripts/compose-init.sh
```

在 `.env` 中将 `BLOG_IMAGE` / `BLOG_OPS_IMAGE` 分别设置为 `IMAGE` / `OPS_IMAGE` 文件内的值；数据库密码已由初始化脚本生成。随后执行 `docker compose up -d --no-build --pull never` 并按上述步骤安装。部署归档不包含源码或 Dockerfile，不能使用 `--build`。CI 归档面向 Linux amd64；ARM 主机可从源码原生构建。

源码中的基础镜像和 PostgreSQL 部署镜像均使用 `tag@sha256:...`，Dockerfile frontend 也固定 digest。发布包记录数据库的 `DATABASE_IMAGE` 和 `DATABASE_IMAGE_ID`，包内 Compose 直接引用已导入的数据库 image ID，避免旧版 Docker save/load 丢失 RepoDigests 后要求联网拉取。`SHA256SUMS` 用于核对交付文件；基础镜像固定不等于整个构建逐字节可复现，APT 软件源仍会更新。

Dependabot 每周检查 Docker 基础镜像更新并提出 PR；合入前运行 Compose 验收。更新 PostgreSQL Alpine 镜像时须同步 `compose.yaml`、CI 服务和 `scripts/dev-db.sh` 的引用。镜像 digest 固定后，安全修复通过显式更新进入发布，不能只靠重复构建。

本地验证同一镜像：

```sh
docker build -t blog:local .
docker build --target ops -t blog-ops:local .
docker pull "$(sed -n 's/^    image: \(postgres:.*\)$/\1/p' compose.yaml)"
python3 -B scripts/test_compose.py --image blog:local --ops-image blog-ops:local
```

验证脚本使用随机 Compose 项目、端口、密码和独立卷，不读取部署目录的 `.env`，完成后只删除它创建的容器与卷。除安装、资源、权限与持久化外，还执行文末备份恢复流程。加密仓库测试使用临时本地 restic 仓库；真实 S3 权限、网络、容量及生产 RPO/RTO 仍需在部署环境验收。

## 持久化与生命周期

| Compose 卷 | 容器路径 | 内容 |
|---|---|---|
| `postgres-data` | `/var/lib/postgresql` | PostgreSQL 18 数据；实际 PGDATA 位于版本子目录 |
| `blog-config` | `/var/lib/blog/config` | TOML 和未完成安装的临时恢复日志 |
| `blog-media` | `/var/lib/blog/media` | 媒体原件与上传暂存 |

默认项目名为 `blog`。保持同一项目名才能复用原卷；改用 `-p` 时，后续命令也必须使用同一名称。普通 `docker compose down` 保留卷，`down --volumes` 会删除这些持久数据。不要通过删卷解决安装或升级错误。

首次创建命名卷时，Docker 复制镜像目录的 UID/GID 和权限，博客以 `10001:10001` 写入配置与媒体。若改成宿主机 bind mount，须提前创建目录并授予该 UID 写权限；配置目录应为 700。根文件系统只读，临时文件使用 `/tmp` 的 tmpfs。

PostgreSQL 初始化脚本只对新卷执行；修改 `.env` 的密码不会修改已有数据库角色密码。已有密码需用 `psql` 的 `\password` 修改，并同步实际连接配置。[官方镜像说明](https://hub.docker.com/_/postgres)

## 运行配置、账号与公网访问

统一在根目录 `.env` 中配置。Compose 自动读取它，用于服务配置插值，并只将 `blog.environment` 明确列出的应用变量传入博客；数据库管理员密码和维护凭据不传入 HTTP 服务。Rust 程序自身仍不直接加载 `.env`，应用配置默认来自配置卷中的 TOML。

运行连接 `DATABASE_URL`、连接池 `BLOG_DB_*`、站点地址、可信代理、Cookie、恢复模式、`RUST_LOG`、`IDP_SECRET` 和 `GH_SECRET` 已列入传递清单。需要覆盖时直接编辑 `.env` 并重新创建 `blog` 容器。其他 OAuth secret_ref 名称需要同时加入 `compose.yaml` 的 `blog.environment`。`BLOG_THEME_DIR` 在 Compose 中须为容器内路径，恢复脚本用它选择恢复的主题。不要设置空的 `DATABASE_URL`；未配置时应完全省略该项。

公开部署前，应按[数据库账号说明](operations-and-recovery.md#数据库账号与保留期)创建独立的 `blog_app` 和 `blog_maintenance` 登录角色。可通过下列交互入口使用 `CREATE ROLE ... LOGIN` 和 `\password` 设置密码：

```sh
docker compose exec db psql -U postgres -d blog
docker compose exec -T db \
  psql -U postgres -d blog -v app_role=blog_app -v maintenance_role=blog_maintenance \
  < scripts/database-roles.sql
```

随后在 `.env` 配置 `DATABASE_URL` 使用 `blog_app` 并执行：

```sh
docker compose up -d --no-build --force-recreate blog
```

`blog_owner` 的安装连接仍可保留在私有 TOML 中作为配置来源，运行时由环境覆盖；不要向 HTTP 服务注入集群管理员或独立维护账号的环境凭据。

默认仅暴露 `127.0.0.1:8080`，PostgreSQL 没有宿主机端口。公网部署在前面接 HTTPS 反向代理；若代理运行于其他容器，需通过受控网络连接博客，或按实际网络调整 `BLOG_HTTP_HOST`。只将确定的代理地址加入 `BLOG_TRUSTED_PROXIES`，不要信任所有来源。修改域名时在 `.env` 设置 `BLOG_PUBLIC_BASE_URL`，重建容器；不要通过关闭 Secure Cookie 解决 HTTPS 配置问题。

## 健康检查与日志

数据库通过 `pg_isready` 后，Compose 才启动博客；`depends_on` 使用 `service_healthy`。[Compose 启动顺序](https://docs.docker.com/compose/how-tos/startup-order/)

博客镜像用 `/readyz` 做 readiness 检查，`/healthz` 保留相同语义。安装期间返回 503，因此首次运行可能显示 `unhealthy`；Docker 的重启策略不会仅因 unhealthy 重启容器，站点仍可安装。第一次不要使用 `up --wait`，也不要接入按此探针强制重启的工具。安装完成后可用 `up -d --wait` 验证就绪；数据库故障最多等待 2 秒后返回 503，独立 `/livez` 仍返回 200。

应用默认向 stderr 写 JSON 日志，保留请求编号、路由、状态与耗时；CLI 结果仍写 stdout。Docker `local` driver 轮转，每个容器最多配置 5 个 10 MB 日志文件。安装码也在日志中，日志访问需受控。可在现有 `.env` 设置 `BLOG_LOG_FORMAT=text` 切换文本。

`/version` 提供版本与编译提交，指标通过独立的 `http://127.0.0.1:9090/metrics` 访问。指标宿主机端口只绑定 loopback，可在现有 `.env` 设置 `BLOG_METRICS_PORT` 换端口；不要加入公网反向代理。详细字段、Prometheus 抓取及查询示例见[可观测性](observability.md)。

```sh
docker compose ps
docker compose logs --tail 100 -f blog
```

## 升级、维护与备份边界

当前版本追加 `0002_admin_query_indexes.sql`，并将分类树 advisory 锁与会话锁分开。升级时须停止旧版 server 和会修改数据库的旧版 CLI，再迁移并启动新版本；不要混跑这两个版本的写入进程。HTTP 关闭默认总预算 25 秒，须小于此文件配置的 30 秒容器退出宽限期。

升级前先完成匹配版本的备份恢复演练并保存旧镜像标识。构建或加载新镜像后停止全部写入；将结构管理 DSN 安全注入终端的 `DATABASE_URL`，不要将其写进命令历史：

```sh
docker compose stop blog
docker compose run --rm --no-deps -e DATABASE_URL blog migrate
```

迁移成功后重跑新版本生成的授权脚本，再启动 `blog`。已有 `0001_initial_schema.sql` 保留原始校验和，新版本只追加迁移；跨版本恢复先用备份匹配的版本恢复并核验，再升级，详见[迁移演进](schema-migrations.md)。受限运行账号只校验迁移历史，不负责升级结构。旧数据库基线仍不支持原地转换；切回旧镜像也不等于数据库回滚。

在现有 `.env` 设置 `BLOG_MAINTENANCE_DATABASE_URL=postgres://blog_maintenance:<编码后的密码>@db:5432/blog`，再运行：

```sh
sh scripts/compose-backup.sh maintenance
```

此入口使用独立维护容器，只传入维护凭据与必要运行参数，和备份共用操作锁；正常 HTTP 服务无需停止。每日调度使用 `ops/blog-maintenance.service` / `.timer`，按实际部署修改 `/opt/blog` 和运行用户后安装并启用 timer。无需额外的环境文件。正式媒体物理清理使用同一脚本的 `media-plan` / `media-apply`，复核与重试规则见[运维说明](operations-and-recovery.md#正式媒体物理清理)。

命名卷是持久存储。Compose 的完整备份、隔离恢复、定时执行与加密异地副本见[Compose 备份恢复](compose-backup.md)。原有宿主机 `recovery.py` 入口继续支持非 Compose 部署；Compose 用户无需安装 Python、导出命名卷或手工设置文件权限。

## 连接容量与反代来源

连接池与超时可直接在现有 `.env` 配置 `BLOG_DB_*`，字段与边界见[配置参考](configuration.md#连接池查询超时与数据库-tls)。修改后执行 `docker compose up -d blog` 重建服务配置；备份恢复保留这些参数。

反代部署必须将实际连接应用的代理 IP 配入 `BLOG_TRUSTED_PROXIES`；只接受精确 IP，容器重建后代理地址变化需同步更新。代理正确传递 `X-Forwarded-For` 后，登录和自助改密按真实客户端分桶。不要把所有客户端地址都设成可信代理，也不要仅靠增加限流阈值解决共享代理桶。当前认证仍按单实例部署。
