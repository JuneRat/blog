# blog

Rust 模块化单体博客，公开站点使用服务端渲染，管理后台使用 React、TypeScript 和 Ant Design。

目前支持文章与独立页面、标签/分类/系列、OAuth 与本地密码登录、角色权限、媒体库、站点设置、主题切换，以及 RSS、sitemap 和 SEO 元数据。项目仍在开发阶段，接口与数据模型可直接调整，不维护旧管理接口的兼容层。交付状态和后续范围见[路线图](docs/product-roadmap.md)。

**正在分批切换数据库。** 新的 [19 表基线](migrations/postgres/0001_initial_schema.sql)已替换旧迁移链，身份、持久会话、媒体、内容、目录与评论已适配；保留期维护、新库备份恢复和正式媒体显式清理已接入，已有业务写入口已补齐事务审计、可信来源 IP 和后台只读查询，生产部署验收仍待完成。请先用[独立空库](docs/development.md#新基线的隔离验证)验证，暂不切换原开发库。完整目标见 [blog_schema.sql](blog_schema.sql)，已实现边界见[数据库实现参考](docs/database-current.md)。

## 快速开始

准备 Rust stable、Node.js 22、pnpm（版本见 [package.json](apps/admin/package.json)）和 Docker。在仓库根目录运行：

```bash
# 启动本地 PostgreSQL 18 并初始化数据库
./scripts/dev-db.sh
cargo run -p server -- migrate

# 创建首个管理账号；密码交互输入，不回显
cargo run -p server -- user create sun --display-name "Sun"
cargo run -p server -- user passwd --user sun
cargo run -p server -- role assign --user sun --role owner

# 构建后台并启动站点
(cd apps/admin && pnpm install --frozen-lockfile && pnpm build)
cargo run -p server -- serve --addr 127.0.0.1:8080
```

站点入口为[公开站点](http://127.0.0.1:8080/)和[管理后台](http://127.0.0.1:8080/admin/)。已验证身份、媒体、内容发布与回收站、多系列、预约发布和评论；保留期维护、独立数据库权限和含媒体的隔离恢复已有验证，操作见[备份与恢复](docs/operations-and-recovery.md)。

默认连接本地开发数据库，环境变量覆盖方式见[配置参考](docs/configuration.md)。程序不会自动加载 `.env`。旧结构不能原地升级；新迁移不会删除旧数据，需另建空库。

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
