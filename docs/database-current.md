# 当前数据库实现

新建库已采用 **19 张表（18 张业务表和 sessions）**，不含 SQLx 迁移记录。已适配身份、会话、媒体、内容与目录；评论与恢复工具仍在分批迁移，整站尚未通过新库验收。完整设计见[数据库设计](database-design.md)，逐项状态见[路线图](product-roadmap.md#已采纳数据库设计的实施)。

## 1. 权威来源与迁移

执行依据是 [0001_initial_schema.sql](../migrations/postgres/0001_initial_schema.sql)，由已确认的 [blog_schema.sql](../blog_schema.sql) 整理而来；[汇总 DDL](sql/postgres-core.sql)同步相同结构。原 `0001_identity_rbac.sql` 至 `0009_comments.sql` 已删除，不保留升级或兼容链。

迁移仅支持空库或已应用新基线的数据库。入口检测到旧 users 结构时明确拒绝，SQLx 仍检查迁移历史及校验和；不会自动 DROP、清空历史或跳过校验。不要手工导入设计稿后再运行迁移。开发时先用[独立空库](development.md#新基线的隔离验证)，其余模块适配后再显式重建原开发库。

SQLx 为初始迁移包事务，因此该文件没有额外的 BEGIN/COMMIT。重复执行已成功应用的新基线不重复建表。DDL 不创建账号；权限目录和内置角色由应用可信注册表在身份命令或启动时同步。`migrate` 命令本身不创建 Owner。

`migrate_schema` 只执行结构；完整 `migrate` 还检查需要重建的正文 HTML。空库没有派生记录，身份维护命令不依赖主题和内容渲染。

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

本人资料通过 `PUT /api/admin/v1/me/profile` 更新展示名和简介，必填 expected_version，冲突拒绝覆盖。返回同一提交的新版本，登录态不变。前端表单、状态管理入口和首次安装流程尚未提供；Owner 仍通过受控 CLI 分步创建、设密码、分配角色。

## 4. 内容、目录与并发关系

文章系列已切换至 `post_series(post_id, series_id, position)`，一篇文章可属于多个系列，非负 position 可重复，公开成员按 position、post_id 稳定排序。正文、HTML、标签/系列关系、媒体引用、相关版本与审计在同一事务提交；返回事务内读取的完整记录。

文章关系写入、系列变更、标签删除和文章永久删除共用内容关系 advisory transaction lock，再取得实体行锁并检查版本。系列重排校验完整成员集合与系列版本，仅递增权重变化的文章版本；删除标签或系列保留文章、移除关联，并递增受影响文章版本。分类仍保留引用与防环保护。

Post/Page 已支持 scheduled、可逆归档、回收站与恢复。恢复一律回草稿，保留发布时间与已锁定的 slug；永久删除只接受回收站记录，清除多态媒体引用，文章同时清除整棵评论树。所有公开查询统一检查 published、public、未删除和发布时间已到。

预约任务使用 `FOR UPDATE SKIP LOCKED` 领取到期记录，更新前复核状态、删除标记和时间，内容增版与系统审计同事务提交。服务每 30 秒检查并在启动时补发；`publish-due` 命令可独立运行。具体状态规则见[内容生命周期](content-lifecycle.md)。

## 5. 媒体与多态引用

媒体仓储、上传与管理 API、公开读取和后台回收站已切换到 media/media_refs。文件完成暂存与原子就位后，媒体行和上传审计一起提交；不保存上传中间状态。上传者可空，空值显示“未知上传者”。

图片 URL 独立公开，读取不查询会话或引用；私密内容、用户停用及媒体软删除均不撤销访问。正常库与回收站分别分页；软删除和恢复带版本条件，与审计同事务提交，保留文件和所有引用。

引用同步在来源写事务内按媒体 ID 顺序加共享锁。新增引用拒绝不存在或已软删除图片；同一来源可保留历史引用。头像、站点 logo、系列封面及正文共用该协议，使用位置仍按内容阅读权限过滤。内容软删除保留引用，永久删除同事务清除引用。

`blog media cleanup-staging` 仅删除超过一小时的暂存残留，不扫描或删除正式对象。软删除、零站内引用均不是物理清理依据；数据库登记失败但提交结果不确定的正式文件也保留。独立物理清理和恢复核验留在收尾批次。

## 6. 分组设置

settings 保存 JSON 对象和独立 version。OAuth 配置保存已改为仅在值变化时递增版本，不存秘密。site.logo 的媒体引用已适配；comments/audit 分组、保留期配置和维护任务待后续适配，不因新表已存在就自动可用。

## 7. 持久会话

字段为 token_hash、user_id、csrf_token、auth_version、created_at、last_seen_at、expires_at。摘要和 CSRF 值均为 64 位小写 hex；数据库不保存明文登录令牌。用户外键级联删除；时间约束保证 created_at ≤ last_seen_at ≤ expires_at 且绝对期限晚于创建时间。

PostgreSQL 校验在同一 UPDATE 中核对当前用户状态、认证快照、绝对期限和空闲期限，全部通过才刷新 last_seen_at。绝对期限固定，认证快照不会随活跃更新。过期清理与并发容量限制保留；旧认证快照即使在撤销事务之后才落库也无法认证。单次退出删除当前会话；改密后当前浏览器获得新 Cookie，其他旧 Cookie 失效。

跨连接读取与撤销、空闲和绝对到期、并发创建容量、资料修改保持会话，以及禁用/认证版本不符时不刷新活跃时间已有数据库验证。恢复工具尚待适配，恢复后撤销会话的全流程仍需收尾验收。

## 8. 审计基础

`append_audit_log` 仅接受调用方已有的 PostgreSQL 事务，追加成功后由业务调用者统一提交。资料/头像、媒体上传/软删除/恢复、Post/Page 写入、标签/系列变更及其关联变化已接入，记录实际用户、目标、动作和版本；到期发布使用空 actor 表示系统任务。不写正文或凭据，上传仅补充大小和 MIME。审计写入失败会使业务变更一同回滚。

其余身份、分类、设置、评论等写入覆盖、HTTP 来源 IP 上下文、运行账号只追加授权、后台查询和保留期任务仍待实现。现阶段不能将审计表的存在解释为所有成功操作已有不可遗漏的审计。

## 原生评论

新 comments 已包含受限 Markdown 的持久化 HTML、parent/root 关系、回收站状态与来源 IP；开关已从两张独立设置表迁入目标 posts/settings 字段。评论用例、API、后台和公开前端仍需按新设计改造，旧评论流程尚不能用于新库。
