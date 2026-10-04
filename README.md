# blog

Rust 模块化单体博客，公开站点使用服务端渲染，管理后台使用 React、TypeScript 和 Ant Design。

目前支持文章与独立页面、标签/分类/系列、OAuth 与本地密码登录、角色权限、媒体库、站点设置、主题切换，首页分页、页面导航、后台跨页搜索与多人内容管理，以及 RSS、sitemap 和 SEO 元数据。项目仍在开发阶段，接口与数据模型可直接调整，不维护旧管理接口的兼容层。交付状态和后续范围见[路线图](docs/product-roadmap.md)。

**新数据库基线已完成适配。** [19 表基线](migrations/postgres/0001_initial_schema.sql)已替换旧迁移链，身份、持久会话、媒体、内容、目录与评论已适配；保留期维护、新库备份恢复和正式媒体显式清理已接入，已有业务写入口已补齐事务审计、可信来源 IP 和后台只读查询。首次安装至备份恢复的[全链路验收](docs/acceptance.md)已通过，本地空库安装也已验证，生产部署验收仍单独执行。旧结构需另建空库，不能原地升级。生成的 DDL 参考见 [汇总 DDL](docs/sql/postgres-core.sql)，后续变更遵循[迁移演进规则](docs/schema-migrations.md)，已实现边界见[数据库实现参考](docs/database-current.md)。

## 快速开始

使用 1Panel 时，复制 [compose.yaml](compose.yaml)，填写博客镜像、域名与安装码并启动，再通过安装向导验证已有 PostgreSQL 连接、创建管理员并保存配置。如果希望数据库也由同一编排运行，选择完整的 [compose.postgres.yaml](compose.postgres.yaml)。部署无需配置包，具体见 [网页部署指南](docs/1panel.md)。GitHub Actions 发布一个包含网站、后台和备份恢复工具的博客镜像，Docker 在启动时自动拉取本地缺少的镜像。

日常操作位于后台“系统 → 备份与恢复”：下载恢复密钥、立即或定时备份、配置 S3 远程存储、上传副本和原地恢复。数据库故障时仍可访问应急页面；操作步骤见[后台备份与恢复](docs/browser-backup.md)。

从源码构建使用 `docker build -t blog:local .`，再将所选 Compose 中的镜像改为 `blog:local`。旧脚本编排保留为 `compose.legacy.yaml`；高级连接、分离数据库账号和旧恢复入口见 [Docker Compose 部署](docs/docker-compose.md)与 [Compose 备份恢复](docs/compose-backup.md)。

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
