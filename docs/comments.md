# 原生评论

文章页的 default 和 paper 主题都提供评论列表、提交表单及一层回复。公开组件通过同源 API 加载，需启用 JavaScript；共享脚本/CSS 随 Rust 二进制提供，不依赖后台构建产物。

游客填写 1–64 字昵称和 1–2,000 字正文；正文支持换行，按 Unicode 字符计数。登录用户的客户端昵称不参与校验或存储，使用服务端账号名称（展示名取前 64 字），正文仍需完整校验。只有文章作者本人的账号显示独立的「作者」徽标；游客昵称中的「作者」文字不会获得徽标样式。所有提交，包括后台回复，默认待审核，成功返回「已提交，等待审核」。

同一文章页内，评论翻页和重复打开回复表单会保留尚未提交的昵称、正文与重试请求编号；成功提交后清空正文。草稿仅保留在当前页面内，刷新或离开页面不会持久保存。

后台「评论管理」支持按状态和文章筛选、审核、回复和永久删除。`post.update` 管理本人文章评论，`post.update_any` 管理全部；全站开关要求 `settings.manage`。单篇文章编辑页有立即保存的评论开关。关闭开关保留已通过历史评论，但拒绝新评论与回复。评论和开关的独立版本冲突返回 409 `version_conflict`，重新加载后再操作。

## 可见性和安全

- 所有公开读取、计数和提交都检查文章 published + public + 未进回收站；不满足时返回 404。公开响应不缓存。
- 只有 approved 状态可展示；回复的主评论也必须 approved。删除主评论级联删除回复；永久删除文章清理全部评论与单篇开关。
- 回复必须关联同篇文章的主评论，不能回复一条回复。主评论和回复分别每页 20 条，页码为 1–100,000。
- 前台使用 `textContent`，后台使用 React 文本节点，不执行 Markdown/HTML，也不使用文章正文的 `safe` 输出。依据 [OWASP 输出编码建议](https://cheatsheetseries.owasp.org/cheatsheets/Cross_Site_Scripting_Prevention_Cheat_Sheet.html)。
- 公开提交要求 Origin 精确匹配 `BLOG_PUBLIC_BASE_URL` 的来源；拒绝缺失/跨站 Origin，若有 Sec-Fetch-Site 必须 same-origin。带会话 cookie 的写请求额外复用后台会话、CSRF 检查，不降级成游客。参见 [OWASP CSRF 建议](https://cheatsheetseries.owasp.org/cheatsheets/Cross-Site_Request_Forgery_Prevention_Cheat_Sheet.html)。
- `/me` 确认会话失效时返回 401 并清理无效 cookie；存储故障不清理 cookie。填写过程中遇到 401，表单保留正文并重新确认身份，提示用户确认昵称或登录身份后手动重试，不自动重新提交。
- 请求体上限 16 KiB；每个连接来源 IP 每分钟最多 3 条成功提交；相同来源、文章、父评论及正文在 10 分钟内拒绝重复。数据库事务锁和计数使多进程共享限制。IP 只保存 SHA-256 摘要，不返回公开接口。
- 客户端为一次提交生成 `request_id` UUID，网络失败重试复用。同一请求及内容重复调用只返回相同待审核回执，不泄漏当前审核状态。变更内容后需新 UUID。

服务端不信任客户端的 `X-Forwarded-For` 等代理头。经反向代理部署时，来自同一代理 IP 的读者共享限流；部署前需确认流量入口，当前未提供可信代理真实 IP 配置。此版本无验证码或外部反垃圾服务。

## 接口

| 方法与路径 | 参数与行为 |
|---|---|
| `GET /api/v1/posts/{slug}/comments` | `page=1`；传 `parent_id=UUID` 读取该主评论的回复；返回 `{items,total,enabled}` |
| `POST /api/v1/posts/{slug}/comments` | `{nickname,body,parent_id?,request_id}`；202 待审核回执；登录身份由会话决定 |
| `GET /api/admin/v1/comments` | `page=1&status=pending&post_id=UUID`，状态和文章筛选可省略；返回有权限范围内分页列表 |
| `POST /api/admin/v1/comments/{id}` | `{version,status}` 修改审核状态，或 `{version,delete:true}` 永久删除；204 |
| `GET /api/admin/v1/comment-settings` | 读取全站开关 `{enabled,version}` |
| `PUT /api/admin/v1/comment-settings` | `{enabled,version}`，返回保存结果 |
| `GET /api/admin/v1/posts/{id}/comment-settings` | 读取单篇开关；未显式保存时 `{enabled:true,version:0}` |
| `PUT /api/admin/v1/posts/{id}/comment-settings` | `{enabled,version}`，返回保存结果 |

管理端使用现有会话和 CSRF；评论错误沿用现有业务码，包括 invalid_request、not_found、forbidden、version_conflict、rate_limited（含 Retry-After）。

## 升级与恢复

运行现有迁移流程应用 `0009_comments.sql`，并重新构建 Rust 服务与后台 SPA；两种主题的文章模板也需一起发布。`scripts/recovery.py` 的表存在性及数量核验包含 comments、comment_settings 和 post_comment_settings。使用迁移前的旧备份恢复旧版本服务后，再按常规流程升级迁移；当前恢复工具仍要求备份和服务的 schema 版本匹配。

验证入口：`crates/infrastructure/tests/comments.rs`（真实 PostgreSQL 状态、权限、分页、去重、并发、级联）、`crates/server/tests/admin_api.rs` 的评论 HTTP 测试，以及后台 Vitest 的公开组件/审核界面测试。
