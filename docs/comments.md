# 原生评论

评论已适配[新数据库设计](database-design.md#5-评论)：受限 Markdown 与清洗 HTML 一起持久化，多级回复以 `parent_id/root_id` 保存，公开界面按两级展示。default 与 paper 主题共享 Rust 提供的评论脚本和样式；需要启用 JavaScript。

## 领域模型与分层

[domain::comment::Comment](../crates/domain/src/comment.rs) 是单条评论聚合，与 `content` 并列。它持有身份、作者快照、源文、父/根 ID、审核状态和版本，负责按策略确定新评论状态、回复关系校验、审核转换及版本前提；文章和其他评论只通过 ID 关联，不把整棵讨论树装入聚合。创建后源文、作者与关系不可通过聚合修改。数据库重建入口校验快照但不重新规范化旧正文；读取的快照是副本，不能修改聚合内部状态。

[应用用例](../crates/application/src/comments.rs) 负责输入校验、可信账号身份、写入渠道和权限范围。`CommentRepository` 是业务提交端口：适配器在一致的文章开关、账号和父/根事实下调用领域行为，继续在同一事务提交评论、版本及审计。PostgreSQL 的具体锁与 SQL 留在[基础设施](../crates/infrastructure/src/comments.rs)；这些事务语义要求适用于其他适配器。HTML、来源 IP、时间戳和审计记录由基础设施维护。

后台查询返回 `CommentDto`，其 `status` 使用领域 `CommentStatus`；HTTP 边界映射成既有字符串字段。公开查询使用不含私密信息的 `PublicComment` 投影，不重建写聚合。查询 DTO 与领域聚合分别演进，HTTP 格式不因内部改名而改变。

## 输入与展示

游客填写 1–64 字昵称、1–2,000 字正文和可选邮箱；服务端按 Unicode 字符计数。登录用户使用服务端账号名称快照，去除控制字符并截取前 64 字，忽略客户端昵称和邮箱；后台回复可省略这两个字段，游客省略昵称仍会被拒绝。只有文章作者的登录账号获得独立「作者」徽标。

正文支持分段、换行、粗体、斜体、删除线、行内/围栏代码、引用、列表、HTTP(S) 链接、网址自动链接与 Unicode Emoji。不支持标题样式、表格和媒体嵌入；原始 HTML 按文字转义。链接只允许 HTTP(S)，附加 `nofollow ugc noopener noreferrer`。前台和后台均提供精简工具栏与服务端预览，预览与入库共用同一渲染器。

公开响应只有服务端清洗的 `content_html`，不返回 Markdown 源文、邮箱或 IP。昵称、错误和占位文案使用文本节点。后台在授权范围内可读取源文、邮箱及提交来源 IP；它们不进入审计摘要。

新评论和后台回复统一按全站审核策略处理。待审返回 202、`status: pending` 和「已提交，等待审核」；直接发布返回 201、`status: approved` 和「评论已发布」，前台随后重新读取列表。同一页面内翻页、重开回复、网络失败和会话失效会保留未提交的昵称、邮箱与正文；成功后清空正文，刷新或离开页面不保存草稿。

评论提交使用已有的公共请求限流，超过额度返回 429。没有提交去重或 `request_id`、`client_hash` 字段，每次获准提交的有效请求独立创建记录；网络结果不明确时由用户决定是否重试，可能产生重复评论。

## 审核策略

后台「设置 → 账号与评论」集中管理全站评论开关、审核策略和游客评论权限；单篇开关仍在文章编辑器中。`settings.comments.moderation` 支持：

| 策略 | 值 | 新提交的处理 |
|---|---|---|
| 全部审核（默认） | `all` | 所有账号和游客均待审 |
| 仅游客审核 | `guests` | 游客待审，登录账号直接发布 |
| 首次评论审核 | `first_comment` | 账号尚无有效人工通过记录时待审；已有记录则直接发布，游客始终待审 |
| 无需审核 | `none` | 所有获准提交的评论直接发布 |

首次评论审核在全站按登录账号的 `user_id` 判断，只认可仍为 approved 且 `moderation_reason = manual_approval` 的评论。游客自行填写的邮箱、昵称，以及此前自动发布的评论不构成审核记录。最后一条有效人工通过记录被退回、标记垃圾、移入回收站或随文章永久删除后，后续评论再次待审。管理员和作者回复也适用当前策略。

策略只影响新提交，不自动处理已有待审评论。后台展示当前待审原因：全站要求审核（`all_comments`）、游客（`guest`）、账号首次审核（`first_comment`）、人工退回（`manual_review`）、垃圾/回收站恢复（`restored`）。原因随人工状态变更更新，只在后台返回。创建和审核的审计同时记录处理结果与原因。

升级迁移 `0004_comment_moderation.sql` 为已有 approved 评论保留人工通过资格，为已有 pending 评论填充人工待审原因，不修改原状态、业务版本或时间；未配置策略的站点继续全部审核。

## 回复、审核和回收站

根评论的 parent_id/root_id 均为空。回复的 parent_id 指向直接对象，root_id 指向其根；可以继续回复任意深度的已通过评论。服务端验证同篇文章和根节点形态，创建后不可修改关系。根列表和按 root_id 查询的所有后代分别每页 20 条，页码范围为 1–100,000；后代平铺在第二级，显示直接回复对象。

状态为 pending、approved、spam、trash。删除只将本条移入 trash，保留全部子回复。公开查询展示 approved 节点及连接这些节点所必需的祖先占位：trash 显示「该评论已删除」，pending/spam 显示「该评论暂不可用」。占位不含原昵称、作者徽标、源文或 HTML。没有公开后代的隐藏节点不展示；已删除根节点仍有公开后代时可继续查看整条讨论。占位节点不可被直接回复，已通过的后代仍可被回复。

从 spam/trash 恢复必须先回 pending，再单独审核通过。后台不提供单条评论的物理删除；数据库拒绝物理删除仍被父级/根级关系引用的节点。永久删除整篇文章时清理整棵树。

回复对象暂时不可用时保留草稿并暂停提交；通过「刷新评论」或重新加载回复列表确认该节点已恢复后，可继续提交原草稿。这同样适用于嵌套回复，占位节点仍不能被直接回复；讨论恢复可读本身不代表隐藏的根评论可被直接回复。

后台可按状态或文章筛选。`post.update` 管理本人文章评论，`post.update_any` 管理全部；全站设置要求 `settings.manage`。审核使用评论 version，版本冲突返回 409 `version_conflict`。相同状态不递增版本，也不重复记审计；旧版本的重复请求仍返回冲突。

## 开关、可见性与事务

全站 `settings.comments.enabled` 与单篇 `posts.comments_enabled` 默认 true，两者均开启才允许提交。任一开关关闭时，前台隐藏整个评论区域（含历史评论、标题和表单），并停止新增；历史数据保留，重新开启后恢复显示。评论区域初始隐藏，等待同源 API 确认已开启后再显示。没有覆盖值时不预建全站设置行，返回 version 0；实际保存变化后递增设置组版本，并保留同组其他字段。

单篇开关使用文章当前 version，修改后递增 posts.version。后台编辑器使用自身已加载版本保存开关，成功后接收该次更新的新版本并保留未保存正文；它不会用独立设置查询读到的新版本跳过文章冲突。评论提交/审核只改变评论，不递增文章版本。

公开读取、计数、提交都要求文章已发布、公开、未进回收站且发布时间已到，否则 404。所有评论 API 响应禁用缓存。创建、审核及两个开关的更新与审计同事务提交，审计失败整体回滚。提交与开关/策略变更通过事务锁排序，父评论审核与回复写入也互斥；首次评论审核使用的人工通过记录在提交结束前保持有效。游客还须满足 `settings.access.guest_comments_enabled`，无需审核也不会绕过关闭的评论开关或游客权限。

## 请求与来源地址

提交和预览要求 Origin 精确匹配 `BLOG_PUBLIC_BASE_URL`，若有 Sec-Fetch-Site 则必须为 same-origin。带会话 Cookie 的写请求必须通过现有会话与 CSRF 校验，不能失败后自动降级为游客。请求体上限 16 KiB。

会话失效时，公开表单保留正文并通过 `/me` 重新确认身份，等待用户确认后显式重试。只有确定失效的 Cookie 才清除；数据库故障不会清除 Cookie。

在其他标签页登录或切换账号后，可使用「刷新登录状态」重新读取身份和 CSRF token，保留当前草稿。身份校验失败时该入口改为重试；成功刷新不自动重发评论，用户确认显示的身份后再次提交。

默认记录 socket 对端 IP，不信任转发头。`BLOG_TRUSTED_PROXIES` 可配置逗号分隔的精确 IPv4/IPv6 地址；只有 socket 对端在列表中才解析 `X-Forwarded-For`。从右向左剥离可信代理，取第一个非可信地址；无来源、缺失/非法链、超过 20 跳或全为可信地址时保存 NULL。不支持 CIDR，也不读取 `Forwarded`、`X-Real-IP`。例如 `BLOG_TRUSTED_PROXIES=127.0.0.1,::1`。非法配置会使 `serve` 启动失败，登录和自助改密限流也使用此解析结果；未知来源时限流回退 socket 桶，审计/评论 IP 仍留空。

IP 以可空 inet 保存主机地址，审核不覆盖提交 IP。默认保留 180 天，可在后台设置中调整；独立维护账号运行 blog maintenance 分批清空超期 IP，不改编辑版本或更新时间。部署层需另行启用调度，详见[保留期与运维](operations-and-recovery.md)。

## 接口

| 方法与路径 | 参数与行为 |
|---|---|
| `GET /api/v1/posts/{slug}/comments` | `page=1`；`root_id=UUID` 读取某根全部后代；返回 `{items,total,enabled,guest_comments_enabled,time_zone}`，total 含必要占位 |
| `POST /api/v1/posts/{slug}/comments` | `{body,nickname?,email?,parent_id?}`；游客必须提供昵称；202 待审或 201 已发布回执 `{message,status}`；登录身份由会话决定 |
| `POST /api/v1/comments/preview` | `{body}`；返回 `{content_html}`，不写数据库 |
| `GET /api/admin/v1/comments` | `page=1&status=pending&post_id=UUID`；状态和文章筛选可省略 |
| `POST /api/admin/v1/comments/{id}` | `{version,status}`，status 为 pending/approved/spam/trash；204 |
| `GET /api/admin/v1/comment-settings` | 全站策略 `{enabled,moderation,version}`；未配置时 `{enabled:true,moderation:"all",version:0}` |
| `PUT /api/admin/v1/comment-settings` | `{enabled,moderation?,version}`，省略 moderation 保留原策略；返回保存结果 |
| `GET /api/admin/v1/posts/{id}/comment-settings` | `{enabled,version}`，version 是当前文章版本 |
| `PUT /api/admin/v1/posts/{id}/comment-settings` | `{enabled,version}`，返回保存结果；不允许设置审核策略 |

公开节点字段为 id、parent_id、root_id、parent_nickname、nickname、content_html、is_author、placeholder、deleted、created_at。被隐藏的直接父级不提供 parent_nickname。后台节点另含文章信息、body、author_email、ip_address、status、moderation_reason、version；不直接返回数据库整行。

## 重建与验证

评论使用独立的 `COMMENT_RENDER_VERSION`，显式 `blog rebuild-html` 命令重建版本不匹配的 HTML；启动和结构迁移不触发重建。更新同时核对源文和编辑版本，不改变业务版本或修改时间，不写 media_refs。公开渲染不回退到未清洗源文。规则升级步骤见[运维](operations-and-recovery.md#html-显式重建)。

验证入口包括领域聚合测试（创建、关系、审核状态矩阵、快照重建与过期无变化请求）、基础设施评论集成测试（真实 PostgreSQL 关系、权限、分页、CAS、事务回滚、提交与开关/父审核竞争及重建竞争）、评论渲染单元测试、server 的评论 HTTP 测试，以及公开组件、审核界面和文章编辑器 Vitest。恢复工具核验评论根关系、媒体引用和新迁移校验和，见[备份恢复](operations-and-recovery.md)。

公开评论列表响应包含 `time_zone`（站点 IANA 时区）。评论 `created_at` 为 RFC 3339 绝对时刻，默认评论组件按该时区显示；后台评论列表使用 `/me.time_zone`。
