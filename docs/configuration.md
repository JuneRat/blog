# 配置参考

部署参数使用 **TOML + 环境变量覆盖**；运行期业务设置保存在 PostgreSQL `settings`；安装恢复记录单独保存。配置入口位于 [server/config.rs](../crates/server/src/config.rs)，业务层不读取 TOML 或部署环境。

## 加载与优先级

配置文件位置：`--config` > `BLOG_CONFIG_FILE` > `data/config.toml`。字段优先级：**命令行参数 > 进程环境变量 > TOML > 内置默认值**。例如 `serve --addr` 覆盖 `BLOG_BIND` 和 `server.bind`。程序不自动读取 `.env`，需要 shell、开发工具或容器注入；修改父进程环境不会更新已启动服务。

完整模板见 [config.example.toml](../config.example.toml)，变量参考见 [.env.example](../.env.example)。所有来源的相对资源路径都以**进程工作目录**为基准；生产环境建议使用绝对路径并固定工作目录。

```bash
# 为新部署创建配置。
install -d -m 700 data
install -m 600 config.example.toml data/config.toml
cargo run -p server -- config check
cargo run -p server -- config show --sources
cargo run -p server -- serve
```

TOML 和安装记录须为普通文件，Unix 权限不得开放给组或其他用户（建议 `600`），不能使用符号链接。解析器拒绝未知字段、不支持的 `config_version`、空连接串和非法类型。布尔环境变量接受大小写不敏感的 `true`/`false`、`1`/`0`；拼错或空值报错。错误信息不打印包含凭据的 TOML 源码。

TOML 可以先只配置路径、代理或 `[bootstrap]`：没有数据库连接和安装记录时 `serve` 进入安装向导。显式配置的连接失败或文件损坏时不会回退到无配置安装。业务 CLI 必须通过 `database.url` 或 `DATABASE_URL` 提供连接；保留期维护必须配置独立维护连接，均没有隐式数据库地址。

## 启动配置

下列值在进程启动时解析，修改后重启服务或重新执行 CLI。TOML 的代理列表使用字符串数组，环境变量使用逗号分隔的精确 IP；不支持 CIDR。

| TOML 字段 | 环境变量 | 默认值 / 用途 |
|---|---|---|
| `database.url` | `DATABASE_URL` | PostgreSQL 业务连接；推荐用环境注入凭据 |
| `database.migrations_dir` | `BLOG_MIGRATIONS_DIR` | `migrations/postgres` |
| `maintenance.database_url` | `BLOG_MAINTENANCE_DATABASE_URL` | 维护任务独立连接，无兜底；不应把维护凭据注入 HTTP 服务 |
| `server.bind` | `BLOG_BIND` | `127.0.0.1:8080`；支持 IPv4/IPv6 的 IP:端口 |
| `server.public_base_url` | `BLOG_PUBLIC_BASE_URL` | `http://127.0.0.1:8080`；公开链接、OAuth 回调与来源校验 |
| `server.trusted_proxies` | `BLOG_TRUSTED_PROXIES` | 空数组；评论与业务审计可信代理 |
| `server.secure_cookies` | `BLOG_SECURE_COOKIES` | 缺省时按公开 URL 是否为 HTTPS 推导 |
| `paths.theme_dir` | `BLOG_THEME_DIR` | `themes/default`；扫描同级目录建立主题注册表 |
| `paths.admin_dist` | `BLOG_ADMIN_DIST` | `apps/admin/dist`；安装要求存在 index.html |
| `paths.media_dir` | `BLOG_MEDIA_DIR` | `data/media` |
| `logging.filter` | `RUST_LOG` | `info,sqlx=warn`；非法过滤表达式报错 |
| `recovery.enabled` | `BLOG_RECOVERY_MODE` | `false`；恢复核验只监听 loopback，停用自动发布 |

`BLOG_PG_PORT` 仅用于开发数据库脚本；`BLOG_TEST_ADMIN_URL` 仅用于集成测试，默认 `postgres://blog:blog@127.0.0.1:5432/postgres`。它们不是应用部署字段。修改数据库端口时需同步调整连接串。

## 运行期设置与初始值

| 设置 | 权威来源 | 生效时机 |
|---|---|---|
| 标题、描述、Logo | `settings.site` | 后续请求 |
| 当前主题 | `settings.theme` | 后续请求；仅限启动时已加载的主题 |
| 评论开关 | `settings.comments` | 后续请求 |
| 评论 IP / 审计保留期 | `settings.comments.ip_retention_days` / `settings.audit.retention_days` | 下次维护任务，默认各 180 天 |
| OAuth 提供商配置、`secret_ref` | `settings.oauth` | 后续认证操作；密钥通过环境引用，变更环境密钥需要重启 |

标题和描述使用 **数据库已保存字段 > 内置默认值**。默认标题为 `Sun's Blog`，描述为 `一个 Rust 博客`。缺失字段回退；已保存的空描述是合法选择，不回退。公开读取失败时使用内置默认值，管理接口返回错误。后台未配置时仍显示 `source: "fallback"`、`version: 0`。

`[bootstrap] title` / `description` 只提供首次安装初始值，与首个 Owner 一起提交到数据库；启动现有站点时不重新应用。标题最多 200 字符且不能为空，描述最多 500 字符且允许为空，使用后台相同的校验规则。没有提供的初始字段不会强制写入。

Logo 只来自数据库。后台整组保存时省略或传 null 的 `logo_media_id` 会清除 Logo，并校验版本和媒体引用。主题回退行为保持不变：保存的主题未加载时使用默认主题，查询或执行失败则报错。安装/编辑主题文件需要重启；切换已加载主题不需要。渲染并发、缓存等策略仍由代码管理，未新增配置项。

## 安装状态

安装向导在 TOML 同目录保存 `config.install-state.json`（名称随配置文件 stem 变化）。安装记录是内部 JSON 状态，保存安装 ID 和发布前后的配置快照，只用于中断恢复，不参与正常参数覆盖。TOML 发布后可以修改公开地址、连接凭据或恢复库地址；旧快照不会替换这些正常编辑。两个文件都含部署秘密，需单独受控备份。

## 检查与命令边界

```bash
cargo run -p server -- config check --for serve
cargo run -p server -- config show --sources --for database
cargo run -p server -- config check --for maintenance
cargo run -p server -- config show --for resources
```

`check` 只校验字段和语义，不连接数据库、不检查资源是否完整、不写文件；不代表数据库可连接或主题可加载。`show` 输出 JSON，所有数据库连接均为 `[redacted]`，`--sources` 标出 env/TOML/default/推导来源及生效时机。范围支持 `serve`（默认）、`database`、`maintenance`、`media`、`resources`、`all`；`all` 同时要求独立维护连接。

各命令共享 TOML 语法、字段名称和日志配置解析，但按职责校验字段类型和值。`user`、`role`、`oauth`、`post`、`migrate`、`publish-due`、`rebuild-html` 不校验主题、公开 URL、代理和 Cookie 设置；`media cleanup-staging` 额外校验媒体目录。`maintenance` 只取独立维护连接与恢复模式，不回退到业务 DSN、不读取安装记录、不运行迁移。

配置文件本身语法损坏、权限不合格或未知字段会报错；需要修复时可通过 `--config` 指向独立的最小维护 TOML。`rebuild-html` 保持显式执行，以上配置修改本身不触发 HTML 重建。

备份和媒体清理脚本的资源参数采用 **命令行 > env > TOML > 默认值**，统一通过已构建的 `blog config show --for resources` 解析，包括仅使用环境变量或默认值的部署。提供 `--config` / `--blog-bin` 可指定配置与程序路径；程序按 `--blog-bin`、PATH 中的 `blog`、仓库的 `target/debug/blog` 依次选择。数据库管理凭据仍必须显式使用 `DATABASE_URL`，不从 TOML 自动取得。详见[运维与恢复](operations-and-recovery.md)。

## 公开地址与代理

`BLOG_PUBLIC_BASE_URL` 必须是带主机的绝对 HTTP/HTTPS URL，不允许用户名、密码、查询参数、片段或根路径以外的路径前缀。例如 `https://blog.example.com` 有效，`https://example.com/blog` 不受支持。它是公开绝对链接的可信来源，不从请求 `Host` 推导。

监听地址和公开地址分别配置：反向代理终止 TLS 时，程序可以监听 `127.0.0.1:8080`，公开地址设为 `https://blog.example.com`，cookie 默认随公开地址启用 Secure。

浏览器写请求的 Origin 检查是另一条独立路径：后台将提供的 Origin 与请求 Host 比较，允许 `http://{Host}` 或 `https://{Host}`；未提供 Origin 时不执行该项检查，已认证写请求仍要求 CSRF token。代理须保持与浏览器入口一致的 Host。认证登录限流仍读取 socket 对端。评论提交/预览额外要求 Origin 与配置的公开地址精确匹配；评论与业务审计来源 IP 共用 `BLOG_TRUSTED_PROXIES`，从 X-Forwarded-For 右侧剥离可信代理，非法或未知来源留空，规则见[评论](comments.md#请求与来源地址)。认证规则见[身份与权限](identity-and-admin.md)。

业务审计通过显式上下文把已验证账号和来源 IP 传入写事务，不接受客户端请求体声明操作者/IP。可信代理链缺失、包含非法值、超过 20 个地址或没有可识别客户端时留空；非可信 socket 对端的转发头被忽略。审计 IP 随整条审计记录按 audit 保留期删除，评论 IP 单独按 comment 保留期清空。
