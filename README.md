# blog

Rust 模块化单体博客，公开站点使用服务端渲染，管理后台使用 React、TypeScript 和 Ant Design。

目前支持文章与独立页面、标签/分类/系列、OAuth 与本地密码登录、角色权限、媒体库、站点设置、主题切换，首页分页、页面导航、后台跨页搜索与多人内容管理，以及 RSS、sitemap 和 SEO 元数据。项目仍在开发阶段，接口与数据模型可直接调整，不维护旧管理接口的兼容层。交付状态和后续范围见[路线图](docs/product-roadmap.md)。

**新数据库基线已完成适配。** [19 表基线](migrations/postgres/0001_initial_schema.sql)已替换旧迁移链，身份、持久会话、媒体、内容、目录与评论已适配；保留期维护、新库备份恢复和正式媒体显式清理已接入，已有业务写入口已补齐事务审计、可信来源 IP 和后台只读查询。首次安装至备份恢复的[全链路验收](docs/acceptance.md)已通过，本地空库安装也已验证，生产部署验收仍单独执行。旧结构需另建空库，不能原地升级。生成的 DDL 参考见 [汇总 DDL](docs/sql/postgres-core.sql)，后续变更遵循[迁移演进规则](docs/schema-migrations.md)，已实现边界见[数据库实现参考](docs/database-current.md)。

## 快速开始

使用 Docker Compose 部署时，只需 Docker，无需在主机安装 Rust 或 Node.js：先运行 `sh scripts/compose-init.sh` 初始化根目录 `.env`（自动补齐随机密码和私有权限，保留已有值），再运行 `docker compose up -d --build`。镜像包含后台、主题和迁移，数据库、安装配置与媒体分别持久化。首次安装、已有镜像部署与升级步骤见 [Docker Compose 部署](docs/docker-compose.md)。

默认使用一个非超级用户的博客专用数据库账号，启动时自动迁移，保留期维护复用安装连接；无需额外创建运行或维护账号。需要数据库强制权限隔离时，可选择分离账号部署。

Compose 备份使用 `sh scripts/compose-backup.sh backup`，首次从源码部署需先 `docker compose build ops`。它沿用 `.env`，自动编排停写、重新启动原服务、完整备份与保留清理；独立项目恢复、登录/媒体验证、定时和加密异地副本见 [Compose 备份恢复](docs/compose-backup.md)。

以下是源码开发启动方式：

准备 Rust stable、Node.js 22、pnpm（版本见 [package.json](apps/admin/package.json)）和 Docker。在仓库根目录运行：

```bash
# 启动本地 PostgreSQL 18（已有数据时，另建空库用于安装）
./scripts/dev-db.sh

# 构建后台并启动站点
(cd apps/admin && pnpm install --frozen-lockfile && pnpm build)
cargo run
```

不指定子命令时默认启动 `serve`，默认监听 `127.0.0.1:8080`。已有配置会按 `BLOG_BIND`、TOML 的 `server.bind` 覆盖地址；例如使用 3000 端口可运行 `BLOG_BIND=127.0.0.1:3000 cargo run`，也可继续显式传入 `serve --addr`。

未设置 `DATABASE_URL`、也没有本地安装配置时，首次访问会跳转到安装页。填入终端显示的安装码、PostgreSQL 空库地址、站点地址和管理员账号。程序初始化结构与权限，原子创建首个 admin，然后直接进入登录页。部署配置保存在 `config.toml`，下一次启动自动读取；临时恢复日志 `config.install-state.json` 在安装完成后自动清理，完成标记保存在数据库。配置和临时日志权限均为 600，容器部署需持久挂载配置目录。详见[首次安装](docs/installation.md)。

站点入口为[公开站点](http://127.0.0.1:8080/)和[管理后台](http://127.0.0.1:8080/admin/)。已验证身份、媒体、内容发布与回收站、多系列、预约发布和评论；保留期维护、独立数据库权限和含媒体的隔离恢复已有验证，操作见[备份与恢复](docs/operations-and-recovery.md)。

已有 CLI 初始化的数据库通过 `database.url` 或 `DATABASE_URL` 显式连接，不会触发安装向导；维护 CLI 可分步创建用户、设置密码和分配角色。TOML、环境覆盖和运行期设置见[配置参考](docs/configuration.md)。Rust 程序不会自动加载 `.env`；Compose 部署会读取它并传入明确配置的应用变量。旧结构不能原地升级；新迁移不会删除旧数据，需另建空库。

## 开发与检查

[开发指南](docs/development.md)包含 CLI 写作、OAuth 配置、后台 SPA 联调和测试库说明。已启动本地数据库并安装前端依赖后，可执行：

```bash
./scripts/check.sh
(cd apps/admin && pnpm build)
```

## 项目结构

```text
crates/domain          领域规则与状态转换
crates/application     用例、授权与出站端口
crates/infrastructure  PostgreSQL、渲染、认证与文件存储适配器
crates/interfaces      HTTP / CLI 入站适配器
crates/server          配置、依赖装配与进程生命周期
apps/admin             React 管理后台
migrations/postgres    数据库迁移（结构的执行依据）
themes                 公开站点主题
scripts                开发、检查与恢复工具
docs                   参考文档、规划与架构决策
```

文章和页面保留 Markdown 源文，保存时生成经过清洗的 `content_html`，与媒体引用一起提交；公开页面读取已保存的 HTML。分层、事务和渲染边界见[架构说明](docs/architecture.md)。

## 文档

从[文档导航](docs/README.md)选择阅读路径。常用入口：[开发](docs/development.md) · [配置](docs/configuration.md) · [管理 API](docs/admin-api.md) · [架构](docs/architecture.md) · [运维与恢复](docs/operations-and-recovery.md) · [ADR](docs/adr/README.md)。
