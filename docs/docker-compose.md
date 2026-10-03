# Docker Compose 部署

单机常驻服务为 `blog` 和 `db`；备份恢复和媒体清理使用按需启动的 `ops` 服务，保留期维护使用独立的 `maintenance` 服务。主机只需 Docker Engine/Desktop 与 Docker Compose 2.24 或更新版本；构建及备份所需工具均在镜像内。运行镜像包含 release 二进制、后台生产资源、Default 主题和 PostgreSQL 迁移，全部资源使用容器内绝对路径。Paper 通过[第三方主题包](../theme-packages/README.md#paper)上传安装。

## 从源码构建并首次安装

在仓库根目录操作：

```sh
sh scripts/compose-init.sh
```

初始化脚本复用已有 `.env`，不存在时从 `.env.example` 创建；只为缺失或空的 `BLOG_POSTGRES_PASSWORD` / `BLOG_OWNER_PASSWORD` 生成随机密码，并自动设置文件为仅所有者可读写。重复运行不会更换已有密码，也不会覆盖其他配置。`600` 用于保护文件内的密码，不是 Docker 的运行要求，用户无需手动执行 chmod。

`BLOG_POSTGRES_PASSWORD` 仅用于 PostgreSQL 集群管理。默认部署只使用一个博客专用账号 `blog_owner`，其密码为 `BLOG_OWNER_PASSWORD`；它拥有本站数据库，负责安装、启动迁移、日常读写与维护，但不是超级用户，也不能创建其他数据库或角色。首次安装时不要设置 `DATABASE_URL`，否则会按已有部署启动。

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

`container` workflow 构建匹配的应用与 ops 镜像，再验证安装、SMTP 邀请和密码找回、修改草稿与历史发布、主题升级回滚、媒体/页面持久化、备份、加密仓库存取、失败恢复和独立项目恢复。每次运行保存 `compose-verification-<commit>` 报告，记录镜像 ID、编译提交、脚本指纹和各阶段结果。`v*` 标签或手动触发另产出 `blog-compose-linux-amd64-<commit>` artifact，包含应用、ops 和 PostgreSQL 镜像归档、部署脚本、定时任务单元、`IMAGE` / `OPS_IMAGE` 和 `SHA256SUMS`。迁移与恢复工具随匹配镜像交付，宿主机无需再保留重复副本。当前不自动推送镜像仓库，也不创建 GitHub Release；发布 artifact 保留 90 天，正式发布应将它与对应备份保存到长期存储。

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
release_revision=$(git rev-parse HEAD)
release_source=$(mktemp -d)
git archive "$release_revision" | tar -x -C "$release_source"
docker build --build-arg VCS_REF="$release_revision" -t blog:local "$release_source"
docker build --target ops --build-arg VCS_REF="$release_revision" -t blog-ops:local "$release_source"
docker pull "$(sed -n 's/^    image: \(postgres:.*\)$/\1/p' compose.yaml)"
BLOG_EXPECT_REVISION="$release_revision" python3 -B "$release_source/scripts/test_compose.py" \
  --image blog:local --ops-image blog-ops:local --report compose-verification.json
rm -rf "$release_source"
```

验证脚本将镜像标签解析为不可变 ID，使用随机 Compose 项目、端口、密码和独立卷，不读取部署目录的 `.env`，完成后只删除它创建的容器与卷。SMTP 收件器只监听测试应用容器的回环地址，不转发邮件；一次性链接通过私有管道读取，收件容器禁用日志。备份保留 SMTP 配置，恢复核验明确验证隔离期间关闭邮件；重新开放后核验修改草稿、历史、主题配置及上一版本，并发布恢复的草稿。加密仓库测试使用临时本地 restic 仓库；真实 SMTP TLS/认证与外部邮箱送达、S3 权限、网络、容量及生产 RPO/RTO 仍需在部署环境验收。

另可用两个已加载的本地镜像验证真实版本升级：

```sh
python3 -B scripts/test_release_upgrade.py --from-image blog:previous --image blog:local \
  --report upgrade-verification.json
```

此测试要求旧镜像尚未应用 `0009`、`0010`。先在旧版创建已发布文章、页面、草稿和媒体，再停止旧进程并用候选镜像迁移原测试卷；验证旧会话、站点身份和内容保留，以及旧文章/页面首次编辑、历史记录、发布与恢复。它不会升级任何现有部署。

## 持久化与生命周期

| Compose 卷 | 容器路径 | 内容 |
|---|---|---|
| `postgres-data` | `/var/lib/postgresql` | PostgreSQL 18 数据；实际 PGDATA 位于版本子目录 |
| `blog-config` | `/var/lib/blog/config` | TOML 和未完成安装的临时恢复日志 |
| `blog-media` | `/var/lib/blog/media` | 媒体原件与上传暂存 |
| `blog-themes` | `/opt/blog/themes` | 内置及后台安装的主题 |

默认项目名为 `blog`。保持同一项目名才能复用原卷；改用 `-p` 时，后续命令也必须使用同一名称。普通 `docker compose down` 保留卷，`down --volumes` 会删除这些持久数据。不要通过删卷解决安装或升级错误。

首次创建命名卷时，Docker 复制镜像目录的 UID/GID 和权限，博客以 `10001:10001` 写入配置、媒体与主题。若改成宿主机 bind mount，须提前创建目录并授予该 UID 写权限；配置目录应为 700。主题卷首次创建时复制镜像内的主题；已有卷不会随镜像替换自动更新内置主题，升级主题需显式维护卷内容。备份应包含实际使用的主题卷。根文件系统只读，临时文件使用 `/tmp` 的 tmpfs。

`blog` 启动前，一次性 `theme-volume-init` 服务将主题卷根目录设为 `10001:10001`。这兼容早期归属 root 的主题卷，允许新版创建主题管理锁文件；只调整卷根目录所有权，不递归修改或替换主题文件。该初始化进程使用 root 和单项 `CHOWN` 能力，无网络、配置或媒体卷访问，执行完即退出；HTTP 服务保持非 root。

PostgreSQL 初始化脚本只对新卷执行；修改 `.env` 的密码不会修改已有数据库角色密码。已有密码需用 `psql` 的 `\password` 修改，并同步实际连接配置。[官方镜像说明](https://hub.docker.com/_/postgres)

## 运行配置、账号与公网访问

统一在根目录 `.env` 中配置。Compose 自动读取它，用于服务配置插值，并只将 `blog.environment` 明确列出的应用变量传入博客；集群管理员密码和独立维护连接覆盖项不传入 HTTP 服务。Rust 程序自身仍不直接加载 `.env`，应用配置默认来自配置卷中的 TOML。

运行连接 `DATABASE_URL`、连接池 `BLOG_DB_*`、邮件 `BLOG_SMTP_*`、站点地址、可信代理、Cookie、恢复模式、`RUST_LOG`、`IDP_SECRET` 和 `GH_SECRET` 已列入传递清单。需要覆盖时直接编辑 `.env` 并重新创建 `blog` 容器。其他 OAuth secret_ref 名称需要同时加入 `compose.yaml` 的 `blog.environment`。`BLOG_THEME_DIR` 在 Compose 中须为容器内路径，恢复脚本用它选择恢复的主题。不要设置空的 `DATABASE_URL`；未配置时应完全省略该项。

安装完成后默认继续使用保存的 `blog_owner` 连接，无需再创建运行或维护账号。服务启动时自动执行未应用的迁移，失败则退出，不接受 HTTP 请求。维护容器以只读方式挂载同一配置卷，复用 `DATABASE_URL` 或安装保存的 `database.url`。

独立的 `blog_app` / `blog_maintenance` 是可选权限加固方式，见下文。默认账号拥有本站结构，因此审计记录的追加约束由应用代码保证；数据库权限不会阻止它直接修改或删除审计。

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

当前迁移链截至 `0010_content_revisions.sql`，涵盖后台查询索引、读者注册、评论审核、内容搜索、持久任务、主题和插件、账号邮件链接、内容修改草稿及修订历史。升级时须停止旧版 server、外部调度和会修改数据库的旧版 CLI，不混跑不同版本的写入进程。默认 `blog_owner` 模式由新服务在监听 HTTP 前自动迁移；受限账号模式按下文先停写迁移、重新授权，再启动。HTTP 关闭默认总预算 25 秒，须小于此文件配置的 30 秒容器退出宽限期。

升级前先完成匹配版本的备份恢复演练并保存旧镜像标识。停止外部 CLI 与定时维护任务后，源码部署执行下列命令；发布包部署跳过 `build`，先加载匹配镜像并更新 `.env` 的镜像标识：

```sh
docker compose build blog ops
docker compose stop blog
docker compose up -d --no-build --wait blog
```

默认模式无需单独运行 `migrate` 或授权脚本。迁移失败会使新服务无法就绪，查看 `docker compose logs blog` 处理，不要直接开放流量。已有 `0001_initial_schema.sql` 保留原始校验和，新版本只追加迁移；跨版本恢复先用备份匹配的版本恢复并核验，再升级，详见[迁移演进](schema-migrations.md)。旧数据库基线仍不支持原地转换；切回旧镜像也不等于数据库回滚。

安装后即可运行保留期维护，无需额外账号配置：

```sh
sh scripts/compose-backup.sh maintenance
```

此入口使用独立维护容器，读取站点配置或可选维护连接，和备份共用操作锁；正常 HTTP 服务无需停止。每日调度使用 `ops/blog-maintenance.service` / `.timer`，按实际部署修改 `/opt/blog` 和运行用户后安装并启用 timer。无需额外的环境文件。正式媒体物理清理使用同一脚本的 `media-plan` / `media-apply`，复核与重试规则见[运维说明](operations-and-recovery.md#正式媒体物理清理)。

也可在「任务管理」手动执行清理或启用默认关闭的后台每日周期。默认 `blog_owner` 可直接执行；分离 app 账号模式下，`.env` 的维护连接仍仅传给 ops，不自动赋予 HTTP 维护能力。需要后台清理时，由部署方在受保护的站点 TOML 中显式设置 `[maintenance] database_url`，连接同一数据库且只授予规定的维护权限。启用后台周期后应停用重复的外部 timer；运行账号不增加 audit_logs DELETE，能力失败不会阻止网站登录。完整计划与历史见[后台任务管理](operations-and-recovery.md#后台任务管理)。

## 可选：分离数据库权限

需要数据库强制限制日常服务修改结构、删除审计时，按[数据库账号说明](operations-and-recovery.md#数据库账号与保留期)创建不拥有对象、不继承其他角色的 `blog_app` 和 `blog_maintenance` LOGIN 角色。使用 `docker compose exec db psql -U postgres -d blog`，通过 `CREATE ROLE ... LOGIN` 和 `\password` 设置密码，再授权：

```sh
docker compose exec -T db \
  psql -U postgres -d blog -v app_role=blog_app -v maintenance_role=blog_maintenance \
  < scripts/database-roles.sql
```

将配置卷中 `config.toml` 的 `database.url` 改为 `blog_app` 连接，移除原结构所有者凭据；若 `.env` 配置了 `DATABASE_URL`，也须同步更新。仅用环境变量覆盖连接不会移除 TOML 中仍可读取的旧凭据。在 `.env` 设置 `BLOG_MAINTENANCE_DATABASE_URL=postgres://blog_maintenance:<编码后的密码>@db:5432/blog`，重新创建 `blog`。独立维护连接优先使用，显式配置错误时不会回退；它不注入 HTTP 服务。

受限运行账号启动时只校验迁移，无法自动升级。此模式升级时先停止全部写入，将结构所有者连接安全注入终端的 `DATABASE_URL`，执行 `docker compose run --rm --no-deps -e DATABASE_URL blog migrate`；再重跑新版本授权脚本，撤销终端中的所有者连接覆盖，最后启动使用 `blog_app` 的服务。迁移与授权凭据由部署侧保管，不保留在服务配置卷中。

命名卷是持久存储。Compose 的完整备份、隔离恢复、定时执行与加密异地副本见[Compose 备份恢复](compose-backup.md)。原有宿主机 `recovery.py` 入口继续支持非 Compose 部署；Compose 用户无需安装 Python、导出命名卷或手工设置文件权限。

## 连接容量与反代来源

连接池与超时可直接在现有 `.env` 配置 `BLOG_DB_*`，字段与边界见[配置参考](configuration.md#连接池查询超时与数据库-tls)。修改后执行 `docker compose up -d blog` 重建服务配置；备份恢复保留这些参数。

反代部署必须将实际连接应用的代理 IP 配入 `BLOG_TRUSTED_PROXIES`；只接受精确 IP，容器重建后代理地址变化需同步更新。代理正确传递 `X-Forwarded-For` 后，登录和自助改密按真实客户端分桶。不要把所有客户端地址都设成可信代理，也不要仅靠增加限流阈值解决共享代理桶。当前认证仍按单实例部署。
