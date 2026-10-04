# Docker Compose 部署

博客镜像包含网站、后台、主题、迁移与备份恢复工具。用户通过一份 Compose 启动镜像，在安装向导验证数据库并保存配置。1Panel 的完整界面操作见 [1Panel 部署](1panel.md)。

## 选择数据库方式

- [compose.yaml](../compose.yaml)：只启动博客，连接已有 PostgreSQL。
- [compose.postgres.yaml](../compose.postgres.yaml)：完整的博客 + PostgreSQL 示例。初次数据库初始化使用文件内嵌的配置，创建非超级用户 `blog_owner`，无需额外脚本文件。

选择一个文件，在 1Panel 中粘贴，或保存为部署目录的 `compose.yaml`。示例已固定经过验收的 `ghcr.io/junerat/blog` 镜像，无需自行构建；填写域名与私有安装码，带数据库示例还需设置两个数据库密码。Compose 2.24 或更新版本可使用内嵌数据库初始化配置。镜像采用 `pull_policy: missing`，启动时会自动拉取本地缺少的版本。[Docker 镜像拉取规则](https://docs.docker.com/reference/compose-file/services/#pull_policy)、[内嵌配置支持](https://docs.docker.com/reference/compose-file/configs/)

```sh
docker compose up -d
```

访问 `/install`，验证安装码、填写并验证数据库连接，随后创建管理员并执行安装。连接检查不保存配置或创建表，正式安装再次检查；失败不会清理已有数据。数据库连接写入配置卷中的 `config.toml`，重启时直接读取。带数据库示例的连接为 `postgres://blog_owner:编码后的密码@db:5432/blog`；已有数据库须从博客容器可达，不能将博客容器自己的 `localhost` 当成外部数据库。

## 从 GHCR 拉取镜像并部署到 1Panel

直接部署本项目时，使用上述 Compose 内的固定镜像地址，或从 [GitHub Releases](https://github.com/JuneRat/blog/releases) 下载指定版本的 Compose 附件。发布标签采用 `vMAJOR.MINOR.PATCH`（例如 `v0.1.0`），镜像同时提供版本标签与提交标签：

```text
ghcr.io/<github-owner>/<repository>:0.1.0
ghcr.io/<github-owner>/<repository>:sha-<完整提交 SHA>
```

版本标签与 SHA 标签指向同一镜像，不发布浮动 `latest` 标签。正式发布前，同步 `crates/server/Cargo.toml` 与 `apps/admin/package.json` 的应用版本，更新 Cargo 锁文件中的应用版本，并在 [CHANGELOG.md](../CHANGELOG.md) 增加 `## 版本号` 及变更说明。依赖升级不是发布的前提，依赖和工具链可继续沿用已验证版本。提交推送后，以 `0.1.0` 为例：

```bash
git tag -a v0.1.0 -m "Release v0.1.0"
git push origin v0.1.0
```

`container` 工作流先校验标签、前后端版本和更新记录，再运行完整 CI（后端、前端、安全审计、全链路验收）及容器验收。全部通过后，发布任务加载同一个已测试镜像，检查 ID、提交、来源和 Linux amd64 架构，然后推送 GHCR，并通过 digest 拉取核对。已存在的版本标签只有在摘要相同时才允许复用，不会覆盖为其他镜像。

最后一个 job 自动创建 GitHub Release：正文包含当前版本的更新记录、安装说明和 GitHub 生成的变更列表；附件包含 `compose.yaml`、`compose.postgres.yaml`、`published-image.json` 与 `SHA256SUMS`。两份 Compose 的 `blog.image` 已填入该版本标签和本次验收镜像的固定 digest，无需手工替换。它们是无凭据的独立模板，不读取或打包用户配置，也不要求下载 Actions artifact。

Release 先保持草稿，附件全部上传并下载核对后才公开。上传中断后可在 Actions 重跑失败的 job；重试会复用一致的附件，已发布的 Release 不被覆盖。镜像已经发布时应优先重跑失败的 job：重新构建可能因构建源变化产生不同摘要，同一版本号会拒绝覆盖。`v0.1.0-rc.1` 等预发布标签也受相同版本校验约束，并标记为 GitHub prerelease。

普通分支推送与 PR 只构建验收。手工 **Actions → container → Run workflow** 选择分支时，完整检查通过后仅发布 SHA 镜像，结果页给出固定镜像地址，不创建 Release；选择版本标签时执行对应的完整发布流程。GitHub Release 由工作流创建，无需预先手工发布。

发布只使用该仓库的 `GITHUB_TOKEN`：仅 `publish` job 有 `packages: write`，仅最后的 `release` job 有 `contents: write`；PR 和普通分支构建保持只读。本项目镜像已公开并验证匿名拉取。自行发布的私有镜像需在 1Panel 配置仓库凭据，或由包所有者在 **Package settings → Change visibility → Public** 设置公开；发布流程不自动改变镜像可见性，源码公开也不代表镜像公开。可从[账号的 Packages 页面](https://github.com/JuneRat?tab=packages)进入博客镜像设置。[GitHub 包发布权限](https://docs.github.com/en/packages/managing-github-packages-using-github-actions-workflows/publishing-and-installing-a-package-with-github-actions)、[GHCR 访问说明](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry)

流水线当前发布 Linux amd64。ARM 主机可以从源码原生构建；尚未提供经过 CI 验收的 ARM 发布镜像。使用自己仓库发布的镜像时，将两个示例中的 `blog.image` 替换为对应发布结果。

## 从源码构建并首次安装

```sh
docker build --build-arg VCS_REF="$(git rev-parse HEAD)" -t blog:local .
```

选择上述任一部署示例，把 `blog.image` 改为 `blog:local`，填好参数后启动。构建工具都在 Docker 中，服务器无需安装 Rust、Node.js 或 PostgreSQL 客户端。

[compose.legacy.yaml](../compose.legacy.yaml)保留旧的源码开发与独立恢复编排，包含 `ops`、外部 `maintenance` 和旧主题卷权限初始化。需要这些高级工具时，使用仓库源码：

```sh
sh scripts/compose-init.sh
docker compose -f compose.legacy.yaml up -d --build
```

该兼容入口继续使用 `.env`，脚本会优先选用 `compose.legacy.yaml`；旧独立恢复目录只有 `compose.yaml` 时继续使用原文件。日常浏览器备份不需要此入口。

## 镜像交付与验证

发布工作流依次验证 STARTTLS 和隐式 TLS 下的安装、邮件邀请/找回、发布、主题、完整备份与独立恢复，再使用新版 Compose 验证浏览器备份、原地恢复、中断回滚及外置数据库恢复。只有通过验证的同一个镜像能够发布。构建与发布任务之间保留一天的内部镜像传递 artifact，用户无需下载；验收报告独立保留。

本地验证：

```sh
python3 -B scripts/test_browser_compose.py --image blog:local --report browser.json
# 安装前端依赖及 Playwright Chromium 后，可加 --browser 检查实际页面。
python3 -B scripts/test_compose.py --image blog:local --ops-image blog:local --smtp-security starttls --report compose.json
```

验收使用随机项目、密码、端口与独立卷，结束后只清理自己的资源。S3 与邮件使用本地协议服务，真实存储供应商、互联网邮件、域名证书和服务器容量仍需在目标环境验证。本次镜像的提交、digest 与云端结果见 [GHCR 交付验收](validation/github-release-2026-10-04.md)；历史记录见[网页恢复验收](validation/browser-recovery-2026-10-04.md)与[安装部署验收](validation/simple-deployment-2026-10-04.md)。

基础镜像与 PostgreSQL 示例固定 digest。更新 PostgreSQL 时同步 `compose.postgres.yaml`、`compose.legacy.yaml`、CI 服务及 `scripts/dev-db.sh`，并运行验收。基础镜像固定不等于整个构建逐字节可复现，APT 软件源仍会更新。

## 持久化与生命周期

| 卷 | 容器路径 | 内容 |
|---|---|---|
| `blog-config` | `/var/lib/blog/config` | TOML、安装日志、备份策略、恢复任务和加密备份 |
| `blog-media` | `/var/lib/blog/media` | 媒体文件 |
| `blog-themes` | `/opt/blog/themes` | 内置及安装的主题 |
| `postgres-data`（可选） | `/var/lib/postgresql` | PostgreSQL 数据 |

命名卷第一次创建时继承镜像目录内容与权限，博客以 `10001:10001` 运行。自定义宿主机目录时须提前授予该 UID 写权限，配置目录应为 `700`。根文件系统只读，临时文件使用默认 2 GiB 的 tmpfs；大型备份需调整 Compose 的 `tmpfs` 大小并规划内存。

保持编排名和卷名，更新镜像即可继续使用安装保存的配置。普通 `down` 保留卷，`down --volumes` 会删除数据。数据库初始化只对新卷生效；修改 Compose 中的密码不会修改已有数据库账号。

## 健康检查与日志

安装完成前 `/readyz` 返回 503，容器可能显示 unhealthy，但安装页可访问；首次启动不要使用等待就绪来阻塞人工安装。`/livez` 继续响应。安装后以数据库状态判定就绪，数据库故障或恢复维护期间保留独立 `/recovery`。

默认向 stderr 输出 JSON 日志，Compose 自动轮转。安装码由编排设置时无需查日志；未设置则由程序生成并在启动日志显示。`/version` 提供版本和编译提交。可按[可观测性](observability.md)显式配置内网指标端口。

## 升级、维护与备份边界

在后台生成并下载备份，记录原镜像地址，然后修改原 Compose 的 `blog.image`，拉取并重新创建博客容器。保留原编排名及全部数据卷。1Panel 可在界面完成；终端操作为：

```sh
docker compose pull blog
docker compose up -d --no-deps blog
```

使用数据库结构所有者连接时，启动自动执行支持的迁移。受限运行账号须由部署方先迁移及授权，见[数据库账号](operations-and-recovery.md#数据库账号与保留期)。新旧版本不能同时写同一数据库。镜像回退不撤销迁移；恢复需选择兼容备份格式和结构的镜像。

日常保留期维护使用后台“任务管理”，备份和原地恢复使用[后台入口](browser-backup.md)。脚本化运维、分离数据库账号和旧独立恢复流程见 [Compose 备份恢复](compose-backup.md)。

## 连接容量与反代来源

其他部署参数可写入配置卷的 TOML，或显式加入 Compose 的 `blog.environment`；环境变量优先，修改后重建服务。连接池、SMTP 和 OAuth 配置见[配置参考](configuration.md)。空的 `DATABASE_URL` 不表示未设置，新安装应完全省略它。

默认端口只绑定宿主机 `127.0.0.1:8080`，由 1Panel HTTPS 反向代理访问。代理为独立容器时需连通相应内部网络。将实际可信代理的精确 IP 写入 `BLOG_TRUSTED_PROXIES`，正确转发来源 IP，不能信任所有来源。公开部署通过 `BLOG_PUBLIC_BASE_URL` 明确域名和 HTTPS，安装时也会保存该地址；当前认证与恢复按单实例部署。
