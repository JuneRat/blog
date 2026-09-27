# 数据库设计

本文记录已确认的 PostgreSQL 18 目标设计，共 **19 张表：18 张业务表和 `sessions`**。字段、外键、CHECK 与索引以根目录的 [blog_schema.sql](../blog_schema.sql) 为准，选择理由见 [ADR-0016](adr/0016-confirmed-blog-schema.md)。

**新建库基线、身份会话、媒体、内容、目录与评论已接入。** `migrate` 现在执行新的 [0001_initial_schema.sql](../migrations/postgres/0001_initial_schema.sql)，原九个迁移已替换，仅支持空库或已应用新基线的库；检测到旧结构时退出，不自动清库。保留期任务、独立授权和新库恢复工具已接入，正式媒体物理清理与生产上线验收仍待完成。实际适配边界见[当前数据库实现](database-current.md)，后续验收见[路线图](product-roadmap.md#已采纳数据库设计的实施)。

## 1. 表清单与通用约定

| 范围 | 表 | 关系与职责 |
|---|---|---|
| 身份 | `users`、`oauth_accounts` | 本站账号与固定提供商的外部身份 |
| 权限 | `roles`、`permissions`、`user_roles`、`role_permissions` | 多角色与权限并集 |
| 内容 | `posts`、`pages` | 各自保存一份当前正文；页面为全站资源 |
| 目录 | `categories`、`tags`、`series` | 分类树、标签、系列 |
| 内容关联 | `post_tags`、`post_series` | 文章与标签、系列均为多对多 |
| 媒体 | `media`、`media_refs` | 媒体元数据、使用位置与删除保护 |
| 评论 | `comments` | 多级回复、根评论、审核与回收站 |
| 系统 | `settings`、`audit_logs` | 分组配置与成功业务变更审计 |
| 会话 | `sessions` | 令牌摘要、认证版本快照与期限 |

实体 UUIDv7 由应用生成；关系表使用复合主键。`permissions.code`、`settings.key` 与 `sessions.token_hash` 直接作主键，不额外添加 UUID。时间使用 `timestamptz`，状态使用文本加 CHECK。`updated_at` 由实际写入维护，默认值不代替更新逻辑。

三个版本字段分别存储，不放入 metadata：

| 字段 | 用途与更新规则 |
|---|---|
| `version bigint` | 可编辑实体的正整数乐观锁，初值 1；更新校验客户端读到的版本，在同一事务递增并维护 `updated_at` |
| `users.auth_version bigint` | 独立认证修订号，初值 1；凭据撤销、禁用/删除账号、退出全部设备等操作递增 |
| `content_render_version integer` | Post/Page/Comment 的正整数渲染规则版本；重建 HTML 不递增编辑版本，也不改变业务更新时间 |

关联表不维护独立编辑版本，其修改由所属实体的版本和事务保护。`sessions.auth_version` 是签发时的快照，不是会话自身的编辑版本。`audit_logs` 只追加，不设编辑版本。

## 2. 身份与权限

| 表 | 主要字段与约束 |
|---|---|
| `users` | 用户名、可空邮箱/密码哈希/展示名/纯文本 bio/头像；`status` 为 active 或 disabled；独立 `version`、`auth_version` 与软删除时间 |
| `oauth_accounts` | `(provider, subject)` 复合主键、`user_id`、`created_at`；无额外 ID、邮箱快照和更新时间 |
| `roles` | UUID、稳定唯一 `code`、可编辑名称/描述、版本与时间 |
| `permissions` | `code` 文本主键、名称；由代码注册可执行权限 |
| `user_roles` | `(user_id, role_id)` 主键；删除用户清理分配，仍被分配的角色拒绝删除 |
| `role_permissions` | `(role_id, permission_code)` 主键；删除角色或权限清理关联 |

用户名在应用写入边界规范化；用户名和邮箱分别通过 `lower(...)` 唯一索引实现不区分大小写的唯一性。软删除仍占用这些标识。仅 active 且未软删除的用户可以登录或通过受保护请求。

用户禁用/软删除不自动隐藏其文章、评论和媒体。有文章、媒体或评论引用时拒绝物理删除用户；允许物理删除时，OAuth 绑定、角色分配和会话随之清理。头像使用 `avatar_media_id` 外键，普通资料及头像编辑维护 `version`，不因此递增 `auth_version`。

OAuth/OIDC 的 provider 固定到可信实例，subject 使用稳定外部账号标识；不按相同邮箱自动合并账号。提供商授权码流程、state/nonce 校验、绑定前核验、最后登录方式和最后管理员保护由应用完成，相关身份变更在共享事务锁内检查。

受保护请求重读当前账号状态、认证版本及权限。角色/权限变化通过重读即时生效，不为此强制退出所有会话；角色代码与权限代码不能由显示名称代替。密码哈希仍通过独立凭据端口处理，秘密值不进入普通用户 DTO 或设置 JSON。

## 3. 内容、目录与提交

Post 保留作者、可选分类、摘要、封面和单篇评论开关。Page 没有作者，按全站页面权限管理；页面也有 `deleted_at`。两者各存一份 Markdown，不引入修订历史或编辑副本，保存已发布内容仍直接更新当前正文。

文章至多一个分类，可以属于多个标签和多个系列：

| 关系 | 规则 |
|---|---|
| 分类树 | `parent_id` 外键与 CHECK 排除缺失父级和自身父级；应用在共享树事务锁内检查祖先链，阻止多节点环 |
| `post_tags` | `(post_id, tag_id)` 唯一，删除标签只清理关联，保留文章 |
| `post_series` | `(post_id, series_id)` 唯一；`position >= 0`，默认 0，可重复，按 `position, post_id` 稳定排序 |
| 删除目录 | 有子分类或文章引用的分类拒绝删除；删除系列只清理成员关联及自己的媒体引用，保留文章 |

系列位置是排序权重，不是唯一章节号；不再使用 `posts.series_id/series_order` 或延迟唯一位置约束。关联增减、批量排序与所属实体的编辑版本在同一事务维护。文章软删除保留标签、系列与媒体关系，公开关联查询另行过滤文章可见性。

内容保存将源文、清洗 HTML、渲染版本、关联关系、编辑版本、更新时间与审计一起提交。应用先完成渲染及输入校验，仓储在事务内检查版本、关系和引用；失败整体回滚。返回本次提交的完整记录，避免提交后再查询关系并拼接出另一版本的结果。

### 正文与持久化 HTML

Post/Page 的 `content_type` 只允许 `markdown`，数据库使用 `CHECK (content_type = 'markdown')`；不预留 HTML、纯文本三种正文模式。Comment 固定使用受限 Markdown，不设置 `content_type`。

`content`、`content_html`（NOT NULL，允许空串）和正整数 `content_render_version` 必须在同次保存写入。空草稿的 HTML 可以是空串；正常记录不使用 NULL 或渲染版本 0 作为待生成占位。客户端不能指定可信 HTML，服务端负责渲染、清理和 URL 校验。

| 字段 | 上限 |
|---|---|
| Post/Page 标题 | 300 字符 |
| Post 摘要 | 1,000 字符 |
| Post/Page Markdown 源文 | 1 MiB UTF-8 字节 |
| Post/Page/Comment 清洗 HTML | 768 KiB UTF-8 字节 |
| slug | 200 UTF-8 字节 |
| Comment 源文 | 非空，最多 2,000 字符 |

文章、页面、评论各自使用明确的渲染规则版本。升级规则后重建不匹配的 HTML，以原文与编辑版本为条件更新，避免覆盖并发保存；重建正文图片引用与 HTML 同事务提交。输出前保证 HTML 对应当前规则，不以原文作为 HTML 回退。

### 状态、预约与路径

Post/Page 统一使用 draft、scheduled、published、archived 四种状态，以及 public/private 可见性。软删除由 `deleted_at` 单独表达。

| 操作 | 目标行为 |
|---|---|
| 创建草稿 | 允许空标题与正文，`published_at` 可空 |
| 预约发布 | 完整校验标题/正文和预约时间，写入 scheduled 与 `published_at` |
| 到期发布 | 任务仅处理仍 scheduled、未删除且已到期的记录，事务性转为 published，维护版本、时间与审计 |
| 立即发布 | 完整校验；若保留了未来预约时间，改为当前时间；到期任务则保留原预约时间 |
| 取消预约或撤回 | 回到 draft，保留 `published_at` 和 slug 锁定；不会自动重新发布 |
| 归档 | 可从 archived 回到 draft，归档不再是不可恢复终态 |
| 移入回收站/恢复 | 保留 slug 和全部引用；Post/Page 恢复统一回到 draft |
| 永久删除 | 清理所属关系及多态媒体引用；文章删除还清理整棵评论树 |

`published_at` 用于预约及公开展示时间，不再表示不可变的首次实际发布时间。首次预约或发布就锁定 slug，应用禁止将时间清回 NULL 以绕过锁定；重新预约可以调整时间。未首次预约/发布的草稿可以改名。

文章使用 `/posts/{slug}`，页面使用 `/{slug}`，两个命名空间分开；页面拒绝系统保留路径。slug 的合法字符与锁定由应用保证，数据库 UNIQUE 保证占用，回收站记录仍占用 slug。

所有公开详情、列表、目录、模板数据、RSS 和 sitemap 统一要求：`deleted_at IS NULL AND status = 'published' AND visibility = 'public' AND published_at <= now()`。定时任务与编辑、取消预约、撤回和删除共享事务/版本边界；仅靠查询时间过滤不能代替状态发布。`now()` 不进入永久 CHECK 或部分索引谓词。

## 4. 媒体与引用

`media` 保存稳定 `path`、展示文件名、MIME、正数大小/尺寸、SHA-256、可空上传者、版本与时间、`deleted_at`。图片限 PNG/JPEG/GIF/WebP；服务端检查实际文件类型与尺寸。路径不包含域名或过期签名，哈希不作为去重唯一键。文件完成写入后才注册可引用记录；目标业务表不保留 staged/ready/pending_deletion/deleted 状态列。

**所有媒体链接独立公开。** 文章变私密、撤回、进入回收站，或用户停用，都不使图片链接失效。媒体软删除只改变管理记录，保留对象与 URL。需要保密的材料不能依靠私密文章隐藏其媒体链接。

`media_refs` 保留，负责使用位置查询和删除保护，不参与媒体读取鉴权：

| 字段 | 约束 |
|---|---|
| `media_id` | 指向 media 的 RESTRICT 外键 |
| `source_type` | post/page/series/user/site 五种值 |
| `source_id` | 来源 UUID；site 当且仅当使用 nil UUID |
| 主键 | `(media_id, source_type, source_id)`；同一来源的正文、封面等引用取并集 |

正文引用从同一份清洗 HTML 提取；封面、头像采用明确媒体外键；站点 logo 的 ID 放在 `settings.site`，与站点引用同事务保存。`source_id` 是多态关系，来源存在性、更新和清理由应用保证。草稿、私密、归档及回收站内容都计入引用；软删除不清理引用。

保存时同步当前来源的引用集合，不需要每次扫描全站。全量扫描可用于核对和修复，但不能发现全部站外链接。物理清理由独立流程执行：与新增引用遵循同一媒体行锁协议，存在已知引用就拒绝删除；即使零引用也需要显式确认清理范围，不能自动判定为可删除。文件与数据库不能原子提交，物理清理仍须处理失败重试，具体运行适配器另行实现。

## 5. 评论

评论使用 `post_id`、直接回复对象 `parent_id` 和所属根评论 `root_id` 表达任意层级关系，前端只展示两级：

- 根评论的 parent_id/root_id 均为空。
- 回复的 parent_id 指向直接回复对象；父级为根时 root_id 取父级 ID，否则继承父级的 root_id。
- 根评论分页，后代按 root_id 平铺在第二级，parent_id 用于展示“回复谁”。
- 数据库复合外键保证父级和根属于同篇文章，CHECK 保证空值形态及不引用自身。应用验证根本身没有父级；post_id/parent_id/root_id 创建后不可修改，父评论必须已存在。

评论状态为 pending、approved、spam、trash；所有新评论和后台回复默认 pending，从 spam/trash 恢复也统一回 pending。普通审核不通过可移入 trash，不保留 rejected 状态。

删除只把本条评论移入 trash，不删除或重新审核子评论。已删除节点仍有可见后代时，公开列表保留“该评论已删除”占位，并隐藏昵称、作者标记、原文与 HTML；pending/spam 祖先仍有可见后代时保留匿名的“该评论暂不可用”占位。根已删除也不应让已通过审核的后代消失。单独物理删除被 parent_id/root_id 引用的节点会被外键拒绝；文章永久删除时可以清理整棵树。

### 输入与展示

受限 Markdown 支持普通换行、分段、粗体、斜体、删除线、行内/围栏代码、引用、列表、HTTP(S) 链接、普通网址自动识别及 Unicode Emoji。编辑器提供精简工具栏和预览；不支持标题、表格、嵌入媒体或可执行原始 HTML。原始 HTML 按文字转义，代码正常转义，链接和最终 HTML 经服务端规则处理。

评论保存源文、清洗 HTML 和渲染规则版本，按正文相同的提交与重建规则维护。评论不嵌入媒体，因此不写入 media_refs。昵称保持纯文本，并保存登录用户或游客提交时的展示快照；游客邮箱可选且仅作私密联系信息，不用于身份认证。

不保留评论提交 `request_id`、`client_hash`，不实现评论去重或评论提交频率限制。每次有效提交独立创建待审核记录，包括网络重试后再次提交；HTTP 请求追踪编号与认证登录限流不属于这项移除范围。

### 开关与隐私

全站 `settings.comments.enabled` 与 `posts.comments_enabled` 默认均为 true，只有同时开启才允许新评论/回复；关闭只停止提交，仍展示已通过历史评论。不建立 `comment_settings` 或 `post_comment_settings`。修改全站开关递增该设置组版本，修改单篇开关递增 posts.version；评论编辑/审核使用自己的 version，不递增文章版本。

`author_email` 和 `ip_address` 不进入公开响应，只允许授权后台读取。IP 用可空 inet 保存单个 IPv4/IPv6 主机地址，未知或离线导入时留空；编辑、审核不覆盖提交来源。HTTP 层只从配置的可信代理解析真实来源，不信任任意转发头，也不使用数据库连接 IP。默认 180 天后清空评论 IP，保留正文、关系和审核状态。

## 6. 分组设置与审计

`settings(key, value, version, updated_at)` 每组保存 JSON 对象。代码定义组名、类型、默认值、授权和公开读取白名单；数据库只保存覆盖值，不为默认值预插入行，不接收任意未注册配置。

| 组 | 内容 |
|---|---|
| site | 站点资料与 logo_media_id；logo 引用同事务维护 |
| theme | 已安装主题选择 |
| oauth | 非敏感提供商配置及秘密引用；秘密本身留在部署秘密存储 |
| comments | enabled 默认 true、ip_retention_days 默认 180 |
| audit | retention_days 默认 180 |

`audit_logs` 保存 actor_id、来源 IP、action、target_type、文本 target_id、脱敏 metadata 对象及 created_at。目标既可能是 UUID，也可能是设置/权限键，因此 target_id 使用 text。操作者和目标是历史快照，不建立业务外键。

成功业务变更和审计记录在同一事务提交；失败登录等事件留在安全日志，不能伪装成成功事务审计。系统任务允许 actor_id/IP 为空。metadata 只保存必要的脱敏摘要，不转储正文、邮箱、Cookie 或凭据。

应用运行账号只能按需读取/追加审计，不允许 UPDATE/DELETE/TRUNCATE；默认 180 天保留期的删除由单独授权的维护身份执行。运行账号授权、维护任务和可信代理配置需随实现交付，DDL 不自动配置这些能力。该表不保证抵御数据库管理员篡改。

## 7. 持久会话

`sessions` 是独立运行表，不放入 settings，也不增加通用 metadata、UUID 或编辑 version。

| 字段 | 规则 |
|---|---|
| `token_hash` | SHA-256 的 64 位小写 hex 摘要，使用 C 排序规则并直接作主键；浏览器保存独立随机令牌原值 |
| `user_id` | 必填用户外键，用户物理删除时 CASCADE |
| `csrf_token` | 另一份独立随机值，64 位小写 hex；供同源前端校验，不作为会话登录凭据 |
| `auth_version` | 签发时 users.auth_version 的正整数快照，不能在活跃刷新时自动同步 |
| `created_at` | 创建时间 |
| `last_seen_at` | 最近成功鉴权时间，不早于创建、不晚于绝对期限 |
| `expires_at` | 签发时固定的绝对期限，必须晚于创建时间 |

每次鉴权都要求账号 active/未删除、认证版本相等、`expires_at >= 当前时间` 且 `last_seen_at >= 当前时间 - 空闲 TTL`。全部校验通过后才刷新活跃时间，不延长绝对期限；TTL 来自会话配置，不写死在 CHECK 中。权限实时重读，不在会话内缓存角色/权限。

单设备退出删除对应行；退出全部设备、改密、移除登录方式、禁用/删除账号等撤销操作，同事务递增 users.auth_version，并可清理全部会话。旧快照尚未物理清理也必须鉴权失败；资料编辑只维护用户编辑版本，不让会话掉线。

用户、过期时间、活跃时间分别有索引，用于按用户撤销、绝对/空闲过期清理及按需容量淘汰。创建、清理、淘汰与撤销的并发由适配器事务协调，表约束不能代替鉴权、令牌轮换或 CSRF 检查。数据库恢复后明确撤销旧会话，避免备份重新引入有效登录凭据。

## 8. 与原实现的主要差异

原结构与目标结构都为 19 张应用表，但不能因表数相同而互换。下表保留设计变更的对照；新初始迁移及身份会话已落地，其他代码、API 和前端待分批适配。实际状态见[当前数据库实现](database-current.md)。

| 范围 | 原实现 | 已采纳目标 |
|---|---|---|
| 媒体 | media_assets/content_media_refs；状态机及公开来源决定匿名读取 | media/media_refs；链接独立公开，软删除保留链接，独立物理清理 |
| 系列 | 文章单系列、唯一正整数位置 | post_series 多对多、可重复非负排序权重 |
| 身份版本 | users.version 同时承担会话失效；sessions.user_version | 编辑 version 与 auth_version 分离；会话保存 auth_version 快照 |
| 用户/OAuth/RBAC | 旧字段与 ID/slug/key 组合 | 用户状态/bio/邮箱不区分大小写；OAuth 复合主键；角色 code、权限 code 主键 |
| 内容状态 | 无 scheduled；首次发布时间不变；归档终态；Page 物理删除 | 定时发布、预约即锁 slug、归档可退草稿、Page 回收站 |
| 评论 | 纯文本、一层回复、rejected、父删除级联、去重限流 | 受限 Markdown 与 HTML 持久化、多级关系两级展示、trash 占位保留后代、不去重限流 |
| 评论设置 | comment_settings/post_comment_settings 独立表及版本 | settings.comments 与 posts.comments_enabled |
| 审计和 IP | 无事务业务审计，评论只存来源摘要 | audit_logs 与可空主机 IP，默认 180 天保留策略 |

新初始迁移、身份会话、媒体引用、内容生命周期、多系列、评论根关系及 HTML 重建已在隔离 PostgreSQL 18 中验证。评论 IP/审计保留期、独立审计授权及含媒体的隔离恢复已接入；正式媒体清理、其余审计写入覆盖及生产演练仍需完成，各项边界以实施路线为准。
