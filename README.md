# blog

Rust 模块化单体博客。当前进度：**M1 内容闭环 + M2 RBAC/会话/OAuth/管理写 API**（迁移 → 权限化写入 → 公开 SSR 阅读 → OAuth 登录 → `/api/admin/v1` 文章管理）；React 后台为 M2 最后一块。

## 快速开始

```bash
# 1. 本地 PostgreSQL 18（Docker）
./scripts/dev-db.sh

# 2. 数据库迁移（也可省略，serve/写命令前会自动迁移）
cargo run -p server -- migrate

# 3. 受控 CLI：建用户、分配角色、写文章、发布
cargo run -p server -- user create sun --display-name "Sun"
cargo run -p server -- role assign --user sun --role author
cargo run -p server -- post create --author sun --slug hello-world \
  --title "你好，世界" --content-file path/to/post.md
cargo run -p server -- post publish --slug hello-world

# 4. 公开 SSR 服务
cargo run -p server -- serve --addr 127.0.0.1:8080
```

角色与权限（M2 第一段已交付）：

```bash
blog role list                       # 内置角色：owner/admin/editor/author
blog role assign --user X --role Y   # 分配（幂等；users.version 递增）
blog role remove --user X --role Y   # 移除（最后一个有效 Owner 会被拒绝）
blog user show X                     # 查看角色与有效权限并集
```

- 权限目录是应用可信注册表（`PERMISSION_REGISTRY`），启动时幂等同步，普通入口不能创造任意 key。
- 文章动作按 own/any 权限对检查（如 `post.update` / `post.update_any`），any 覆盖 own，角色名称不替代动作检查。
- 身份写路径同样校权：建用户需 `user.manage`，角色分配/移除需 `role.manage`，且不得超出调用者自身权限集合（委派上限）；授予/移除 Owner 另需 `ownership.manage`，OAuth 提供商与绑定需 `oauth.manage`。受控 CLI 以引导身份（本机 shell 信任）持有全部已注册权限。
- “有效 Owner”= 未软删除 + 持有 owner 角色 + 至少一种有效登录方式（oauth_accounts）；移除最后一个可登录 Owner 会被拒绝，登不进去的 Owner 可被清理。
- 身份/角色变更在统一 `pg_advisory_xact_lock(2048001,1)` 排他锁下执行（docs/identity-and-admin.md §3）。

### OAuth 登录与会话（M2 第二段已交付）

```bash
# 配置提供商（秘密经环境变量 secret_ref 提供，不落库）
blog oauth add-oidc --id keycloak --issuer https://idp.example.com/realms/main \
  --client-id demo --secret-ref IDP_SECRET
blog oauth add-github --client-id gh-demo --secret-ref GH_SECRET

# 受控绑定外部身份（需核对稳定 sub / 数值用户 ID；未绑定身份不得登录）
blog oauth bind --user sun --provider keycloak --external-id <sub>
blog oauth bindings --user sun
```

- 浏览器访问 `GET /auth/login?provider=<id>&next=/admin` → OIDC（PKCE S256 + nonce + JWKS 校验）或 GitHub → `GET /auth/callback/{provider}` 签发会话。发起登录会下发短命 `blog_oauth_state` 绑定 cookie（Secure 部署用 `__Host-` 前缀），回调必须由同一浏览器带回，防登录 CSRF。
- `GET /auth/providers` 是公开只读端点（`no-store`），只返回 `[{id, name, kind}]`，供登录页渲染按钮，不含 client_id/issuer/secret_ref 等配置细节；展示名由 `oauth add-* --name` 设置，缺省回退到 id。
- 会话为单实例内存存储（HttpOnly/SameSite=Lax cookie，服务端只存 SHA-256 摘要）；空闲/绝对过期、容量淘汰、重启全部失效。
- `GET /api/admin/v1/me` 返回当前用户与权限并集（每次重新读取，撤权即时生效）；`POST /auth/logout` 需会话 + `X-CSRF-Token` 头 + 同源 Origin。
- 相关环境变量：`BLOG_PUBLIC_BASE_URL`（回调 redirect_uri 基址）、`BLOG_SECURE_COOKIES`（不设时按 `BLOG_PUBLIC_BASE_URL` 的 scheme 推断，HTTPS 部署自动加 Secure）。

### 管理写 API（M2 第三段已交付）

会话认证 + CSRF（写方法必须带 `X-CSRF-Token`，值来自 `/api/admin/v1/me`）+ own/any 授权的文章管理端点，响应一律 `Cache-Control: no-store`，请求体上限 2 MiB：

| 方法与路径 | 说明 |
|---|---|
| `POST /api/admin/v1/posts` | 创建草稿（`post.create`；作者即会话用户），返回详情（含正文） |
| `GET /api/admin/v1/posts/{slug}` | 任意状态详情（own 限本人；`post.read_any` 全部），含 Markdown 源文 |
| `GET /api/admin/v1/posts?author=` | 列表（默认本人，需 `post.read`；他人需 `read_any`）；摘要形态，不含正文 |
| `PATCH /api/admin/v1/posts/{slug}` | 编辑（`post.update` own / `post.update_any`；支持 `expected_version`） |
| `POST /api/admin/v1/posts/{slug}/publish` | 发布（`post.publish` / `_any`；幂等） |
| `POST /api/admin/v1/posts/{slug}/unpublish` | 撤回（`post.unpublish` / `_any`） |

错误语义：JSON `{"error": ..., "code": ..., "request_id": ...}`，401 未登录（带 `WWW-Authenticate: Session`）、403 越权/CSRF/跨源、404 不存在、409 slug 占用（`code=conflict`）或版本冲突（`code=version_conflict`）、400 校验失败；内部错误只回通用文案（`code=internal_error`）。同一状态码可能对应不同业务原因，客户端按 `code` 分支而不是只看状态码。

稳定业务码清单（发布后即契约，改动需同步客户端与文档）：`unauthenticated`、`invalid_request`、`version_conflict`、`conflict`、`not_found`、`forbidden`、`external_error`、`internal_error`。

请求编号与访问日志：全站最外层中间件为每个请求生成 UUIDv7，回写 `x-request-id` 响应头（**含被认证提取器提前拒绝的 401/403**），管理 JSON 错误体的 `request_id` 与响应头一致；后台界面把编号显示在错误提示里，报障时可直接对照服务端日志。每个请求另输出一条完成日志：`method`（不含 query）、`path`、`status`、`elapsed_ms`，以及**仅来自已验证会话**的 `actor_id`（匿名为空）；4xx 记 `info`、5xx 记 `warn`，绝不记录 Cookie、token 与正文。这满足「谁请求了哪个接口、结果如何」，但不等于业务审计——角色变更、身份绑定等动作的动作/对象级审计仍需专用存储。

环境变量：

| 变量 | 默认 | 说明 |
|---|---|---|
| `DATABASE_URL` | `postgres://blog:blog@127.0.0.1:5432/blog` | PostgreSQL 连接 |
| `BLOG_BIND` | `127.0.0.1:8080` | serve 监听地址（`--addr` 优先） |
| `BLOG_THEME_DIR` | `themes/default` | 主题目录（模板 + assets） |
| `BLOG_MIGRATIONS_DIR` | `migrations/postgres` | 迁移目录 |
| `BLOG_ADMIN_DIST` | `apps/admin/dist` | 后台 SPA 构建产物；目录不存在时不注册 `/admin` |
| `BLOG_SITE_TITLE` / `BLOG_SITE_DESCRIPTION` | Sun's Blog / 一个 Rust 博客 | 站点信息（M3 迁入 settings） |

## 后台 SPA（apps/admin）

React + TypeScript + Vite，挂在 `/admin` 子树：

```bash
cd apps/admin
pnpm install
pnpm build        # 产物 apps/admin/dist，由 blog serve 按 BLOG_ADMIN_DIST 提供
pnpm dev          # 开发服务器 http://localhost:5173/admin/
```

`/admin` 由后端只在该子树内注册（`mount_admin_spa`）：`index.html` 与深链回退 `no-cache`，`/admin/assets/*` 的**成功**响应带指纹 `immutable`（缺失资源是 404，同样 `no-cache`，避免错误被缓存固化）。SPA 内部路由为 `/admin/`、`/admin/posts/new`、`/admin/posts/{slug}/edit`——编辑地址带 `/edit` 后缀，slug 恰好是 `new` 的文章才不会与新建页相撞。axum 按路径匹配，SPA fallback 结构上不可能遮挡 `/api`、`/auth`、`/posts/{slug}` 等路由。

### 开发期同源不变量（务必遵守）

开发期不引入 CORS、不加 dev origin 白名单，靠 Vite 代理让浏览器看到的也是同源：

1. **统一用一个主机名，别混用 `localhost` 和 `127.0.0.1`。** cookie 不区分端口但区分主机：`localhost:5173` 与 `localhost:8080` 共享 cookie，`127.0.0.1:8080` 不共享。混用会出现「登录成功但 SPA 仍显示未登录」。
2. **Vite 代理不要设 `changeOrigin`**（默认 false，Host 保持 `localhost:5173`）。设为 true 会把 Host 改写成 `127.0.0.1:8080`，与浏览器 Origin 对不上，后端同源校验直接 403；这不是靠白名单绕开的问题，而是本来就不该改 Host。
3. 开发期后端设 `BLOG_PUBLIC_BASE_URL=http://localhost:5173`，并在 OIDC/GitHub 侧为 dev 客户端注册回调 `http://localhost:5173/auth/callback/{id}`（GitHub 只允许一个回调 URL，用单独的 dev OAuth App）。

### 前端状态约定

- **CSRF token 只在内存**：启动与每次登录后从 `/api/admin/v1/me` 取，存 React context，刷新页面重新取；绝不写 `localStorage`/`sessionStorage`。任何 401 清空 token（已登录状态下跳登录，未登录状态切登录页，避免重定向环）。
- **`src/api.ts` 收口所有请求**：`credentials: "same-origin"`、非 GET 自动附 `X-CSRF-Token`、统一解析 `{error}`；401 跳登录、403 提示无权限、409 进冲突流程。
- **409 冲突流程**：编辑器保留用户当前输入并提示「内容已在别处修改」，提供「重新加载」（丢弃本地改动）与「仍然覆盖」（二次确认后用服务器最新 version 重新提交）。不自动重试、不静默覆盖。

## 测试

```bash
cargo test --workspace
# 或本地提交前一键检查（fmt + clippy -D warnings + 测试）
./scripts/check.sh
```

- `crates/domain`：聚合与值对象规则（无数据库）。
- `crates/application`：用例 + 内存 fake（权限、委派上限、Owner 保护、版本冲突、真并发 join!）。
- `crates/infrastructure/tests`：真实 PostgreSQL（迁移、约束、三态保存、两连接真并发、公开过滤、RBAC 幂等与 Owner 并发）。
- `crates/server/tests`：完整装配 + HTTP（会话/CSRF/Origin、own/any 越权、浏览器绑定、撤权与软删除后旧会话、`/auth/providers`、SPA 挂载与缓存头、草稿/private 不可公开访问）。
- `apps/admin`：`pnpm test`（Vitest + React Testing Library）覆盖编辑器交互回归；`pnpm build`（`tsc --noEmit` + Vite 构建）检查前端类型与产物。

集成测试需要可写的 PostgreSQL，且**只允许 loopback 主机**：默认 `postgres://blog:blog@127.0.0.1:5432`，可用 `BLOG_TEST_ADMIN_URL` 覆盖（infrastructure 与 server 的测试库 DSN 都自动从它推导），会重建 `blog_test` / `blog_server_test` / `blog_admin_test` / `blog_auth_test` 数据库。CI 见 `.github/workflows/ci.yml`。

## 结构

```text
crates/
├── domain          # 聚合、值对象、业务规则（无框架依赖）
├── application     # 用例、端口、DTO（定义出站接口）
├── infrastructure  # SQLx 持久化、MiniJinja 渲染、Markdown 清洗
├── interfaces      # 公开 HTTP 路由 + 受控 CLI（不依赖 infrastructure）
└── server          # 装配入口（bin: blog）
apps/admin          # React + TypeScript + Vite 后台 SPA（构建产物 dist/ 不进仓库）
migrations/postgres # 13 表核心 DDL（sqlx 布局）
themes/default      # 模板与静态资源
docs/               # 设计文档与 ADR
```

依赖方向与边界见 [docs/architecture.md](docs/architecture.md)；里程碑见 [docs/product-roadmap.md](docs/product-roadmap.md)。

## 已验证（M1 验收）

- 写入真实数据库的一篇文章：发布后可正常阅读（Markdown 渲染 + HTML 清洗）。
- 撤回后立即不可访问（无页面缓存）。
- 并发编辑基于 `expected_version` 乐观锁，后提交方收到明确冲突，不互相覆盖。
- slug 草稿创建即唯一、首次发布后锁定（撤回也不可改名）、重新发布保留首次 `published_at`。
- 匿名可见条件唯一：`status='published' AND visibility='public' AND deleted_at IS NULL`。
- 系列位置唯一约束（可延后）交换、外键 RESTRICT、用户名/slug 冲突映射。

## 下一步

M2（身份与后台）已交付：RBAC/委派、OAuth 登录闭环、管理写 API、后台 SPA（文章屏幕）。M3 按 [roadmap](docs/product-roadmap.md) 推进：分类树、标签、Series 排序、settings、Page 屏幕、RSS/sitemap、Post 回收站、备份恢复。

M2 遗留（已知、未做）：

- 禁用 provider 时校验是否使最后 Owner 失去登录方式（需跨 settings 与 `oauth_accounts` 的检查）。
- `post.purge` / `post.transfer_author` 与所有权转移的重新认证流程。
- 角色编辑 API（当前只有分配/移除；内置 slug 保护与委派上限已就位）。
