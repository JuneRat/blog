# 开发指南

本文负责本地运行、CLI、后台联调和检查流程。完整环境变量见[配置参考](configuration.md)，业务规则见[内容生命周期](content-lifecycle.md)，接口见[管理 API](admin-api.md)。

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
blog media reclaim
```

命令使用 `BLOG_MEDIA_DIR`，重试未完成的回收，并清扫暂存目录中满足宽限条件的残留；当前不会扫描正式媒体目录中的无记录孤儿。状态、引用与删除规则见[内容生命周期](content-lifecycle.md)，数据保护流程见[运维与恢复](operations-and-recovery.md)。

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
