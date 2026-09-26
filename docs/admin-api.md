# 管理 API 参考

本文描述当前 HTTP 路由与通用请求约定。业务状态和可见性见[内容生命周期](content-lifecycle.md)，权限和会话见[身份、权限与后台](identity-and-admin.md)。项目仍在开发阶段，客户端应随接口变更同步更新。

路由与传输 DTO 的实现入口：[内容和设置](../crates/interfaces/src/http_admin.rs)、[身份](../crates/interfaces/src/http_identity.rs)、[媒体](../crates/interfaces/src/http_media.rs)、[认证](../crates/interfaces/src/http_auth.rs)。

## 通用约定

- 管理 API 前缀为 `/api/admin/v1`，使用会话 cookie 认证，响应设置 `Cache-Control: no-store`。
- 先调用 `GET /api/admin/v1/me` 获取当前身份、有效权限和 `csrf_token`；已认证写请求带 `X-CSRF-Token`。自助改密成功后使用新会话与新 token。
- 请求带 Origin 时校验其与 Host 的关系；当前接受 `http://{Host}` 或 `https://{Host}`，缺失 Origin 不拒绝。跨源拒绝为 403；普通管理写入的 CSRF 失败为 400，退出和自助改密的 CSRF 失败为 403。
- JSON 请求使用 `Content-Type: application/json`，媒体上传例外，直接发送图片字节。
- Post、Page 和 Media 的路径标识是 UUID。Tag、Category、Series 使用 slug；角色分配路径使用 username 和 role key。管理身份不应统一推断为 slug 或 UUID。
- 每个响应有 `x-request-id`，应用错误体也有 `request_id`。报障优先保留响应头编号，当前存在媒体上传错误体编号不同的例外，见下方追踪说明。

## 评论

评论管理位于 `/admin/comments`。评论接口、开关和独立版本规则见[评论 API](comments.md#接口)。

## 认证与本人资料

此表列出完整路径，不应用管理前缀。

| 方法与路径 | 用途 / 载荷 |
|---|---|
| `GET /auth/providers` | 公开提供商列表，仅含 `id`、`name`、`kind` |
| `GET /auth/login?provider=…&next=…` | 发起 OAuth 登录，建立浏览器绑定状态 |
| `GET /auth/callback/{provider}` | OAuth 回调 |
| `POST /auth/login/password` | `{ "username": "sun", "password": "…" }`，成功设置会话 cookie |
| `POST /auth/logout` | 会话、CSRF 与来源校验通过后退出 |
| `GET /api/admin/v1/me` | 当前用户、有效权限和 CSRF token |
| `POST /api/admin/v1/me/password` | `new_password`；已启用密码时还需 `current_password` |
| `PUT /api/admin/v1/me/avatar` | `{ "avatar_media_id": "UUID" }`；`null` 清除 |

密码登录是匿名写入口，没有可用的会话 CSRF token，执行来源检查。密码登录和本人资料请求体上限为 4 KiB；密码失败与限流行为见下方错误表。

## 文章与回收站

以下所有路径均相对于 `/api/admin/v1`。读写权限按文章作者区分 own / any；创建文章的作者取当前会话用户。

| 方法与路径 | 行为 |
|---|---|
| `GET /posts` | 当前用户文章摘要数组；显式 `?author=用户名` 需 `post.read_any` |
| `POST /posts` | 创建草稿，返回 201 和详情 |
| `GET /posts/{id}` | 详情，包含 Markdown `content` 和 `excerpt` |
| `PATCH /posts/{id}` | 编辑并返回详情；编辑已发布文章会直接更新线上内容 |
| `POST /posts/{id}/publish` | 发布，返回详情 |
| `POST /posts/{id}/unpublish` | 撤回为草稿，返回详情 |
| `POST /posts/{id}/trash` | 移入回收站，返回详情 |
| `GET /post-trash?page=1` | 回收站分页，可带 `author`；返回 `items/total/page/per_page` |
| `POST /posts/{id}/restore` | 恢复为草稿，归档记录保留终态；返回详情 |
| `POST /posts/{id}/purge` | 永久删除回收站文章，返回 204 |

创建字段为 `slug`、`title`、`excerpt`、`content`、`visibility`、`tag_ids`、`category_id`、`series`、`cover_media_id`。`slug` 可省略生成临时值，草稿允许未完成的标题与正文；发布要求见[内容生命周期](content-lifecycle.md)。`series` 为 `{ "id": "UUID", "order": 1 }`。

编辑支持 `new_slug`、`title`、`excerpt`、`content`、`visibility`、`tag_ids`、`category_id`、`series`、`cover_media_id` 和 `expected_version`。注意不同字段的更新语义：

| 字段 | 省略 | 清除 | 设置 |
|---|---|---|---|
| `tag_ids` | 保持 | `[]` | UUID 数组整体替换 |
| `category_id` / `cover_media_id` | 保持 | `null` | UUID |
| `series` | 保持 | `null` | `{ "id": "UUID", "order": 1 }` |
| `excerpt` | 保持 | `""` | 字符串；`null` 与省略同义 |

`visibility` 为 `public` 或 `private`。状态由动作端点变更，不通过编辑请求直接赋值。`content` 始终是 Markdown；`content_html` 由服务端派生并持久化，不是客户端可设置的字段，也不是管理详情的正文格式。

## 独立页面

Page 没有作者，使用站点级 `page.*` 权限。

| 方法与路径 | 行为 |
|---|---|
| `GET /pages` | 页面摘要数组 |
| `POST /pages` | 创建草稿，返回 201 和详情 |
| `GET /pages/{id}` | 包含 Markdown 的详情 |
| `PATCH /pages/{id}` | 编辑并返回详情 |
| `POST /pages/{id}/publish` | 发布 |
| `POST /pages/{id}/unpublish` | 撤回为草稿 |
| `DELETE /pages/{id}` | 永久删除；JSON 必须含 `expected_version`，成功返回 204 |

创建字段为 `slug`、`title`、`content`、`visibility`；编辑改用 `new_slug` 并可带 `expected_version`。公开地址为 `/{slug}`，系统保留路径不可用，首次发布后 slug 锁定。Page 当前没有回收站接口。

## 标签、分类与系列

已认证用户可读取标签、分类和系列目录供编辑器选择，写操作按 `tag.manage`、`category.manage`、`series.manage` 授权。系列成员列表另有下述权限检查。

| 方法与路径 | 行为 / 关键载荷 |
|---|---|
| `GET /tags`、`POST /tags` | 列表；创建需 `name`、`slug` |
| `PATCH /tags/{slug}`、`DELETE /tags/{slug}` | 修改名称；删除未使用标签 |
| `GET /categories`、`POST /categories` | 列表；创建需 `name`、`slug`，可带 `parent`、`description` |
| `PATCH /categories/{slug}`、`DELETE /categories/{slug}` | 修改名称、描述、父级；删除受文章与子分类引用保护 |
| `GET /series`、`POST /series` | 列表；创建需 `name`、`slug`，可带 `description` |
| `PATCH /series/{slug}`、`DELETE /series/{slug}` | 修改名称、描述、封面；删除空系列 |
| `GET /series/{slug}/members` | 需 `series.manage`，且逐篇检查 `post.read` / `post.read_any`；用于整体排序 |
| `POST /series/{slug}/reorder` | `ordered_post_ids` 必须是全部成员的完整排列；可带 `expected_series_version` |

目录修改不接受更换 slug。分类 `parent` 使用父分类 slug，PATCH 中省略为保持、`null` 为移到根。系列封面 `cover_media_id` 使用省略/`null`/UUID 三态。PATCH 请求仍需传 `name`，分类和系列的 `description` 省略或 `null` 都会清除，不能套用关联字段的“省略保持”。系列成员有任意一篇不可读时整体返回 403；排序需 `series.manage` 及对每篇成员文章的更新权限，不能只提交当前可见的一部分文章。

## 用户与角色

| 方法与路径 | 行为 / 权限 |
|---|---|
| `GET /users?limit=…&offset=…` | 分页查询，返回用户数组；`user.manage` 或 `role.manage` |
| `POST /users` | 创建用户：`username`，可带 `email`、`display_name`；`user.manage` |
| `GET /roles` | 角色列表，`user.manage` 或 `role.manage` |
| `PUT /users/{username}/roles/{role}` | 分配角色，成功 204 |
| `DELETE /users/{username}/roles/{role}` | 移除角色，成功 204 |

用户查询默认取 50 条，最多 200 条，负 offset 收敛为 0。角色变更需要 `role.manage`，授予范围不能超出操作者权限，Owner 变更额外需要 `ownership.manage`，最后 Owner 保护仍生效。用户与角色请求体上限为 4 KiB。当前 API 不提供自定义角色编辑、OAuth 提供商配置或管理员强制重置密码；后两者使用受控 CLI。

## 设置

读取与写入均需 `settings.manage`，请求体上限为 16 KiB。

| 方法与路径 | 行为 / 载荷 |
|---|---|
| `GET /settings/site` | 标题、描述、logo、生效来源和版本 |
| `PUT /settings/site` | `title`、`description`、`logo_media_id`、`expected_version` |
| `GET /settings/theme` | 所选 slug、生效 slug、来源、版本与可用主题 |
| `PUT /settings/theme` | `slug`、`expected_version` |

`site` 是整组替换，省略或传 `null` 的 logo 会被清除；设置 logo 还需要满足媒体附着权限。未配置的设置版本为 0。仅注册 `site` 和 `theme`，`/settings/oauth` 等未知分组返回 404。生效优先级见[配置参考](configuration.md)。

## 媒体

| 方法与路径 | 行为 / 权限 |
|---|---|
| `GET /media?page=1` | 分页，`media.read` |
| `POST /media?filename=…` | 裸图片字节上传，`media.upload`；文件名只用于展示 |
| `GET /media/{id}` | 资产详情、可见引用位置及 `hidden_references`，`media.read` |
| `DELETE /media/{id}` | JSON 必须含 `expected_version`；本人 `media.delete` / 任意 `media.delete_any`，成功 204 |

上传不是 multipart。业务上限为 10 MiB、单边 12000 px、总计 6000 万像素，仅支持 PNG/JPEG/GIF/WebP，格式从内容识别；HTTP 上传体上限为 12 MiB，最终仍执行业务大小校验。

文件读取路径为站点根下的 `GET /media/{id}`，不含管理前缀。匿名可见性由公开引用决定，公开响应 `no-cache` 配 ETag，后台私有预览 `no-store`。完整引用、附着和回收规则见[内容生命周期](content-lifecycle.md)。

## 版本与请求限制

一般编辑和动作请求可带 `expected_version`。文章、页面的发布/撤回及回收站动作可发送 `{ "expected_version": 3 }`；Page 与 Media 的 DELETE 必须发送版本。系列整体排序使用独立的 `expected_series_version`。

显式版本过期返回 409 `version_conflict`，即使操作本身幂等也不会绕过版本检查。允许省略版本的用例会读取当前版本再条件写入，仍能检测读取后的并发变化，但不能识别客户端此前编辑的是旧副本。编辑器应总是提交已读取的版本，冲突后让用户选择重新加载或基于新版本再次提交。

内容和目录请求体默认上限为 2 MiB，认证、身份、设置、媒体的特定限制见各节。列表响应形态并不统一：文章和页面为摘要数组；用户接受 limit/offset，但仍返回数组，不包含总数；回收站与媒体返回 `items/total/page/per_page`。客户端不能假定所有列表共享一个分页 DTO。

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
| 403 | `forbidden`（含退出/自助改密的 CSRF 失败）、`last_owner`、`media_not_attachable` |
| 404 | `not_found` |
| 409 | `version_conflict`、`conflict`、`username_taken`、`email_taken`、`tag_in_use`、`category_in_use`、`series_in_use`、`media_in_use` |
| 429 | `rate_limited`；带 `Retry-After` |
| 502 | `external_error` |
| 500 | `internal_error`；响应不暴露内部错误细节 |

自助改密时当前密码错误为 403 `invalid_credentials`，避免被误判为会话失效。密码登录时用户名不存在、密码错误或账号不可用统一返回 401 `invalid_credentials`。

上表覆盖管理接口和密码认证的应用错误。OAuth 登录启动与回调仍使用纯文本错误；非法 JSON、UUID 路径解析、请求体超限和未注册路由等也可能由 Axum 直接拒绝，当前并未全部规范为上述 JSON 形态，客户端需处理非 JSON 错误响应。

完成日志记录方法、路径、状态、耗时和已验证的 actor，不记录 query、Cookie、token 或正文。大多数应用错误复用请求上下文编号；当前媒体上传处理器另行生成错误体的 `request_id`，可能与全站响应头编号不同，定位完成日志以 `x-request-id` 为准。这是请求追踪能力，不能替代角色变更、身份绑定等动作级审计；审计交付范围见[路线图](product-roadmap.md)。
