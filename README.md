# blog

Rust 模块化单体博客。当前进度：**M1 内容闭环 + M2 RBAC 核心**（迁移 → 受控 CLI 写入（按权限） → 公开 SSR 阅读）；OAuth、内存会话与 React 后台为 M2 后续部分。

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
- 身份/角色变更在统一 `pg_advisory_xact_lock(2048001,1)` 排他锁下执行（docs/identity-and-admin.md §3）。

环境变量：

| 变量 | 默认 | 说明 |
|---|---|---|
| `DATABASE_URL` | `postgres://blog:blog@127.0.0.1:5432/blog` | PostgreSQL 连接 |
| `BLOG_BIND` | `127.0.0.1:8080` | serve 监听地址（`--addr` 优先） |
| `BLOG_THEME_DIR` | `themes/default` | 主题目录（模板 + assets） |
| `BLOG_MIGRATIONS_DIR` | `migrations/postgres` | 迁移目录 |
| `BLOG_SITE_TITLE` / `BLOG_SITE_DESCRIPTION` | Sun's Blog / 一个 Rust 博客 | 站点信息（M3 迁入 settings） |

## 测试

```bash
cargo test --workspace
# 或本地提交前一键检查（fmt + clippy -D warnings + 测试）
./scripts/check.sh
```

- `crates/domain`：聚合与值对象规则（无数据库）。
- `crates/application`：用例 + 内存 fake（权限、版本冲突、真并发 join!）。
- `crates/infrastructure/tests`：真实 PostgreSQL（迁移、约束、三态保存、两连接真并发、公开过滤）。
- `crates/server/tests`：完整装配 + HTTP（草稿/private/软删除不可访问，撤回即 404，标题/摘要模板转义）。

集成测试需要可写的 PostgreSQL，且**只允许 loopback 主机**：默认 `postgres://blog:blog@127.0.0.1:5432`，可用 `BLOG_TEST_ADMIN_URL` 覆盖（测试库 DSN 自动从它推导），会重建 `blog_test` / `blog_server_test` 数据库。CI 见 `.github/workflows/ci.yml`。

## 结构

```text
crates/
├── domain          # 聚合、值对象、业务规则（无框架依赖）
├── application     # 用例、端口、DTO（定义出站接口）
├── infrastructure  # SQLx 持久化、MiniJinja 渲染、Markdown 清洗
├── interfaces      # 公开 HTTP 路由 + 受控 CLI（不依赖 infrastructure）
└── server          # 装配入口（bin: blog）
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

## 下一步（M2 前置）

OAuth（OIDC/GitHub）、RBAC（own/any 权限替换 CLI 归属检查）、React 后台。会话与 OAuth 临时状态使用单实例内存存储。
