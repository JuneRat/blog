# 保留期、备份与恢复

当前工具适配新的 [19 表基线](database-design.md)。采用维护窗口备份和隔离恢复；部署验收仍见[路线图](product-roadmap.md)，不提供在线一致备份或零数据丢失承诺。

## 数据库账号与保留期

数据库角色与博客 Owner/Admin 是不同层次的权限。

| 身份 | 用途 |
|---|---|
| 结构管理账号 | 建库、迁移、授权、备份和恢复；不注入 HTTP 服务 |
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

保留期不清理备份副本；备份保留规则另行制定。正式媒体文件也不属于此命令：零引用仍可能有站外链接。当前仅 blog media cleanup-staging 清理过期暂存文件；正式对象的显式物理清理及失败重试流程仍待实现。

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

恢复模式只允许 loopback IP 监听，停用自动预约发布；它仍允许人工编辑，不是只读模式。部署层确保反向代理不转发公网流量、旧进程和其他版本 worker 已停止。标记不能约束外部程序或数据库管理员。启动仍可能重建派生 HTML，须使用匹配版本。手工 pg_restore 同样需要隔离、停用调度并清空 sessions。

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

无数据库测试：

```sh
PYTHONPATH=scripts python3 -B -m unittest scripts/test_recovery.py
```

真实往返演练只允许 loopback 管理地址，随机创建并清理专用库和角色：

```sh
cargo build -p server --bin blog
# 提前设置 BLOG_TEST_ADMIN_URL；本机 PostgreSQL 工具可用时省略容器变量。
BLOG_RECOVERY_TEST=1 BLOG_TEST_PG_CONTAINER=blog-postgres \
PYTHONPATH=scripts python3 -B -m unittest scripts/test_recovery_postgres.py
```

覆盖授权脚本、受限运行账号、维护权限、媒体各状态及多类型引用、多系列、评论树、会话撤销、隔离启动/调度、缺文件拒绝备份、引用缺失拒绝开放。测试不代替生产维护互斥、RPO/RTO 和故障中断验收；部署层记录备份大小、维护时长、恢复点和实际恢复耗时。

2026-09-27 已在独立 PostgreSQL 18 临时实例完成上述往返演练，全量检查及前端生产构建通过；受限账号迁移并发和失败后释放锁另有集成测试。现有开发数据库未重建或切换。

## 凭据泄露与后续外部系统

密码泄露时通过 blog user passwd 的隐藏输入或 --password-stdin 轮换；改密递增认证版本并撤销会话。核对角色、外部绑定与有效 Owner，OAuth 秘密独立轮换。不能靠恢复旧备份撤销泄露，恢复后必须保留必要轮换并清空会话。密码不放进参数、日志或脚本回显。

目前没有外部搜索、Webhook、任务队列或跨请求整页缓存。以后引入时同步交付恢复隔离：搜索重建新索引/水位，事件建立新 stream_epoch 并显式核对/重放；数据库回退不能撤销外部副作用。详见[扩展候选](extensions-and-data.md)与 [ADR-0005](adr/0005-consistent-backup-and-recovery.md)。
