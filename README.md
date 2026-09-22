# blog

Rust 模块化单体博客。当前进度：**M1 内容闭环（Post + Page）+ M2 RBAC/会话/OAuth/本地密码/管理写 API + React 后台**（迁移 → 权限化写入 → 公开 SSR 阅读 → OAuth 与本地密码登录 → `/api/admin/v1` 文章与页面管理）。

## 快速开始

```bash
# 1. 本地 PostgreSQL 18（Docker）
./scripts/dev-db.sh

# 2. 数据库迁移（也可省略，serve/写命令前会自动迁移）
cargo run -p server -- migrate

# 3. 受控 CLI：建用户、设置本地密码、分配角色、写文章、发布
cargo run -p server -- user create sun --display-name "Sun"
cargo run -p server -- user passwd --user sun        # 交互式设置本地密码（不回显、二次确认）
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
- 文章动作按 own/any 权限对检查（如 `post.update` / `post.update_any`），any 覆盖 own，角色名称不替代动作检查。独立页面是**站点级** `page.*`（`page.read/create/update/publish/unpublish/archive/delete`），Page 无 author_id，不套用 own/any；内置 Editor 与 Owner 持有这些权限，Author 不持有。
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

### 本地密码登录（已交付）

```bash
blog user passwd --user sun                    # 设置/重置密码；交互隐藏输入 + 二次确认
blog user passwd --user sun --password-stdin   # 自动化：整段 stdin，只去掉一个行尾
blog user passwd --user sun --clear            # 关闭密码登录（最后一种登录方式会被拒绝）
blog user show sun                             # 显示「密码登录：已启用/未启用」
```

- 存储为 **Argon2id** PHC 字符串（m=19456 KiB、t=2、p=1、32 字节输出、16 字节随机盐），参数自描述；存储参数弱于当前策略时在成功登录后透明重哈希升级。并发哈希有信号量上限（默认 4 路），避免并发登录打满内存。
- `POST /auth/login/password`（JSON，`Cache-Control: no-store`，4 KiB 上限）成功下发与 OAuth 相同的 `blog_session` cookie；失败一律 `401` + `code=invalid_credentials`（用户名不存在、密码错误、账号已停用不可区分，未知用户也执行等价开销校验防时间侧信道）；触发限流为 `429` + `code=rate_limited` + `Retry-After`。
- 失败限流按**账号**（15 分钟 5 次）与**来源地址**（15 分钟 50 次）分别计数，临时锁定 15 分钟。额度在哈希校验**之前**预占：只做事后计数的话，并发请求会在任何失败被记录前全部通过，一次突发就是 N 次爆破机会。锁定期间拒绝但不延长锁定，成功登录清账号历史失败并保留其他请求的预占，来源地址维度只归还本次预占（不清历史失败）。请求取消/超时自动归还额度；主体容量耗尽且无空闲条目可淘汰时拒绝新增主体。
- 来源地址只取 socket 对端，**不读 `X-Forwarded-For`**；反向代理后的真实客户端地址需要部署侧配置可信转发（后续能力）。
- `POST /api/admin/v1/me/password`（会话 + `X-CSRF-Token` + 同源 Origin）自助改密：已启用密码时需重新提供当前密码（**与登录共用失败预算，同样受限流**）；凭据写入是条件写入，**并发的管理员强制重置不会被自助改密覆盖**；成功后轮换会话并回新的 `csrf_token`。
- **设置/重置/清除密码与自助改密都会撤销该用户全部会话**，旧 Cookie 立即失效。会话绑定签发时的 `users.version`，所以**另一个进程**（运维跑 CLI 改密、改角色、软删除）也能让运行中服务的旧会话立即失效，不依赖服务进程内的内存撤销。「至少保留一种登录方式」的检查与清除在同一把身份锁内完成，与解绑外部身份互斥。
- 设置/清除需 `user.manage`，且只经受控入口（CLI 引导身份 / 后台会话）；口令不接受 `--password` 参数，避免进入进程表与 shell 历史。
- 自助邮箱找回**未交付**（需要一次性令牌存储与邮件投递）；忘记密码只能由有部署权限的运维用 CLI 重置。泄露处置见 [docs/operations-and-recovery.md §6](docs/operations-and-recovery.md)，取舍见 [ADR-0009](docs/adr/0009-local-password-authentication.md)。

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
| `POST /api/admin/v1/pages` | 创建页面草稿（`page.create`；站点级，无作者），返回详情（含正文） |
| `GET /api/admin/v1/pages` | 全部页面列表（`page.read`）；摘要形态，不含正文 |
| `GET /api/admin/v1/pages/{slug}` | 页面详情（`page.read`），含 Markdown 源文 |
| `PATCH /api/admin/v1/pages/{slug}` | 编辑页面（`page.update`；支持 `expected_version`） |
| `POST /api/admin/v1/pages/{slug}/publish` | 发布页面（`page.publish`；幂等） |
| `POST /api/admin/v1/pages/{slug}/unpublish` | 撤回页面（`page.unpublish`） |
| `POST /api/admin/v1/me/password` | 自助改密（会话 + CSRF；需当前密码重新认证；成功后轮换会话） |

页面与文章共用同一套错误契约、乐观并发与首次发布后锁定 slug 的规则；区别是 `page.*` 为站点级权限，且公开地址是根路径 `/{slug}`（`admin`、`api`、`auth`、`posts`、`assets`、`healthz` 等系统路径在创建、改名与发布时都会拒绝，固定路由优先）。幂等操作（重复发布/撤回、无变化的编辑）同样校验显式传入的 `expected_version`：版本不一致一律 `version_conflict`，不会因为「本来就不写库」而假装成功。

错误语义：JSON `{"error": ..., "code": ..., "request_id": ...}`，401 未登录（带 `WWW-Authenticate: Session`）、403 越权/CSRF/跨源、404 不存在、409 slug 占用（`code=conflict`）或版本冲突（`code=version_conflict`）、400 校验失败、429 登录限流（`code=rate_limited`，带 `Retry-After`）；内部错误只回通用文案（`code=internal_error`）。同一状态码可能对应不同业务原因，客户端按 `code` 分支而不是只看状态码。登录失败统一 `401` + `code=invalid_credentials`；自助改密时「当前密码不正确」用 `403` + 同一 code，避免前端把用户误判为掉线。

稳定业务码清单（发布后即契约，改动需同步客户端与文档）：`unauthenticated`、`invalid_credentials`、`invalid_request`、`rate_limited`、`version_conflict`、`conflict`、`not_found`、`forbidden`、`external_error`、`internal_error`。

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

`/admin` 由后端只在该子树内注册（`mount_admin_spa`）：`index.html` 与深链回退 `no-cache`，`/admin/assets/*` 的**成功**响应带指纹 `immutable`（缺失资源是 404，同样 `no-cache`，避免错误被缓存固化）。SPA 内部路由为 `/admin/`（文章）、`/admin/posts/new`、`/admin/posts/{slug}/edit`、`/admin/pages`（页面）、`/admin/pages/new`、`/admin/pages/{slug}/edit`——编辑地址带 `/edit` 后缀，slug 恰好是 `new` 的内容才不会与新建页相撞。axum 按路径匹配，SPA fallback 结构上不可能遮挡 `/api`、`/auth`、`/posts/{slug}` 与页面根路径等路由。

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

- `crates/domain`：聚合与值对象规则（无数据库），含本地密码策略（长度、含用户名、常见/规律口令）。
- `crates/application`：用例 + 内存 fake（权限、委派上限、Owner 保护、版本冲突、真并发 join!；密码登录的统一失败语义、等价开销校验、限流、设置/清除与自助改密轮换）。
- `crates/infrastructure/tests`：真实 PostgreSQL（迁移、约束、三态保存、两连接真并发、公开过滤、RBAC 幂等与 Owner 并发、密码凭据的软删除作用域）；单元测试覆盖 Argon2id 哈希/校验/参数升级与内存限流。
- `crates/server/tests`：完整装配 + HTTP（会话/CSRF/Origin、own/any 越权、浏览器绑定、撤权与软删除后旧会话、`/auth/providers`、密码登录全链路与限流/Retry-After、改密轮换、SPA 挂载与缓存头、草稿/private 不可公开访问）。
- `apps/admin`：`pnpm test`（Vitest + React Testing Library）覆盖登录表单、编辑器交互（含标签选择）、标签管理屏、用户与角色屏幕与路由解析；`pnpm build`（`tsc --noEmit` + Vite 构建）检查前端类型与产物。
- `spikes/template-bridge`：M0 原型（独立 workspace，不在根清单），`cargo test` 跑桥接/预算/隔离集成测试；生产 crate 不依赖它。

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
- 标签闭环（M3 第一段）：目录管理（创建/改名/删除，`tag.manage`，slug 创建后不可改，被引用标签删除受保护，业务码 `tag_in_use`）；文章编辑器多标签选择，正文与标签关系同一事务保存（仅标签变化也递增 version，重复 id 幂等去重）；公开标签页 `/tags/{slug}` 分页（每页 20，只列公开已发布文章，页码越界渲染空页）；文章详情展示标签链接。
- 分类树（M3 第二段）：创建/更新/移动/删除（category.manage，slug 创建后不可改）；移动在分类树事务锁内做深度受限祖先链校验防环；被文章引用或含子分类时删除受 category_in_use 保护；文章编辑器分类选择与正文/标签同事务保存；公开分类页 /categories/（slug） 分页（直接归属），详情页展示分类链接。
- Series（M3 第三段）：目录管理（series.manage，被文章引用时删除受 series_in_use 保护）；文章设置系列与序号（同事务，位置唯一冲突为可定位 409）；整体重排在系列行锁 + series.version 前提下进行，成员按 id 序加锁、位置唯一约束 DEFERRED 到提交检查，同时递增涉及 posts.version 与 series.version；重排逐篇核验文章授权；公开系列页 /series/（slug） 按阅读顺序分页（草稿占位不外泄）。
- M0 主题桥接原型（`spikes/template-bridge`）：同步模板函数 ↔ 异步 SQL 查询桥接验证可行，预算/隔离/失败场景 17 项集成测试；结论见原型 README 与 ADR-0002。

## 下一步

M2（身份与后台）已交付：RBAC/委派、OAuth 登录闭环、本地密码登录（Argon2id + 限流 + 受控重置）、管理写 API、后台 SPA（文章/页面/用户与角色屏幕）；其后用户与角色管理界面也已交付（见 [身份与后台 §8](docs/identity-and-admin.md)）。M0 主题桥接原型已完成（结论可行）。M3 按 [roadmap](docs/product-roadmap.md) 推进：标签闭环、分类树（防环树锁 + 引用保护）与 Series（并发重排锁协议 + 公开系列页）**已交付**，接下来是 settings、RSS/sitemap、Post 回收站、备份恢复与正式主题函数。

M2 遗留（已知、未做）：

- 禁用 provider 时校验是否使最后 Owner 失去登录方式（需跨 settings 与 `oauth_accounts` 的检查）。
- `post.purge` / `post.transfer_author` 与所有权转移的重新认证流程。
- 角色编辑 API（当前只有分配/移除；内置 slug 保护与委派上限已就位）。
- 本地密码自助找回（邮箱一次性令牌 + 投递）与多实例共享的会话/限流存储；当前重置只走部署权限 CLI，限流计数为单实例内存。
