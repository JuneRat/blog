# 功能范围与实施路线

状态：M1 内容闭环（Post + Page）与 M2 身份/后台**已交付**；M0 主题桥接原型**已完成**（结论可行，见 `spikes/template-bridge/README.md`，正式主题函数冻结不再被阻塞）；M3 进行中：标签闭环、**分类树**与 **Series 目录与并发重排**（管理/引用保护/文章关联/整体重排锁协议/公开系列页）**已交付**，**settings 第一段（site 分组：站点标题/描述的后台读写闭环、数据库>环境变量>默认值的生效优先级、版本 CAS 与分组隔离）已交付**，**RSS/sitemap 与基础 SEO（/feed.xml、/sitemap.xml、/robots.txt、canonical/标题/描述统一）已交付**，备份恢复与 settings 后续分组（theme/seo 等）待交付。当前数据库基线为用户确认的 [13 表设计](database-design.md)，最新范围由 [ADR-0008](adr/0008-thirteen-table-blog-core.md) 记录，覆盖此前有冲突的持久化建议。

## 1. 已确认基线

- Rust 模块化单体，Cargo workspace 四层加独立装配入口，共五个 crate。
- 公开站点 MiniJinja SSR 与受控只读模板函数；后台 React + TypeScript + Vite SPA。
- 单站点多人协作、RBAC、内置和自定义角色、OAuth；通用 OIDC + GitHub 为首批提供商。
- 首期仅 PostgreSQL；预留多数据库适配，插件采用主题、Webhook 和外部服务等受限扩展点。
- 作者可直接发布自己的文章，后端检查资源归属，无强制审核。
- 当前采用 users、oauth_accounts、roles、permissions、user_roles、role_permissions、categories、series、posts、tags、post_tags、pages、settings 共 13 表。
- 一篇文章最多一个分类、一个系列，多个标签；分类可有父节点，系列有文章顺序；Page 独立且无文章组织关系。
- 不预建修订、路径、媒体、会话、令牌、通知或任务等辅助表；这些能力需要时再扩展。
- 当前已实现：M1 的 Post 内容闭环与公开 SSR、Page 的创建/编辑/发布/撤回与根路径 `/{slug}` 公开访问（含系统保留路径校验）；M2 的 RBAC、OAuth 登录、单实例会话、管理写 API 与 React 后台；M2 之后的本地密码认证（Argon2id + 登录限流 + 受控重置，见 [ADR-0009](adr/0009-local-password-authentication.md)），以及后台用户与角色管理（账号列表/创建、角色目录、角色分配与移除，含授权边界、最后可登录 Owner 保护与撤权会话失效；角色目录只读，自定义角色的创建/授权编辑仍属后续，见 [身份与后台 §8](identity-and-admin.md)）。Post 的受控 CLI 写通道已交付；Page 目前只经后台管理 API，尚未提供 CLI 子命令。
- M3 已交付第一段：标签目录管理（创建/改名/删除，`tag.manage`，slug 创建后不可改，引用保护拒绝删除）、文章编辑器多标签选择与正文同事务保存（仅标签变化也递增 posts.version）、公开标签页 `/tags/{slug}` 分页（每页 20，只列公开已发布文章）、文章详情展示标签；管理 API `GET/POST /api/admin/v1/tags`、`PATCH/DELETE /api/admin/v1/tags/{slug}`，业务码 `tag_in_use`（409，与 slug 占用区分）。
- M3 已交付第二段：分类树（创建/更新/移动/删除，category.manage，slug 创建后不可改；移动在分类树事务锁内做深度受限祖先链防环；被文章引用或含子分类时删除受 category_in_use 保护）、文章编辑器分类选择（三态 category_id，与正文/标签同事务，仅分类变化也递增 version）、公开分类页 /categories/（slug） 分页（直接归属）、详情页分类链接。
- M3 已交付第三段：Series 目录管理与并发重排（创建/更新/删除，series.manage，slug 创建后不可改，被文章引用时删除受 series_in_use 保护）；文章设置系列与序号（三态，同事务保存，位置唯一冲突报 conflict）；整体重排在系列行锁 + series.version 校验下进行，成员按 id 序加锁、位置唯一约束 DEFERRED 到提交检查，递增涉及 posts.version 与 series.version，跨系列移动按 ID 序锁两个系列；重排逐篇核验文章授权（Author 不能借重排改他人文章）；公开系列页 /series/（slug） 按阅读顺序分页（草稿占位不外泄，页内连续编号）。
- M3 已交付第四段：settings 第一段——`site` 分组（站点标题/描述）。管理 API `GET/PUT /api/admin/v1/settings/site`（读写都要求 `settings.manage`，内置 admin/owner 持有）；生效优先级数据库 site 行 > 环境变量 `BLOG_SITE_TITLE`/`BLOG_SITE_DESCRIPTION` > 内置默认值，公开页面每次渲染解析（保存即生效，重启后保留），行内缺字段按字段回退、存储读取失败公开页整体回退；`expected_version` 条件写入（未配置行为 0，插入前提），内容一致的保存幂等不递增版本，并发覆盖一方 409 `version_conflict`；标题 trim 非空 ≤200 字符、描述 ≤500 字符（空描述合法）；oauth 等受保护分组不在 settings API 面（未知分组 404），写路径只触碰 key='site'。后台 SPA `/admin/settings`（来源提示 + 统一 409 冲突流程）。
- M3 已交付第五段：RSS/sitemap 与基础 SEO。公开端点 `GET /feed.xml`（RSS 2.0，最新 20 篇公开已发布文章；`guid` 取 canonical URL 且 `isPermaLink="true"`，带 `pubDate`、绝对链接与 `atom:link` 自指；`application/rss+xml`）、`GET /sitemap.xml`（首页 + 公开文章 + 公开 Page，各自带 `lastmod`；标签/分类/系列页**只在至少有一篇公开文章时**收录，分页变体不单独收录；50,000 条是**整个文件**的预算，按「首页 → 文章 → Page → 目录」依次占用并在渲染层兜底截断）、`GET /robots.txt`（放行公开内容，`Disallow` 后台/接口/认证前缀，声明 sitemap）；三者均 `no-cache`，内容变化后下一次请求立即反映。SEO：`seo` 上下文由应用层统一计算标题（详情页「页面标题 - 站点标题」）、单行且截断到 160 字符的描述、绝对 canonical（列表页第 2 页起自指 `?page=N`）与 `og:*`，主题模板不再各自拼 `<title>`；站点公开地址一律取可信配置 `BLOG_PUBLIC_BASE_URL`（用 `url` crate 解析：绝对 http/https、无凭据、无查询片段、无路径前缀，不用请求 Host 头；子路径部署明确拒绝并在文档说明），Unicode slug 按百分号编码进入 canonical/feed/sitemap。可见性与公开页面同一条谓词：草稿、私密、已撤回、软删除内容不出现在任何机器可读入口。机器可读输出不经过主题模板，XML 转义与控制字符处理在应用层纯函数内单独测试。
- M3 已交付第六段：Post 回收站。普通列表隐藏、回收站按作者分页；移入/恢复使用 post.delete own/any 和版本 CAS，恢复为 draft（原 archived 保持 archived）；永久删除仅限回收站并要求独立 post.purge 权限，释放 slug/标签关联/系列位置，系列版本与重排共用锁协议。公开详情、目录、RSS、sitemap 使用统一可见性谓词。Page 物理删除仍单独交付。
- 尚未开始：M3 的备份恢复与隔离恢复演练、settings 后续分组（theme/seo 等，各自补齐分组校验与授权）、正式主题数据函数（M0 已验证可行，契约冻结随 M3 主题函数交付）与第二主题、M4 的邀请/审计/媒体等扩展、M5 上线验收。

## 2. 决策与范围变化

| 项目 | 当前方案 | 来源/状态 |
|---|---|---|
| 内容存储 | Post/Page 各自一张当前内容表，无历史修订 | 用户提供并确认的 schema |
| RBAC 存储 | roles、permissions、user_roles、role_permissions | 已确认；key 范围和委派细则为工程建议 |
| 分类/系列 | 单分类、单系列、系列内唯一序号；分类树 | 已确认；树防环/重排事务待验证 |
| 配置 | settings 按 key 保存 JSONB | 已确认；按组校验、秘密引用与敏感授权为工程约定 |
| 本地密码 | users.password_hash 存 Argon2id PHC 字符串；CLI 设置/重置，登录限流 | 已交付（ADR-0009）；自助邮箱找回仍未交付 |
| 编辑发布 | 保存已发布内容直接更新线上；不提供线上旧版与编辑新版隔离 | 单份正文模型的明确行为 |
| Slug | 创建时唯一；首次发布后锁定，无旧链接跳转；硬删除释放 | 工程约定，替代原路径登记建议 |
| 页面地址 | 根路径 /{slug}，保留系统路由 | 已实现：领域在创建/改名/发布校验保留路径，公开读取再拒绝一次，固定路由优先 |
| 删除 | Post 回收站；Page 无回收站，物理删除 | Post 已交付；Page 物理删除用例尚未实现 |
| Owner/账号开通 | CLI 显式创建用户并绑定稳定外部身份 | 工程建议；禁止首个任意登录自动提权 |
| 会话/OAuth 临时状态 | 单实例有界内存存储，重启失效 | 不建辅助表下的首版实现建议 |
| 管理员邀请 | 保留后续协作需求，暂不属于核心闭环 | 需补持久化或共享短期存储、一次性消费与权限复核 |
| 审计 | 当前脱敏运行/安全日志；事务审计后续增加 | 当前无 audit_logs，不承诺无遗漏业务审计 |
| 主题桥接/缓存 | M0 验证桥接；初期无页面缓存 | 保持既定方向，generation 仍为待验证方案 |
| 媒体/修订/路径历史 | 对应功能交付时另加模型与 DDL | 当前不建立空表，也不隐藏进 settings |
| 外部事件 | 聚合 version 排序；可靠任务随 outbox 一起交付 | M4 扩展，不依赖不存在的 revision_id |
| 备份恢复 | 已交付数据库、静态资源、主题和秘密材料一致备份 | 工程建议，RPO/RTO 与保留期待部署规格确定 |

“已确认 schema”不等于应用、迁移、登录和授权已实现。原来邀请、媒体、历史恢复等较大范围需要重新按下述阶段交付，不能用 13 张表宣称已经覆盖。

## 3. 实施里程碑

| 阶段 | 交付范围 | 验收边界 |
|---|---|---|
| 开工前置 | 冻结字段/状态/权限 key、路由保留与事务规则，建立项目依赖检查 | 核心 DDL 在目标 PostgreSQL 建表验证 |
| M0：主题原型（已完成） | MiniJinja 桥接、预算、请求隔离、查询依赖 | 原型位于 `spikes/template-bridge`，17 项真实库测试全绿，结论**可行**：spawn_blocking 内 `Handle::block_on(timeout(剩余截止时间))` 桥接开销 µs 级；预算组合（deadline/查询/调用/fuel/递归/许可）实测校准；fuel 不限宿主 I/O、输出无引擎上限需宿主实现。结论与措辞见原型 README 与 [ADR-0002](adr/0002-template-data-functions.md) |
| M1：内容闭环（已交付） | users、posts/pages、草稿→发布→SSR；Post 受控 CLI 驱动（Page 经后台 API） | 单份正文更新语义、version、公开/private 隔离、slug 唯一和根 Page 保留路由；无 React/OAuth 依赖 |
| M2：身份与后台（已交付） | 核心角色/权限、OIDC/GitHub、CLI 账号开通、单实例内存会话、React 文章与页面编辑 | own/any、角色编辑防提权、Owner 保护、OAuth 防重放、CSRF、重启后会话失效 |
| M3：核心运营 | 分类树（**已交付**）、标签（**已交付**）、Series 目录与重排（**已交付**：锁协议/引用保护/公开系列页）、Series 排序、settings（**第一段已交付**：site 分组后台读写/优先级/版本 CAS/分组隔离）、主题函数/第二主题、RSS/sitemap 与基础 SEO（**已交付**：feed/sitemap/robots + canonical/标题/描述统一）、Post 回收站、备份恢复 | 13 表核心完整；分类防环、系列并发重排、设置隔离、维护备份与隔离恢复 |
| M4：按需扩展 | 邀请、持久审计、媒体；Webhook、外部搜索/统计与可靠 outbox/任务 | 每项补齐自己的存储、权限、失败与恢复规则后才开放；不为维持 13 表省略必要可靠性 |
| M5：上线验收 | 对拟上线范围做端到端、压测、迁移与完整恢复演练 | 无未实现能力的支持承诺；达到已确定的 RPO/RTO 和部署指标 |

M1 的测试/CLI 身份不能进入公开管理 HTTP。提前提供 HTTP 写接口时，认证、授权、CSRF 必须一起交付。核心建表顺序按 FK 排序；完整 DDL 不等于所有表都需在第一条用例实现前建立。

M4 不是一次性补回旧版全部辅助表。某项功能确有使用场景才设计它；历史修订、URL 改名/重定向、私有附件、多实例及持久会话都需要独立范围与迁移，不因架构预留而自动进入本版。

## 4. 后续候选能力

| 能力 | 需要同时补齐的设计 |
|---|---|
| 管理员邀请 | 身份/邮箱绑定、一次性消费、邀请者/角色变化复核 |
| 修订与编辑副本 | 线上隔离、历史恢复、快照保留与资源引用 |
| 旧路径跳转 | 当前/历史路径冲突、撤回可见性、删除后的占位策略 |
| 媒体管理 | 上传状态、保留/公开引用、私有读取、幂等回收与备份 |
| 定时发布/审核 | 状态转换、任务可靠性、角色权限 |
| MFA/Passkey | 凭据存储、限流、会话撤销和恢复流程（本地密码的等价契约已在 ADR-0009 落地，可作为实现基线） |
| 多实例/持久会话 | 共享认证状态、原子一次性消费与撤权语义 |
| 评论、邮件订阅、多语言 | 分别明确读者身份、通知投递和路径关联 |
| 第二数据库 | 独立迁移、事务/约束/备份契约，不只连接驱动 |

默认主题仍应具备响应式布局、键盘导航、深浅色、代码高亮和阅读排版；这些无需先做成插件。

## 5. M0 原型（已完成）

原型位于 spikes/template-bridge/，与生产 workspace 隔离（空 `[workspace]` 脱离根清单）；生产 crate 不依赖原型。已记录环境、预算、延迟、失败场景和结论（`spikes/template-bridge/README.md`），并更新 ADR-0002/0004。**结论：桥接方案可行**，正式主题函数 API 冻结不再被阻塞；硬约束（多线程 runtime、fuel 特性、宿主 I/O 用剩余 deadline 包裹、输出上限宿主实现、许可持有到退出、per-request env 副本）见原型报告 §7。

M0 可以形成失败结论，但不能未经验证静默取消模板数据函数或更换技术栈。本次原型形成的是**通过**结论。
