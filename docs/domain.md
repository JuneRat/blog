# 领域模型与不变量

本文描述当前 `domain` crate。内容的完整操作语义见[内容生命周期](content-lifecycle.md)，跨层依赖见[架构](architecture.md)，数据库并发与约束见[当前数据库实现](database-current.md)。

[ADR-0016](adr/0016-confirmed-blog-schema.md) 采纳的编辑/认证版本分离、系列多对多、定时发布、Page 回收站和媒体独立公开已进入实现。评论已支持受限 Markdown 校验、多级关系和回收站状态；完整规则见[数据库设计](database-design.md)。

## 当前模块

领域层按业务概念组织，简单模型保留在单文件；[crate 入口](../crates/domain/src/lib.rs) 公开 `content`、`identity`、`media`、`comment`、`settings` 五个模块。

| 模块 | 已实现类型 | 保护的规则 |
|---|---|---|
| [content/post.rs](../crates/domain/src/content/post.rs) | `Post`、`SeriesPlacement`、创建元数据与编辑补丁 | 当前正文、发布状态、路径锁定、系列位置格式 |
| [content/slug.rs](../crates/domain/src/content/slug.rs)、[visibility.rs](../crates/domain/src/content/visibility.rs) | `Slug`、`SlugError`、`Visibility` | 共享路径格式与可见性；外部统一从 `domain::content` 导入 |
| [content/page.rs](../crates/domain/src/content/page.rs) | `Page`、页面状态与编辑补丁 | 独立页面正文、发布状态、根路径保留名 |
| [content/category.rs](../crates/domain/src/content/category.rs)、[tag.rs](../crates/domain/src/content/tag.rs)、[series.rs](../crates/domain/src/content/series.rs) | `Category`、`Tag`、`Series` | 名称/描述规范化、长度、不可变 slug |
| [identity/user.rs](../crates/domain/src/identity/user.rs) | `User`、`UserId`、`UserSnapshot`、`Username`、`Email` | 用户名与资料格式、身份修订号语义 |
| [identity/password.rs](../crates/domain/src/identity/password.rs) | `PasswordError`、密码策略与常量 | 新密码长度、用户名包含与常见口令规则 |
| [identity/permissions.rs](../crates/domain/src/identity/permissions.rs) | `PermissionSet` | 权限并集、成员判断与子集关系 |
| [media.rs](../crates/domain/src/media.rs) | `Media`、软删除标记、图片格式与校验结果 | 软删除和恢复、上传大小与声明尺寸、文件名规范化 |
| [comment.rs](../crates/domain/src/comment.rs) | `CommentBody`、`CommentNickname`、`CommentStatus`、`ModerationAction` | 评论正文与昵称校验、审核动作与状态 |
| [settings.rs](../crates/domain/src/settings.rs) | `SiteSettings` | 站点标题、描述与 logo 的完整写入值 |

没有 `appearance`、邀请、主题激活或通用扩展聚合；角色授权、OAuth 绑定和设置持久化由应用用例与对应端口组织。数据库关系表不自动对应一个领域聚合。

## Post 与 Page

两个聚合各维护一份当前 Markdown。草稿允许空标题/正文，发布和预约要求两者非空；编辑已发布或已预约内容时不能清空。编辑先验证所有候选字段，再整体更新，失败不留下部分修改。

Post 的身份、作者和关联使用 ID；不嵌入完整 User、Category、Series 或 Media 聚合。`PostDraftMetadata` 让创建时的分类、系列、封面通过构造入口设置；`Vec<SeriesPlacement>` 保存多个系列及独立排序权重，权重非负、系列 ID 不重复，创建与编辑复用相同规则。Page 无作者、分类、标签或系列字段。

| 行为 | 聚合规则 |
|---|---|
| `create_draft` / `create_draft_with_metadata` | 生成 UUIDv7 身份，初始 `version = 1`，验证可写字段 |
| `edit` | 保留未提供字段；受支持的可空关联区分“不修改、清空、设置”；返回是否实际变化 |
| `publish(now)` | draft/scheduled → published；无发布时间或时间在未来则写入 now，过去的时间保留；已发布无操作 |
| `schedule(at, now)` | draft/scheduled → scheduled；at 必须晚于 now，写入 `published_at` 并锁定 slug |
| `withdraw()` | published/scheduled/archived → draft；保留发布时间与 slug 锁定状态 |
| `archive()` | 变为 archived，不再公开；重复操作无变化 |
| `trash(now)` | 设置删除时间，保留状态和发布时间；重复操作无变化 |
| `restore()` | 清除删除时间，一律恢复为草稿，避免自动上线或重新预约 |

`archived` 禁止直接编辑和发布，须先显式退回草稿。Post/Page 均有独立于状态的 `deleted_at`，回收站内容须先恢复才能编辑或发布；永久删除由应用授权与版本条件删除端口完成。预约到期由存储任务加锁复核状态、删除标记和时间后转换，聚合不自行运行时钟任务。

Post/Page 聚合报告是否发生变化，提交后的版本与更新时间由仓储返回；标签单独变化也由应用识别为一次内容提交。并发版本比较、内容是否仍存在以及恢复/删除的数据库竞争不由聚合自行判断。

### 路径与格式

`Slug` 是单段路径值对象：非空、最多 200 个 UTF-8 字节，只允许 Unicode 字母数字、`-`、`_`。它不查询数据库；各表唯一性由持久化约束保证。

Post/Page 第一次预约或发布后禁止改 slug，取消预约、撤回与恢复不解锁。Page 在创建、改名和发布时另行拒绝[系统保留根路径](../crates/domain/src/content/page.rs)。分类、标签和系列的 slug 创建后始终不可变。

标题上限为 300 字符，文章摘要上限为 1,000 字符。分类和标签名称上限 100 字符，系列名称上限 200 字符；目录名称先 trim，不能为空。分类和系列描述允许为空，非空时最多 2,000 字符。具体常量与错误均保留在所属领域模块，不在 HTTP handler 重写另一套规则。

## 跨聚合规则的归属

单个聚合只验证它持有的状态；需要检查其他记录或并发竞争的规则，由应用协调、存储事务兜底。

| 规则 | 执行边界 |
|---|---|
| 分类父节点存在、移动后无环、含子节点或引用时拒绝删除 | 分类用例与分类树事务锁 |
| 系列成员完整性、可重复权重、重排与多系列关系 | 系列/文章用例、内容关系事务锁、行锁和版本条件 |
| 删除标签/系列时解除关联、增版并保留文章 | 对应目录删除事务；聚合不查询其他内容 |
| 新媒体引用可用性、软删除保留历史引用、禁止物理删除仍被引用资产 | 应用授权与媒体引用事务协议 |
| 作者归属、own/any 权限、Page 站点权限 | 应用 `Actor` 与授权用例 |
| 最后 Owner、最后登录方式、角色委派上限 | 身份用例与身份变更锁 |

Post 只引用 `identity::UserId`，不引用 User 聚合或权限实现。公开作者信息、目录当前名称和图片 URL 由读取用例组装，不让聚合互相持有展示数据。

## 身份、媒体与站点值

`User` 创建入口规范化用户名（trim、ASCII 小写），验证用户名、可选邮箱和展示名。`PermissionSet` 只表达权限并集、成员判断和子集关系；具体权限 key、内置角色与授权策略由应用层定义。密码策略也是纯规则，密码哈希、登录限流、会话和提供商协议在外层。详细身份流程见[身份与后台](identity-and-admin.md)。

`UserSnapshot::version` 是资料及关联编辑版本，`auth_version` 是独立认证版本。资料、头像和角色修改保持登录，凭据变更递增认证版本并撤销会话；仅 active 且未删除用户允许认证。identity 按 user/password/permissions 拆分，通过 [mod.rs](../crates/domain/src/identity/mod.rs) 显式重导出；调用方继续从 `domain::identity` 导入。

`Media` 在文件就位后登记，使用 deleted_at 表达回收站；软删除/恢复改变编辑版本，保留路径、文件与引用。链接独立公开；新引用要求未软删除，历史引用可继续使用。没有 staged/ready/pending_deletion 等持久化状态。

图片校验接受 PNG、JPEG、GIF、WebP，按文件头识别格式与尺寸；上限为 10 MiB、单边 12,000 像素、总像素 60,000,000。它不完整解码图片，不能据此宣称已检查所有损坏文件。存储路径由资产 ID 决定，原始文件名只供展示。

`SiteSettings` 是值对象，构造时 trim 标题/描述，标题非空且最多 200 字符，描述最多 500 字符。logo 使用可空媒体 ID；资产可用性由外层检查。数据库、环境变量和默认值的读取回退属于应用层，不属于该值对象。

## 可见性与可信重建

聚合内部字段私有，对外提供业务方法、必要访问器及 `snapshot()` 副本。[content/mod.rs](../crates/domain/src/content/mod.rs) 显式重导出常用类型，同时保留公开子模块；现有调用方可以使用 `domain::content::Post` 或 `domain::content::post::Post`。

快照结构的字段公开，`reconstitute(snapshot)` 用于从仓储结果恢复聚合，并校验结构不变量（例如路径、版本、发布状态及字段格式），无效快照会返回错误。重建与新建规则并非完全相同：Post/Page 允许加载历史超长正文以便缩短，但编辑和发布仍校验源文预算。重建不是 HTTP 输入入口，也不是按调用者限制访问的安全机制；应用写流程应调用聚合行为，不能修改快照后用重建绕过业务动作。Post/Page 写端口以及用户、标签、分类、系列和媒体创建端口均接收聚合；Snapshot 用于读取、重建和返回结果。媒体状态迁移、密码更新、目录树移动等仍使用专用原子操作。内容提交细节见 [ADR-0014](adr/0014-content-commits-and-stable-admin-identity.md)。

私有字段保护日常状态修改，Cargo 与模块可见性限制可达范围，但统一 `domain` 中的公开模型对所有合法依赖者可见。若将来需要某业务上下文在编译期完全无法使用另一聚合，应拆分 crate 并收紧依赖；仅移动文件不能提供这种隔离。

## 模型之外

源文长度上限保留在领域 [content/budget.rs](../crates/domain/src/content/budget.rs)。正文 HTML 上限、主题额外输出空间和整页 HTML 上限集中在应用 [rendering_budget.rs](../crates/application/src/rendering_budget.rs)，由基础设施执行；修改渲染策略无需改动领域层。

领域不保存清洗 HTML、渲染规则版本、模板状态、SQL 事务、HTTP 请求或会话。`content_html` 是基础设施维护的派生物；仓储、查询、时钟、渲染端口及 DTO 位于应用层。领域错误描述规则失败，不携带 HTTP 状态或数据库错误。

当前不发布领域事件，也不预建 `BaseEntity`、通用仓储或扩展聚合。新增模型应先说明它保护的不变量和可独立验证的行为；后续功能与外部集成边界见[产品路线图](product-roadmap.md)和[扩展设计](extensions-and-data.md)。
