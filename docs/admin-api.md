# 管理 API 参考

本文描述当前 HTTP 路由与通用请求约定。业务状态和可见性见[内容生命周期](content-lifecycle.md)，权限和会话见[身份、权限与后台](identity-and-admin.md)。项目仍在开发阶段，客户端应随接口变更同步更新。后台请求类型与响应校验流程见[HTTP 契约与客户端](admin-development.md#http-契约与客户端)。

[新建库基线](database-current.md)已接入，身份、sessions、媒体、内容、目录、评论及保留期设置已适配。

路由与传输 DTO 的实现入口：[内容和设置](../crates/interfaces/src/http_admin/mod.rs)、[身份](../crates/interfaces/src/http_identity.rs)、[媒体](../crates/interfaces/src/http_media.rs)、[认证](../crates/interfaces/src/http_auth.rs)。

## 通用约定

- 管理 API 前缀为 `/api/admin/v1`，使用会话 cookie 认证，响应设置 `Cache-Control: no-store`。
- 先调用 `GET /api/admin/v1/me` 获取当前身份、有效权限和 `csrf_token`；已认证写请求带 `X-CSRF-Token`。自助改密成功后使用新会话与新 token。
- 请求带 Origin 时校验其与 Host 的关系；当前接受 `http://{Host}` 或 `https://{Host}`，缺失 Origin 不拒绝。跨源拒绝为 403；普通管理写入的 CSRF 失败为 400，退出和自助改密的 CSRF 失败为 403。
- JSON 请求使用 `Content-Type: application/json`，媒体上传例外，直接发送图片字节。
- Post、Page 和 Media 的路径标识是 UUID。Tag、Category、Series 使用 slug；角色分配路径使用 username 和 role key。管理身份不应统一推断为 slug 或 UUID。
- 每个响应有 `x-request-id`，应用错误体也有 `request_id`。报障时保留该编号。

## 评论

评论管理位于 `/admin/comments`。评论接口、开关和版本规则见[评论 API](comments.md#接口)。

## 后台任务

后台 `/admin/tasks` 仅管理 `html_rebuild`、`retention`、`publish_due`，以下路由均复用 `settings.manage` 和通用会话/CSRF/Origin/no-store 契约。未登录返回 401，无权限返回 403；恢复隔离只允许读取。

| 方法与路径（省略 `/api/admin/v1`） | 载荷与结果 |
|---|---|
| `GET /tasks` | 可选 `kind`、`cursor`、`limit`；limit 默认 20，范围 1–100。返回 `{available,retention_available,pending_html,latest,schedules,runs:{items,next_cursor}}`；`latest` 分别保留每类最新任务，不受本页历史挤占 |
| `POST /tasks` | `{kind,run_at:null}` 立即执行；仅 HTML 可传未来 365 天内、含时区 RFC3339 的 `run_at`。202 返回任务；已有同类活动请求时返回原记录 |
| `POST /tasks/{id}/retry` | 明确重试可重试记录，202 返回新 ID，并用 `retry_of` 指向旧记录；以响应 `can_retry` 为准 |
| `POST /tasks/{id}/cancel` | 只取消 queued 的手动、一次性或重试请求，200 返回 cancelled 记录；running 或不可取消记录返回 409，以 `can_cancel` 为准 |
| `PUT /tasks/retention-schedule` | `{enabled,interval_seconds,next_run_at,version}`；间隔 3,600–2,592,000 秒，版本用于 CAS。启用时可给含时区的未来时间，null 表示按当前时间加间隔；停用时清空下次时间 |

任务状态为 `queued/running/completed/failed/interrupted/cancelled`，触发来源为 `manual/once/periodic/retry`。单条含 ID、类型、状态、执行/创建/开始/完成时间、`retry_of`、报告和两个可操作标志。报告分别提供 `html`、`retention`、`publication` 或 null，以及经过安全处理的 `error`；运行中 HTML 剩余量未知时为 null。每类终态历史最多 500 条，仍需按游标读取，不能用本页条数推算全部历史。

retention 默认间隔 86,400 秒且禁用，publish_due 固定 30 秒启用、没有可编辑计划入口。保留期不可用不阻止其他任务或网站登录。旧 `GET/POST /maintenance/html-rebuild` 保留为同一持久任务的兼容投影，queued 映射为 running、cancelled 映射为 interrupted；完整计划和历史使用新路由。生命周期、专用维护连接及 CLI 边界见[任务操作](operations-and-recovery.md#后台任务管理)。

## 备份与应急恢复

统一镜像默认启用独立入口 `/recovery`，后台“系统 → 备份与恢复”链接到该页面。以下路径不使用管理 API 前缀或业务会话；即使业务数据库离线，恢复控制器仍可响应。操作说明见[后台备份与恢复](browser-backup.md)，实现入口为 [http_backup.rs](../crates/interfaces/src/http_backup.rs)。

| 方法与路径 | 载荷与结果 |
|---|---|
| `POST /api/recovery/session` | 使用 `{username,password}` 验证受保护的 `admin` 角色账号，或 `{key}` 验证保存的恢复密钥；安装期间也可使用 `{installation_token}`。返回 `{csrf}` 并设置独立 HttpOnly、SameSite=Strict cookie |
| `GET /api/recovery/session` | 返回 `csrf`、备份/最近 50 项任务、脱敏配置、`initialized/maintenance/installing/busy/recovery_required/database_configured` 和上传上限；不返回存储凭据或私钥 |
| `DELETE /api/recovery/session` | 撤销当前恢复会话，成功返回 204 |
| `POST /api/recovery/action` | `{action,input:{…}}`；短操作直接返回结果，长任务返回 `{job_id}`，通过 session 状态轮询结果 |
| `POST /api/recovery/upload?offset=0&complete=false` | 直接发送加密字节，每块最多 4 MiB；响应含 `id/name/offset/complete`。后续块携带返回的 `id` 和精确 `offset`，末块设 `complete=true`；单文件最多 2 GiB |
| `GET /api/recovery/download/{name}` | 下载已完成的本地加密备份，不允许任意路径；流式返回 attachment |

写请求需同源校验；已认证写入另带 `X-CSRF-Token`。普通 JSON 上限 64 KiB；上传在缓冲请求体之前校验会话。敏感操作授权 15 分钟，读授权最长 1 小时，重启即失效。数据库在线时每次操作重新核验账号认证版本和 Admin 角色；改密、禁用或撤权后不能继续使用旧恢复会话。维护期间使用独立短期授权，不依赖正在恢复的数据库。

`action` 包括 `keygen/key-confirm/backup/inspect/restore/resume/delete/discard-upload/schedule/remote-save/remote-list/remote-upload/remote-download`。备份、校验、恢复及远程存储操作均为后台任务；一次只允许一个任务。任务记录保存在配置卷，状态为 `running/succeeded/failed/interrupted`，包含阶段、结果、请求者及脱敏错误；浏览器关闭不取消任务。

`inspect/restore` 的 input 使用 `{name,imported,key}`：`name` 必须来自本地列表或上传结果；上传及远程取回的文件设 `imported:true`。恢复另外要求 `confirm:"恢复此站点"`，仅在明确接受缺少恢复前副本时设置 `allow_without_snapshot:true`。恢复过程再次完整验证文件，成功后撤销旧业务会话与邮件账号链接。`resume` 只会重新加载可用站点，不能绕过未完成的恢复。完整备份格式和维护边界见 [ADR-0021](adr/0021-browser-backup-and-in-place-recovery.md)。

## 首次安装

仅安装模式提供以下入口，不使用管理会话。正常站点 `/api/install` 返回 404，`/install` 跳转 `/admin/`；完整启动条件见[首次安装](installation.md)。

| 方法与路径 | 用途 / 载荷 |
|---|---|
| `GET /api/install` | 需 `X-Install-Token`；`{ "database_configured": false, "public_base_url": null }`；续装时仅说明配置已保存，不返回数据库地址、账号密码或安装码 |
| `POST /api/install/check` | `{ "database_url": "postgres://…" }`；验证连接、空库及建表/扩展权限，通过返回 `{ "ready": true }`，不写配置或创建表；预配置或续装时使用已保存连接 |
| `POST /api/install` | `{ "database_url": "postgres://…", "public_base_url": "https://blog.example.com", "username": "sun", "password": "…" }`；成功返回 `{ "redirect": "/admin/" }` |

GET 和 POST 都必须带 `X-Install-Token`：使用部署设置的 `BLOG_INSTALL_TOKEN`，或未设置时启动终端显示的随机码，并执行 Origin 检查（请求带 Origin 时必须同源）。未知字段、无效 JSON、超过 16 KiB、弱密码、非空库等返回 400 `invalid_request`；错误安装码/跨源返回 403；同时正在处理安装时返回 429 `rate_limited`。响应均 no-store，包含请求编号。预配置数据库或续装时沿用已保存数据库和站点地址，输入不能覆盖；部署设置的 `BLOG_PUBLIC_BASE_URL` 优先。配置文件不保存账号明文密码或安装码，Admin 与安装完成审计同事务提交。

连接检查使用同一安装码、来源校验、请求体限制与安装互斥门闩。页面仅在检查通过后展开管理员表单，编辑数据库地址使检查结果失效。正式安装重新检查目标和权限，不能借用此前的成功结果绕过空库或权限要求。

## 审计日志

后台入口 `/admin/audit-logs`，接口 `GET /api/admin/v1/audit-logs`，独立要求 `audit.read`；默认仅 Admin 持有。未登录返回 401，无权限返回 403。不提供写入、编辑或清除接口。

| 查询参数 | 规则 |
|---|---|
| `action` | 动作精确匹配，如 `post.update`；最长 128 字符 |
| `actor_id` | 账号 UUID；账号已被物理删除也可查询历史 |
| `without_actor` | `true` 只取空 actor，不能同时指定 actor_id；包含访客、系统或 CLI |
| `target_type` / `target_id` | 目标类型 / ID 精确匹配，最长分别 64 / 256 字符，可组合 |
| `from` / `until` | 含时区 RFC3339，下限包含、上限不包含；同时提供时 from 必须早于 until |
| `limit` | 默认 50，整数 1–100 |
| `cursor` | 使用上一次返回的 next_cursor 原值，经 URL 编码传入 |

响应为 `{ "items": [...], "next_cursor": "…" }`，末页 next_cursor 为 null。单条含 `id`、`created_at`、`actor_id`、`actor_display`、`ip_address`、`action`、`target_type`、`target_id` 和 `summary: [{ "key": "version", "value": "2" }]`。actor_display 是当前展示名（无展示名时为用户名），不是历史名字快照；历史 actor_id 保留，账号不存在时 display 为 null。IP 未知时为 null。摘要只提供存储时允许的变更信息，全部按文本展示。

按 `(created_at DESC, id DESC)` 连续读取，不返回总数；新增日志及旧边界记录清理不移动已取得的分页边界。它不是冻结快照，已经超过保留期的记录不会继续显示。更改筛选后丢弃旧 cursor，刷新从首页开始。未知查询字段、非法 UUID、游标、范围或日期返回 400 `invalid_request`。响应均使用 `Cache-Control: no-store`，不写入浏览器持久缓存。

## 认证与本人资料

此表列出完整路径，不应用管理前缀。

| 方法与路径 | 用途 / 载荷 |
|---|---|
| `GET /auth/providers` | 公开提供商列表，仅含 `id`、`name`、`kind` |
| `GET /auth/login?provider=…&next=…` | 发起 OAuth 登录，建立浏览器绑定状态 |
| `GET /auth/callback/{provider}` | OAuth 回调 |
| `POST /auth/login/password` | `{ "username": "sun", "password": "…" }`，成功设置会话 cookie |
| `POST /auth/logout` | 会话、CSRF 与来源校验通过后退出 |
| `GET /api/admin/v1/me` | 当前用户、有效权限、CSRF token、`bio`、资料编辑 `version` 和站点 `time_zone` |
| `PUT /api/admin/v1/me/profile` | 本人 `display_name`、`bio` 及必填 `expected_version`；返回资料与新版本，保持登录 |
| `POST /api/admin/v1/me/password` | `new_password`；已启用密码时还需 `current_password` |
| `PUT /api/admin/v1/me/avatar` | `{ "avatar_media_id": "UUID", "expected_version": 1 }`；`null` 清除；版本冲突返回 409，需重新读取资料后确认 |

密码登录是匿名写入口，没有可用的会话 CSRF token，执行来源检查。密码登录和本人资料请求体上限为 4 KiB；密码失败与限流行为见下方错误表。

资料 PUT 为整值替换：`display_name`、`bio` 省略或 `null` 表示清空，简介按纯文本存储。版本过期返回 409；未知字段拒绝。只增 `users.version`，不修改 `auth_version`，并同事务追加脱敏审计。改密递增认证版本并轮换会话；角色变更保持 Cookie 有效，权限在下一次请求生效。

后台 `/admin/profile` 提供本人展示名和简介表单。冲突时保留本地输入并暂停保存，用户明确重新加载后才采用新版本，不自动覆盖其他位置的修改。

## 文章与回收站

以下所有路径均相对于 `/api/admin/v1`。读写权限按文章作者区分 own / any；创建文章的作者取当前会话用户。

正文预览使用 `POST /content-preview`，请求 `{ "content": "Markdown" }`，返回 `{ "content_html": "清洗后的 HTML", "head_html": "宿主生成的插件资源标签" }`。需要会话、CSRF，以及 `post.create/post.update/post.update_any/page.create/page.update` 中任一权限。它复用保存时的正文渲染器、源文与 HTML 预算；正文和头部资源使用同一插件快照。它不读取已有内容，不更新正文、引用或审计，响应 `Cache-Control: no-store`。后台用沙箱 iframe 展示正文并加载已启用的公式/图表资源，不代表完整主题、发布校验或媒体引用提交已经成功。评论预览的返回结构仍仅含 `content_html`。

| 方法与路径 | 行为 |
|---|---|
| `GET /posts` | 默认本人文章摘要分页；`scope=all` 或显式 `author=用户名` 需 `post.read_any` |
| `POST /posts` | 创建草稿，返回 201 和详情 |
| `GET /posts/{id}` | 详情，包含 Markdown `content` 和 `excerpt` |
| `PATCH /posts/{id}` | 编辑并返回详情；已发布文章保存到服务端编辑稿，需显式发布更新 |
| `POST /posts/{id}/publish` | 立即发布或应用修改草稿，返回详情 |
| `POST /posts/{id}/schedule` | 预约或更新预约时间，返回详情；需发布权限 |
| `POST /posts/{id}/unpublish` | 撤回、取消预约或解除归档，回到草稿；需撤回权限 |
| `POST /posts/{id}/archive` | 归档，返回详情；沿用 `post.unpublish` / `post.unpublish_any` |
| `POST /posts/{id}/trash` | 移入回收站，返回详情 |
| `GET /post-trash?page=1` | 回收站分页，支持与文章列表相同的筛选；返回 `items/total/page/per_page` |
| `POST /posts/{id}/restore` | 一律恢复为草稿，返回详情 |
| `POST /posts/{id}/purge` | 永久删除回收站文章，返回 204 |
| `POST /posts/batch` | 原子批量操作，见下方契约；返回 200 和逐项提交结果 |

普通文章、页面及两类回收站列表统一返回 `{ "items": [...], "total": 23, "page": 1, "per_page": 20 }`。`page` 默认 1，每页固定 20 条，超出末页返回空 `items` 与实际总数；零、负数及会导致偏移溢出的页码返回 400。可选筛选 `status=draft|scheduled|published|archived`、`visibility=public|private`，非法值返回 400。普通列表按 `updated_at DESC, id DESC`，回收站按 `deleted_at DESC, id DESC`；总数与当前页属于同一数据库快照，跨次翻页不冻结内容集合。

服务端搜索 `q` 在分页前筛选全部授权记录：对标题、slug、Markdown 正文进行不区分大小写的字面子串匹配，文章另含摘要；两端空白移除，最多 200 个 Unicode 字符，空值不筛选。`%`、`_` 不作通配符。文章及文章回收站另支持 `category_id=UUID`（直接归属分类）与 `scope=mine|all`（默认 mine）；`all` 需 `post.read_any`，可再组合 `author=用户名`。显式作者参数兼容原接口，同样要求 `post.read_any`，可查询停用或软删除账号留下的文章；不会因此恢复其登录能力。

列表条目只含 `id/slug/title/status/visibility/version/published_at/updated_at`，文章另含 `author_id/author_username`。正文、摘要、标签、分类、系列与封面元数据只由详情端点返回。后台筛选变化回到首页，搜索、作者、分类、状态和页码写入 URL；刷新及从编辑器返回列表保留条件，同一登录会话中的菜单返回也恢复最近列表。内容写入使全部分页缓存失效，当前页删空时回到有效页。Post CLI 的 `post list` 同样支持 `--page`、`--status`、`--visibility`，每次输出一页及总数。

创建字段为 `slug`、`title`、`excerpt`、`content`、`visibility`、`tag_ids`、`category_id`、`series`、`cover_media_id`。`slug` 可省略生成临时值，草稿允许未完成的标题与正文；发布要求见[内容生命周期](content-lifecycle.md)。`series` 为数组，例如 `[{ "series_id": "UUID", "position": 0 }]`，省略时为空数组；创建或替换时最多 100 项。position 省略时为 0，范围为 0–2147483647，同一系列内可重复；数组内不能重复指定同一系列 ID。文章详情使用相同数组格式。

编辑支持 `new_slug`、`title`、`excerpt`、`content`、`visibility`、`tag_ids`、`category_id`、`series`、`cover_media_id` 和 `expected_version`。注意不同字段的更新语义：

| 字段 | 省略 | 清除 | 设置 |
|---|---|---|---|
| `tag_ids` | 保持 | `[]` | UUID 数组整体替换 |
| `category_id` / `cover_media_id` | 保持 | `null` | UUID |
| `series` | 保持（`null` 同义） | `[]` | `[{ "series_id": "UUID", "position": 0 }]` 整体替换 |
| `excerpt` | 保持 | `""` | 字符串；`null` 与省略同义 |

`visibility` 为 `public` 或 `private`。状态由动作端点变更，不通过编辑请求直接赋值。`content` 始终是 Markdown；`content_html` 由服务端派生并持久化，不是客户端可设置的字段，也不是管理详情的正文格式。

Post/Page 的 schedule 请求为 `{ "published_at": "2026-10-01T10:00:00+08:00", "expected_version": 3 }`。时间必须为带时区的 RFC 3339 且晚于当前时间，只接受草稿或已预约状态；已发布或已归档内容须先退回草稿。内容 DTO 的 published_at 和 updated_at 同样使用 RFC 3339，后台输入按 `/me.time_zone` 指定的站点时区转换后提交。预约即锁定 slug，取消预约不解锁。

## 服务端编辑稿与历史版本

Post/Page 的 GET 与编辑详情包含 `has_pending_changes`，为 true 时源文和元数据来自服务端编辑稿，状态、作者和发布时间保持主记录。`PATCH` 已发布内容不会更新公开页面；发布接口在版本校验后应用编辑稿并清除标志。预约、撤回和回收站规则见[内容生命周期](content-lifecycle.md)。

以下路径中的 `{kind}` 为 `posts` 或 `pages`；所有接口复用管理会话、no-store，写入另校验 CSRF 和 Origin。

| 方法与路径 | 行为 |
|---|---|
| `GET /{kind}/{id}/revisions` | 原内容阅读权限；最多 50 条、版本倒序，返回 `id/version/title/created_at/actor_id` 数组 |
| `GET /{kind}/{id}/revisions/{revision}` | 同上；只读取属于指定内容的版本，返回 `slug/title/content/visibility/excerpt/tag_ids/category_id/series/cover_media_id`；Page 的文章专属字段为空 |
| `POST /{kind}/{id}/revisions/{revision}/restore` | 原内容读写权限；必填正整数 `expected_version`，返回编辑详情；已发布内容恢复为修改草稿 |

历史不可改写，恢复是新的编辑提交，不恢复作者、状态、发布时间或已锁定的 slug。归档不可编辑，已在回收站的内容返回 404；不存在或属于其他内容的版本返回 404，版本过期返回 409。历史中的目录、媒体按当前规则校验，不复活已删除关联。仅有编辑权限的账号不能借恢复操作发布。迁移前记录在首次修改时补入，历史最多保留 50 份并保护活动编辑稿。

## 文章与评论批量操作

`POST /api/admin/v1/posts/batch` 和 `POST /api/admin/v1/comments/batch` 沿用管理会话、CSRF、同源 Origin 和 no-store 契约。请求体最多 16 KiB，`items` 必须包含 1–100 项；每项必填 UUID `id` 和正整数 `expected_version`，不能重复 ID。直接使用列表条目的 `version` 作为该项的前提版本。未知动作、未知字段、缺少字段、错误类型或超预算均返回 400 `invalid_request`。

```json
{
  "action": "trash",
  "items": [
    { "id": "018f3000-0000-7000-8000-000000000001", "expected_version": 3 },
    { "id": "018f3000-0000-7000-8000-000000000002", "expected_version": 7 }
  ]
}
```

文章动作及参数如下；`trash/restore/purge` 不接受 `params`。包含服务端修改草稿时，批量发布、预约或修改分类返回 400，需先在对应编辑页处理；撤回、归档、回收站动作保留编辑稿。没有修改草稿的批量分类调整仍立即应用并写入历史。

| action | params | 权限与行为 |
|---|---|---|
| `trash` | 无 | `post.delete` / `post.delete_any`；移入回收站，保留发布状态和媒体引用 |
| `restore` | 无 | 同上；只能恢复回收站文章，一律回到 draft |
| `purge` | 无 | `post.purge`；只能永久删除回收站文章；同事务删除其评论树、标签/系列关系和媒体引用；媒体对象仍保留，受影响系列各递增一次版本 |
| `change_status` | `{"status":"published"}` | `post.publish` / `post.publish_any`；沿用立即发布规则，空标题/正文及直接发布归档文章拒绝 |
| `change_status` | `{"status":"scheduled","published_at":"2027-01-01T10:00:00+08:00"}` | 同上；必须是含时区的未来时间，只接受 draft/scheduled |
| `change_status` | `{"status":"draft"}` 或 `{"status":"archived"}` | `post.unpublish` / `post.unpublish_any`；撤回或归档 |
| `change_category` | `{"category_id":"UUID"}` 或 `{"category_id":null}` | `post.update` / `post.update_any`；设置或清空分类，字段必须明确出现；分类存在性在事务内校验 |

评论动作是 `approve/spam/trash/restore/pending`，均无 `params`。`approve` 对应 approved；`restore/pending` 都回到 pending。垃圾或回收站评论必须先恢复为 pending，才能再次通过审核。沿用 `post.update` / `post.update_any`，逐项按当前锁定的文章作者授权；正文、身份和回复关系保持不变。单条评论的物理删除不属于此端点，`delete` 返回 400；保留父链与公开占位规则，整篇文章永久删除时清理完整评论树。

两个端点都在同一个数据库事务中核验全部对象、权限、版本和领域转换，再提交变化。任何一项失败都回滚整批（含关联清理和审计），没有部分成功响应：403 `forbidden`、404 `not_found`、409 `version_conflict`、非法转换 400 `invalid_request`；审计或存储失败返回 500。客户端遇到冲突应刷新相关列表，核对后重新选择版本；不要自动用新版本重放写入。

成功响应的 `items` 顺序与请求一致，`affected` 是实际改变的条目数。未变化条目返回原版本和 `changed:false`，仍须匹配当前版本；全部未变化时不写审计，也不更新时间或版本。永久删除后的 `version` 为 null，其他变化条目的版本加一。

```json
{
  "items": [
    { "id": "018f3000-0000-7000-8000-000000000001", "version": 4, "changed": true },
    { "id": "018f3000-0000-7000-8000-000000000002", "version": 8, "changed": true }
  ],
  "affected": 2
}
```

有实际变化时追加一条 `post.batch` 或 `comment.batch` 聚合审计，摘要含动作、改变条目的 ID 和前后版本/状态，不含正文、邮箱或凭据。前端成功后应刷新相关列表和目录/媒体使用位置缓存；批量永久删除还会影响评论与系列成员。生成契约提供 `PostBatchInput`、`CommentBatchInput` 和 `BatchResult`。

## 独立页面

Page 没有作者，使用站点级 `page.*` 权限。

| 方法与路径 | 行为 |
|---|---|
| `GET /pages` | 页面摘要分页 |
| `POST /pages` | 创建草稿，返回 201 和详情 |
| `GET /pages/{id}` | 包含 Markdown 的详情 |
| `PATCH /pages/{id}` | 编辑并返回详情 |
| `POST /pages/{id}/publish` | 立即发布；`page.publish` |
| `POST /pages/{id}/schedule` | 预约或更新预约时间；`page.publish` |
| `POST /pages/{id}/unpublish` | 撤回、取消预约或解除归档；`page.unpublish` |
| `POST /pages/{id}/archive` | 归档；`page.archive` |
| `POST /pages/{id}/trash` | 移入回收站；`page.delete`，JSON 必须含 `expected_version` |
| `GET /page-trash?page=1` | 回收站分页，返回 `items/total/page/per_page`；`page.read` |
| `POST /pages/{id}/restore` | 一律恢复为草稿；`page.delete`，JSON 必须含 `expected_version` |
| `POST /pages/{id}/purge` | 永久删除回收站页面；`page.purge`，JSON 必须含 `expected_version`，成功 204 |

创建字段为 `slug`、`title`、`content`、`visibility`；编辑改用 `new_slug` 并可带 `expected_version`。公开地址为 `/{slug}`，系统保留路径不可用，首次预约或发布后 slug 锁定。详情包含 `deleted`，正常列表与 GET 详情排除回收站记录。永久删除权限默认仅授予 Admin，普通删除与恢复授予 Editor；旧 `DELETE /pages/{id}` 已移除。

## 标签、分类与系列

已认证用户可读取标签、分类和系列目录供编辑器选择，写操作按 `tag.manage`、`category.manage`、`series.manage` 授权。系列成员列表另有下述权限检查。

| 方法与路径 | 行为 / 关键载荷 |
|---|---|
| `GET /tags`、`POST /tags` | 列表；创建需 `name`、`slug` |
| `PATCH /tags/{slug}`、`DELETE /tags/{slug}` | 修改名称；删除标签并解除文章关联，保留文章 |
| `GET /categories`、`POST /categories` | 列表；创建需 `name`、`slug`，可带 `parent`、`description` |
| `PATCH /categories/{slug}`、`DELETE /categories/{slug}` | 修改名称、描述、父级；删除受文章与子分类引用保护 |
| `GET /series`、`POST /series` | 列表；创建需 `name`、`slug`，可带 `description` |
| `PATCH /series/{slug}`、`DELETE /series/{slug}` | 修改名称、描述、封面；删除系列并解除文章关联，保留文章 |
| `GET /series/{slug}/members` | 需 `series.manage`，且逐篇检查 `post.read` / `post.read_any`；用于整体排序 |
| `POST /series/{slug}/reorder` | `ordered_post_ids` 必须是全部成员的完整排列；可带 `expected_series_version` |

目录修改不接受更换 slug。分类 `parent` 使用父分类 slug，PATCH 中省略为保持、`null` 为移到根。系列封面 `cover_media_id` 使用省略/`null`/UUID 三态。PATCH 请求仍需传 `name`，分类和系列的 `description` 省略或 `null` 都会清除，不能套用关联字段的“省略保持”。系列成员有任意一篇不可读时整体返回 403；排序需 `series.manage` 及对每篇成员文章的更新权限，不能只提交当前可见的一部分文章。

成员 DTO 使用 `position`，按 position、post_id 排序，包含草稿、预约、私密及回收站成员。整体重排将权重写为 `0..n-1`，有实际变化时递增系列版本，并递增权重变化的文章版本。删除标签或系列也会递增所有受影响文章的版本，过期编辑副本须重新加载。

## 用户与角色

| 方法与路径 | 行为 / 权限 |
|---|---|
| `GET /users?page=1&per_page=50` | 分页查询，返回 `{items,total,page,per_page}`；`user.manage` 或 `role.manage` |
| `POST /users` | 创建用户：`username`，可带 `email`、`display_name`；`user.manage` |
| `POST /users/{id}/invitation` | 向无密码且已填邮箱的启用账号发送邀请；`user.manage`，Admin 目标另需 `admin.manage`；成功返回 `{message}` |
| `PUT /users/{id}/status` | UUID 定位；`status: "active" / "disabled"`、必填正整数 `expected_version`；需 `user.manage`，目标持有 Admin 时另需 `admin.manage` |
| `GET /roles` | 角色列表，`user.manage` 或 `role.manage` |
| `PUT /users/{username}/roles/{role}` | 分配角色，成功 204 |
| `DELETE /users/{username}/roles/{role}` | 移除角色，成功 204 |

创建用户响应的 `created_at` 使用 RFC 3339 字符串，与其它 HTTP 时间字段一致。

用户查询默认第 1 页、每页 50 条；page 范围 1–100,000，per_page 范围 1–200，越界或未知查询字段返回 400。总数与条目来自同一数据库快照，均包含软删除账号；超出最后一页时返回空 items，保留实际 total 和请求分页参数。角色变更需要 `role.manage`，授予范围不能超出操作者权限，Admin 变更额外需要 `admin.manage`，最后 Admin 保护仍生效。用户与角色请求体上限为 4 KiB。当前 API 不提供自定义角色编辑、OAuth 提供商配置或管理员强制重置密码；后两者使用受控 CLI。

账号按用户名及 UUID 稳定排序。后台每页展示 50 条，显示实际总数，支持点击页码和直接跳页；创建账号、角色或状态变更后使全部账号分页缓存失效，当前页超出范围时回到有效页。角色操作因 `last_admin` 被拒时也重新读取列表，更新最后 Admin 标记；最后可登录 Admin 的判定使用全站计数，不受当前页影响。

用户列表包含 `status` 和编辑 `version`；状态与登录方式分开显示，停用不删除密码、外部身份、角色或文章。状态 PUT 成功返回 `{ "id": "UUID", "status": "disabled", "version": 4 }`。实际启用或停用同事务递增 `version/auth_version`、删除全部持久会话、追加 `user.status.update` 审计；启用后必须重新登录。相同状态且版本匹配时不写入、不撤销会话、不重复审计；旧版本仍返回 409 `version_conflict`。最后可登录 Admin 不能停用，返回 403 `last_admin`；软删除账号不能在此恢复，返回 404。未知状态或字段拒绝。后台 `/admin/users` 提供确认操作；允许停用本人，但仍执行最后 Admin 保护，成功后本人会话失效。

## 设置

独立的插件管理使用 `GET /plugins` 和 `PUT /plugins/{id}`，读取与写入均需 `plugins.manage`。PUT 载荷为 `{ enabled, config, expected_version }`，请求体上限 48 KiB。状态与审计同事务保存，版本冲突不覆盖；完整配置、前台资源和正文重建契约见[插件机制](plugins.md)。它不经过下述 `settings.manage` 分组接口。

读取与写入均需 `settings.manage`，请求体上限为 16 KiB。

| 方法与路径 | 行为 / 载荷 |
|---|---|
| `GET /settings/site` | 标题、描述、logo、`home_page_size`、`navigation`、`time_zone`、可选 IANA 名称 `time_zones`、生效来源和版本 |
| `PUT /settings/site` | `title`、`description`、`logo_media_id`、`home_page_size`、`navigation`、`time_zone`、`expected_version` |
| `GET /settings/theme` | 所选 `slug`、`effective_slug`、`fallback_slug`、来源、版本与可用主题；每个可用主题包含 `slug`、`name`、快照 `release`、记录 `id`、`config_version` 和 `config_schema_version` |
| `PUT /settings/theme` | `slug`、`expected_version` |
| `GET /settings/retention` | 评论 IP 与审计的保留天数及两组版本 |
| `PUT /settings/retention` | 必填 `comment_ip_days`、`comment_version`、`audit_days`、`audit_version` |

主题包管理同样需要 `settings.manage`、会话、同源 Origin 和 CSRF，全部响应 `no-store`：

| 方法与路径 | 行为 / 载荷 |
|---|---|
| `POST /themes/validate-package` | 裸 ZIP 字节，推荐 `Content-Type: application/zip`；验证成功 200，不安装 |
| `POST /themes` | 裸 ZIP 字节；先验证再安装，成功 201，不覆盖同名目录 |
| `POST /themes/{slug}/validate` | 无请求体；验证已安装快照，成功 200 |
| `GET /themes/{slug}/settings` | 返回记录 `id`、`slug`、`release`、字段 `fields`、生效 `config`、保存的 `overrides`、`config_schema_version` 和编辑 `version`；未激活主题也可配置 |
| `PUT /themes/{slug}/settings` | JSON `{ "id": "UUID", "expected_release": "…", "config_schema_version": 1, "expected_version": 1, "config": {} }`；整份替换覆盖值，省略字段恢复默认值；身份、快照、结构或编辑版本变化返回 409 |
| `DELETE /themes/{slug}` | JSON `{ "expected_version": 2, "expected_release": "…", "id": "UUID", "expected_config_version": 1, "config_schema_version": 1 }`；前者是选择版本，其余来自打开页面时的主题身份；成功 200 返回更新后的主题设置视图，删除配置与引用，保留媒体文件 |

验证和安装成功均返回 `{ slug, name, release, template_count, asset_count }`；不兼容清单、模板/场景错误、非法包及受保护主题卸载返回 400，未知主题 404，选择、身份、配置版本或主题快照冲突 409，验证繁忙 429（`Retry-After: 1`），上传超过 10 MiB 返回 413。配置及卸载 JSON 请求上限 80 KiB，配置覆盖值独立限制为 64 KiB；ZIP 接口使用上传超时预算（默认 120 秒）。完整打包、保护和持久化边界见[主题机制](themes-and-rendering.md#安装验证激活和卸载)。

`time_zone` 是 IANA 名称（例如 `Asia/Shanghai`），保存后无需重启，`/me` 和公开评论列表读取当前值。未知时区返回 400；旧客户端省略或传 `null` 时保留已保存的时区。时区变更沿用 site 行的版本检查和审计。

`home_page_size` 是首页每页文章数，范围 1–100 的整数，默认 20。保存后立即生效；省略或 null 保留已保存的值。仅影响前台首页，后台列表和目录页的分页规则不变。

`navigation` 是有序数组，单项为 `{ "label": "关于", "page_slug": "about", "placement": "header" }`。位置为 header/footer，每个位置按数组顺序展示；最多 20 项，名称 trim 后为 1–40 字符且无控制字符，目标必须为合法且非系统保留的 Page slug，同一位置不可重复目标。允许预先配置尚未公开的页面，公开渲染只输出当时可读的目标。省略或 null 保留已有导航，`[]` 清空；与站点设置共用版本及事务审计。

`site` 除首页每页数量、时区和导航外的字段是整组替换，省略或传 `null` 的 logo 会被清除；新 logo 要求图片存在且未移入回收站，原有引用可继续保留。未配置的设置版本为 0。保留期默认各 180 天，范围 1–36,500 整数天；更新只合并两项字段并保留其他 JSON 设置，两组版本任一过期返回 409。未知字段拒绝，相同值不增版。评论全站开关与 IP 保留期共享 comments 分组版本。清理由独立维护命令或具备维护能力的后台任务执行；任务入口、计划和权限见上文[后台任务](#后台任务)。`/settings/oauth` 等未知分组返回 404。生效优先级见[配置参考](configuration.md)。

## 媒体

| 方法与路径 | 行为 / 权限 |
|---|---|
| `GET /media?page=1&trash=false&q=…` | 按文件名搜索并分页；trash=true 查询回收站，`media.read` |
| `POST /media?filename=…` | 裸图片字节上传，`media.upload`；文件名只用于展示 |
| `GET /media/{id}` | 资产详情、可见引用位置及 `hidden_references`，`media.read` |
| `DELETE /media/{id}` | 移入回收站，保留文件和引用；JSON 必须含 `expected_version`，本人 `media.delete` / 任意 `media.delete_any`，成功 204 |
| `POST /media/{id}/restore` | 恢复；版本、权限和成功状态码同上 |

可选 `q` 去除首尾空白，空值不筛选，最长 200 字符；按文件名忽略大小写做字面子串匹配，`%` 和 `_` 不作为通配符。搜索先于分页，条目和 total 来自同一快照；正常库与回收站分别查询。编辑器插图面板、封面选择器和媒体库共用此接口。

上传不是 multipart。业务上限为 10 MiB、单边 12000 px、总计 6000 万像素，仅支持 PNG/JPEG/GIF/WebP，格式从内容识别；HTTP 上传体上限为 12 MiB，最终仍执行业务大小校验。

媒体 DTO 使用 deleted_at（null 表示正常库），不再返回 status 或 public_reference_count。owner_id 可空；reference_count 包含所有站内引用，软删除不要求其为零。上传初始 version 为 1。

文件读取为站点根下的 GET/HEAD /media/{id}，不含管理前缀。所有已登记图片，包括回收站图片，均独立公开；不校验 Cookie 或查询引用。使用 `public, max-age=31536000, immutable` 与 SHA-256 ETag，命中返回 304。来源内容的标题仍按阅读权限过滤。完整规则见[内容生命周期](content-lifecycle.md)。

## 版本与请求限制

一般编辑和动作请求可带 `expected_version`，例如 `{ "expected_version": 3 }`。Page 的 trash/restore/purge 及 Media 的删除/恢复必须提供版本；schedule 还必须提供发布时间。系列整体排序使用独立的 `expected_series_version`。

显式版本过期返回 409 `version_conflict`，即使操作本身幂等也不会绕过版本检查。允许省略版本的用例会读取当前版本再条件写入，仍能检测读取后的并发变化，但不能识别客户端此前编辑的是旧副本。编辑器应总是提交已读取的版本，冲突后让用户选择重新加载或基于新版本再次提交。

内容和目录请求体默认上限为 2 MiB，认证、身份、设置、媒体的特定限制见各节。文章、页面、回收站、媒体、用户及后台评论列表均返回 `items/total/page/per_page`；后台评论额外包含全站评论开关 `enabled`，关闭后仍可管理历史评论。目录列表仍返回数组，审计日志使用游标分页，客户端应按各端点契约解析。

## 错误与追踪

应用错误经统一映射返回：

```json
{
  "error": "数据已被其他操作修改",
  "code": "version_conflict",
  "request_id": "请求 UUID"
}
```

客户端按 `code` 分支，不依赖示例文案。错误注册与映射在 [http_support.rs](../crates/interfaces/src/http_support.rs)。

| HTTP | 业务码 |
|---|---|
| 400 | `invalid_request`（含使用 `AdminAuth` 的管理写入 CSRF 校验失败） |
| 401 | `unauthenticated`、`invalid_credentials`；带 `WWW-Authenticate: Session` |
| 403 | `forbidden`（含退出/自助改密的 CSRF 失败）、`last_admin` |
| 404 | `not_found` |
| 409 | `version_conflict`、`conflict`、`username_taken`、`email_taken`、`category_in_use` |
| 429 | `rate_limited`；带 `Retry-After` |
| 502 | `external_error` |
| 500 | `internal_error`；响应不暴露内部错误细节 |

自助改密时当前密码错误为 403 `invalid_credentials`，避免被误判为会话失效。密码登录时用户名不存在、密码错误或账号不可用统一返回 401 `invalid_credentials`。

上表覆盖管理接口和密码认证的应用错误。OAuth 登录启动与回调仍使用纯文本错误；非法 JSON、UUID 路径解析、请求体超限和未注册路由等也可能由 Axum 直接拒绝，当前并未全部规范为上述 JSON 形态，客户端需处理非 JSON 错误响应。

完成日志记录方法、路径、状态、耗时和已验证的 actor，不记录 query、Cookie、token 或正文。应用错误复用请求上下文编号，媒体上传的错误体与 x-request-id 响应头保持一致。这是请求追踪能力，不能替代角色变更、身份绑定等动作级审计；审计交付范围见[路线图](product-roadmap.md)。

时间展示：文章、页面、媒体和评论的时间字段均为带偏移的 RFC 3339 字符串；后台按照 `/me.time_zone` 显示，不解析展示文本。媒体和评论时间不再返回旧的 `YYYY-MM-DD HH:mm UTC` 格式。

### 注册与参与设置

- `GET /auth/register`：公开返回 `{enabled}`，响应不缓存。
- `POST /auth/register`：`{username, display_name?, email, password}`；成功 201，默认分配 reader。注册关闭返回 403 `registration_closed`，用户名/邮箱重复返回 409。沿用密码策略、同源校验和请求限流。
- `POST /auth/login/password` 的 `username` 字段现在接受用户名或邮箱；两者不区分大小写，归属同一账号的失败限流。
- `GET/PUT /api/admin/v1/access-settings`：要求 `settings.manage`，字段为 `registration_enabled`、`guest_comments_enabled`、`version`。默认两个开关均关闭，更新需匹配版本。
- 公开评论列表附带 `guest_comments_enabled`。游客评论关闭时匿名提交返回 401；登录账号仍可按当前审核策略提交评论；评论 API 的策略和回执见[原生评论](comments.md#接口)。
- 内置角色为 admin、editor、author、reader。admin 拥有完整权限，editor 可创建及管理全部文章，author 管理本人文章，reader 仅可评论及管理本人资料。保护错误码为 `last_admin`，用户列表标记为 `is_last_loginable_admin`。

## 安全响应与系列计数

完整站点路由统一返回安全响应头，策略与 HTTPS 部署要求见[配置说明](configuration.md)。Secure 会话 Cookie 名为 `__Host-blog_session`，HTTP 开发使用 `blog_session`；前端仍通过 `/me` 获取 CSRF token，不读取 Cookie。

`SeriesSummary.post_count` 类型为 `number | null`：仅 `post.read_any` 可见含草稿、私密和回收站的总数，其他账号得到 null。`pub_post_count` 始终为公开成员数量。列表、创建和更新响应一致；标签与分类仍仅有公开文章计数。后台对不可见总数仅展示公开数量，不把 null 当作 0。

## 邮件找回（匿名认证入口）

- `GET /auth/password/recovery` 返回 `{enabled}`，表示部署是否配置 SMTP。
- `POST /auth/password/recovery` 接收 `{email}`，统一返回 202 `{message}`，不透露账号是否存在或投递结果。
- `POST /auth/password/reset` 接收 `{token,password}`，成功返回 200 `{message}`；一次性链接失效/过期返回 400。新密码必须满足本站策略。成功撤销全部会话，不签发新 Cookie。

均使用 `no-store`、4 KiB 请求体限制、客户端与全局准入；匿名写请求校验 Origin。未配置 SMTP 返回明确的 400 提示。令牌在邮件 URL fragment，提交时只通过 JSON 正文发送。管理员邀请仍经过会话和 CSRF 校验。
