# 开发指南

本文负责本地运行、CLI、后台联调和检查流程。完整环境变量见[配置参考](configuration.md)，业务规则见[内容生命周期](content-lifecycle.md)，接口见[管理 API](admin-api.md)。后台组件、表单、查询缓存和测试约定见[后台开发指南](admin-development.md)。

迁移链已重写为新的 [19 表初始基线](../migrations/postgres/0001_initial_schema.sql)，仅支持空库。已适配身份、会话和媒体，内容/目录/评论仍待适配，以下相应业务命令保留为后续实现参考，暂不能作为新库的可用功能。不要把 `blog_schema.sql` 手工导入后再执行迁移；统一通过 `migrate` 建立 SQLx 记录。进度见[实施路线](product-roadmap.md#已采纳数据库设计的实施)。

## 环境准备

- Rust stable，包含 `rustfmt`、`clippy`，支持仓库使用的 Rust 2024 edition。
- Node.js 22；pnpm 版本以 [apps/admin/package.json](../apps/admin/package.json) 的 `packageManager` 为准。
- PostgreSQL 18，可通过 Docker 启动；检查脚本还需要 Python 3。

以下命令除明确说明外均在仓库根目录运行。初次建库、创建 Owner 和启动站点的完整流程见[快速开始](../README.md#快速开始)。

```bash
./scripts/dev-db.sh
cargo run -p server -- migrate
(cd apps/admin && pnpm install --frozen-lockfile && pnpm build)
cargo run -p server -- serve
```

数据库脚本使用容器 `blog-postgres` 和持久卷 `blog-pgdata`，只绑定本机回环地址；已有容器时直接启动。数据库连接失败时先检查 Docker 和端口。改变 `BLOG_PG_PORT` 不会修改已有容器的端口映射，也不会自动更新应用连接串。

所有业务命令都会先执行结构迁移；`migrate`、`post` 和 `serve` 还会重建旧渲染版本的 HTML。用户、角色、OAuth 和媒体维护命令只加载各自需要的依赖，站点 URL 或主题配置错误不会阻止身份修复。详见[架构](architecture.md)。

## 新基线的隔离验证

先使用独立数据库，暂不重建正在使用的开发库。例如已有 `blog-postgres` 容器时：

```bash
# 名称已存在时换一个新名字；这一步不删除任何已有库。
docker exec blog-postgres createdb -U blog blog_phase1
export DATABASE_URL=postgres://blog:blog@127.0.0.1:5432/blog_phase1
cargo run -p server -- migrate
cargo run -p server -- user create sun --display-name Sun
cargo run -p server -- user passwd --user sun
cargo run -p server -- role assign --user sun --role owner
```

连接串按实际本机端口和凭据调整。后续命令在同一 shell 使用该 `DATABASE_URL`。用户/角色命令会同步权限目录和内置角色；`migrate` 只负责结构与派生重建，不创建账号。

第一批数据库验证使用以下测试；`BLOG_TEST_ADMIN_URL` 应指向独立 PostgreSQL 实例的管理库，测试会重建固定名称的测试库，不要指向业务库：

```bash
cargo test -p infrastructure --test identity_baseline --test session_postgres
cargo test -p server --test password_http
cargo test -p server --test command_assembly owner_bootstrap_uses_new_identity_baseline
```

验收链路为：空库迁移、权限初始化、CLI 创建 Owner、密码登录、资料更新保持登录、改密撤销旧会话。`PUT /api/admin/v1/me/profile` 提交展示名、纯文本简介和必填 `expected_version`，详见[管理 API](admin-api.md)。资料表单尚未接入后台界面。内容和评论的旧数据库集成测试尚待适配；`check.sh` 全量通过属于后续批次的验收，不能用本批定向测试替代。

媒体批次在同一独立实例验证：

```bash
cargo test -p infrastructure --test media
cargo test -p infrastructure --test write_invariants media_trash_restore
cargo test -p server --test media_http
cargo test -p server --test command_assembly media_cleanup_staging
(cd apps/admin && pnpm typecheck && pnpm test)
```

覆盖独立公开读取、回收站与恢复、引用事务/竞争、私密来源过滤、审计回滚及暂存清理。

## 使用 CLI

```bash
cargo run -p server -- --help
cargo run -p server -- post --help
cargo build -p server
```

构建后的程序为 `target/debug/blog`。下面使用 `blog` 简写；可替换为 `target/debug/blog` 或 `cargo run -p server --`。参数以各子命令 `--help` 为准。

### 文章

```bash
printf '# 你好\n\n这是第一篇文章。\n' | blog post create \
  --author sun --slug hello-world --title "你好，世界" --content-file -
blog post list --author sun
```

创建和列表输出包含文章 UUID。将下面的 `YOUR_POST_UUID` 替换为该值：

```bash
blog post show --id YOUR_POST_UUID
blog post edit --id YOUR_POST_UUID --title "新的标题" --if-version 1
blog post publish --id YOUR_POST_UUID
blog post withdraw --id YOUR_POST_UUID
```

管理命令使用 UUID；`--slug` 与 `--new-slug` 只设置公开地址。首次发布后 slug 锁定。`--content-file` 接受文件路径或 `-`（stdin）；`--if-version` 用于显式并发检查。默认以作者身份检查内容权限，支持的命令可用 `--as 用户名` 指定操作者。页面、目录、回收站等操作使用后台或管理 API，当前没有对应的完整 CLI。

### 用户与角色

```bash
blog user create sun --display-name "Sun"
blog user passwd --user sun
blog user show sun
blog role list
blog role assign --user sun --role owner
blog role remove --user sun --role author
```

`user passwd` 默认隐藏输入并二次确认，`--password-stdin` 可从 stdin 读取，`--clear` 关闭密码登录；不接受明文密码命令行参数。设置或清除密码会撤销已有会话，最后登录方式保护仍然生效。

身份管理 CLI 依赖本机 shell 信任，可执行引导操作；文章命令仍按选定用户的权限检查。角色委派、最后 Owner 等结构性规则见[身份与权限](identity-and-admin.md)。

### OAuth

```bash
blog oauth add-oidc --id keycloak \
  --issuer https://idp.example.com/realms/main \
  --client-id demo --secret-ref IDP_SECRET
blog oauth add-github --client-id gh-demo --secret-ref GH_SECRET
blog oauth list
blog oauth bind --user sun --provider keycloak --external-id YOUR_STABLE_SUB
blog oauth bindings --user sun
```

`secret_ref` 是提供商密钥所在的环境变量名，需通过运行服务的环境注入。提供商回调地址为 `BLOG_PUBLIC_BASE_URL/auth/callback/{provider}`。绑定前核对稳定的 OIDC `sub` 或 GitHub 数值用户 ID，邮箱不用于自动关联账号；未绑定身份不能登录。添加配置不代表已验证提供商可用性。

### 媒体维护

```bash
blog media cleanup-staging
```

命令使用 `BLOG_MEDIA_DIR`，仅清扫一小时前的暂存残留；不扫描或删除任何正式对象，零引用或回收站图片也保留。状态、引用与删除规则见[内容生命周期](content-lifecycle.md)，数据保护流程见[运维与恢复](operations-and-recovery.md)。

## 后台 SPA 联调

后台代码在 [apps/admin](../apps/admin/)，生产构建挂载于 `/admin`。开发时开两个终端：

```bash
# 终端一，仓库根目录
BLOG_PUBLIC_BASE_URL=http://localhost:5173 cargo run -p server -- serve
```

```bash
# 终端二，仓库根目录
cd apps/admin
pnpm install --frozen-lockfile
pnpm dev
```

访问 `http://localhost:5173/admin/`。[Vite 配置](../apps/admin/vite.config.ts)把 `/api` 和 `/auth` 代理到 `127.0.0.1:8080`，保留 Host，使浏览器认证请求与代理入口一致；当前没有 `/media` 代理规则。涉及图片预览或完整公开站点时，使用构建后的后台并直接访问服务端地址验证。

登录过程保持使用同一个主机名，`localhost` 与 `127.0.0.1` 的 cookie 不互通。OAuth 联调需在提供商配置对应的 `http://localhost:5173/auth/callback/{provider}` 回调，宜使用独立的开发配置。

## 检查与测试

安装前端依赖并启动本地 PostgreSQL 后：

```bash
./scripts/check.sh
(cd apps/admin && pnpm build)
```

`check.sh` 执行 Cargo 依赖边界检查及其测试、格式检查、Clippy、Rust 工作区测试、后台类型检查与测试、恢复工具测试；前端生产构建单独运行。CI 分为后端与前端两个 job，入口见 [ci.yml](../.github/workflows/ci.yml)。

按改动范围也可运行：

```bash
python3 -B scripts/check_dependencies.py
cargo test -p domain
cargo test -p application
cargo test -p server --test admin_api
(cd apps/admin && pnpm test)
PYTHONPATH=scripts python3 -B -m unittest scripts/test_recovery.py
```

基础设施和服务端集成测试通过 `BLOG_TEST_ADMIN_URL` 连接本地管理库，为各测试套件删除并重建独立的测试数据库。该连接必须指向可创建数据库的本地测试实例；不要把业务数据放入测试库，也不要同时启动同一套集成测试的多个副本。具体库名由测试代码维护，测试过程不使用开发库 `blog` 保存业务样本。

模板桥接原型不属于工作区测试，可在需要改动该原型时运行：

```bash
(cd spikes/template-bridge && cargo test)
```

## 常见问题

| 现象 | 检查方向 |
|---|---|
| `/admin` 返回 404 | 是否执行前端构建，`BLOG_ADMIN_DIST` 是否指向产物目录 |
| 登录后仍回登录页 | 主机名是否一致，HTTP 环境是否误启用 Secure cookie |
| 写请求失败 | 查看响应 `code` 与 `x-request-id`；检查当前 CSRF token、权限和版本 |
| 修改环境中的站点标题不生效 | 数据库 `settings.site` 优先于环境回退值 |
| 发布后内容仍不可见 | 是否为 `published`、`public`，文章是否仍在回收站 |
| CLI 提示配置或文件缺失 | 确认当前工作目录；相对路径从进程工作目录解析 |

错误码见[管理 API](admin-api.md)，站点配置和作用域见[配置参考](configuration.md)。
