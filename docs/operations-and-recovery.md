# 保留期、媒体清理与备份恢复

当前工具适配新的 [19 表基线](database-design.md)。采用维护窗口备份和隔离恢复；部署验收仍见[路线图](product-roadmap.md)，不提供在线一致备份或零数据丢失承诺。

## 数据库账号与保留期

数据库角色与博客 Owner/Admin 是不同层次的权限。

| 身份 | 用途 |
|---|---|
| 结构管理账号 | 建库、迁移、授权、备份、恢复和显式媒体清理；不注入 HTTP 服务 |
| 普通运行账号 | 必要业务读写；audit_logs 仅 SELECT/INSERT，无建表权限 |
| 独立维护账号 | 读取保留期、清空评论 IP、删除过期审计和写入清理摘要；不能读取评论邮箱或改正文 |

先用结构管理账号迁移，再创建两个不拥有对象、不继承其他角色的 LOGIN 角色。密码通过 psql 的交互密码命令设置，不放进命令历史。以结构管理账号执行：

```sh
psql "$DATABASE_URL" -v app_role=blog_app -v maintenance_role=blog_maintenance \
  -f scripts/database-roles.sql
```

[授权脚本](../scripts/database-roles.sql)在同一事务内重设两角色的表授权，拒绝超级用户、对象所有者和额外审计修改权限；不会替已有库撤销 PUBLIC 授权，异常授权由部署管理员核对修正。受限运行账号启动时只核对全部迁移版本、成功状态和校验和；发现不匹配便退出，由管理账号先执行迁移。新增表时同步更新授权脚本。

后台“设置 → 数据保留期”要求 settings.manage，默认评论 IP、审计各保留 **180 天**。范围为 1–36,500 整数天，分别保存到 settings.comments.ip_retention_days、settings.audit.retention_days，并校验两组版本、保留其他字段。缩短保留期会在下次维护时清理此前仍保留的数据。

维护只读取 BLOG_MAINTENANCE_DATABASE_URL，不回退到运行连接，不执行迁移、权限初始化或 HTML 重建：

```sh
# 维护连接由受保护环境注入，先查看预计处理量。
blog maintenance --dry-run
blog maintenance --batch-size 1000 --max-batches 100
```

按 created_at 严格早于截止时间处理：评论仅置空 IP，不改正文、审核状态、关系、version 或 updated_at；过期审计被永久删除。每批最多分别处理指定数量的两类记录，同事务追加不含个人信息的清理计数。审计追加失败则整批回滚。多维护进程按事务锁串行，评论遇到锁定行时跳过。JSON 结果含 comment_ips、audit_logs、batches、has_more、dry_run；has_more=true 表示达到批次上限或仍有锁定记录，可再次执行。

每日调度示例为 [service](../ops/blog-maintenance.service) 与 [timer](../ops/blog-maintenance.timer)。按部署修改路径和用户，将独立维护凭据放在受保护的 /etc/blog/maintenance.env。仓库不会安装或启用这些服务。恢复隔离期间拒绝执行维护。

保留期不清理备份副本；备份保留规则另行制定。正式媒体文件也不属于此命令：零引用仍可能有站外链接。blog media cleanup-staging 仅清理过期暂存文件；正式对象按下面的显式计划清理。

## HTML 显式重建

文章、页面和评论保存时仍同步生成清洗后的 HTML。普通启动、`migrate`、`post` 和 `publish-due` 不扫描或刷新历史渲染版本；规则升级后，由部署流程使用新版本二进制执行：

```sh
# 结构管理账号先迁移；运行账号只能校验已应用的结构。
blog migrate
# 只读核对迁移状态并分别统计待重建数量，不渲染或写入。
blog rebuild-html --dry-run
# 使用具有业务读写权限的 DATABASE_URL，或已保存的安装连接，分次执行。
blog rebuild-html --batch-size 100 --max-batches 100
```

执行模式先进行结构迁移或校验；`--dry-run` 即使使用结构管理账号也只读校验已应用的迁移，不建表、不创建 SQLx 历史记录。未迁移或校验和不匹配时直接报错。两种模式均不初始化权限注册表，也不加载主题、公开 URL 或后台资源。命令不使用 `BLOG_MAINTENANCE_DATABASE_URL`：保留期维护账号没有修改正文和媒体引用的权限。恢复隔离标记存在或 `BLOG_RECOVERY_MODE=1` 时拒绝执行，先完成恢复核验并解除隔离。

重建顺序为文章、页面、评论；`--batch-size` 默认 100，`--max-batches` 默认 100，两者范围均为 1–1,000。三类内容共用批次上限，单次最多检查 `batch-size × max-batches` 条记录；批次数包含空批次和失败批次。每类按 UUID 升序推进游标，一轮内不会反复处理同一条冲突记录。在事务外渲染后，按源文和编辑版本 CAS 提交；HTML、渲染版本、正文媒体引用与审计同事务更新，编辑版本、业务更新时间和发布状态保持不变。

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

## 正式媒体物理清理

[media_cleanup.py](../scripts/media_cleanup.py) 只接受明确选中的媒体 UUID，每份计划最多 1,000 个。所选媒体必须已进回收站、没有已知引用，文件路径、大小及 SHA-256 与登记一致。不会按软删除时间、零引用或未登记文件自动清扫，也不扫描全站正文。草稿、私密、归档和回收站内容仍计入 media_refs；封面、头像外键及站点 logo 另行复核，缺少引用记账也不会绕过它们。

结构管理连接由 DATABASE_URL 注入；执行主机必须能访问正式媒体目录，并有删除所选文件的权限。Docker 模式只在指定容器内执行 PostgreSQL 工具，媒体路径仍属于脚本所在主机；非 Docker 部署省略 --docker-container 并提供 psql。不要把这些权限授予保留期维护账号。

```sh
# 只读生成计划；重复 --id 明确选择每一个媒体 UUID。
python3 -B scripts/media_cleanup.py plan \
  --id 00000000-0000-0000-0000-000000000001 \
  --media-dir "${BLOG_MEDIA_DIR:-data/media}" \
  --output /secure/maintenance/media-purge.json \
  --docker-container blog-postgres

# 复核计划、站外链接影响，并停止全部写入后执行。
python3 -B scripts/media_cleanup.py apply /secure/maintenance/media-purge.json \
  --docker-container blog-postgres \
  --maintenance-confirmed --break-links-confirmed
```

示例 UUID 须替换为实际选中记录。计划文件以 0600 独占创建，包含数据库名称/OID/连接端点、媒体根目录、所选 ID/path/版本/删除时间/大小/校验和及操作编号。复核后保留原文件，不能编辑或覆盖部分执行的计划。计划摘要用于检测损坏，不是签名或权限凭据；文件和媒体目录由部署方保护。

执行前在部署层取得维护互斥，停止 HTTP、上传、定时任务、保留期任务和所有 CLI 写入，等待在途操作结束；整个执行及重试期间均须维持这一条件。两个确认参数仅记录操作者声明，工具不能验证进程已停止。站外链接无法完整枚举，--break-links-confirmed 表示接受所选图片 URL 永久失效。恢复隔离库禁止清理，旧计划也不能直接用于新恢复库。

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

目标目录必须尚不存在。工具使用 DATABASE_URL。Docker 模式在指定数据库容器内运行工具，忽略 URL 主机/端口，依赖容器内可用认证；资源路径仍是脚本所在主机的路径。非 Docker 部署省略该参数，并提供匹配版本的 pg_dump、pg_restore、psql、createdb。

| 检查 | 行为 |
|---|---|
| 结构 | 精确核对 19 表名单、全部迁移版本与 SHA-384 校验和、成功状态、关键字段；记录列结构，恢复后比对 |
| 数据库 | custom 格式 pg_dump，使用 --no-owner --no-acl，并检查 archive 列表；不备份集群角色/授权 |
| 媒体 | 自动复制 --media-dir（回退 BLOG_MEDIA_DIR/data/media），核对所有注册原件的 path、大小和 SHA-256，包括软删除及零引用媒体 |
| 引用 | 复核正文 HTML、Post/Series 封面、头像、logo 与 media_refs 一致；复核评论根关系和分类树无环 |
| 主题 | 保存默认主题及同级已安装主题，确认数据库选择的主题存在；运行时兼容性需实际启动验证 |
| 清单 | 格式 2，包含结构、媒体清单、所有表计数、内容状态/回收站计数、文件大小与 SHA-256；COMPLETE 保存清单摘要 |
| 秘密 | 只保存 OAuth secret_ref 名称，要求恢复环境提供非空值；不复制秘密，也不能证明值正确 |

首次安装生成的 `BLOG_CONFIG_FILE`（默认 `data/config.json`）含数据库凭据，属于部署秘密，不在工具的自动备份范围内，应通过受控秘密存储单独保存。`settings.installation` 随数据库备份恢复。恢复部署可显式用 `DATABASE_URL` 覆盖新库地址；不要对恢复库重跑安装向导，详见[首次安装](installation.md)。

整个媒体目录中的未注册文件也会保存，不判定为垃圾。附加目录可用 --resource name=目录，media 为保留名称。拒绝符号链接、路径越界及非普通文件。任何注册原件缺失或损坏都会阻止完成备份。

格式 1 和旧迁移链备份须使用匹配的旧工具；恢复与升级分开执行。失败不生成 COMPLETE；异常会清理临时目录，强制中止可能留下不能直接恢复的 .partial-*。备份包含私密正文、密码哈希和会话等敏感材料，0700 目录权限不能替代受控存储、传输保护或加密。

## 隔离恢复与重新开放

停止目标环境所有进程，隔离公开流量。DATABASE_URL 指向具备建库权限的管理数据库。恢复只创建新的 blog_restore_* 库和新输出目录：

```sh
python3 -B scripts/recovery.py restore /secure/backups/blog-2026-09-27 \
  --target-db blog_restore_drill_20260927 \
  --output /secure/isolated/blog-2026-09-27 \
  --docker-container blog-postgres --isolation-confirmed
```

建库后先写数据库隔离标记，再导入和复制文件；清空 sessions，核对结构、全部表计数、媒体清单/原件/引用、评论/分类树，以及至少一个 active、未删除、带密码或外部绑定的 Owner。通过后写 RESTORED，失败写 FAILED 并保留现场。普通启动、publish-due 和保留期维护会检查数据库隔离标记；只删输出目录的 ISOLATED 文件不能绕过它。

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

核验至少覆盖 Owner 实际登录、授权和旧 Cookie 失效，公开/私密/预约/归档/回收站内容，评论多级关系与删除占位，正文/封面/头像/logo 和软删除媒体的独立公开链接，以及主题选择、后台编辑。哈希或绑定存在不等于能登录，OAuth 与秘密须实际验证。

停止核验服务及全部写入，切回管理连接后解除隔离：

```sh
python3 -B scripts/recovery.py release \
  --output /secure/isolated/blog-2026-09-27 \
  --docker-container blog-postgres --verification-confirmed
```

release 核对该次恢复的数据库标记，重检结构、Owner、当前媒体与引用及秘密名称；再次清空包括核验期间创建的所有会话，再解除数据库标记并写 RELEASED。它不恢复流量、不启动服务。重新配置运行/维护账号授权（dump 不含 ACL），取消 BLOG_RECOVERY_MODE，核对预约时间后再启动普通服务与维护任务；普通启动会补发到期内容。

三个 --*-confirmed 参数均为操作者声明，工具不能证明外部所有写入或流量已停止。业务流量重新开放仍由部署层控制。

## 验证与部署证据

[新库全链路验收](acceptance.md)通过真实安装、管理及评论接口创建样本，再调用本节的备份恢复工具完成往返。该流程已接入 CI，保存逐步结果、构建/迁移标识与媒体引用核验报告；它与下方针对数据库角色、恢复失败及清理竞争的专项演练互补。

无数据库测试：

```sh
PYTHONPATH=scripts python3 -B -m unittest scripts/test_recovery.py scripts/test_media_cleanup.py
```

真实往返演练只允许 loopback 管理地址，随机创建并清理专用库和角色：

```sh
cargo build -p server --bin blog
# 提前设置 BLOG_TEST_ADMIN_URL；本机 PostgreSQL 工具可用时省略容器变量。
BLOG_RECOVERY_TEST=1 BLOG_TEST_PG_CONTAINER=blog-postgres \
PYTHONPATH=scripts python3 -B -m unittest scripts/test_recovery_postgres.py
```

覆盖授权脚本、受限运行账号、维护权限、媒体各状态及多类型引用、多系列、评论树、会话撤销、隔离启动/调度、缺文件拒绝备份、引用缺失拒绝开放，以及媒体清理的审计回滚、版本/引用复核、并发恢复、提交结果丢失、部分文件失败和重试。测试不代替生产维护互斥、RPO/RTO 和故障中断验收；部署层记录备份大小、维护时长、恢复点和实际恢复耗时。

2026-09-27 已在独立 PostgreSQL 18 临时实例完成上述往返演练，全量检查及前端生产构建通过；受限账号迁移并发和失败后释放锁另有集成测试。现有开发数据库未重建或切换。

同日补充验证了正式媒体清理的 4 组 PostgreSQL 故障/并发场景，与恢复往返共 5 项通过；维护工具单元测试 20 项、前端测试 209 项及 Rust 全量检查通过。业务审计另有身份/会话、权限同步、设置/引用和 HTML 重建的事务回滚验证。

2026-09-27 补充了审计查询权限、同时间记录游标分页、翻页边界被清理、可信代理与伪造转发头、资料/改密 IP 传递的验证。Rust 全量检查、前端 213 项测试与生产构建、工具单元测试 20 项、Docker 模式 PostgreSQL 演练 5 项通过；另用临时 Owner 验证后台审计筛选和详情。测试使用独立临时容器，未切换现有开发库。

同日完成个人资料和账号启停验证：Rust 全量检查 500 项、前端 223 项、工具单元测试 20 项及生产构建通过。随后补充会话淘汰/迟到创建与账号启停的并发验证，账号状态共 7 项 PostgreSQL 测试通过；包含最后 Owner、旧认证快照、版本冲突和审计回滚。临时站点浏览器验证了资料保存、启停确认和 Owner 按钮保护。未改动原开发库，首次安装及生产验收仍单独推进。

## 凭据泄露与后续外部系统

密码泄露时通过 blog user passwd 的隐藏输入或 --password-stdin 轮换；改密递增认证版本并撤销会话。核对角色、外部绑定与有效 Owner，OAuth 秘密独立轮换。不能靠恢复旧备份撤销泄露，恢复后必须保留必要轮换并清空会话。密码不放进参数、日志或脚本回显。

目前没有外部搜索、Webhook、任务队列或跨请求整页缓存。以后引入时同步交付恢复隔离：搜索重建新索引/水位，事件建立新 stream_epoch 并显式核对/重放；数据库回退不能撤销外部副作用。详见[扩展候选](extensions-and-data.md)与 [ADR-0005](adr/0005-consistent-backup-and-recovery.md)。
