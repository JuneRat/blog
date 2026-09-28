# 当前数据库实现

新建库已采用 **19 张表（18 张业务表和 sessions）**，不含 SQLx 迁移记录。身份、会话、媒体、内容、目录、评论、保留期维护、新库恢复与正式媒体显式清理已适配；已有业务写入口已补齐事务审计、来源 IP 和后台只读查询，生产验收仍待完成。完整设计见[数据库设计](database-design.md)，逐项状态见[路线图](product-roadmap.md#已采纳数据库设计的实施)。

## 1. 权威来源与迁移

执行依据是 [migrations/postgres](../migrations/postgres) 的不可变前向迁移链，当前基线为 [0001_initial_schema.sql](../migrations/postgres/0001_initial_schema.sql)。[blog_schema.sql](../blog_schema.sql) 与[汇总 DDL](sql/postgres-core.sql)均由该链生成，不再独立维护；表集合、迁移哈希及授权策略使用[共享清单](../migrations/postgres/schema.json)，操作规则见[迁移演进](schema-migrations.md)。原 `0001_identity_rbac.sql` 至 `0009_comments.sql` 已删除，不保留升级或兼容链。

迁移仅支持空库或已应用新基线的数据库。入口检测到旧 users 结构时明确拒绝，SQLx 仍检查迁移历史及校验和；不会自动 DROP、清空历史或跳过校验。不要手工导入设计稿后再运行迁移。开发时先用[独立空库](development.md#新基线的隔离验证)，其余模块适配后再显式重建原开发库。

SQLx 为初始迁移包事务，因此该文件没有额外的 BEGIN/COMMIT。重复执行已成功应用的新基线不重复建表。DDL 不创建账号；权限目录和内置角色由应用可信注册表在身份命令或启动时同步。`migrate` 命令本身不创建 Owner。

所有普通命令共用 `migrate_schema` 执行或校验结构，`migrate` CLI 也只处理结构。文章、页面和评论的旧版本 HTML 由 `rebuild-html` 显式分批重建，不作为启动副作用；`rebuild-html --dry-run` 使用只读 `verify_schema`，不会建表或执行迁移。空库没有派生记录，身份维护命令不依赖主题和内容渲染。

## 2. 表与通用约定

| 范围 | 表 |
|---|---|
| 身份与会话 | users、oauth_accounts、sessions |
| 权限 | roles、permissions、user_roles、role_permissions |
| 内容与目录 | posts、pages、categories、tags、series、post_tags、post_series |
| 媒体 | media、media_refs |
| 评论、设置、审计 | comments、settings、audit_logs |

实体 UUID 由应用生成；关联表采用复合主键，settings 以 key 为主键，sessions 以令牌摘要为主键。时间使用 timestamptz；状态使用 text 与 CHECK。可编辑实体的 version 初值 1；updated_at 由应用维护。字段、外键、CHECK 与索引统一查阅初始迁移，避免重复维护另一份结构表。

## 3. 身份与 RBAC

- users 同时保存 `version` 和 `auth_version`。展示名、纯文本 bio 及头像更新只递增编辑版本；密码和 OAuth 绑定变更递增两个版本并在同事务删除持久会话。明确撤销全部登录只递增认证版本并清理会话。
- 仅 `status='active' AND deleted_at IS NULL` 可以登录。用户名在应用边界 trim 并转 ASCII 小写；用户名和邮箱以 lower 索引保证不区分大小写唯一，软删除后仍占用。密码 PHC 通过独立凭据端口读写，不进入用户 DTO。
- OAuth 以 `(provider, subject)` 为主键，不存外部邮箱、access token 或 refresh token。提供商邮箱可以参与协议读取，但不作为本站身份或合并账号的依据。
- roles 使用稳定 `code`，permissions 直接以 `code` 为主键，role_permissions 保存 `permission_code`。角色 DTO 仍用 `slug` 字段承载角色 code；权限来自当前角色并集，不保存在会话中。
- 角色分配变更递增 users.version，保持 auth_version；同一个 Cookie 的下一次请求读取最新权限。重复分配/移除不存在的分配不增版。
- 角色和凭据变更使用统一身份事务锁，保留最后可登录 Owner 和最后登录方式保护。可登录 Owner 必须 active、未删除，并有本地密码或外部绑定；提供商真实可用性不在计数谓词内。

本人资料通过 `PUT /api/admin/v1/me/profile` 更新展示名和简介，必填 expected_version，冲突拒绝覆盖。返回同一提交的新版本，登录态不变。后台 `/admin/profile` 已接入资料表单；`/admin/users` 已提供版本控制的账号启停。状态变更与角色操作共用身份排他锁，复核当前权限和最后可登录 Owner，递增 `version/auth_version`、撤销全部会话并追加审计；启用后旧会话仍失效，软删除账号不能在此恢复。

[首次安装](installation.md)已接入：未配置数据库时跳转安装页，终端安装码保护提交。初始化权限、内置角色、首个用户/密码/Owner、完成标记与审计同事务创建，用户 version/auth_version 均从 1 起；本地连接配置先保存，崩溃后按完成标记恢复。受控 CLI 仍可分步引导，已有数据不允许重新安装。`settings.installation` 是部署完成标记，没有普通设置编辑入口；不新增数据库表。

## 4. 内容、目录与并发关系

文章系列已切换至 `post_series(post_id, series_id, position)`，一篇文章可属于多个系列，非负 position 可重复，公开成员按 position、post_id 稳定排序。正文、HTML、标签/系列关系、媒体引用、相关版本与审计在同一事务提交；返回事务内读取的完整记录。

文章关系写入、系列变更、标签删除和文章永久删除共用内容关系 advisory transaction lock，再取得实体行锁并检查版本。系列重排校验完整成员集合与系列版本，仅递增权重变化的文章版本；删除标签或系列保留文章、移除关联，并递增受影响文章版本。分类仍保留引用与防环保护。

Post/Page 已支持 scheduled、可逆归档、回收站与恢复。恢复一律回草稿，保留发布时间与已锁定的 slug；永久删除只接受回收站记录，清除多态媒体引用，文章同时清除整棵评论树。所有公开查询统一检查 published、public、未删除和发布时间已到。

预约任务使用 `FOR UPDATE SKIP LOCKED` 领取到期记录，更新前复核状态、删除标记和时间，内容增版与系统审计同事务提交。服务每 30 秒检查并在启动时补发；`publish-due` 命令可独立运行。具体状态规则见[内容生命周期](content-lifecycle.md)。

## 5. 媒体与多态引用

媒体仓储、上传与管理 API、公开读取和后台回收站已切换到 media/media_refs。文件完成暂存与原子就位后，媒体行和上传审计一起提交；不保存上传中间状态。上传者可空，空值显示“未知上传者”。

图片 URL 独立公开，读取不查询会话或引用；私密内容、用户停用及媒体软删除均不撤销访问。正常库与回收站分别分页；软删除和恢复带版本条件，与审计同事务提交，保留文件和所有引用。

引用同步在来源写事务内按媒体 ID 顺序加共享锁。新增引用拒绝不存在或已软删除图片；同一来源可保留历史引用。头像、站点 logo、系列封面及正文共用该协议，使用位置仍按内容阅读权限过滤。内容软删除保留引用，永久删除同事务清除引用。

`blog media cleanup-staging` 仅删除超过一小时的暂存残留。正式对象由 `scripts/media_cleanup.py` 按显式 ID 生成计划，在维护窗口确认外链失效后执行；要求已软删除、无已知引用、文件一致。媒体行锁内重检版本和引用，删除与审计凭据一起提交，再按凭据删除文件；同一计划可重试断线或文件失败。不会自动清理零引用或未登记文件，登记提交结果不确定的正式文件也保留。完整流程见[运维](operations-and-recovery.md#正式媒体物理清理)。

## 6. 分组设置

settings 保存 JSON 对象和独立 version。OAuth 配置保存已改为仅在值变化时递增版本，不存秘密。site.logo 的媒体引用与 comments.enabled 已适配。评论全站开关无配置时默认开启、版本为 0，变化时才落库并保留其他 JSON 字段；单篇开关使用 posts.comments_enabled 和文章版本。保留期接口合并更新 comments.ip_retention_days 和 audit.retention_days，校验两组版本并与审计同事务提交；默认各 180 天。

## 7. 持久会话

字段为 token_hash、user_id、csrf_token、auth_version、created_at、last_seen_at、expires_at。摘要和 CSRF 值均为 64 位小写 hex；数据库不保存明文登录令牌。用户外键级联删除；时间约束保证 created_at ≤ last_seen_at ≤ expires_at 且绝对期限晚于创建时间。

PostgreSQL 校验在同一 UPDATE 中核对当前用户状态、认证快照、绝对期限和空闲期限，全部通过才刷新 last_seen_at。绝对期限固定，认证快照不会随活跃更新。过期清理与并发容量限制保留；旧认证快照即使在撤销事务之后才落库也无法认证。单次退出删除当前会话；改密后当前浏览器获得新 Cookie，其他旧 Cookie 失效。

跨连接读取与撤销、空闲和绝对到期、并发创建容量、资料修改保持会话，以及禁用/认证版本不符时不刷新活跃时间已有数据库验证。恢复工具导入后及解除隔离前均清空 sessions，核验期间创建的会话也不会带入重新开放后的服务。

## 8. 审计基础

`append_audit_log` 仅接受调用方已有的 PostgreSQL 事务，追加成功后由业务调用者统一提交。已覆盖用户创建、状态启停、资料/头像、密码与登录方式变更、全会话撤销、角色分配、权限目录/内置角色同步、媒体、Post/Page、标签/分类/系列及关联变化、评论/开关、site/theme/OAuth 设置和保留期。记录实际操作者、目标、动作和必要版本，不用被修改用户替代管理员。不写正文、邮箱、密码哈希、OAuth subject 或秘密；OAuth 配置只记录版本和数量。文章、页面和评论的 HTML 重建追加空 actor 的系统审计，与派生内容及引用同事务提交，保持业务编辑版本和更新时间。

审计失败会使业务字段、版本、引用和会话删除一起回滚；应用识别的同值提交、失效 CAS、重复分配/缺失绑定解绑及无变化的启动同步不产生事件。CLI、到期发布、内置目录同步等系统操作用空 actor。正式媒体清理凭据另记录数据库账号，只证明数据库阶段完成。

scripts/database-roles.sql 提供运行账号只追加授权和独立保留期维护身份。会话签发/活跃刷新/单次退出和失败登录属于运行或安全事件，不作为这里的成功业务变更审计。HTTP 写入经可信边界提取来源 IP，并通过显式 AuditContext 随操作者传到写事务；CLI/系统和未知来源保留空 IP。直接 SQL 和数据库管理员操作不受应用事务审计保证。

后台 `/admin/audit-logs` 与 `GET /api/admin/v1/audit-logs` 已提供只读查询，独立要求 `audit.read`，默认仅授予 Owner。可按动作、账号、目标及时间组合筛选，以 `(created_at,id)` 倒序游标翻页，不做全表总数统计。显示当前账号展示名和历史 actor_id，账号被物理删除也不丢失记录；空 actor 表示无关联账号，可能是访客、系统或 CLI。摘要以文本展示，查询不返回凭据或完整业务对象。响应 `no-store`；保留期继续约束可读历史，接口契约见[审计 API](admin-api.md#审计日志)。

## 原生评论

评论用例、API、后台和两套主题的共享组件已接入受限 Markdown、服务端预览、持久化 HTML、parent/root 多级关系和两级分页展示。删除只进回收站，公开查询保留连接已审核后代所需的匿名占位；从 spam/trash 恢复先回待审核。没有提交去重和频率限制。

公开 DTO 不含源文、邮箱和 IP。提交来源默认取 socket，可通过精确 IP 列表信任代理转发；未知来源留空，审核不修改原 IP。HTML 重建使用源文及版本 CAS，审计失败回滚业务变更，已有数据库和 HTTP 验证。blog maintenance 按创建时间分批清空超期 IP，保留正文、关系、审核状态、编辑版本与更新时间；任务需部署层独立调度。接口及边界见[评论](comments.md)。
