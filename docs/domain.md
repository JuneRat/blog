# 领域模型与不变量

本文描述当前 `domain` crate。内容的完整操作语义见[内容生命周期](content-lifecycle.md)，跨层依赖见[架构](architecture.md)，数据库并发与约束见[数据库设计](database-design.md)。

## 当前模块

领域层按业务概念组织，简单模型保留在单文件；[crate 入口](../crates/domain/src/lib.rs) 公开 `content`、`identity`、`media`、`settings` 四个模块。

| 模块 | 已实现类型 | 保护的规则 |
|---|---|---|
| [content/post.rs](../crates/domain/src/content/post.rs) | `Post`、`Slug`、`SeriesPlacement`、创建元数据与编辑补丁 | 当前正文、发布状态、路径锁定、系列位置格式 |
| [content/page.rs](../crates/domain/src/content/page.rs) | `Page`、页面状态与编辑补丁 | 独立页面正文、发布状态、根路径保留名 |
| [content/category.rs](../crates/domain/src/content/category.rs)、[tag.rs](../crates/domain/src/content/tag.rs)、[series.rs](../crates/domain/src/content/series.rs) | `Category`、`Tag`、`Series` | 名称/描述规范化、长度、不可变 slug |
| [identity.rs](../crates/domain/src/identity.rs) | `User`、`UserId`、`PermissionSet`、密码策略 | 用户名与资料格式、权限集合语义、新密码规则 |
| [media.rs](../crates/domain/src/media.rs) | `Media`、媒体状态、图片格式与校验结果 | 资产状态转换、上传大小与声明尺寸、文件名规范化 |
| [settings.rs](../crates/domain/src/settings.rs) | `SiteSettings` | 站点标题、描述与 logo 的完整写入值 |

没有 `appearance`、邀请、主题激活或通用扩展聚合；角色授权、OAuth 绑定和设置持久化由应用用例与对应端口组织。数据库关系表不自动对应一个领域聚合。

## Post 与 Page

两个聚合各维护一份当前 Markdown。草稿允许空标题/正文，发布要求两者非空；编辑已发布内容时不能清空。编辑先验证所有候选字段，再整体更新，失败不留下部分修改。

Post 的身份、作者和关联使用 ID；不嵌入完整 User、Category、Series 或 Media 聚合。`PostDraftMetadata` 让创建时的分类、系列、封面通过构造入口设置；`SeriesPlacement` 同时约束系列 ID 与正整数序号，创建与编辑复用相同规则。Page 无作者、分类、标签或系列字段。

| 行为 | 聚合规则 |
|---|---|
| `create_draft` / `create_draft_with_metadata` | 生成 UUIDv7 身份，初始 `version = 1`，验证可写字段 |
| `edit` | 保留未提供字段；受支持的可空关联区分“不修改、清空、设置”；返回是否实际变化 |
| `publish(now)` | draft → published；首次写入 `published_at`，再发布保留首次时间；已发布时无操作 |
| `withdraw()` | published → draft；保留首次发布时间与 slug 锁定状态 |
| Post `trash(now)` | 设置删除时间，保留状态和首次发布时间；重复操作无变化 |
| Post `restore()` | 清除删除时间；非归档内容恢复为草稿，避免自动上线；归档仍保持终态 |

`archived` 是已定义的终态：禁止编辑和重新发布。当前没有管理端归档动作，不能把枚举中存在该值等同于已交付归档流程。Page 不含 `deleted_at`，物理删除由应用用例和版本条件删除端口完成。

Post/Page 聚合报告是否发生变化，提交后的版本与更新时间由仓储返回；标签单独变化也由应用识别为一次内容提交。并发版本比较、内容是否仍存在以及恢复/删除的数据库竞争不由聚合自行判断。

### 路径与格式

`Slug` 是单段路径值对象：非空、最多 200 个 UTF-8 字节，只允许 Unicode 字母数字、`-`、`_`。它不查询数据库；各表唯一性由持久化约束保证。

Post/Page 第一次发布后禁止改 slug，撤回不解锁。Page 在创建、改名和发布时另行拒绝[系统保留根路径](../crates/domain/src/content/page.rs)。分类、标签和系列的 slug 创建后始终不可变。

标题上限为 300 字符，文章摘要上限为 1,000 字符。分类和标签名称上限 100 字符，系列名称上限 200 字符；目录名称先 trim，不能为空。分类和系列描述允许为空，非空时最多 2,000 字符。具体常量与错误均保留在所属领域模块，不在 HTTP handler 重写另一套规则。

## 跨聚合规则的归属

单个聚合只验证它持有的状态；需要检查其他记录或并发竞争的规则，由应用协调、存储事务兜底。

| 规则 | 执行边界 |
|---|---|
| 分类父节点存在、移动后无环、含子节点或引用时拒绝删除 | 分类用例与分类树事务锁 |
| 系列成员完整性、位置唯一、重排与跨系列移动 | 系列/文章用例、系列行锁、版本条件和数据库约束 |
| 标签、分类、系列是否被文章引用 | 对应目录删除端口；聚合不查询其他内容 |
| 媒体存在、可用、引用权、禁止删除仍被引用资产 | 应用授权与媒体引用事务协议 |
| 作者归属、own/any 权限、Page 站点权限 | 应用 `Actor` 与授权用例 |
| 最后 Owner、最后登录方式、角色委派上限 | 身份用例与身份变更锁 |

Post 只引用 `identity::UserId`，不引用 User 聚合或权限实现。公开作者信息、目录当前名称和图片 URL 由读取用例组装，不让聚合互相持有展示数据。

## 身份、媒体与站点值

`User` 创建入口规范化用户名（trim、ASCII 小写），验证用户名、可选邮箱和展示名。`PermissionSet` 只表达权限并集、成员判断和子集关系；具体权限 key、内置角色与授权策略由应用层定义。密码策略也是纯规则，密码哈希、登录限流、会话和提供商协议在外层。详细身份流程见[身份与后台](identity-and-admin.md)。

`Media` 区分 `staged`、`ready`、`pending_deletion`、`deleted`，只允许明确的状态转换；未完成上传可从 staged 进入待删除状态。删除决定与文件删除完成分开，以支持重试。只有 ready 资产可被引用，但 ready 本身不代表匿名可读：公开性实时取决于引用它的内容。

图片校验接受 PNG、JPEG、GIF、WebP，按文件头识别格式与尺寸；上限为 10 MiB、单边 12,000 像素、总像素 60,000,000。它不完整解码图片，不能据此宣称已检查所有损坏文件。存储路径由资产 ID 决定，原始文件名只供展示。

`SiteSettings` 是值对象，构造时 trim 标题/描述，标题非空且最多 200 字符，描述最多 500 字符。logo 使用可空媒体 ID；资产可用性由外层检查。数据库、环境变量和默认值的读取回退属于应用层，不属于该值对象。

## 可见性与可信重建

聚合内部字段私有，对外提供业务方法、必要访问器及 `snapshot()` 副本。[content/mod.rs](../crates/domain/src/content/mod.rs) 显式重导出常用类型，同时保留公开子模块；现有调用方可以使用 `domain::content::Post` 或 `domain::content::post::Post`。

快照结构的字段公开，`reconstitute(snapshot)` 用于从可信仓储结果恢复聚合。现有重建函数信任快照，不重新执行创建校验；它不是处理 HTTP 输入的验证入口，也不是按调用者限制访问的安全机制。应用写流程应调用聚合行为，不能修改快照后用重建绕过规则。Post/Page 写端口接收聚合，具体收口见 [ADR-0014](adr/0014-content-commits-and-stable-admin-identity.md)。

私有字段保护日常状态修改，Cargo 与模块可见性限制可达范围，但统一 `domain` 中的公开模型对所有合法依赖者可见。若将来需要某业务上下文在编译期完全无法使用另一聚合，应拆分 crate 并收紧依赖；仅移动文件不能提供这种隔离。

## 模型之外

领域不保存清洗 HTML、渲染规则版本、模板状态、SQL 事务、HTTP 请求或会话。`content_html` 是基础设施维护的派生物；仓储、查询、时钟、渲染端口及 DTO 位于应用层。领域错误描述规则失败，不携带 HTTP 状态或数据库错误。

当前不发布领域事件，也不预建 `BaseEntity`、通用仓储或扩展聚合。新增模型应先说明它保护的不变量和可独立验证的行为；后续功能与外部集成边界见[产品路线图](product-roadmap.md)和[扩展设计](extensions-and-data.md)。
