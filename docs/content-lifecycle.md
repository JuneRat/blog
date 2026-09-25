# 内容、路径与资源生命周期

状态：以用户确认的 [13 表设计](database-design.md) 为当前模型。Post 与 Page 的单份正文、状态转换、首次发布后锁定 slug、`/{slug}` 根路径与系统保留路径校验已实现；标签（目录管理、文章关联同事务保存、公开标签页 `/tags/{slug}` 分页）已交付；分类/系列、媒体、修订与旧路径跳转仍待交付。此前的工作副本/不可变修订与路径登记方案已由 [ADR-0008](adr/0008-thirteen-table-blog-core.md) 替代。

## 1. 一份当前内容

Post 与 Page 分别存储，每条记录只有一份当前正文，不存在 published_revision_id、独立工作副本、历史快照或恢复检查点。Post 另有 author_id、category_id、series_id、series_order、cover_media_id、excerpt；Page 无作者、分类、标签、系列。

- Draft 可以自动保存，公开查询始终排除。
- 保存已发布文章/页面会直接改变线上内容；包括标题、正文、摘要、分类、标签、系列和可见性。
- 已发布内容默认不做服务端自动保存，后台使用明确的“保存并更新线上”按钮。要保留线上旧文同时编辑新稿，需要后续修订功能；当前只能先撤回为 draft。
- Markdown 源文存 content，content_type 首期只有 markdown。HTML 清洗、目录和摘要按内容版本与处理器版本派生，可在有界内存缓存中复用，不新增渲染表；未命中时重新生成。
- 写入使用 expected_version；有持久化变化才递增 version，冲突保留客户端编辑并提示处理，不自动覆盖。
- 内容字段、post_tags 变更、系列位置与相关版本在同一事务完成。后续事件使用聚合提交版本，不以时间戳推断顺序。

| 操作 | 内容与状态 | version | 对公开结果的影响 |
|---|---|---|---|
| 保存草稿 | 更新当前内容 | 有变化 +1 | 不公开 |
| 首次发布 | 校验内容，设 published 和首次 published_at | +1 | public 才公开 |
| 保存已发布内容 | 原位更新当前内容 | 有变化 +1 | 立即更新 |
| 撤回 | published → draft | +1 | 不再公开 |
| 重新发布 | draft → published，保留首次 published_at | +1 | 按当前内容公开 |
| 改为 private | 保留发布状态 | +1 | 退出所有匿名读取 |
| 归档 | published → archived | +1 | 不再公开 |

对已处于目标状态且无字段变化的重复命令按幂等无操作处理。**幂等不等于忽略版本前提**：请求显式携带的 `expected_version` 必须先与当前版本一致，否则一律回 `version_conflict`，即使这次操作本来不会写任何字段。否则调用方会拿到「成功」，以为自己的旧版本已生效，把中间发生的并发修改掩盖到下一次写入才爆发。启用公开缓存后，上表影响公开结果的操作同事务更新缓存版本；当前不启用页面缓存。

公开文章统一满足 `status = 'published' AND visibility = 'public' AND deleted_at IS NULL`；公开页面满足前两项。详情、列表、RSS、sitemap、模板函数、分类/标签/系列目录和未来搜索都复用该条件。private 不等于“有链接就可访问”，必须走有权限的后台或预览入口。

## 2. 归属、状态与删除

作者可直接发布自己的文章。Post.author_id 引用 identity::UserId；后端从可信 Actor 和真实记录核对归属，普通编辑请求不能任意转移作者。公开署名来自 users 的允许字段 DTO，不能暴露邮箱、密码摘要、角色或 OAuth 信息。

Page 没有 author_id，page.* 权限采用站点范围；不会默认继承文章的 own 授权。用户软删除禁用登录和后台操作，文章保留，不级联删除或自动撤回公开内容；匿名化时同步更新公开作者资料。

status 为 draft/published/archived；归档暂按终态处理，不等同于物理删除。visibility 为 public/private，不提前加入审核、定时发布、密码访问或 unlisted。

Post 支持 deleted_at 回收站：移入后不公开，保留 slug、系列位置与标签关系。恢复为 draft，原 archived 仍为 archived；不会自动上线。默认不自动清空回收站，永久删除需 post.purge 权限，删除文章及 post_tags，释放 slug 和系列位置。

Page 按选定 schema 没有 deleted_at，`page.delete` 是物理删除，没有回收站恢复。后台永久删除按钮会二次确认并明确提示不可恢复。请求携带页面 ID 和 `expected_version`；旧版本或同 slug 的新页面不能被旧请求删除。成功后公开详情和 sitemap 下一次请求即消失，slug 释放并可重用。需要 Page 回收站时再增加字段和恢复用例。

本版没有历史修订，因此没有修订固定、100 条/90 天保留、历史关系冻结或历史恢复。备份用于部署恢复，不等于逐篇撤销编辑。

## 3. 分类、系列与标签

Category 表示主题分类，一篇文章至多一个，可为空；parent_id 支持树结构。父分类不自动成为文章的另一条直接分类关系，默认分类列表查询直接归属，包含子树查询需显式参数。

Series 表示有序文章集合，一篇文章至多一个，可为空。series_id 与 series_order 同空或同非空；正整数位置在系列内唯一，允许有间隔。草稿、私有和回收站文章保留其位置，公开目录过滤不可见项后不强制重新编号。

Tag 为多对多，post_tags 的复合主键去重。名称读取当前值，改名会影响全部引用文章；slug 暂按创建后不可修改处理，避免额外历史路径需求。

分类自引用 CHECK 只排除自己作为父节点；创建、移动、删除统一取得分类树事务锁，再校验祖先链，防止并发环。分类/系列/标签有引用时默认拒绝删除；不能通过级联静默改变文章，需显式调整关系。删除文章仅级联其私有 post_tags。

系列重排和跨系列移动在同一事务完成，先按固定 ID 顺序锁系列，再锁涉及文章并校验版本与权限；延后位置唯一约束到提交时检查。管理整个系列顺序需 series.manage，修改他人文章仍需相应 any 权限；Author 不能借系列重排修改他人文章。修改系列成员时递增相关 series.version，防止后台使用过时目录重排。

分类、标签、系列及文章元数据组成一致的公开查询；如需跨多个 SQL 读取一页内容与标签，可使用一致性事务或版本复核，避免输出同一次编辑前后的混合状态。

## 4. URL 与 Slug

当前不建 content_paths，也不增加 published_slug。文章使用 `/posts/{slug}`，Page 按用户方案使用 `/{slug}`，如 /about、/friends、/projects。首页是固定系统入口。

- posts.slug、pages.slug 各自唯一且非空，草稿创建时就占用；无明确 slug 时由应用生成临时唯一值。
- 发布前可以修改 slug；首次 published_at 产生后锁定，即使撤回也不允许改名。
- 软删除/撤回/归档保留记录，因此继续占用 slug；永久删除释放，不保留地址墓碑。
- 不提供旧链接跳转、别名或任意嵌套路径。以后若允许改名并保留旧链接，另行加入路径/重定向存储。
- Slug 只允许单个合法路径片段，固定 Unicode、大小写及编码规范，禁止分隔符、点路径和二次解码绕过。
- Page 禁止占用系统路由及前缀，例如 admin、api、auth、posts、categories、tags、series、assets、media、RSS、sitemap、robots 和图标路由。注册表以实际路由为准，在创建、修改和发布时复核；固定路由优先，Page 最后匹配。
- 机器可读入口已交付：`/feed.xml`（RSS 2.0，最新 20 篇公开文章）、`/sitemap.xml`（首页 + 公开文章 + 公开 Page；标签/分类/系列页只在至少有一篇公开文章时收录，分页变体不单独收录）、`/robots.txt`（声明 sitemap）。三者都复用上面的公开谓词，且不经过主题模板，因此撤回后下一次请求即不再出现（另见 [主题与渲染 §2.1](themes-and-rendering.md)）。
- 分类、标签、系列使用 /categories/{slug}、/tags/{slug}、/series/{slug}；不同类型可使用相同 slug。

数据库只保护各表唯一，系统路由保留由应用校验。无需为当前固定前缀和根页面再建立全站路径表。

## 5. 媒体库（第一版：Post/Page 正文图片；第二段：Post/Series 封面；第三段：用户头像与站点 logo）

**第二段已交付：Post 与 Series 封面接入媒体库。** 封面不再是 `posts.cover` / `series.cover` 文本 URL，而是 `cover_media_id` 外键（引用 `media_assets`）；它与正文图片共用同一张 `content_media_refs`——保存内容时把「正文渲染出的图片 ∪ 封面」写进引用表，因此「仍被引用的图片不能删除」与「匿名访问跟随内容公开状态」对封面逐字生效，不新增第二套判据。

**第三段已交付：用户头像与站点 logo 接入媒体库。** 头像用 `users.avatar_media_id` 真外键，公开来源 = 账号未软删除（软删除后匿名读取立即失效，但引用仍占用，恢复账号后语义不变），本人可在后台自助设置/清除且**不递增 `users.version`**（那是会话绑定版本，换头像不该把人踢下线）。站点 logo 的 id 按用户确认的取舍存在 `settings.site` 的 JSONB 值里，但引用关系仍写入同一张 `content_media_refs`（与配置行同事务），删除保护与公开来源继续以引用表为唯一判据；JSON 里的 id 没有 FK 兜底，属于本条明确的取舍。本段仍不提供私有附件（非图片）、视频与任意上传。

### 5.1 数据模型

两张表（`migrations/postgres/0004_media.sql`）：

| 表 | 作用 |
|---|---|
| media_assets | 资产元数据：上传者、随机存储路径、展示文件名、MIME、字节数、宽高、SHA-256、状态、version |
| content_media_refs | 内容 → 媒体的真实引用关系（`media_id` + `content_type` + `content_id`） |

`content_type` 取 `post` / `page` / `series` / `user` / `site`：Post/Page 的正文图片、Post/Series 的封面、用户头像与站点 logo 都写进这张表。

文件用随机标识存储（存储根目录下 `objects/<uuid>.<ext>`，飞行中的上传在 `staging/`），数据库行是唯一权威。`content_id` 是指向 posts/pages/series/users 的**多态引用**（无法建外键），因此内容物理删除（`post.purge`、`page.delete`、系列删除）必须在同一事务清理对应引用行；站点是单例、`settings` 行没有 uuid，`content_type='site'` 用固定的 nil UUID 占位（`SITE_MEDIA_CONTENT_ID`），由「站点保存整体替换这组引用」保证单例语义。

**封面与头像都是独立列，不是正文文本。** `posts.cover_media_id` / `series.cover_media_id` / `users.avatar_media_id` 是 `media_assets(id)` 的真外键；保存时把该字段与正文图片 id 求并集（同一张图只计一次、按 id 排序）后整体替换引用行。这样它们不依赖任何字符串扫描，数据库也拒绝悬空引用。仅封面/头像变化同样递增所属内容的 version（头像是自助字段，没有版本前提，后写覆盖）。站点 logo 是唯一例外：id 在 settings JSONB 值里，写入时由引用表的 `ready` 校验把关。

**「是否仍被使用」以引用表为唯一判据，不以搜索 Markdown 文本为依据。** 正文只在保存时被解析一次（`infrastructure::media_refs`），解析出的集合同事务整体替换进引用表；删除流程不回头搜索正文。

提取与渲染必须给出**同一个集合**，偏哪边都是线上故障：漏掉引用 → 图片仍在展示却能被删除（破图）；多出引用 → 图片永远删不掉（幽灵占用）。

因此提取**不是**独立解析，而是直接走主题渲染管线（`infrastructure::media_refs`：Markdown → 清洗 HTML），再用 HTML5 分词器（`html5ever`，与 ammonia 同一个解析器）读出 `<img src>`。渲染成 `<img>` 的才算引用，被清洗掉的自然不算——两者在结构上就是同一件事，不需要靠测试去追两条路径。

| 正文形式 | 渲染结果 | 是否建立引用 |
|---|---|---|
| `![alt](/media/{id})` | `<img src="/media/{id}">` | 是 |
| 原始 HTML `<img src="/media/{id}">` | 同上 | 是 |
| `<img alt=">" src="/media/{id}">`（属性值含 `>`） | 标签不被截断，照常渲染 | 是 |
| `<image src="/media/{id}">`（过时标签） | HTML5 按 `<img>` 处理，ammonia 输出即 `<img>` | 是 |
| 普通链接 `/media/{id}` | `<a href>` | 否（不渲染成图片） |
| 代码块 / 注释里的 `<img src="/media/{id}">` | 转义文本 / 被清洗掉 | 否（不会渲染成图片） |
| `<script>` 内或 `javascript:` 的 src | 被清洗策略丢弃 | 否 |

**不要改回手写字符串扫描。** 注释、引号属性值里的 `>`、CDATA、实体转义与 `script`/`style` 原始文本都不是字符串查找能正确处理的东西，而这里判断错的后果分别是「图片永远删不掉」与「图片仍被展示却可被删除」。注释里的 `<img>` 与 `<img alt=">" …>` 两个形状已作为回归用例固定（`comment_wrapped_image_is_not_a_reference`、`attribute_value_containing_gt_does_not_truncate_the_tag`，以及真实库上的 `reference_extraction_matches_what_actually_renders`）。

引用集合在**开启数据库事务之前**算好：提取要完整渲染并清洗正文，是纯 CPU 工作，不该占着事务不放；集合仍只由即将写入的正文推导，因此不存在正文与引用关系漂移的写入路径。

### 5.2 状态机与可重试回收

```text
staged ──promote──▶ ready ──删除请求──▶ pending_deletion ──文件删除成功──▶ deleted
   │                                          ▲
   └──reclaim 放弃（超宽限期，CAS 认领）──────┘
```

- 上传是「落暂存 → 登记 `staged` → 原子移入正式位置 → 标记 `ready`」：`ready` 只在文件确实就位后写入，不存在「记录可被引用但文件没写完」的窗口。
- 删除是「引用保护检查 → `pending_deletion` → 删除文件 → 确认 `deleted`」。文件系统与数据库不假设能原子提交，因此中间状态必须可重放。
- 文件删除失败时**保留 `pending_deletion` 并如实报错**：资产已从媒体库消失、也不再接受新引用，但绝不会被标记为已删除。`blog media reclaim` 幂等地重试；可反复执行，逐项输出资产 id、存储路径与失败原因。
- 「放弃未完成上传」也走 `pending_deletion`（而不是直接 `deleted`）：这样「已决定回收、文件还没删掉」与「文件已删」保持可区分，回收能重试到文件确实消失为止。
- 已删除的行保留（`deleted`）：重放、审计与「同一 id 永不复用」都依赖它。

**回收与上传必须互斥**（否则回收会删掉刚就绪资产的文件，留下指向缺失文件的可用记录）。两道防线：

1. **宽限期**：只有创建时间早于 `now - 1 小时`（`STAGED_GRACE_SECS`）的 `staged` 资产才会被认领。更新的资产可能正被上传推进（含「文件已写入、行尚未插入」的窗口），一律不动。
2. **条件更新互斥**：认领是一条 `UPDATE … WHERE status = 'staged'` 的单语句条件更新，`mark_ready` 也是。两者只有一个能命中——拿到 `staged` 的一方负责文件，另一方绝不触碰。认领在同一条语句里返回被认领的行，不依赖「先查后改」。

**没有数据库记录的暂存残留**（写入文件后插入失败、或进程在两步之间退出）按数据库状态扫描的回收永远找不到，因此：

- 上传在 `insert_staged` 失败时尽力删除刚写入的暂存文件；
- `reclaim` 另做一次**文件系统侧清扫**：删除暂存区里修改时间早于宽限期的文件（含写入中断的 `.part`）。宽限期是安全边界——更新的暂存文件可能属于正在进行的上传。

### 5.3 公开访问边界

新上传的图片默认不公开。匿名请求 `GET /media/{id}` 只在**存在公开来源引用**时返回文件；公开来源的判据与内容可见性逐字一致：

```text
content_type = 'post' AND posts.status='published' AND posts.visibility='public' AND posts.deleted_at IS NULL
OR content_type = 'page' AND pages.status='published' AND pages.visibility='public'
OR content_type = 'series' AND EXISTS (series 行)
OR content_type = 'user' AND EXISTS (users 行且 users.deleted_at IS NULL)
OR content_type = 'site'
```

系列目录页对**任何已存在系列**公开可达（不存在即 404，见 §4），因此系列封面在有系列行时即构成公开来源；删除系列会在同一事务清掉它的引用行。站点配置本身公开，因此站点 logo 只要有引用即公开来源；头像的公开来源是「账号未软删除」，账号软删除后匿名读取立即停止（引用仍占用，恢复账号后语义不变）。因此文章撤回、改为 private、移入回收站，或页面撤回/改为 private，或账号软删除后，只要没有其他公开引用，图片**下一次请求即停止匿名读取**。后台预览需要登录且持有 `media.read`；未获授权的请求返回 404 而不是 403——「不存在」与「不公开」不可区分，避免泄漏资产存在性。

缓存：公开引用响应用 `no-cache` + ETag（必须重校验，撤回后立即失效），后台预览用 `no-store`。媒体**绝不**使用长 max-age，否则撤回后图片会继续从缓存流出。

**使用位置的展示必须按内容权限过滤。** `media.read` 只授予「浏览媒体库」，不能顺带给出他人草稿或私密内容的标题与 slug；媒体库是共享资源，任何人上传的图片都可能被别人的未公开内容引用。因此：

- **引用计数全局**：`reference_count` / `public_reference_count` 不过滤——它们决定「能否删除」与「匿名能否读取」，与调用者能否看见引用无关；
- **使用位置过滤**：Post 按 own/any（`post.read` / `post.read_any`），Page 按站点级 `page.read`，Series 按 `series.manage`，软删除账号的头像按 `user.manage`，站点 logo 按 `settings.manage`；**公开可读的内容直接可见**——它的标题与 slug 本来就能匿名访问、正文里也带着同一个图片地址，过滤它保护不到任何东西，反而会把「公开引用」误报成「无权查看的引用」。可见性规则因此是「调用者有权读该内容 **或** 该内容公开可读」；
- **差额如实返回**：详情返回 `hidden_references`（引用总数 − 可见数），否则界面会显示「被 3 处引用」却只列出 1 处。

### 5.4 上传边界

- 只接受常见**位图**：PNG、JPEG、GIF、WebP。**不开放 SVG**（矢量、可内嵌脚本与外部引用，清洗与访问边界完全不同）、视频与任意附件。
- 格式由**文件内容**判定（文件头嗅探，`domain::media`）：扩展名与请求 `Content-Type` 都不参与判定。大小 ≤10 MiB、单边 ≤12000 px、总像素 ≤6000 万。
- 展示文件名只用于展示（去掉目录成分与控制字符），从不参与路径拼接；存储路径由随机 id 决定。
- 上传是裸字节体（`POST /api/admin/v1/media?filename=…`），因此不需要 multipart 解析依赖，也从根本上避免「按声明类型放行」。

### 5.5 引用保护与并发

删除前的引用检查**不过滤可见性**：草稿、私密与回收站引用同样占用。放行删除会让撤回中的文章一重新发布就出现破图，因此这些引用必须先显式移除（HTTP 409 `media_in_use`，并给出使用位置）。

并发协议避免「引用写入」与「文件回收」交错：

- **内容保存**：同一事务内先按 id 序对涉及媒体行（正文图片、封面、头像、站点 logo 的并集）取 `FOR SHARE` 并确认全部 `ready`，再整体替换引用行；任一媒体不可用则整次保存回滚（正文、引用字段与引用行都不落库）。系列设置封面、本人设置头像、站点保存 logo 走同一条路径（站点 logo 与 settings 行的 CAS 同一事务）。
- **媒体删除**：对同一媒体行取 `FOR UPDATE`，在锁内读状态、校验引用（有引用即拒绝）、迁移到 `pending_deletion`。
- **回收放弃未完成上传**：`staged → pending_deletion` 的单语句条件更新（带宽限期），与 `staged → ready` 只有一个能成功。

两者锁序一致（先媒体行、后引用行），因此不会死锁。结果只有两种：要么引用先落地（删除随后被引用保护拒绝），要么删除先落地（内容保存因媒体不再 `ready` 而失败）。回收侧同理：要么上传先就绪（回收不认领、不碰文件），要么回收先认领（上传的 `mark_ready` 失败并如实报错）。「替换封面」与「删除候选图片」并发时正是这两条路径：封面写入先落地则图片被引用、删除被拒；删除先落地则整次封面保存回滚、封面保持旧图，不会出现「引用已写入、文件已回收」的破图中间态。

### 5.6 权限

| 权限 | 范围 |
|---|---|
| media.read | 浏览媒体库与后台预览（公开引用的图片匿名即可读，不需要该权限） |
| media.upload | 上传位图 |
| media.delete | 删除**本人上传**且已无引用的图片 |
| media.delete_any | 删除任意上传者且已无引用的图片 |

内置角色：Owner 全部；Editor 持 read/upload/delete_any；Author 持 read/upload/delete。媒体库是共享资源——作者可以引用他人上传的图片，但删除他人上传需要显式 any 权限。

**头像与站点 logo 的写入权限**：头像由**本人自助**设置/清除（`PUT /api/admin/v1/me/avatar`，任何有效会话即可，不要求媒体库删除权限，也**不递增 `users.version`**，因此不会让本人的会话失效）；站点 logo 随 `settings.site` 整组保存，需要 `settings.manage` 加 `media.upload`（选择已有图片需要 `media.read`）。

### 5.7 运维与备份

媒体文件根目录由 `BLOG_MEDIA_DIR` 指定（默认 `data/media`），与数据库一起构成恢复单元，必须纳入同一份备份清单（见 [备份与恢复](operations-and-recovery.md)）。回收入口是受控 CLI：`blog media reclaim`，输出四类结果——认领并放弃的超期上传、完成的文件删除、清理掉的暂存孤儿文件、逐项失败原因（含资产 id 与存储路径）。

## 6. 验证边界

当前验证：版本冲突、保存已发布内容立即生效、草稿/private 过滤、作者 own/any、Page 站点权限、分类环、系列序号和重排、标签去重、引用删除保护、回收站恢复不发布、草稿 slug 占用、首次发布后锁定路径、Page 系统路由冲突；媒体库的图片内容校验（格式/尺寸/大小）、正文引用同事务写入与替换、**提取与渲染的图片形式等价**（Markdown 与原始 HTML，代码块排除）、匿名可见性随内容状态变化、引用保护删除、**回收与上传就绪的真并发互斥**、宽限期、暂存孤儿清扫、**使用位置按内容权限过滤**、上传/浏览/删除权限边界与公开文件缓存头。封面：封面引用随内容保存同事务提交、替换/移除释放旧引用、仍被引用时删除受保护、匿名可见性随文章撤回/private/回收站变化、系列封面公开来源与删除系列清理引用、公开详情/系列页渲染封面、**封面写入的版本冲突**与**「替换封面 vs 删除候选图片」的真并发串行化**。头像与站点 logo：本人自助设置/清除且会话不被清理、账号未软删除时头像匿名可读、软删除后立即失效而引用仍占用、站点 logo 随 settings 整组保存并受 `ready` 校验与版本 CAS、移除后引用释放、公开页渲染 logo 与作者头像。

后续修订和外部任务交付时增加对应历史隔离、回收与可靠事件验收；当前 16 表不能被标记为已经覆盖这些能力。

