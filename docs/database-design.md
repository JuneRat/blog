# 数据库设计

当前 PostgreSQL schema 有 **16 张业务表**，不含 sqlx 的迁移记录表。本文说明数据关系、主要字段、数据库约束和提交边界；内容状态见[内容生命周期](content-lifecycle.md)，认证授权见[身份与后台](identity-and-admin.md)。

## 1. 权威来源与迁移

生产 schema 由 [`migrations/postgres/`](../migrations/postgres/) 中按序执行的迁移定义。[汇总 DDL](sql/postgres-core.sql)用于阅读完整结构和空 schema 参考，不用于替代迁移或直接升级已有数据库。两者不一致时应修正文档或汇总 DDL，不能跳过已有迁移历史。

| 迁移 | 变更 |
|---|---|
| `0001_identity_rbac.sql` | 用户、外部身份和 RBAC 六张表 |
| `0002_content.sql` | 分类、系列、文章、标签、文章标签、页面六张表 |
| `0003_settings.sql` | 分组设置 |
| `0004_media.sql` | 媒体资产与内容引用 |
| `0005_sessions.sql` | 持久会话 |
| `0006_media_covers.sql` | Post/Series 文本封面替换为媒体外键 |
| `0007_media_avatar_logo.sql` | 用户头像媒体外键，扩展头像与站点引用类型 |
| `0008_content_html.sql` | Post/Page 持久化清洗 HTML 与生成规则版本 |

迁移不包含种子账号。权限目录和内置角色由可信注册表同步，不由任意配置或用户输入创造可执行权限。

执行入口分两类：`migrate_schema` 只执行 SQL；完整 `migrate` 在 SQL 后重建正文派生物。身份、密码、角色、OAuth 和媒体维护只要求 schema 就绪，不能被无效旧正文或主题阻断；文章命令、serve 和显式迁移执行完整迁移。命令使用方法见[开发与运行](development.md)。

## 2. 表与通用约定

| 范围 | 表 | 用途 |
|---|---|---|
| 身份 | `users`、`oauth_accounts` | 本站账号与外部登录身份分离 |
| 权限 | `roles`、`permissions`、`user_roles`、`role_permissions` | 多角色权限并集 |
| 内容 | `posts`、`pages` | 分别保存文章与独立页面的当前正文 |
| 目录 | `categories`、`series`、`tags`、`post_tags` | 分类树、有序系列和多标签 |
| 媒体 | `media_assets`、`content_media_refs` | 文件元数据、生命周期和真实引用 |
| 系统 | `settings` | 按 key 分组的 JSONB 设置 |
| 会话 | `sessions` | 本站会话摘要、期限和身份版本 |

实体 UUID 由应用生成；关系表采用复合主键，settings 以 key 为主键，sessions 以令牌摘要为主键。时间使用 `timestamptz`；状态使用字符串与 CHECK。`updated_at` 由写入逻辑维护，插入默认值不能代替更新逻辑。

可变的用户、角色、目录、内容、媒体和设置记录有正整数 `version bigint`，初值 1。它表示当前提交版本，不是修订历史。文章标签变化也属于文章提交；角色分配变化递增目标用户版本。系列整体重排递增系列及成员文章版本，不能把所有写入都解释成“同值不增版”。

文本长度、JSON 结构和请求大小由领域、应用及接口共同限制；SQL 的 text/jsonb 类型不代表接受无限输入。准确列类型、CHECK 和索引以迁移 SQL 为准。

## 3. 身份与 RBAC

| 表 | 主要字段和约束 |
|---|---|
| `users` | `id`、唯一 `username`、可空且唯一 `email`、可空 `password_hash`、`display_name`、`avatar_media_id`、`version`、时间和 `deleted_at` |
| `oauth_accounts` | `id`、`user_id`、`provider`、`provider_user_id`、外部邮箱快照与时间；唯一 `(provider, provider_user_id)` |
| `roles` | `id`、`name`、唯一 `slug`、描述、版本和时间 |
| `permissions` | `id`、名称、唯一 `key`、描述 |
| `user_roles` | 主键 `(user_id, role_id)`；两端均为 RESTRICT 外键 |
| `role_permissions` | 主键 `(role_id, permission_id)`；角色端 CASCADE，权限端 RESTRICT |

用户名写入前 trim 并转 ASCII 小写，允许 ASCII 字母数字、`-`、`_`；邮箱 trim、空串视为空，但不转小写。软删除账号仍占用用户名和邮箱唯一值。

`password_hash` 存 Argon2id PHC 字符串，经独立凭据端口读写，不进入普通用户快照。外部邮箱只是资料快照，不唯一，也不用于自动合并账号。OIDC 的 provider 使用精确 issuer，GitHub 使用固定平台实例标识；外部 ID 使用 sub 或稳定用户 ID，不使用展示名或邮箱。数据库不保存第三方 access/refresh token。

用户的文章、媒体、外部身份等业务引用不级联删除；会话属于运行态，可随物理删除账号清理。角色名称不承担授权语义；内置角色保护、委派上限和最后 Owner 校验由应用与事务逻辑执行，FK 不能代替这些规则。

schema 能表达自定义角色，但当前后台仅提供角色目录和用户角色分配，不提供自定义角色创建、改名、权限编辑或删除工作流。

## 4. 内容、目录与并发关系

| 表 | 主要业务字段 |
|---|---|
| `posts` | `id`、作者、分类、系列、标题、slug、摘要、源文、清洗 HTML、生成规则版本、封面、系列位置、状态、可见性、首次发布时间、版本、时间、回收站时间 |
| `pages` | `id`、标题、slug、源文、清洗 HTML、生成规则版本、状态、可见性、首次发布时间、版本和时间；无作者与回收站字段 |
| `categories` | `id`、名称、slug、可空 `parent_id`、描述、版本和时间 |
| `series` | `id`、名称、slug、描述、`cover_media_id`、版本和时间 |
| `tags` | `id`、名称、slug、版本和创建时间 |
| `post_tags` | 主键 `(post_id, tag_id)`；文章端 CASCADE，标签端 RESTRICT |

Post/Page 的 `content_type` 只允许 markdown；status 只允许 draft/published/archived，visibility 只允许 public/private。published 必须有 `published_at`。slug 表内唯一且为 1–200 UTF-8 字节；合法字符、首次发布后锁定和 Page 系统保留路径由领域/应用验证。

文章至多有一个分类和一个系列。分类 parent_id 自引用 CHECK 排除自身，但多节点环还需要统一树事务锁和祖先链检查。分类、系列、标签被内容引用时 RESTRICT；分类有子节点时同样不能删除。

系列 ID 与 `series_order` 同空或同非空，序号必须为正整数。`posts_series_position_unique` 对 `(series_id, series_order)` 唯一，草稿、私有及回收站记录也占位置。约束为 `DEFERRABLE INITIALLY IMMEDIATE`，重排事务可延后检查以交换位置；系列行按 ID 排序加锁，再锁相关文章。重排复核系列版本和完整成员集合；跨系列移动与成员增减递增相关系列版本。

内容提交将正文、关系、版本及媒体引用一起落库。公开查询有与公开谓词一致的部分索引；作者列表、分类、回收站、标签反查另有索引，系列顺序复用位置唯一索引。读写契约及公开边界见[内容生命周期](content-lifecycle.md)。

### 持久化 HTML

`content_html` 是清洗后的派生字段，`content_render_version integer` 标识生成规则，默认 0 表示尚未生成。客户端不能直接提交这两个字段。

受限渲染任务在事务外生成 HTML 与正文媒体 ID；仓储把源文、HTML、规则版本、业务版本和全部媒体引用同事务提交。公开详情读取 HTML，不在请求内重新执行 Markdown 转换。

规则重建按批读取旧规则记录，在事务外渲染，以 ID、原正文、原业务版本和待重建规则版本条件更新。HTML 与引用集合一起提交；并发编辑已改变源文或业务版本时不会被旧重建结果覆盖。重建不增加业务 `version`，不更改 `updated_at`，也不伪造新的编辑历史。

## 5. 媒体与多态引用

| 表/字段 | 存储约束 |
|---|---|
| `media_assets` | UUID 主键、上传者、唯一随机 `storage_key`、展示名、MIME、正数字节数和尺寸、SHA-256、状态、版本及时间 |
| `content_media_refs` | 主键 `(media_id, content_type, content_id)`；`media_id` 为 RESTRICT 外键 |
| `posts.cover_media_id`、`series.cover_media_id`、`users.avatar_media_id` | 可空媒体外键，RESTRICT |
| `settings.site.logo_media_id` | JSONB 中的媒体 ID，无直接外键；保存时通过引用事务验证资产 |

媒体 MIME 只允许 PNG/JPEG/GIF/WebP，状态只允许 staged/ready/pending_deletion/deleted。存储路径是相对媒体根目录的随机路径，原始文件名仅用于显示。ready 列表、上传者和待回收状态有对应索引。

`content_type` 为 post/page/series/user/site。`content_id` 是多态引用，无法建到多张内容表的 FK；站点单例使用固定 nil UUID。文章永久删除、页面删除和系列删除须在同一事务清除对应引用。替换封面、头像或 logo 整体替换引用集合。

引用表承担删除保护和公开来源判定。保存引用先按媒体 ID 顺序取得共享行锁并确认 ready；删除取得排他行锁，锁内检查全部引用，再转入 pending_deletion。文件删除在数据库事务之外执行，通过状态机补偿，而不是假设文件系统和数据库可共同提交。完整行为见[媒体生命周期](content-lifecycle.md)。

站点 logo 的 JSONB ID 是明确例外：没有字段级 FK，但引用行仍有媒体外键和事务保护。读取时无效 logo 按无 logo 处理。头像更新有意不递增 `users.version`，避免普通资料变更使会话失效。

## 6. 分组设置

`settings` 保存 `key`、JSON 对象 `value`、`version`、`updated_at`。当前使用 site、theme、oauth 分组；分组各有专用端口，普通设置 API 不能按任意 key 写整张表。

| 分组 | 内容与写入边界 |
|---|---|
| site | 标题、描述、可空 logo ID；`settings.manage`；logo 引用与配置 CAS 同事务 |
| theme | 已安装主题选择；`settings.manage`；不存在的主题不能保存 |
| oauth | 提供商非敏感元数据与 `secret_ref`；受控 OAuth 维护、`oauth.manage` |

site/theme 行不存在时管理视图版本为 0，首次保存以此作为 CAS 前提；数据库行从版本 1 开始。即使值等于环境回退值，首次保存也建立数据库配置；之后同值保存可幂等返回，但显式版本仍须匹配。

site 生效顺序为数据库、装配环境值、内置默认。缺失/空白标题回退，缺失描述回退，已保存的空描述合法；公开读取设置失败时回退，管理读取如实报错。环境变量说明统一见[配置](configuration.md)。模板只接收允许公开的站点 DTO，不读取任意 settings JSON。秘密本身不保存在此表。

## 7. 持久会话

`sessions` 的字段为 `token_hash`、`user_id`、`csrf_token`、`user_version`、`created_at`、`last_seen_at`、`expires_at`。

- 主键为令牌 SHA-256 的 64 位小写 hex 摘要，数据库不存明文会话令牌。CSRF token 同样为 64 位 hex，但可由同源前端读取。
- `user_id` 为 CASCADE 外键；`user_version` 必须为正整数。最后活跃时间不得早于创建时间，绝对过期时间必须晚于创建时间。
- 主键支持校验/单会话撤销；用户索引支持批量撤销，过期时间和活跃时间索引支持清理与容量淘汰。
- 创建会话在专用事务 advisory lock 内清理过期、按最久未活跃淘汰并插入，保证并发不超容量。按用户撤销使用同一把锁，防止撤销完成后又落入此前尚未提交的旧创建。
- 校验刷新活跃时间但不延长绝对期限；应用另比对 `user_version` 与账号当前版本。期限、Cookie 和改密行为见[身份与后台](identity-and-admin.md)。

会话随数据库备份恢复，旧会话可能随之重新出现；恢复流程必须显式撤销，见[运维与恢复](operations-and-recovery.md)。OAuth 尝试和登录限流目前在有界内存，不存入 settings。
