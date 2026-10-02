# 保留期、媒体清理与备份恢复

当前工具依据[共享结构清单](../migrations/postgres/schema.json)核对当前版本，初始结构为 [19 表基线](database-design.md)，后续任务管理追加 `task_runs` 与 `task_schedules`，主题配置追加 `themes`，插件配置追加 `plugins` 与 `plugin_runtime`，账号邮件追加 `account_links`，当前为 25 张应用表。迁移不可变与跨版本恢复步骤见[迁移演进](schema-migrations.md)。采用维护窗口备份和隔离恢复；部署验收仍见[路线图](product-roadmap.md)，不提供在线一致备份或零数据丢失承诺。

Docker Compose 部署优先使用[Compose 备份恢复入口](compose-backup.md)：自动编排停写、命名卷读取、age 公钥加密、独立项目恢复和 HTTP 核验，并支持定时执行及 restic 异地副本。先在现有 `.env` 配置备份公钥，私钥单独保存；新归档不携带原数据库或异地仓库凭据。下文保留通用宿主机工具的操作方式。

## 数据库账号与保留期

数据库角色与博客 Admin/Admin 是不同层次的权限。默认使用一个非超级用户的博客专用账号，拥有本站数据库结构，承担启动迁移、日常业务与保留期维护；Compose 中为 `blog_owner`。安装后无需创建额外账号或执行授权脚本。它拥有的权限能直接修改审计，默认模式的审计追加规则由应用代码保证。

以下是需要数据库强制权限隔离时的可选分工：

| 身份 | 用途 |
|---|---|
| 结构管理账号 | 迁移、授权、备份和显式媒体清理；分离模式下不保留在 HTTP 服务配置中。恢复到新库还需要具备建库权限的管理账号 |
| 普通运行账号 | 必要业务读写；audit_logs 仅 SELECT/INSERT，无建表权限 |
| 独立维护账号 | 读取保留期、清空评论 IP、删除过期审计和写入清理摘要；不能读取评论邮箱或改正文 |

选择分离模式时，先用结构管理账号迁移，再由有建角色权限的管理员创建两个不拥有对象、不继承其他角色的 LOGIN 角色。密码通过 psql 的交互密码命令设置，不放进命令历史。以结构管理账号执行：

```sh
psql "$DATABASE_URL" -v app_role=blog_app -v maintenance_role=blog_maintenance \
  -f scripts/database-roles.sql
```

[授权脚本](../scripts/database-roles.sql)在同一事务内重设两角色的表授权，拒绝超级用户、对象所有者和额外审计修改权限；不会替已有库撤销 PUBLIC 授权，异常授权由部署管理员核对修正。受限运行账号启动时只核对全部迁移版本、成功状态和校验和；发现不匹配便退出，由管理账号先执行迁移。新增表或调整权限时修改 `migrations/postgres/schema.json`，运行 `python3 -B scripts/schema_contract.py --write` 生成授权脚本和 DDL 参考，不能手改生成文件。脚本先核对完整表集合，再清除两角色旧的表级及列级授权并重新授予；CI 还核对实际有效权限。

后台“设置 → 数据保留期”要求 settings.manage，默认评论 IP、审计各保留 **180 天**。范围为 1–36,500 整数天，分别保存到 settings.comments.ip_retention_days、settings.audit.retention_days，并校验两组版本、保留其他字段。缩短保留期会在下次维护时清理此前仍保留的数据。

维护优先读取 `BLOG_MAINTENANCE_DATABASE_URL`，其次是 TOML 的 `maintenance.database_url`；两者都未设置时复用有效的 `database.url`（`DATABASE_URL` 优先于 TOML）。显式维护连接为空、无效或连接失败时不回退。维护不执行迁移、权限初始化或 HTML 重建：

```sh
# 默认使用站点连接，先查看预计处理量。
blog maintenance --dry-run
blog maintenance --batch-size 1000 --max-batches 100
```

按 created_at 严格早于截止时间处理：评论仅置空 IP，不改正文、审核状态、关系、version 或 updated_at；过期审计被永久删除。每批最多分别处理指定数量的两类记录，同事务追加不含个人信息的清理计数。审计追加失败则整批回滚。多维护进程按事务锁串行，评论遇到锁定行时跳过。JSON 结果含 comment_ips、audit_logs、batches、has_more、dry_run；has_more=true 表示达到批次上限或仍有锁定记录，可再次执行。

每日调度示例为 [service](../ops/blog-maintenance.service) 与 [timer](../ops/blog-maintenance.timer)。它通过 Compose 启动独立维护容器，与备份共用操作锁，默认只读挂载安装配置并复用站点连接；按部署修改路径和用户即可。分离模式在现有 `.env` 设置 `BLOG_MAINTENANCE_DATABASE_URL`（数据库主机为 `db`），此覆盖项不会传入 HTTP 服务。手工执行使用 `sh scripts/compose-backup.sh maintenance`。仓库不会安装或启用这些服务。恢复隔离期间拒绝执行维护。

保留期不清理备份副本；备份保留规则另行制定。正式媒体文件也不属于此命令：零引用仍可能有站外链接。blog media cleanup-staging 仅清理过期暂存文件；正式对象按下面的显式计划清理。

## HTML 显式重建

文章、页面和评论保存时仍同步生成清洗后的 HTML。正文的 `content_render_version` 同时标记 Markdown 转换、HTML 清洗和媒体引用提取规则。版本 2 按浏览器规则规范化根相对图片路径中的点段、反斜杠和 ASCII 空白，再将查询串、fragment 和一次解码后有效的 `/media/{uuid}` 图片路径计入引用；已有版本 1 的落库 HTML 必须显式补建。启动本身不创建重建请求，`migrate`、`post` 和 `publish-due` 不扫描或刷新历史版本；已明确提交的 queued 重建按计划执行。规则升级后，也可由部署流程使用新版本二进制执行：

```sh
# 结构管理账号先迁移；运行账号只能校验已应用的结构。
blog migrate
# 只读核对迁移状态并分别统计待重建数量，不渲染或写入。
blog rebuild-html --dry-run
# 使用具有业务读写权限的 DATABASE_URL，或已保存的安装连接，分次执行。
blog rebuild-html --batch-size 100 --max-batches 100
```

执行模式先进行结构迁移或校验；`--dry-run` 即使使用结构管理账号也只读校验已应用的迁移，不建表、不创建 SQLx 历史记录。未迁移或校验和不匹配时直接报错。两种模式均不初始化权限注册表，也不加载主题、公开 URL 或后台资源。命令不使用 `BLOG_MAINTENANCE_DATABASE_URL`：保留期维护账号没有修改正文和媒体引用的权限。恢复隔离标记存在或 `BLOG_RECOVERY_MODE=1` 时拒绝执行，先完成恢复核验并解除隔离。

重建顺序为文章、页面、评论；`--batch-size` 默认 100，`--max-batches` 默认 100，两者范围均为 1–1,000。三类内容共用批次上限，单次最多检查 `batch-size × max-batches` 条记录；批次数包含空批次和失败批次。每类按 UUID 升序推进游标，一轮内不会反复处理同一条冲突记录。在事务外渲染后，按源文和编辑版本 CAS 提交；HTML、渲染版本、正文媒体引用与审计同事务更新，即使新旧 HTML 字节相同也补建引用，编辑版本、业务更新时间和发布状态保持不变。旧 HTML 或原封面已包含的图片即使后来进入媒体回收站，也可补回同一来源的历史关系；该例外仅用于重建，不允许普通编辑把软删除媒体加入新来源。不存在的媒体仍使重建失败。

标准输出为 JSON，运行日志（包括 `RUST_LOG` 开启的调试日志）进入标准错误，字段如下：

| 字段 | 含义 |
|---|---|
| `rebuilt` | 本轮已确认提交数量，按 `posts`、`pages`、`comments` 分列 |
| `skipped` | CAS 未提交数量，包括被并发修改、已由其他进程重建或删除的记录 |
| `pending` | 正常结束时三类待重建数量的只读快照；`--dry-run` 仅返回这一统计，`rebuilt`/`skipped`/`batches` 为零 |
| `batches` / `dry_run` | 实际尝试批次数及是否预检 |
| `has_more` | 是否仍有旧版本记录；失败且剩余未知时保守返回 `true` |
| `failure` | 成功为 `null`；失败包含 `kind`、`id`、`message`，非记录级错误的定位字段可为 `null` |

退出码为 0 表示本轮正常结束，仍须检查 `has_more`；为 `true` 时可继续运行相同命令。下一轮重新从剩余旧版本记录开始，已提交记录自动跳过，无需保存外部游标。并发导致的跳过不等于仍待重建，最终以 `pending` 为准；每次统计是独立快照，不承诺冻结全站状态。

重建失败时，标准输出仍保留已确认完成数量和失败类型/UUID，`pending=null`；标准错误报告原因并非零退出。修复源文、媒体引用或渲染故障后重跑即可继续。失败事务不提交，先前提交保持生效；若连接在提交确认时中断，计数只包含已确认提交部分，重跑会重新判断实际渲染版本。参数、连接或结构校验失败发生在用例之前，只输出错误，不产生重建结果 JSON。

清洗安全规则升级应在恢复公开访问前完成重建，并确认 `has_more=false`；未完成时公开页面仍使用旧的已存储 HTML，不会自动转换，也不回退到源文。

### 在管理后台执行

具有 `settings.manage` 的用户可在「任务管理」的 HTML 重建页查看文章、页面、评论的待重建数量，立即执行或安排未来时间的一次性计划。后台调用同一重建用例，固定每批 100 条、最多 100 批，三类共用额度，单次最多检查 10,000 条；达到额度或存在并发跳过时，查看剩余数量后再次执行。此入口不运行 shell 命令，也不触发结构迁移。

`GET/POST /api/admin/v1/maintenance/html-rebuild` 保留为兼容入口，共用任务管理的持久执行记录；完整计划、取消、重试及历史在任务管理中操作。请求仍使用会话、`settings.manage`、CSRF 和同源 Origin，响应为 `Cache-Control: no-store`；CSRF 缺失或错误返回 400，跨源返回 403。报告沿用 CLI 的确认提交数、跳过数、批次数、剩余量与失败定位，后台错误消息不返回数据库内部诊断。正常完成仅表示本轮结束，仍须检查 `has_more` 与剩余数量。

关闭浏览器不会取消任务，刷新可读取保存的进度。已排队的一次性计划跨重启保留，到期后由正常服务领取；运行中任务在关闭或租约过期后按中断处理，须人工重试并获得新 ID。已提交的数据与审计保留，重试依据当前派生版本跳过已完成记录。详细生命周期见下一节。

后台和 CLI 都禁止在 `BLOG_RECOVERY_MODE=1` 或数据库恢复隔离标记存在时执行。后台逐记录审计记录可信启动者和来源 IP，CLI 沿用空 actor 的系统来源。HTML、媒体引用与审计继续同事务提交，业务 `version`、`updated_at`、发布状态保持不变。任务租约协调服务进程之间的执行，独立 CLI 不加入队列，仍依赖逐记录 CAS 防止覆盖并发编辑。演进依据见 [ADR-0019](adr/0019-admin-html-maintenance.md) 和 [ADR-0020](adr/0020-persistent-admin-tasks.md)。

## 后台任务管理

「任务管理」位于 `/admin/tasks`，只提供三种固定任务，读取、计划和操作复用 `settings.manage`。HTTP 接受请求后返回任务 ID，执行与浏览器连接分开；页面可刷新查看最新状态、执行报告和历史。不会接受 shell 命令、任意任务类型或 cron 表达式。

| 任务 | 执行方式与默认值 |
|---|---|
| `html_rebuild` | 手动立即执行或未来 365 天内的一次性计划；每轮固定 100 条 × 100 批 |
| `retention` | 手动执行或启用 1 小时至 30 天的固定间隔；默认间隔一天，周期未启用；保留天数仍由既有评论/审计设置决定 |
| `publish_due` | 可手动执行；固定启用的周期每 30 秒检查到期 Post/Page，保留启动后处理积压的语义，后台不能关闭或更改周期 |

每种任务只允许一个活动执行，重复启动返回已有记录；一个 HTML 任务内文章、页面、评论仍共用额度。计划和运行报告保存在 `task_schedules`、`task_runs`，不放在浏览器或进程内作为唯一事实。每种任务保留最新状态和最多 500 条终态历史，活动记录不参与修剪；此数量上限独立于评论 IP 和审计日志的天数保留期。

queued 任务跨重启继续等待或在到期后执行。可取消尚未执行的手动、一次性计划或重试请求；运行中任务不支持网页取消，取消请求返回 409。失败、服务关闭中断和租约过期均保留此前已提交结果，不回滚整轮业务。中断不自动重试；人工再次执行或重试生成新任务 ID，旧记录留在历史中。后台和任务报告仅显示可确认的进度，进度持久化可能落后于批次提交；重启和重试以当前业务数据为准，不能从旧计数推算剩余量。

服务进程以数据库时间领取和续租，每次领取使用新的 token。进度、完成结果和业务写事务都核验当前租约，已失去租约的旧进程不能提交后续变更。服务关闭时停止领取，取消并回收在途执行，在总关闭期限内处理任务状态和数据库连接。独立 CLI 的 `blog rebuild-html`、`blog maintenance`、`blog publish-due` 不纳入队列或任务租约；它们仍使用原有 CAS、发布条件与清理锁，也不因此出现在任务执行历史中。

retention 执行复用既有维护连接选择规则；与运行连接相同时复用连接池。默认站点所有者可以直接运行，受限运行账号没有审计删除权限时须由部署方配置已有的独立维护连接。显式维护连接无效、连接失败或权限不足时不退回运行连接执行清理，任务页面显示清理不可用，站点登录和其他任务继续可用；错误目标数据库还会被执行事务的任务租约核验拒绝。不能通过向应用账号增加 audit_logs DELETE 来消除错误。选择后台清理意味着 HTTP 进程在该配置下持有维护能力，范围由维护角色权限限定。

Compose 默认 owner 支持后台清理；现有 `.env` 的 `BLOG_MAINTENANCE_DATABASE_URL` 仍只供独立 ops 维护，不自动传入 HTTP 服务。分离 app 账号若需后台清理，应由部署方在受保护的站点 TOML 中显式配置 `[maintenance] database_url`，并保证连接同一数据库。启用后台周期后，应明确停用重复的外部 timer；CLI 与服务并行仍有清理锁，但会产生不必要的重复检查。

恢复配置或数据库隔离期间，任务页面和预检可读，所有任务变更与执行禁止；读取不会初始化默认计划、续租、修剪历史或把过期租约落成终态。恢复库包含备份时的计划与 queued 请求，重新开放前须核对这些状态；只有正常服务解除隔离后才恢复领取。安装前不初始化计划，安装事务完成和 Router 激活后才启用统一监督器。此协调范围不表示整站认证已支持多实例，具体规则见 [ADR-0020](adr/0020-persistent-admin-tasks.md)。

2026-10-01，代码提交 `49a8adc` 通过独立本地 PostgreSQL、生产构建的 Edge 页面和 Linux arm64 Docker runtime/ops 验证。覆盖三类实际任务、跨重启 queued 计划、跨进程单次领取、失败及中断后新 ID 重试、受限维护角色、历史上限和恢复模式零任务写入；浏览器刷新和关闭后服务端继续执行。完整 Compose 安装、升级、备份和隔离恢复也通过，未变更现有开发数据库。前端 383 项测试及构建通过，工作区依赖边界、Clippy、格式、结构与生成契约校验通过；[执行器回归](../crates/server/src/tasks_tests.rs) 固化了 12 秒合法慢事务与关闭后延迟回滚的场景。这些结果记录本地实现验证，生产验收仍按路线图执行。

## 正式媒体物理清理

`blog media purge` 只接受明确选中的媒体 UUID，每份计划最多 1,000 个。所选媒体必须已进回收站、没有已知引用，文件路径、大小及 SHA-256 与登记一致。不会按软删除时间、零引用或未登记文件自动清扫，也不扫描全站正文。草稿、私密、归档和回收站内容仍计入 media_refs；封面、头像外键、站点 logo 及 themes 配置媒体字段另行复核，缺少引用记账也不会绕过它们。

生成计划和提交新数据库删除前，工具检查全部 Post/Page 派生版本；有任何旧版本记录时拒绝清理，并提示先运行 `blog rebuild-html`，直至 `pending.posts=0` 且 `pending.pages=0`。这包含升级前已有计划，普通启动或结构迁移不能代替补建。Compose 可在应用容器执行 `docker compose exec blog blog rebuild-html --batch-size 100 --max-batches 100`，按结果继续运行。已提交且有匹配 `media.purge` 凭据的原计划仍可重试剩余文件删除，因为它不会再次删除数据库媒体行。

清理由 Rust 应用用例协调，基础设施负责 PostgreSQL 事务和本地文件校验；Python 仅保留 Compose 部署封装。命令只校验已应用的迁移，不自动升级结构或初始化权限，也不依赖主题、公开 URL 与后台资源。

Compose 部署使用以下入口，计划写入 `backups/plans/`，无需主机 Python 或 PostgreSQL 工具：

```sh
sh scripts/compose-backup.sh media-plan media-purge.json \
  00000000-0000-0000-0000-000000000001
# 复核计划和站外链接影响，停止其他写入后执行。
sh scripts/compose-backup.sh media-apply media-purge.json \
  --maintenance-confirmed --break-links-confirmed
```

包装脚本与备份、恢复和保留期维护共用操作锁；执行时自动停止本站 `blog`，结束或失败后恢复原先运行的服务，并拒绝仍有其他数据库客户端连接的执行。外部写入进程仍须由操作者停止。数据库记录提交删除后，即使文件删除失败，旧图片 URL 也已经失效；保留原计划重试即可。

非 Compose 部署使用本机 CLI，通过 `DATABASE_URL` 或安装配置连接结构管理账号，执行主机须有媒体目录的删除权限。不要把这些权限授予保留期维护账号：

```sh
# 只读生成计划；重复 --id 明确选择每一个媒体 UUID。
blog media purge plan \
  --id 00000000-0000-0000-0000-000000000001 \
  --media-dir "${BLOG_MEDIA_DIR:-data/media}" \
  --output /secure/maintenance/media-purge.json

blog media purge apply /secure/maintenance/media-purge.json \
  --maintenance-confirmed --break-links-confirmed
```

`--media-dir` 优先于环境、TOML 与默认值；配置文件由 `BLOG_CONFIG_FILE` 选择。原 Python 工具生成的格式 1 计划和已提交审计凭据仍可重试。旧计划若采用 Docker 端点标识，在 `purge` 后添加 `--legacy-container 原容器名`，同时让 `DATABASE_URL` 实际连接该原数据库；该参数只兼容旧端点标识，不调用 Docker，也不映射主机媒体路径。原主机计划不能直接搬进 Compose 容器执行。

示例 UUID 须替换为实际选中记录。计划文件以 0600 独占创建，包含数据库名称/OID/连接端点、媒体根目录、所选 ID/path/版本/删除时间/大小/校验和及操作编号。复核后保留原文件，不能编辑或覆盖部分执行的计划。计划摘要用于检测损坏，不是签名或权限凭据；文件和媒体目录由部署方保护。

执行前在部署层取得维护互斥，停止 HTTP、上传、定时任务、保留期任务和所有 CLI 写入，等待在途操作结束；整个执行及重试期间均须维持这一条件。两个确认参数是操作者声明；CLI 本身不能验证进程已停止，Compose 封装额外检查当前数据库连接。站外链接无法完整枚举，--break-links-confirmed 表示接受所选图片 URL 永久失效。恢复隔离库禁止清理，旧计划也不能直接用于新恢复库。

| 阶段 | 保护与失败行为 |
|---|---|
| 预检 | 全部文件和计划身份先通过校验；不接受路径越界或符号链接 |
| 数据库事务 | 按 ID 排序取得媒体行 FOR UPDATE，与引用同步的 FOR SHARE 互斥；重检版本、回收站状态和引用；全部所选媒体行及 media.purge 审计凭据一起提交，任一失败整体回滚 |
| 文件删除 | 只有确认该计划的数据库凭据已提交且 ID/path 未重新登记，才复核并删除文件；对象变更、凭据丢失或磁盘错误时保留文件并报告失败 |
| 重试 | 数据库提交结果不确定时不碰文件；用同一份计划再次 apply，根据持久审计凭据继续。已完成文件允许缺失，不重复追加审计 |

JSON 结果含 operation_id、records_purged、files_deleted、files_already_absent、failures。records_purged 是本计划已确认清除的记录总数，重试时不表示本轮新增删除量。files_deleted/已缺失计数只描述本轮观察结果。有失败时进程返回非零；修复权限等外部故障后，用原计划重试，直到 failures 为空。数据库记录删除后图片入口立即不可用，文件删除失败会留下受控残留，不会恢复公开访问。

media.purge 记录数据库账号和必要文件摘要，actor_id 为空，不冒充某个博客用户；该记录证明数据库阶段完成，不能单独证明文件已删除。保留计划及执行结果直至结束，及时重试：审计凭据也受保留期约束，凭据过期/丢失后工具拒绝继续，须人工核对。ID 和存储路径不得复用。清理不删除旧备份或外部缓存；恢复旧备份会带回旧图片，需重新核对清理范围。

## 维护备份

在部署层取得维护互斥，停止 HTTP、上传、定时发布、保留期任务及所有 CLI 写入，等待在途事务与文件写入结束。另行保存匹配的应用构建/源码、部署配置、角色授权和秘密恢复材料。保持写入停止，执行：

```sh
python3 -B scripts/recovery.py backup \
  --output /secure/backups/blog-2026-09-27 \
  --theme-dir "${BLOG_THEME_DIR:-themes/default}" \
  --media-dir "${BLOG_MEDIA_DIR:-data/media}" \
  --docker-container blog-postgres --maintenance-confirmed

python3 -B scripts/recovery.py verify /secure/backups/blog-2026-09-27
```

目标目录必须尚不存在。资源目录从命令行、环境、TOML 依次解析；可用 --config 和 --blog-bin 指定配置文件与程序。数据库连接仍只使用显式 DATABASE_URL。Docker 模式在指定数据库容器内运行工具，忽略 URL 主机/端口，依赖容器内可用认证；资源路径仍是脚本所在主机的路径。非 Docker 部署省略该参数，并提供匹配版本的 pg_dump、pg_restore、psql、createdb。

| 检查 | 行为 |
|---|---|
| 结构 | 精确核对共享清单的全部表、全部迁移版本与 SHA-384 校验和、成功状态、关键字段；记录列结构，恢复后比对 |
| 数据库 | custom 格式 pg_dump，使用 --no-owner --no-acl，并检查 archive 列表；不备份集群角色/授权 |
| 媒体 | 自动复制 --media-dir（按 BLOG_MEDIA_DIR、TOML paths.media_dir、data/media 回退），核对所有注册原件的 path、大小和 SHA-256，包括软删除及零引用媒体 |
| 引用 | 复核正文 HTML、Post/Series 封面、头像、logo、themes 媒体字段与 media_refs 一致；复核评论根关系和分类树无环 |
| 主题 | 保存默认主题、配置声明及同级已安装主题；themes 全表随数据库备份，确认数据库选择的主题存在；未完成操作日志须先正常启动恢复，备份会拒绝；运行时兼容性需实际启动验证 |
| 清单 | 格式 2，包含结构、媒体清单、所有表计数、内容状态/回收站计数、文件大小与 SHA-256；COMPLETE 保存清单摘要 |
| 秘密 | 只保存 OAuth secret_ref 名称，要求恢复环境提供非空值；不复制秘密，也不能证明值正确 |

首次安装生成的 TOML（默认 `config.toml`）含部署凭据，属于部署秘密，不在工具的自动备份范围内，应通过受控秘密存储单独保存。临时日志 `config.install-state.json` 在安装完成后自动清理；未完成安装时同样须保护其凭据，完成后的备份恢复无需携带日志。`settings.installation` 随数据库备份恢复。恢复部署可显式用 `DATABASE_URL` 覆盖新库地址；不要对恢复库重跑安装向导，详见[首次安装](installation.md)。

整个媒体目录中的未注册文件也会保存，不判定为垃圾。附加目录可用 --resource name=目录，media 为保留名称。拒绝符号链接、路径越界及非普通文件。任何注册原件缺失或损坏都会阻止完成备份。

格式 1 和旧迁移链备份须使用匹配的旧工具；恢复与升级分开执行。新增表不改变备份格式 2，但新迁移链不能直接恢复旧链备份；先用旧工具恢复、核验并在停写状态解除数据库隔离，再执行新版本迁移，详见[跨版本恢复顺序](schema-migrations.md#部署与恢复顺序)。失败不生成 COMPLETE；异常会清理临时目录，强制中止可能留下不能直接恢复的 .partial-*。备份包含私密正文、密码哈希和会话等敏感材料，0700 目录权限不能替代受控存储、传输保护或加密。

## 隔离恢复与重新开放

主题恢复隔离不会初始化 themes 记录或恢复磁盘操作日志。正常启动处理持久化主题操作，只有已提交的卸载日志可以触发隔离目录回收；目录缺失、包校验失败和记录不兼容都保留配置。详细操作协议与单实例租约见 [主题文档](themes-and-rendering.md#安装验证激活和卸载)。

停止目标环境所有进程，隔离公开流量。DATABASE_URL 指向具备建库权限的管理数据库。恢复只创建新的 blog_restore_* 库和新输出目录：

```sh
python3 -B scripts/recovery.py restore /secure/backups/blog-2026-09-27 \
  --target-db blog_restore_drill_20260927 \
  --output /secure/isolated/blog-2026-09-27 \
  --docker-container blog-postgres --isolation-confirmed
```

建库后先写数据库隔离标记，再导入和复制文件；清空 sessions，核对结构、全部表计数、媒体清单/原件/引用、评论/分类树，以及至少一个 active、未删除、带密码或外部绑定的 Admin。通过后写 RESTORED，失败写 FAILED 并保留现场。普通启动、publish-due 和保留期维护会检查数据库隔离标记；只删输出目录的 ISOLATED 文件不能绕过它。

用匹配构建进行核验，显式连接恢复库并设置恢复后的路径：

```sh
# DATABASE_URL 此时指向 blog_restore_drill_20260927。
BLOG_RECOVERY_MODE=1 \
BLOG_MEDIA_DIR=/secure/isolated/blog-2026-09-27/resources/media \
BLOG_THEME_DIR=/secure/isolated/blog-2026-09-27/resources/installed-themes/default \
BLOG_PUBLIC_BASE_URL=http://127.0.0.1:8081 \
blog serve --addr 127.0.0.1:8081
```

恢复模式只允许 loopback IP 监听，停用自动预约发布；它仍允许人工编辑，不是只读模式。部署层确保反向代理不转发公网流量、旧进程和其他版本 worker 已停止。标记不能约束外部程序或数据库管理员。启动不重建历史 HTML，隔离期间也禁止 `rebuild-html`；仍须使用匹配版本核验结构与存储结果。手工 pg_restore 同样需要隔离、停用调度并清空 sessions。

核验至少覆盖 Admin 实际登录、授权和旧 Cookie 失效，公开/私密/预约/归档/回收站内容，评论多级关系与删除占位，正文/封面/头像/logo 和软删除媒体的独立公开链接，以及主题选择、后台编辑。哈希或绑定存在不等于能登录，OAuth 与秘密须实际验证。

停止核验服务及全部写入，切回管理连接后解除隔离：

```sh
python3 -B scripts/recovery.py release \
  --output /secure/isolated/blog-2026-09-27 \
  --docker-container blog-postgres --verification-confirmed
```

release 核对该次恢复的数据库标记，重检结构、Admin、当前媒体与引用及秘密名称；再次清空包括核验期间创建的所有会话，再解除数据库标记并写 RELEASED。它不恢复流量、不启动服务。重新配置运行/维护账号授权（dump 不含 ACL），取消 BLOG_RECOVERY_MODE，核对预约时间后再启动普通服务与维护任务；普通启动会补发到期内容。

恢复时的正文引用复核按记录保存的流水线版本执行：旧版本 0/1 使用原始 `/media/{uuid}` 契约，版本 2 识别浏览器规范化后的根相对路径、查询串、fragment 和编码参数；未知版本直接拒绝核验。这样旧备份可以按当时契约完成隔离核验与 `release`，随后由新版本 `blog rebuild-html` 补建引用。解除隔离和普通启动都不会自动升级派生数据；补建完成前，媒体物理清理仍拒绝新的数据库删除。

三个 --*-confirmed 参数均为操作者声明，工具不能证明外部所有写入或流量已停止。业务流量重新开放仍由部署层控制。

## 验证与部署证据

2026-09-28 完成 Compose 专项演练：创建完整备份、加密仓库上传/取回与保留清理、损坏备份拒绝、媒体缺失时原服务恢复、独立项目导入、错误密码拒绝、管理员登录/页面/图片检查、会话撤销，以及恢复后的站点再次备份和恢复。53 项工具测试和 6 项 PostgreSQL 专项测试通过。加密存取使用独立临时本地 restic 仓库验证，真实 S3 和生产 RPO/RTO 仍待部署环境验收。

[新库全链路验收](acceptance.md)通过真实安装、管理及评论接口创建样本，再调用本节的备份恢复工具完成往返。该流程已接入 CI，保存逐步结果、构建/迁移标识与媒体引用核验报告；它与下方针对数据库角色、恢复失败及清理竞争的专项演练互补。

无数据库测试：

```sh
BLOG_RECOVERY_TEST=0 PYTHONPATH=scripts python3 -B -m unittest discover -s scripts -p 'test_*.py'
```

真实往返演练只允许 loopback 管理地址，随机创建并清理专用库和角色：

```sh
cargo build -p server --bin blog
# 提前设置 BLOG_TEST_ADMIN_URL；本机 PostgreSQL 工具可用时省略容器变量。
BLOG_RECOVERY_TEST=1 BLOG_TEST_PG_CONTAINER=blog-postgres \
PYTHONPATH=scripts python3 -B -m unittest scripts/test_recovery_postgres.py
```

Python 专项覆盖临时新增表升级、匹配版本备份往返、旧备份恢复后升级、完整表集合与有效权限检查、授权脚本、受限运行账号、维护权限、媒体各状态及多类型引用、多系列、评论树、会话撤销、隔离启动/调度、缺文件拒绝备份、引用缺失拒绝开放。媒体清理的审计回滚、版本/引用复核、并发恢复、提交结果丢失、部分文件失败和重试由 Rust 用例与 PostgreSQL 集成测试覆盖：

```sh
cargo test -p application --test media_cleanup_usecase
cargo test -p infrastructure --features sqlx-test-support --test media_cleanup
```

测试不代替生产维护互斥、RPO/RTO 和故障中断验收；部署层记录备份大小、维护时长、恢复点和实际恢复耗时。

2026-09-27 已在独立 PostgreSQL 18 临时实例完成上述往返演练，全量检查及前端生产构建通过；受限账号迁移并发和失败后释放锁另有集成测试。现有开发数据库未重建或切换。

同日补充验证了正式媒体清理的 4 组 PostgreSQL 故障/并发场景，与恢复往返共 5 项通过；维护工具单元测试 20 项、前端测试 209 项及 Rust 全量检查通过。业务审计另有身份/会话、权限同步、设置/引用和 HTML 重建的事务回滚验证。

2026-09-27 补充了审计查询权限、同时间记录游标分页、翻页边界被清理、可信代理与伪造转发头、资料/改密 IP 传递的验证。Rust 全量检查、前端 213 项测试与生产构建、工具单元测试 20 项、Docker 模式 PostgreSQL 演练 5 项通过；另用临时 Admin 验证后台审计筛选和详情。测试使用独立临时容器，未切换现有开发库。

同日完成个人资料和账号启停验证：Rust 全量检查 500 项、前端 223 项、工具单元测试 20 项及生产构建通过。随后补充会话淘汰/迟到创建与账号启停的并发验证，账号状态共 7 项 PostgreSQL 测试通过；包含最后 Admin、旧认证快照、版本冲突和审计回滚。临时站点浏览器验证了资料保存、启停确认和 Admin 按钮保护。未改动原开发库，首次安装及生产验收仍单独推进。

2026-10-01 主题配置变更的[验证记录](validation/theme-config-2026-10-01.json)包含全项目检查、后台生产构建、真实 PostgreSQL 主题生命周期和备份恢复结果。覆盖配置与媒体引用回滚、六个进程退出边界、尚未提交的数据库决定、首次安装初始化失败及恢复隔离。记录也区分了进程中断测试与未注入的断电/网络故障。

## 凭据泄露与后续外部系统

密码泄露时通过 blog user passwd 的隐藏输入或 --password-stdin 轮换；改密递增认证版本并撤销会话。核对角色、外部绑定与有效 Admin，OAuth 秘密独立轮换。不能靠恢复旧备份撤销泄露，恢复后必须保留必要轮换并清空会话。密码不放进参数、日志或脚本回显。

目前没有外部搜索、Webhook、通用任务队列或跨请求整页缓存。以后引入时同步交付恢复隔离：搜索重建新索引/水位，事件建立新 stream_epoch 并显式核对/重放；数据库回退不能撤销外部副作用。详见[扩展候选](extensions-and-data.md)与 [ADR-0005](adr/0005-consistent-backup-and-recovery.md)。

邮件邀请/找回的 `account_links` 随数据库备份，但 restore 和 release 均清空，避免旧链接恢复有效；恢复核验模式关闭邮件功能。恢复后需重新申请链接，并单独恢复受保护的 SMTP 配置。
