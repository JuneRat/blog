# 配置参考

程序读取进程环境及安装向导保存的 `BLOG_CONFIG_FILE`，**不会自动加载 `.env`**。[.env.example](../.env.example) 用于查阅变量；需要由 shell、开发工具或部署环境注入。相对路径从进程工作目录解析，本地开发建议在仓库根目录启动。

`DATABASE_URL`、`BLOG_PUBLIC_BASE_URL` 各自优先于配置文件中的对应值。`serve` 在没有数据库环境变量和配置文件时进入[首次安装](installation.md)；已有配置无效、文件无法读取、权限过宽或数据库连接失败时直接报错，不回退到安装页。其他业务 CLI 在两者均缺失时仍使用本地开发 DSN。`maintenance` 只读独立维护 DSN，不读取安装文件。

```bash
export DATABASE_URL='postgres://blog:blog@127.0.0.1:5432/blog'
export BLOG_SITE_TITLE='我的博客'
cargo run -p server -- serve
```

当前配置入口是 [server/config.rs](../crates/server/src/config.rs)，命令依赖装配见[架构](architecture.md)。

评论开关使用 `settings.comments`，可信代理配置已支持。评论 IP/审计保留期分别使用 `settings.comments.ip_retention_days`、`settings.audit.retention_days`，默认各 180 天，可在后台设置中调整。清理由独立维护任务执行，见[保留期与运维](operations-and-recovery.md)。

## 环境变量

| 变量 | 默认值 | 生效范围与用途 |
|---|---|---|
| `DATABASE_URL` | 安装文件中的地址；CLI 最后回退到 `postgres://blog:blog@127.0.0.1:5432/blog` | 所有业务命令的 PostgreSQL 连接；显式提供时 serve 按已有部署启动 |
| `BLOG_CONFIG_FILE` | `data/config.json` | 安装连接配置与安装 ID；安装写入权限 600，不能是符号链接；包括 CLI 在内共同读取 |
| `BLOG_MAINTENANCE_DATABASE_URL` | 无，必填 | 仅 `maintenance`；使用独立受限维护角色，不回退到 DATABASE_URL |
| `BLOG_RECOVERY_MODE` | `0` | `1`/`true` 启用核验模式，serve 只监听 loopback、停用自动发布；`0`/`false` 关闭，其余非空值拒绝 |
| `BLOG_MIGRATIONS_DIR` | `migrations/postgres` | 所有业务命令使用的结构迁移目录 |
| `BLOG_BIND` | `127.0.0.1:8080` | `serve` 监听地址；命令行 `--addr` 优先 |
| `BLOG_PUBLIC_BASE_URL` | 安装文件中的地址，最后回退到 `http://127.0.0.1:8080` | `serve` 的公开基础 URL，供 OAuth 回调、canonical、RSS 和 sitemap 使用 |
| `BLOG_TRUSTED_PROXIES` | 空 | `serve` 评论与业务审计来源 IP 可信代理列表，逗号分隔精确 IP，不支持 CIDR；仅解析可信 socket 对端提供的 X-Forwarded-For |
| `BLOG_THEME_DIR` | `themes/default` | `serve` 的默认主题目录；从同级目录发现其他已安装主题 |
| `BLOG_ADMIN_DIST` | `apps/admin/dist` | `serve` 的后台构建产物；已有部署目录不存在时不挂载 `/admin`，首次安装要求存在 index.html |
| `BLOG_MEDIA_DIR` | `data/media` | `serve` 与 `media cleanup-staging` 使用的文件根目录 |
| `BLOG_SITE_TITLE` | `Sun's Blog` | `serve` 的站点标题回退值 |
| `BLOG_SITE_DESCRIPTION` | `一个 Rust 博客` | `serve` 的站点描述回退值 |
| `BLOG_SECURE_COOKIES` | 按公开 URL 是否为 HTTPS 推断 | `serve` 的 cookie Secure 开关；显式值为 `1` 或不区分大小写的 `true` 时启用，其余显式值关闭 |
| `RUST_LOG` | `info,sqlx=warn` | 服务端日志过滤器 |
| `BLOG_PG_PORT` | `5432` | 仅 `scripts/dev-db.sh` 新建容器时使用的宿主端口 |
| `BLOG_TEST_ADMIN_URL` | `postgres://blog:blog@127.0.0.1:5432/postgres` | 集成测试的本地管理连接，用来创建和重建测试库 |

OAuth 提供商通过 `secret_ref` 引用任意命名的环境变量，例如 `IDP_SECRET`、`GH_SECRET`；这些是配置示例，不是固定的内置变量。数据库只保存引用名，不保存 client secret 的明文。

修改数据库端口时，同步设置 `DATABASE_URL` 和 `BLOG_TEST_ADMIN_URL`。`BLOG_PG_PORT` 不会替应用修改连接串，也不会改变已存在容器的映射。

## 站点设置优先级

标题和描述按以下顺序解析：

1. 数据库 `settings.site` 中已保存的字段。
2. `BLOG_SITE_TITLE` / `BLOG_SITE_DESCRIPTION` 提供的启动回退值。
3. 内置默认值。

没有数据库配置时后台显示 `source: "fallback"`、`version: 0`。首次保存后由数据库接管，即使保存值等于回退值也会建立配置行。行内缺字段时按字段回退；空描述是合法值，不回退。公开读取遇到设置存储错误时整体使用启动回退值，管理接口则返回错误。

站点 logo 只来自数据库设置。`PUT /api/admin/v1/settings/site` 是整组替换，省略或传 `null` 的 `logo_media_id` 会清除 logo。保存携带版本并受并发检查，具体载荷见[管理 API](admin-api.md)。

主题使用独立的 `settings.theme` 分组。已保存且在启动注册表中的主题优先；未配置或对应主题未加载时使用默认主题，后台同时显示所选主题与实际生效主题。设置查询或模板执行失败会返回错误，不触发这项回退。主题发现、清单和资源路径见[主题与渲染](themes-and-rendering.md)。

上述设置在请求时读取，保存后不需要重启；环境回退值与已加载的主题注册表在启动时确定。

## 公开地址与代理

`BLOG_PUBLIC_BASE_URL` 必须是带主机的绝对 HTTP/HTTPS URL，不允许用户名、密码、查询参数、片段或根路径以外的路径前缀。例如 `https://blog.example.com` 有效，`https://example.com/blog` 不受支持。它是公开绝对链接的可信来源，不从请求 `Host` 推导。

监听地址和公开地址分别配置：反向代理终止 TLS 时，程序可以监听 `127.0.0.1:8080`，公开地址设为 `https://blog.example.com`，cookie 默认随公开地址启用 Secure。

浏览器写请求的 Origin 检查是另一条独立路径：后台将提供的 Origin 与请求 Host 比较，允许 `http://{Host}` 或 `https://{Host}`；未提供 Origin 时不执行该项检查，已认证写请求仍要求 CSRF token。代理须保持与浏览器入口一致的 Host。认证登录限流仍读取 socket 对端。评论提交/预览额外要求 Origin 与配置的公开地址精确匹配；评论与业务审计来源 IP 共用 `BLOG_TRUSTED_PROXIES`，从 X-Forwarded-For 右侧剥离可信代理，非法或未知来源留空，规则见[评论](comments.md#请求与来源地址)。认证规则见[身份与权限](identity-and-admin.md)。

业务审计通过显式上下文把已验证账号和来源 IP 传入写事务，不接受客户端请求体声明操作者/IP。可信代理链缺失、包含非法值、超过 20 个地址或没有可识别客户端时留空；非可信 socket 对端的转发头被忽略。审计 IP 随整条审计记录按 audit 保留期删除，评论 IP 单独按 comment 保留期清空。

## 命令配置边界

| 命令 | 加载范围 |
|---|---|
| `migrate` | 数据库与迁移；只执行结构迁移或校验 |
| `rebuild-html` | 普通数据库连接、结构迁移或校验及正文/评论渲染；显式重建旧版本 HTML；恢复隔离期间拒绝执行 |
| `user` / `role` / `oauth` | 数据库、结构迁移及对应身份依赖；不加载站点主题和公开 URL |
| `post` | 数据库、结构迁移及本次写入的 Markdown 渲染；不扫描旧版本 HTML 或加载网站模板 |
| `media cleanup-staging` | 数据库、结构迁移和媒体目录 |
| `maintenance` | 独立维护连接、保留期策略及恢复标记；不迁移、不重建 HTML、不加载网站配置 |
| `publish-due` | 数据库、结构迁移与预约发布；不重建 HTML；恢复隔离期间拒绝执行 |
| `serve` | 数据库、结构迁移与完整站点配置、主题、后台静态资源；不重建历史 HTML |

因此坏掉的主题或公开 URL 不会阻止 CLI 修复账号、角色与 OAuth 配置。渲染并发、排队时间、结果等待和缓存容量目前由代码中的执行策略控制，不存在对应的 `BLOG_*` 环境变量，详见[主题与渲染](themes-and-rendering.md)。

媒体文件与数据库共同构成恢复单元。恢复脚本读取 `--media-dir`，未指定时使用 `BLOG_MEDIA_DIR`，再回退到 `data/media`。操作与限制见[运维与恢复](operations-and-recovery.md)。
