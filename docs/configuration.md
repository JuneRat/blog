# 配置参考

部署参数使用 **TOML + 环境变量覆盖**；运行期业务设置保存在 PostgreSQL `settings`；安装恢复记录仅在安装期间临时保存。配置入口位于 [server/config.rs](../crates/server/src/config.rs)，业务层不读取 TOML 或部署环境。

## 加载与优先级

配置文件位置：`--config` > `BLOG_CONFIG_FILE` > `config.toml`。字段优先级：**命令行参数 > 进程环境变量 > TOML > 内置默认值**。例如 `serve --addr` 覆盖 `BLOG_BIND` 和 `server.bind`。程序不自动读取 `.env`，需要 shell、开发工具或容器注入；修改父进程环境不会更新已启动服务。

完整模板见 [config.example.toml](../config.example.toml)，变量参考见 [.env.example](../.env.example)。所有来源的相对资源路径都以**进程工作目录**为基准；生产环境建议使用绝对路径并固定工作目录。

```bash
# 为新部署创建配置。
install -m 600 config.example.toml config.toml
cargo run -- config check
cargo run -- config show --sources
cargo run
```

本地默认把配置放在项目根目录，`data/` 仅存放媒体等运行数据。已有部署升级时，将原 `data/config.toml` 移到根目录；若安装尚未完成，同目录的 `config.install-state.json` 也要一起移动。也可继续通过 `--config` 或 `BLOG_CONFIG_FILE` 显式指定旧位置。Docker Compose 保持独立配置卷内的 `/var/lib/blog/config/config.toml`。

TOML 和安装记录须为普通文件，Unix 权限不得开放给组或其他用户（建议 `600`），不能使用符号链接。解析器拒绝未知字段、不支持的 `config_version`、空连接串和非法类型。布尔环境变量接受大小写不敏感的 `true`/`false`、`1`/`0`；拼错或空值报错。错误信息不打印包含凭据的 TOML 源码。

TOML 可以先只配置路径、代理或 `[bootstrap]`：没有数据库连接和安装记录时 `serve` 进入安装向导。显式配置的连接失败或文件损坏时不会回退到无配置安装。业务 CLI 必须通过 `database.url` 或 `DATABASE_URL` 提供连接；保留期维护默认复用该连接，也可显式覆盖，均没有隐式数据库地址。

## 启动配置

下列值在进程启动时解析，修改后重启服务或重新执行 CLI。TOML 的代理列表使用字符串数组，环境变量使用逗号分隔的精确 IP；不支持 CIDR。

| TOML 字段 | 环境变量 | 默认值 / 用途 |
|---|---|---|
| `database.url` | `DATABASE_URL` | PostgreSQL 业务连接；推荐用环境注入凭据 |
| `database.migrations_dir` | `BLOG_MIGRATIONS_DIR` | `migrations/postgres` |
| `maintenance.database_url` | `BLOG_MAINTENANCE_DATABASE_URL` | 可选独立维护连接；缺省复用有效的 `database.url`，显式错误不回退；独立维护凭据不注入 HTTP 服务 |
| `server.time_zone` | `BLOG_TIME_ZONE` | `UTC`；兼容旧配置，仅在数据库未保存站点时区时作为回退值（新部署请使用后台设置） |
| `server.bind` | `BLOG_BIND` | `127.0.0.1:8080`；支持 IPv4/IPv6 的 IP:端口 |
| `server.public_base_url` | `BLOG_PUBLIC_BASE_URL` | `http://127.0.0.1:8080`；公开链接、OAuth 回调与来源校验 |
| `server.trusted_proxies` | `BLOG_TRUSTED_PROXIES` | 空数组；登录/改密限流、评论与业务审计可信代理 |
| `server.secure_cookies` | `BLOG_SECURE_COOKIES` | 缺省时按公开 URL 是否为 HTTPS 推导；HTTPS 地址禁止设为 false |
| `paths.theme_dir` | `BLOG_THEME_DIR` | `themes/default`；扫描同级目录建立主题注册表，后台安装/卸载需父目录可写 |
| `paths.admin_dist` | `BLOG_ADMIN_DIST` | `apps/admin/dist`；安装要求存在 index.html |
| `paths.media_dir` | `BLOG_MEDIA_DIR` | `data/media` |
| 无（环境变量） | `TZ` | `UTC`；服务与 CLI 日志时区，与后台站点时区独立 |
| `logging.filter` | `RUST_LOG` | `info,sqlx=warn`；非法过滤表达式报错 |
| `logging.format` | `BLOG_LOG_FORMAT` | 原生默认 `text`，可选 `json`；Compose 默认 `json` |
| `metrics.bind` | `BLOG_METRICS_BIND` | 原生默认不监听；例如 `127.0.0.1:9090`，只对 serve 生效 |
| `recovery.enabled` | `BLOG_RECOVERY_MODE` | `false`；恢复核验只监听 loopback，停用自动发布 |

迁移目录须同时包含匹配的 SQL 文件与 `schema.json`；自定义路径应成套复制整个目录。路径按进程工作目录解析，生产部署可用绝对路径；Compose 镜像已设置为 `/opt/blog/migrations/postgres`。迁移文件不可改写，新增结构见[迁移演进](schema-migrations.md)。

`BLOG_PG_PORT` 仅用于开发数据库脚本；`BLOG_PG_WAIT_SECONDS` 控制就绪等待，默认 60 秒，范围 1–3600 秒。脚本使用 Python 3 对 Docker 探测设置单次超时，容器退出或超过截止时间时输出有限诊断并非零退出，不删除容器或数据卷。`BLOG_TEST_ADMIN_URL` 仅用于集成测试，默认 `postgres://blog:blog@127.0.0.1:5432/postgres`。它们不是应用部署字段。修改数据库端口时需同步调整连接串。

## 时区

首页每页文章数在后台「站点设置 → 常规设置」修改，存入 `settings.site.home_page_size`。默认 20，支持 1–100，保存后立即生效，无需重启，也不需要修改部署配置。

站点时区在后台「站点设置」选择并保存，存入数据库 `settings.site.time_zone`，默认 UTC。支持 IANA 名称（例如 `Asia/Shanghai`、`Europe/London`）；空值、未知名称和 `+08:00` 这样的固定偏移会被拒绝。保存沿用 `settings.manage` 权限和版本冲突检查，无需重启或修改 `config_version`。

公开文章、页面、评论、主题数据函数，以及后台列表、审计日志、恢复副本时间都按站点时区显示。预约发布时间和审计筛选输入也按此时区解释，控件标明时区，不依赖浏览器或宿主机设置。夏令时切换导致的不存在/重复时间必须重新选择，不静默调整。保存后当前后台窗口立即更新，其它已打开的后台窗口重新加载后采用新值。

数据库继续保存绝对时刻，API 使用带偏移的 RFC 3339；修改站点时区只改变显示和后续输入的解释，不改变已预约的时刻。CLI 结果、备份名称及 RSS/sitemap 继续使用 UTC。`/me` 和匿名评论列表返回当前站点 `time_zone`。Rust 二进制内置 IANA 规则，无需给容器额外安装 tzdata；前端使用浏览器的 IANA 规则。

进程日志（服务和 CLI）独立读取环境变量 `TZ`，未设置时为 UTC，支持相同的 IANA 名称；时间保留毫秒及显式偏移。Compose 可在现有 `.env` 中设置 `TZ=Asia/Shanghai` 后运行 `docker compose up -d blog`，恢复工具会保留该值。原生启动不自动读取 `.env`，使用 `TZ=Asia/Shanghai cargo run`，或在终端/进程管理器中导出 `TZ`。修改 `TZ` 需重启进程，后台站点设置不会改变日志时区。

兼容过渡：已有 `server.time_zone` / `BLOG_TIME_ZONE` 仍作为数据库缺少时区字段时的回退值，保留升级前的显示。后台第一次保存时将所选时区写入数据库，之后以数据库为准，可删除旧配置。旧 API 客户端省略 `time_zone` 时保留已保存的值。该字段是现有 JSON 设置的向后兼容扩展，无需拆分或重写初始迁移；随数据库备份恢复。

## 连接池、查询超时与数据库 TLS

参数对运行池与 CLI 池生效，支持现有 TOML 和 `.env`，不需要额外环境文件。首次安装的临时初始化池仍使用最多 2 个连接、30 秒语句限制和 10 秒锁限制；安装后的运行池使用下表。`serve` 未配置查询期限时使用 HTTP 默认；CLI 和维护未配置时继续沿用数据库限制。长迁移或批量维护可用独立进程环境覆盖限制。

| TOML 字段 | 环境变量 | 默认值 | 范围 / 语义 |
|---|---|---:|---|
| `database.max_connections` | `BLOG_DB_MAX_CONNECTIONS` | 5 | 1–1000 |
| `database.min_connections` | `BLOG_DB_MIN_CONNECTIONS` | 0 | 0–max_connections |
| `database.acquire_timeout_ms` | `BLOG_DB_ACQUIRE_TIMEOUT_MS` | 5000 | 1–120000 毫秒 |
| `database.idle_timeout_secs` | `BLOG_DB_IDLE_TIMEOUT_SECS` | 600 | 0–86400 秒；0 禁用连接空闲回收 |
| `database.max_lifetime_secs` | `BLOG_DB_MAX_LIFETIME_SECS` | 1800 | 0–86400 秒；0 禁用连接寿命限制 |
| `database.statement_timeout_ms` | `BLOG_DB_STATEMENT_TIMEOUT_MS` | serve：20000；CLI：0 | 0–86400000 毫秒；HTTP 默认不超过请求期限的 2/3 |
| `database.lock_timeout_ms` | `BLOG_DB_LOCK_TIMEOUT_MS` | serve：3000；CLI：0 | 0–86400000 毫秒；HTTP 默认不超过语句期限的 1/4 |
| `database.idle_in_transaction_timeout_ms` | `BLOG_DB_IDLE_IN_TRANSACTION_TIMEOUT_MS` | serve：60000；CLI：0 | 0–86400000 毫秒 |
| `database.connect_retries` | `BLOG_DB_CONNECT_RETRIES` | 3 | 0–10 次额外建连尝试 |
| `database.connect_retry_backoff_ms` | `BLOG_DB_CONNECT_RETRY_BACKOFF_MS` | 250 | 1–5000 毫秒，指数退避上限 5 秒 |

语句、锁、事务空闲限制显式设为 **0 时不覆盖 PostgreSQL 角色、数据库或 DSN 已有设置**，并非强制关闭服务器限制；部署方须确认继承的限制有效。非零时作为每条新连接的启动参数应用，连接替换后仍生效。`serve` 要求非零语句期限短于 HTTP 请求期限、非零锁期限短于非零语句期限；缩短请求期限会同步收紧未配置的数据库默认。`config show --for serve --sources` 展示派生后的值和 `default:serve` 来源，`--for database` 展示 CLI 策略。依据慢查询与维护耗时调整；大规模迁移应先使用独立 `migrate` 进程执行，再启动网站。池的 `idle_timeout_secs` 回收空闲连接，与 PostgreSQL 的“事务中空闲超时”不同。

HTTP 返回超时只表示停止等待处理器，不能当作 PostgreSQL 工作已取消的证明。数据库语句/锁期限负责终止已发送的 SQL；连接回收会先读完取消响应并完成待执行的事务回滚。回归覆盖慢语句、外部事务持锁和 TLS 下的请求取消，确认单连接池仍在数据库期限内恢复使用，取消事务的未提交写入不会保留。

重试仅发生在池首次建立时，且只针对临时网络故障、启动中或连接容量不足等错误；参数、凭据、证书错误不由应用层重试。每次尝试受获取超时约束，退避有上限。不自动重放 SQL 或事务，尤其不重放提交结果不确定的写入。SQLx 后续补充池连接仍遵循其内部连接管理策略。

连接总预算应涵盖 HTTP 进程、独立 CLI/维护、备份和其他应用，不能把每个副本的池大小都设为 PostgreSQL 的 `max_connections`。默认保持 5；用 [公开读取容量测试](public-read-capacity.md)比较真实负载。池满时 `/readyz`（以及 `/healthz`）可能在 2 秒后返回 503；`/livez` 不访问数据库。不要将 readiness 当成自动重启进程的依据。

SQLx 已启用 Rustls。连接公网/远程数据库时可在 `DATABASE_URL` 使用 `?sslmode=verify-full`；私有 CA 追加 `&sslrootcert=/容器内路径/ca.crt`，证书文件须只读挂载且运行用户可读。使用内置公共根证书认可的证书链时无需另设根证书文件。`sslmode=require` 只保证加密，不替代 `verify-full` 的服务器身份验证；默认连接行为不强制 TLS。Compose 自带数据库仍在私有网络，数据库 TLS 与公网 HTTP 的反代 TLS 是两条独立链路。

原生 `recovery.py` 同样保留 URL 中的 TLS、证书、认证、连接超时和会话参数，通过 [libpq 环境变量](https://www.postgresql.org/docs/current/libpq-envars.html) 传给所有 PostgreSQL 工具；切换管理库或恢复库时仅替换数据库名。不继承 `PGSERVICE`、`PGSERVICEFILE` 或 `PGHOSTADDR`，避免服务文件或另一目标地址覆盖已经校验的 URL。支持项见 `scripts/recovery.py` 的 `CONNECTION_OPTIONS`；未知、重复、空值或试图覆盖 URL 主机/数据库的查询参数会在建连前报错，不会静默忽略。`--docker-container` 模式使用容器本地 socket，拒绝带查询参数的 URL；需要指定网络 TLS 策略时使用原生工具。原生路径中的证书文件须在工具运行主机可读。

## 运行期设置与初始值

| 设置 | 权威来源 | 生效时机 |
|---|---|---|
| 标题、描述、Logo | `settings.site` | 后续请求 |
| 当前主题 | `settings.theme` | 后续请求；已验证的启动主题或后台新安装主题 |
| 评论开关 | `settings.comments` | 后续请求 |
| 评论 IP / 审计保留期 | `settings.comments.ip_retention_days` / `settings.audit.retention_days` | 下次维护任务，默认各 180 天 |
| 固定后台任务计划 | `task_schedules` | 后续领取；retention 默认每天且禁用，publish_due 固定 30 秒启用；HTML 可提交未来一次性请求 |
| OAuth 提供商配置、`secret_ref` | `settings.oauth` | 后续认证操作；密钥通过环境引用，变更环境密钥需要重启 |

标题和描述使用 **数据库已保存字段 > 内置默认值**。默认标题为 `Sun's Blog`，描述为 `一个 Rust 博客`。缺失字段回退；已保存的空描述是合法选择，不回退。公开读取失败时使用内置默认值，管理接口返回错误。后台未配置时仍显示 `source: "fallback"`、`version: 0`。

后台 retention 使用既有 `maintenance.database_url` / `BLOG_MAINTENANCE_DATABASE_URL` 选择规则，无覆盖时复用站点连接；与运行连接相同则复用池。显式维护连接失败或权限不足时，后台清理不可用，不阻止网站登录；错库连接不能通过任务租约校验，不会执行该任务的业务变更。配置专用维护连接意味着 HTTP 进程持有该角色限定的清理能力，不为受限应用账号补授审计删除权限。Compose 仍不将 `.env` 的维护覆盖传入 HTTP 服务；分离账号需由部署方在受保护的站点 TOML 中明确配置。计划、取消和恢复隔离规则见[后台任务管理](operations-and-recovery.md#后台任务管理)。

`[bootstrap] title` / `description` 只提供首次安装初始值，与首个 Admin 一起提交到数据库；启动现有站点时不重新应用。标题最多 200 字符且不能为空，描述最多 500 字符且允许为空，使用后台相同的校验规则。没有提供的初始字段不会强制写入。

Logo 只来自数据库。后台整组保存时省略或传 null 的 `logo_media_id` 会清除 Logo，并校验版本和媒体引用。主题回退行为保持不变：保存的主题未加载时使用默认主题，查询或执行失败则报错。安装/编辑主题文件需要重启；切换已加载主题不需要。渲染并发、缓存等策略仍由代码管理，未新增配置项。

## 安装状态

安装向导在 TOML 同目录临时保存 `config.install-state.json`（名称随配置文件 stem 变化），记录安装 ID 和发布前后的配置快照，用于中断恢复。数据库提交 Admin 和 `settings.installation` 完成标记后自动删除该日志；清理失败不影响安装成功，下次启动核对完成标记后重试。未完成安装或标记不匹配时保留日志。

常态运行只需要 TOML，完成标记保存在数据库，不参与配置覆盖。可以修改公开地址、连接凭据或恢复库地址，残留日志的快照不会替换正常编辑。TOML 需受控备份；未完成安装的临时日志同样含部署秘密，完成后的备份恢复无需携带它。

## 检查与命令边界

```bash
cargo run -- config check --for serve
cargo run -- config show --sources --for database
cargo run -- config check --for maintenance
cargo run -- config show --for resources
```

`check` 只校验字段和语义，不连接数据库、不检查资源是否完整、不写文件；不代表数据库可连接或主题可加载。`show` 输出 JSON，所有数据库连接均为 `[redacted]`，`--sources` 标出 env/TOML/default/推导来源及生效时机。维护连接复用站点配置时以 `fallback:env:DATABASE_URL` 或 `fallback:toml:路径` 标明来源。范围支持 `serve`（默认）、`database`、`maintenance`、`media`、`resources`、`all`；`all` 要求维护有可用连接，但无需单独配置。

各命令共享 TOML 语法、字段名称和日志配置解析，但按职责校验字段类型和值。`user`、`role`、`oauth`、`post`、`migrate`、`publish-due`、`rebuild-html` 不校验主题、公开 URL、代理和 Cookie 设置；`media cleanup-staging` 额外校验媒体目录。`maintenance` 使用显式维护连接或有效站点连接，共用连接池策略与恢复模式，不读取安装记录、不运行迁移；显式维护连接有效时不校验未使用的站点连接。

配置文件本身语法损坏、权限不合格或未知字段会报错；需要修复时可通过 `--config` 指向独立的最小维护 TOML。`rebuild-html` 保持显式执行，以上配置修改本身不触发 HTML 重建。

备份和媒体清理脚本的资源参数采用 **命令行 > env > TOML > 默认值**，统一通过已构建的 `blog config show --for resources` 解析，包括仅使用环境变量或默认值的部署。提供 `--config` / `--blog-bin` 可指定配置与程序路径；程序按 `--blog-bin`、PATH 中的 `blog`、仓库的 `target/debug/blog` 依次选择。数据库管理凭据仍必须显式使用 `DATABASE_URL`，不从 TOML 自动取得。详见[运维与恢复](operations-and-recovery.md)。

## 公开地址与代理

`BLOG_PUBLIC_BASE_URL` 必须是带主机的绝对 HTTP/HTTPS URL，不允许用户名、密码、查询参数、片段或根路径以外的路径前缀。例如 `https://blog.example.com` 有效，`https://example.com/blog` 不受支持。它是公开绝对链接的可信来源，不从请求 `Host` 推导。

监听地址和公开地址分别配置：反向代理终止 TLS 时，程序可以监听 `127.0.0.1:8080`，公开地址必须设为 `https://blog.example.com`，cookie 随公开地址启用 Secure。HTTPS 公开地址下显式设置 `server.secure_cookies=false`（或 `BLOG_SECURE_COOKIES=false`）会使配置校验和启动失败；HTTP 本地开发仍可使用默认值。不要从反向代理到程序的内部 HTTP 协议推断公开地址。

Secure 模式使用 `__Host-blog_session`（`Secure; HttpOnly; SameSite=Lax; Path=/`，无 Domain），只读取该名称，不回退到 `blog_session`。从旧版本升级的 HTTPS 用户需重新登录一次；HTTP 开发仍使用 `blog_session`。

主站点所有路由统一设置 `nosniff`、`X-Frame-Options: DENY`、`Referrer-Policy: same-origin` 和 CSP。公开主题禁止对象嵌入、限制 base 并禁止被嵌入；后台额外限制脚本和连接为本站，允许 antd 的内联样式与正文图片预览。HTTPS 公开地址启用 `Strict-Transport-Security: max-age=31536000`，不包含子域；判断不依赖请求 Host 或 X-Forwarded-Proto。

部署后检查 HTTPS `/admin/` 响应中的这些安全头，以及实际登录响应中的 `__Host-blog_session` 与 Secure 属性。代码测试不能替代生产反向代理和环境变量的核对。

浏览器写请求的 Origin 检查是另一条独立路径：后台将提供的 Origin 与请求 Host 比较，允许 `http://{Host}` 或 `https://{Host}`；未提供 Origin 时不执行该项检查，已认证写请求仍要求 CSRF token。代理须保持与浏览器入口一致的 Host。登录与自助改密限流使用同一可信代理解析结果；无法解析时回退 socket 对端桶，不跳过来源限流。评论提交/预览额外要求 Origin 与配置的公开地址精确匹配；评论与业务审计来源 IP 共用 `BLOG_TRUSTED_PROXIES`，从 X-Forwarded-For 右侧剥离可信代理，非法或未知来源留空，规则见[评论](comments.md#请求与来源地址)。认证规则见[身份与权限](identity-and-admin.md)。

业务审计通过显式上下文把已验证账号和来源 IP 传入写事务，不接受客户端请求体声明操作者/IP。可信代理链缺失、包含非法值、超过 20 个地址或没有可识别客户端时留空；非可信 socket 对端的转发头被忽略。审计 IP 随整条审计记录按 audit 保留期删除，评论 IP 单独按 comment 保留期清空。

## HTTP 期限与匿名请求准入

以下部署参数可写在 `[server]`；修改后重启，Compose 会传入对应环境变量。

| TOML 字段 | 环境变量 | 默认值 |
|---|---|---|
| `request_timeout_secs` | `BLOG_REQUEST_TIMEOUT_SECS` | 30 秒 |
| `upload_timeout_secs` | `BLOG_UPLOAD_TIMEOUT_SECS` | 120 秒，媒体上传及主题 ZIP 上传/验证 |
| `header_timeout_secs` | `BLOG_HEADER_TIMEOUT_SECS` | 10 秒 |
| `io_idle_timeout_secs` | `BLOG_IO_IDLE_TIMEOUT_SECS` | 30 秒，socket 读写停滞期限 |
| `connection_max_age_secs` | `BLOG_CONNECTION_MAX_AGE_SECS` | 300 秒，含持续缓慢传输和 keep-alive |
| `shutdown_timeout_secs` | `BLOG_SHUTDOWN_TIMEOUT_SECS` | 25 秒，含连接排空与数据库关闭 |
| `max_http_connections` | `BLOG_MAX_HTTP_CONNECTIONS` | 每个监听端口 1024 条 |

秒数必须为 1–3600；上传期限不小于普通请求期限，连接最长寿命必须大于上传与请求头期限。连接上限为 1–65536。长连接达到寿命后关闭，由客户端建立新连接。请求处理超时返回 408、`request_timeout` 和请求编号；写请求超时不保证操作尚未提交，重试前应读取结果并使用原有版本冲突机制。请求体读取计入处理期限，传输层另限制慢请求头和响应发送停滞。

关闭时立即停止接入并停止预约任务，公开与指标监听并行排空；80% 预算到期后取消残留连接，剩余预算用于关闭数据库池。Compose 的 `stop_grace_period` 为 30 秒；提高应用关闭预算时也需相应提高容器期限。

公开入口使用独立令牌桶：OAuth 发起每来源容量 10、全局 60；评论提交每来源 5、全局 120；预览每来源 20、全局 240。每分钟补满一桶，允许桶容量内的短突发；成功、失败和被后续校验拒绝的已准入请求均计数。超限返回 429 和 `Retry-After`，公开读取不计入这些桶。OAuth 尝试池满时保留已接受的状态，拒绝新状态并提示重试时间；不淘汰仍有效的登录。

来源使用 `trusted_proxies` 与 socket 对端解析；不信任任意 `X-Forwarded-For`，代理转发信息缺失时回落到代理地址，来源完全未知时使用共享桶。准入计数与 OAuth 临时状态仍是单进程状态；多副本必须在网关统一限流并另行解决回调状态共享。

## 账号邮件

账号邀请和密码找回使用 `[mail]`。不配置 `host` 时关闭邮件提交能力，并保留本地密码登录及 CLI 重置。修改后重启服务。Compose 已透传下列环境变量。

| TOML | 环境变量 | 含义 |
|---|---|---|
| `mail.host` | `BLOG_SMTP_HOST` | SMTP 主机 |
| `mail.port` | `BLOG_SMTP_PORT` | 端口，默认 587；隐式 TLS 通常使用 465 |
| `mail.security` | `BLOG_SMTP_SECURITY` | 默认 `starttls`（必须升级 TLS）；`tls` 为隐式 TLS |
| `mail.from` | `BLOG_SMTP_FROM` | 必填发件地址，可写 `博客 <noreply@example.com>` |
| `mail.username` | `BLOG_SMTP_USERNAME` | 可选认证用户名，必须与密码成对配置 |
| `mail.password` | `BLOG_SMTP_PASSWORD` | SMTP 密码；推荐从受保护环境注入 |
| `mail.ca_pem` | `BLOG_SMTP_CA_PEM` | 可选的私有 CA PEM 文本或证书链，最大 64 KiB；不是文件路径，仅适用于 `tls` / `starttls` |

TLS 校验证书且不自动降级为明文。`local` 仅用于回环地址的开发 SMTP，不允许认证。每次投递总期限 12 秒，错误日志不打印 SMTP 返回、收件地址或邮件内容。`config show` 对用户名、密码脱敏。请确保 `server.public_base_url` 为用户可访问的正式 HTTPS 根地址，邮件链接只从该配置生成，不信任请求 Host。

默认使用内置公共 CA。私有邮件服务可在 `mail.ca_pem` 中提供 PEM 证书（建议使用 TOML 多行字符串），为该 SMTP 连接额外增加信任；仍验证服务器主机名和有效期，不修改系统、数据库或其他 HTTP 客户端的信任。不要填写私钥，格式无效、超长或明文模式中的 CA 配置会阻止启动。CA 文本随配置／Compose 环境一起进入加密备份，恢复不依赖原主机的证书文件路径。

SMTP 配置、凭据与发件域名验证由部署方提供；可先使用测试邮箱验证收件。此功能没有自动重试队列，失败或服务中断时重新申请即可。恢复核验模式不会发送邮件。
