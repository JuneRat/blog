# 身份、RBAC 与 SPA 后台

状态：采用 [13 表设计](database-design.md)；会话持久化后又新增 `sessions` 一张表（[ADR-0010](adr/0010-persistent-postgres-sessions.md)），当前共 16 表。单站点、多作者、内置/自定义角色、OAuth/OIDC 与 React 后台保留；数据库仍不建 invitations、oauth_states 或 audit_logs。管理员邀请与持久审计转为后续扩展，当前先支持 CLI 预建并绑定的用户登录。

## 1. 前后台结构

公开站点使用 MiniJinja SSR；后台 React + TypeScript + Vite + Ant Design v6（选型见 [ADR-0011](adr/0011-admin-ui-library.md)）通过版本化管理 API 调用用例。推荐同源部署：/admin/* 提供 SPA（`apps/admin`，构建产物不进备份），/api/admin/v1/* 提供 JSON API，/auth/* 提供认证入口。SPA fallback 只注册在 /admin 子树内，不覆盖 API、认证和公开页面。登录页渲染用的 `GET /auth/providers` 是公开只读端点，只返回提供商的 id/展示名/类型。

管理响应和预览使用 Cache-Control: no-store，前端路由守卫只改善体验，权限由后端执行。OpenAPI 维护接口契约，错误响应为 `{"error", "code", "request_id"}`：`code` 是业务码，同一状态码的不同原因必须可区分（409 的 slug 占用是 `conflict`，版本冲突是 `version_conflict`，用户名/邮箱占用分别是 `username_taken`/`email_taken`；403 的最后可登录 Owner 保护是 `last_owner`），客户端不得只按状态码分支；`request_id` 为每请求 UUIDv7，与 `x-request-id` 响应头一致，客户端报障文案需展示它。编辑携带 expected_version，分页限制上限，批量命令逐项授权。

## 2. RBAC 与权限目录

存储使用 users → user_roles → roles → role_permissions → permissions。角色可自定义，权限为允许集合并集，不增加用户直授权、继承、deny 优先级或任意策略脚本。

permissions.key 使用 resource.action。application 各模块声明可信权限描述符，server 装配，迁移/初始化同步 permissions 目录；domain 只维护 PermissionId/PermissionKey 和角色关系，不枚举全部业务动作。普通后台不能创建任意可执行 key，未知权限拒绝授权。

原来的数据库 action/scope 列已移除；资源范围由权限 key 的可信描述符定义，不由请求参数决定：

| 权限 | 范围/规则 |
|---|---|
| post.create | 创建本人文章 |
| post.read / post.update / post.publish / post.delete | 本人文章；delete 为回收站，不是永久删除 |
| post.read_any / post.update_any / post.publish_any / post.delete_any | 对应所有文章动作 |
| post.unpublish | 本人文章；_any 版本覆盖其他文章 |
| post.restore | 回收站恢复沿用 post.delete / post.delete_any 授权，尚无独立 key |
| post.purge / post.transfer_author | 独立敏感权限，默认只授予管理者 |
| page.read / page.create / page.update / page.publish / page.unpublish / page.archive / page.delete | 站点范围，Page 无 author_id；delete 是物理删除 |
| category.manage / tag.manage / series.manage | 分类树、标签与系列管理；跨文章修改仍核验文章授权 |
| user.manage / role.manage | 账号与角色操作，还需满足委派/Owner 限制 |
| settings.manage | 普通站点设置，不能修改受保护 OAuth 配置 |
| oauth.manage / ownership.manage | 提供商配置、所有权操作，要求受保护身份及重新认证 |

这些是建议种子权限，仅随用例注册。Author 默认获得文章 own 动作，Editor 获得内容 any 及所需 Page 管理权限；Administrator 管理普通身份和设置，Owner 另有所有权和恢复能力，Analyst 的 analytics.read 随统计功能加入。角色名称不替代动作检查，any 覆盖 own 的关系在注册表明确声明。已注册并交付：`media.read`/`media.upload`/`media.delete`/`media.delete_any`（媒体库第一版：Owner 全持、Editor 持 read/upload/delete_any、Author 持 read/upload/delete；公开引用的图片匿名即可读，无需 media.read）；`tag.manage`（Owner 与 Editor 内置持有；标签**目录读取**对全部已认证会话开放——Author 编辑文章要选标签，但无目录管理权；文章与标签的关联仍按 post.update/post.update_any 核验归属）。

公开阅读不要求后台角色，但只返回 public、published、未软删除内容。私有文章和草稿必须校验用户及 post.read/post.read_any；页面按 page.read。接口只能传递可信 Actor，不能相信前端提交的 author_id 或权限列表。

## 3. 授权变更与 Owner

roles.slug 是稳定角色标识；内置 slug 由初始化种子保留并禁止普通 API 创建、改名、编辑或删除。Owner 通过受保护的 owner 角色分配识别，不从显示名称或“拥有全部权限”推导。自定义角色不能借用该身份。

委派上限由可信代码/部署策略限制，拥有业务权限不自动允许授予它。角色编辑前、后的完整集合、角色分配/移除及受影响用户都要检查，不能通过修改自己持有的角色提权；role.manage 也不能绕过此检查。修改授权集合递增 roles.version，修改用户状态/身份/角色分配递增 users.version。

初期不缓存有效权限。推荐单站点事务协议：敏感业务写入先取得共享 transaction advisory lock，身份/角色/Owner 变更取得同一键的排他锁，例如 (2048001, 1)。使用 READ COMMITTED，锁取得后的下一条语句重新读账号和权限；排他用例直接取得排他锁，不从共享锁升级。全部 HTTP、CLI 和后台入口遵守同一顺序。

所有可能减少有效 Owner 的操作在排他锁下检查至少保留一个未删除且有有效登录方式的 Owner。所有权转移要求专门权限和重新认证，同事务变更；普通角色分配不能授予 Owner。provider 禁用也要检查是否使最后 Owner 失去登录方式。

授权与业务写入在锁内完成；协议网络请求在事务外执行，返回后重新授权和校验版本。行外约束和授权规则不能仅靠数据库 FK 保证。

## 4. OAuth / OIDC

首期提供商仍为通用 OIDC + GitHub；贴文中的 Google、Apple 是模型可以容纳的身份示例，不代表已经实现额外适配器。OIDC 的 provider 绑定精确 issuer，provider_user_id 为 sub；GitHub 使用固定平台实例和稳定用户 ID，不用 login 或邮箱。

提供商非敏感元数据放 settings 的 oauth 分组，秘密由部署 secret_ref 提供。首次 CLI 可写入配置；后台修改使用 oauth.manage 与重新认证，不受普通 settings.manage 覆盖。更改身份命名空间不能静默重新解释已有 oauth_accounts；需要显式迁移和绑定核对。

采用后端 Authorization Code + PKCE S256，OIDC 同时使用 nonce：

1. 生成 state、PKCE verifier、浏览器绑定，OIDC 再生成 nonce；放短期服务端尝试存储并限制 TTL/容量。
2. 回调核对浏览器、state、提供商、精确 redirect URI 和期限，原子一次消费尝试。
3. 在事务外交换 code；OIDC 校验签名、issuer、audience/必要 azp、有效期和 nonce；GitHub 通过受信用户接口读取稳定 ID。
4. 重新检查账号、当前提供商配置和尝试有效期，再按 (provider, provider_user_id) 找绑定并签发本站会话。
5. 仅跳转到允许的本站相对路径；错误尝试重新发起，不重用 state。

OAuth 是授权协议；仅 OAuth 平台必须通过受信身份接口适配，不能把任意 token 当作身份证明。安全要求参考 [OIDC Core](https://openid.net/specs/openid-connect-core-1_0.html) 和 [OAuth 安全最佳实践](https://www.rfc-editor.org/info/rfc9700/)。

相同邮箱不自动合并用户。oauth_accounts.email 只是外部资料快照；用户主邮箱唯一冲突时显式处理，不将碰撞账号绑定给登录者。绑定外部身份要求有效会话、近期重新认证和目标身份验证；重新认证必须回到同一用户。解绑不能去掉最后一种有效登录方式。

不持久化第三方 access/refresh token，不将它们、client secret 或 PKCE verifier 返回 SPA/localStorage。`password_hash` 存 Argon2id 的 PHC 字符串，OAuth-only 用户为空；本地密码登录、限流与重置契约见 §7。

## 5. 会话、初始化与账号开通

本站会话已持久化到 PostgreSQL 的 `sessions` 表（[ADR-0010](adr/0010-persistent-postgres-sessions.md)）：重启后仍登录，服务与运维进程（或多个实例）共享同一份会话，因此按用户批量撤销本身即可跨进程即时生效。一次性 OAuth 尝试仍是进程内有 TTL 和容量上限的存储，重启即作废——这是一次性 state 的有意行为。会话与尝试都通过 application 端口隔离，数据库实现只是其中一个适配器。

浏览器持有高熵不透明 Cookie，HttpOnly、Secure、合适的 SameSite；服务端只保存令牌的 SHA-256 摘要（不存明文），支持空闲/绝对过期、退出撤销与重新认证时间。会话表存 `user_id`、CSRF token、签发时版本与创建/最后活跃/绝对过期时间，并为按用户撤销与过期清理建索引。回调策略须与 SameSite 兼容。写请求校验 CSRF token 和 Origin；退出也是受保护写操作。

账号软删除或解绑/离线恢复时清除相关会话与尝试；恢复账号不恢复旧会话。**设置、重置、清除密码或自助改密同样撤销该用户全部会话**，旧 Cookie 一律失效。每次敏感操作重新读用户与权限，因此旧 Cookie 不可绕过撤权。重启不再清空会话；从备份恢复会带回旧会话行，恢复流程必须显式撤销（见 [备份与恢复 §3](operations-and-recovery.md)）。

会话还绑定**签发时的身份修订号**（`users.version`）：服务端校验会话时重读账号并比对版本，不一致即判未登录。这让**另一个进程**发起的变更（运维在 shell 里跑 `blog user passwd`、改角色、软删除）同样立刻生效——既靠共享存储里的真实撤销，也靠数据库里的版本号兜底。代价是任何递增 `users.version` 的动作都会登出该用户，这正是“撤权即时生效”想要的效果，但改角色会让本人当前会话失效，需知其然。

首个 Owner 由受控 CLI 配置提供商、核对稳定外部身份，并在授权排他锁内创建用户、Owner 分配及 oauth_accounts。已有 Owner 分配则拒绝重复初始化。其他用户也先由受控 CLI 创建并绑定身份；任意首次 OAuth 登录不得自动注册或成为管理员。

管理员邀请是后续能力，交付时必须同时加入一次性令牌/期限、邀请者与角色版本复核、原子账号创建和消费记录；当前不以普通 settings JSON 模拟邀请表。离线恢复需部署权限并记录运维安全日志，不开放永久公开安装入口。

## 6. 后台与验证

后台优先实现文章、页面、分类、标签、系列顺序、角色用户、外部身份绑定与站点设置。**其中「外部身份绑定」尚未交付**：后端只有用户与角色路由，前端也没有绑定/解绑界面，绑定当前仍只能靠受控 CLI（见 §4/§5），不应把它算进已交付能力。没有修订表，所以不展示历史恢复；媒体库覆盖正文图片、Post/Series 封面、用户头像与站点 logo 的上传、引用与保护删除，仍不宣称具备完整附件库（无目录、无版本、无批量整理）。保存已发布内容会直接更新线上，不能标为“保存草稿”。当前后台已交付：文章与页面编辑、标签/分类/系列管理、媒体库、用户与角色管理（见 §8；角色目录只读，分配在用户界面完成）、自助改密与自助头像（`/admin` 右上角，契约见 §7.5）、站点设置屏（`/admin/settings`，site 分组的标题/描述/logo，读写都要求 `settings.manage`，生效优先级与冲突流程见 [数据库设计 §6](database-design.md)；oauth 等受保护分组不在此界面，仍走受控 CLI 与 `oauth.manage`）。

访问日志为每个请求记录 request ID、method/path、结果状态与耗时，actor 只取自服务端验证过的会话（匿名留空），隐藏 Cookie、code、token、密码和秘密；预期 4xx 记 info、5xx 记 warn。它只回答"谁请求了哪个接口、结果如何"，**不构成业务审计**。角色变更、身份绑定等动作仍需记录明确的动作与对象：当前 13 表不承诺事务内持久审计；如需不可遗漏的业务审计，须在该功能交付时补充专用存储和事务实现。

验收覆盖：水平越权、Author 访问他人私有文章、角色编辑/分配提权、Page 站点权限、最后 Owner 并发操作、禁用提供商后的登录方式保护、撤权和写入并发、OAuth 重放/错误 issuer、邮箱碰撞、绑定冲突、跨进程会话读取与撤销、空闲/绝对过期、CSRF 与无权限 API 直接访问。

## 7. 本地密码认证

状态：已实现（[ADR-0009](adr/0009-local-password-authentication.md)）。`users.password_hash` 不再只是预留：密码登录与 OAuth 是对等的登录方式，二者都通过同一会话签发路径进入后台。

### 7.1 凭据存储与参数

- 算法固定 Argon2id（PHC 字符串，哈希自带算法、参数与盐），参数为 OWASP 推荐的最小配置：内存 19456 KiB、迭代 2、并行度 1、输出 32 字节、随机盐 16 字节。同一明文每次哈希不同。
- 存储值参数弱于当前策略时（或算法不是 argon2id），登录成功后用同一明文透明重哈希覆盖，因此将来提高参数不需要重置用户密码。
- 每次哈希约 19 MiB。实现用信号量限制并发哈希数（当前 4 路，约 76 MiB 峰值），超出排队而不是无限占用内存；KDF 在阻塞线程池执行，不占异步执行器。
- 哈希只经独立的凭据读写方法进出数据库，不进入 `UserSnapshot`，避免口令材料随实体在用例间传播。
- 登录与改密并发时互不覆盖：登录触发的升级用**条件写入**（仅当存储值仍是刚校验的那份），并在签发会话前复核哈希未被替换；若期间口令已被改掉，本次登录作废而不是用旧口令建会话。否则一次并发的登录升级就能把刚换掉的口令「复活」。
- 会话绑定签发时的 `users.version`（见 §5）：改密、改角色、软删除之后，连「恰好卡在改密瞬间通过校验」的登录也会因版本不符而作废，不依赖进程内的撤销动作是否先到。

### 7.2 登录接口

`POST /auth/login/password`（JSON，`Cache-Control: no-store`，请求体上限 4 KiB）：

```json
{ "username": "sun", "password": "...", "next": "/admin/" }
```

- 成功 `200`，响应体 `{"user_id", "next"}` 并下发与 OAuth 相同的 `blog_session` cookie。
- 失败 `401` + `code=invalid_credentials`。**用户名不存在、密码错误、账号已停用回同一状态、同一业务码、同一文案**；未知用户也执行一次等价开销的哈希校验，堵住时间侧信道。
- 触发限流 `429` + `code=rate_limited` + `Retry-After: <秒>`。
- 匿名写请求没有可用的 CSRF token，靠同源 `Origin` 校验 + `SameSite=Lax` cookie 防登录 CSRF。

### 7.3 限流与锁定

失败按**账号**与**来源地址**两个维度分别计数，任一维度额度用尽即拒绝，且被拒绝的尝试不进入哈希校验：

| 维度 | 阈值 | 锁定时长 |
|---|---|---|
| 账号（规范化后的用户名） | 15 分钟内 5 次 | 15 分钟 |
| 来源地址（socket 对端） | 15 分钟内 50 次 | 15 分钟 |

- **预占式计数**：额度必须在昂贵的口令校验**之前**占用，出结果后再转成失败或成功。只做事后计数（先读检查、再慢慢校验、最后才记失败）会留下 TOCTOU 窗口——N 个并发请求会在任何一次失败被记录之前**全部**通过检查，一次突发就等于 N 次离线爆破机会。在飞预占与已确认失败共享同一上限，因此并发也不会超发。
- 未得出结论就返回（另一维度已锁定、内部错误、请求取消或超时）必须**归还**预占，否则额度会泄漏成永久锁定。作用域守卫在释放 future 时同步归还，不依赖异步清理任务。
- **容量上限**：内存主体条目最多 10,000 个；新增主体时优先淘汰最早的空闲条目，全部条目均有在飞请求时返回 `rate_limited`（建议 1 秒后重试），不突破上限。已有主体仍按各自额度判定。
- **重新认证与登录共用同一份失败预算**：`POST /api/admin/v1/me/password` 的「当前密码」校验同样先预占再校验，被盗会话无法无限次试当前密码。
- 锁定期间拒绝但**不延长锁定**：否则攻击者用已知用户名反复尝试就能把真实用户永久锁在门外。锁定期满给一个干净的计数窗口重新尝试，不做逐次指数退避（行为可预期、便于对用户解释）。
- 成功登录清空账号维度的历史失败和锁定，只归还本次预占，保留其他在飞请求；来源地址维度只**归还本次预占**，**不清历史失败**——否则攻击者可用自有账号反复清零 IP 计数，等于关掉该维度。
- 计数存单实例有界内存（有 TTL 与容量上限），重启清空计数但不清空凭据；多实例部署前必须换成共享存储。
- 来源地址只取 socket 对端，**不读 `X-Forwarded-For`**。反向代理后的真实客户端地址需要部署侧显式配置可信转发，属于后续设计。

### 7.4 设置、重置与恢复

- **唯一重置入口是受控 CLI**：`blog user passwd --user <用户名>`。交互终端下隐藏回显并二次确认；`--password-stdin` 从标准输入读取（只去掉一个行尾，空白字符保留）；非终端 stdin 自动按管道读取。**不提供 `--password` 参数**，避免口令进入进程表与 shell 历史。
- 新密码按策略校验后才写入：12–128 个字符、不得包含用户名、拒绝常见/规律口令。重置后该用户全部会话立即撤销。
- `blog user show <用户名>` 显示 `密码登录：已启用/未启用`；`blog user passwd --user X --clear` 关闭密码登录，但当它是该用户最后一种登录方式时拒绝（与解绑外部身份同一条保护），需要改密码请直接设置新值。
- **「是否还有其他登录方式」与清除在同一把身份排他锁、同一事务内完成**（与解绑外部身份互斥）。分开做会让两条路径各自看到「对方还在」而同时通过，最终把账号的登录方式清空（write skew），因此这条不变式必须靠锁而不是靠调用顺序。
- 设置/清除需 `user.manage`；受控 CLI 以引导身份执行，不开放为公开管理入口。
- **自助找回未交付**：邮箱找回需要一次性令牌存储、TTL 与原子消费、邮件投递和防枚举，不能塞进 settings 冒充。忘记密码只能由有 shell/部署权限的运维重置；这是当前明确的功能缺口，不是可以靠前端补上的部分。

### 7.5 自助改密与重新认证

`POST /api/admin/v1/me/password`（需会话 + `X-CSRF-Token` + 同源 `Origin`）：

```json
{ "current_password": "...", "new_password": "..." }
```

- 已启用密码登录时必须提供当前密码（重新认证）；未启用密码的 OAuth 用户可凭会话直接设置初始密码。
- 重新认证走与登录相同的限流（§7.3）：超出失败预算直接 `429`，不再做哈希校验。
- **写入是条件写入（compare-and-swap）**：只在凭据仍等于刚校验过的那一份时才替换。期间若管理员下发了强制重置（或别人改了密码），本次自助改密作废并返回 `409 version_conflict`，**不会**把管理员的新口令覆盖掉——否则泄露处置的强制重置会被一次并发的自助改密静默撤销。OAuth 用户设置初始密码同理，期望值为「当前必须为空」。 条件写入通过同一条 `UPDATE … RETURNING version` 返回本次产生的版本，新会话只绑定此版本；不重新读取并借用后续重置的版本，因此写入后再发生的管理员重置仍会让本次会话失效。
- 成功后轮换会话：撤销该用户全部会话，再签发新会话下发给当前浏览器，响应体回新的 `csrf_token`。
- 当前密码不正确返回 `403` + `code=invalid_credentials`（不是 401）：用户并未掉线，前端不应清空登录态。
- 前端入口是 `/admin` 右上角「修改密码」（`apps/admin/src/components/PasswordChangeModal.tsx`）：成功即用响应里的新 `csrf_token` 覆盖内存 token（会话已轮换，旧 token 立即失效），失败按 code 分支提示，`403` 明确告知「仍然处于登录状态」。`Me` 不含 `password_enabled`，所以「当前密码」在界面上可留空，由服务端判定是重新认证还是首次设置密码——前端不复制口令策略。

**自助头像**（同一类「本人对自己」的写操作）：`PUT /api/admin/v1/me/avatar`，体为 `{ "avatar_media_id": "<uuid>" | null }`（null = 清除），需会话 + `X-CSRF-Token` + 同源 `Origin`，**不需要额外权限**。它与改密的关键差别是**不撤销会话、也不递增 `users.version`**：`users.version` 是会话绑定版本，换头像不该把本人所有会话踢下线。头像的媒体引用在仓储同一事务内整体替换，资产必须存在且 `ready`；`GET /api/admin/v1/me` 返回 `avatar_media_id` / `avatar_url` 供界面显示。公开可读性按 [内容生命周期 §5](content-lifecycle.md)：账号未软删除时匿名可读，软删除后立即失效。

### 7.6 泄露处置

口令哈希泄露时按 [备份与恢复 §6](operations-and-recovery.md) 处置：轮换凭据（受控 CLI 下发新口令）、撤销全部会话、核对来源地址限流日志与账号动作、必要时临时禁用密码登录（`--clear`，前提是该账号仍有外部身份）。因为哈希是内存硬的，泄露本身不等于明文泄露；处理目标是尽快让**旧口令与旧会话同时失效**。

### 7.7 稳定业务码新增

本节新增两个发布后即视为契约的码，客户端按 `code` 分支而不是状态码：

| code | 默认状态 | 含义 |
|---|---|---|
| `invalid_credentials` | 401（自助改密的重新认证失败用 403） | 凭据无效，不区分具体原因 |
| `rate_limited` | 429（带 `Retry-After`） | 登录失败次数超阈值，临时锁定 |

它们与既有码并列，改动需同步 [接口契约](#1-前后台结构)、前端与穷举映射测试。

## 8. 用户与角色管理界面

状态：已实现。用例复用 §2/§3 的账号与角色操作，接口层只做传输映射，不重复实现权限判断。

### 8.1 接口

全部挂在 `/api/admin/v1` 下，响应 `Cache-Control: no-store`；写方法需会话 + `X-CSRF-Token` + 同源 `Origin`：

| 方法 | 路径 | 所需权限 | 说明 |
|---|---|---|---|
| GET | `/users` | `user.manage` 或 `role.manage` | 账号列表：用户名、邮箱、展示名、角色、登录方式；`limit` 缺省 50、上限 200 |
| POST | `/users` | `user.manage` | 创建账号（不自动分配角色） |
| GET | `/roles` | `role.manage` 或 `user.manage` | 角色目录：slug、名称、内置标记、权限数 |
| PUT | `/users/{username}/roles/{role}` | `role.manage` | 幂等分配；授予 Owner 另需 `ownership.manage`；不得超出调用者权限集合 |
| DELETE | `/users/{username}/roles/{role}` | `role.manage` | 移除；移除 Owner 另需 `ownership.manage`；最后一个可登录 Owner 受保护 |

列表一次批量读取整页角色（不是逐账号查询），并用一次**全局**计数给出每行的 `is_last_loginable_owner`；只回账号字段与登录方式（`can_login`），绝不回 `password_hash`。

### 8.2 结构化冲突码

`UseCaseError::Conflict(ConflictKind)` 取代裸字符串：适配器把数据库约束名翻译成稳定枚举，接口层再映射业务码。同一个 409 下：

| code | 结构化原因 | 界面消费者 |
|---|---|---|
| `username_taken` | `ConflictKind::Username` | 创建账号表单定位到用户名 |
| `email_taken` | `ConflictKind::Email` | 创建账号表单定位到邮箱 |
| `conflict` | slug 等其余唯一冲突 | 沿用既有通用码 |

只有已有界面消费者的字段才分配专属业务码；新增字段必须同时更新 `unique_conflict_target`、`admin_error_code` 的穷尽映射与穷举测试，否则编译或测试失败。

### 8.3 保护与撤权

- **最后一个可登录 Owner**：列表的 `can_login` 与存储的 `active_owner_count` 使用同一谓词（未软删除，且至少一条外部身份或已启用本地密码），两处定义不得漂移。移除会减少有效 Owner 时在身份排他锁下拒绝，返回 **403 `last_owner`**——与笼统的 `forbidden` 分开，界面据此解释原因而不是显示“无权操作”。**「是否最后一个」必须由后端按全站计数返回**（`is_last_loginable_owner`）：列表分页上限 200，另一个可登录 Owner 可能在后续页，前端按当前页推断会误标并错误禁用移除。界面据此提前禁用只是提示，真正的边界始终在后端。
- **角色目录**：`GET /roles` 失败时必须与「目录为空」区分展示并可重试；把一次网络故障显示成“暂无可分配角色”会静默阻断角色分配。
- **撤权后会话失效**：角色分配/移除递增目标用户 `users.version`，其旧 cookie 下一次请求即判未登录（§5）。重复分配同一角色是幂等的，不递增版本、不登出。若操作目标是本人，界面在成功后重新读 `/me`，直接进入登录态而不是继续显示已失效会话。
- **委派上限与所有权**：接口层不重复判断，`RoleInteractor` 在用例层执行——不能授予自己不具备的权限；授予/移除 Owner 需要专门权限。

## 9. 媒体库（第一版：Post/Page 正文图片）

媒体库是站点级共享资源：任何作者都能引用任意已上传的图片，但删除他人上传需要显式 `media.delete_any`。业务规则（状态机、公开访问边界、引用保护与并发协议）见 [内容生命周期 §5](content-lifecycle.md)。

### 9.1 接口

| 方法 | 路径 | 所需权限 | 说明 |
|---|---|---|---|
| GET | `/media?page=N` | `media.read` | 媒体库按上传时间倒序分页（每页 24）：文件名、大小、尺寸、上传者、引用计数与公开引用计数 |
| POST | `/media?filename=…` | `media.upload` | 上传；**请求体就是图片字节**（不是 multipart），`filename` 只提供展示名 |
| GET | `/media/{id}` | `media.read` | 资产详情 + **有权查看的**使用位置（含每条引用是否公开可读）与 `hidden_references` |
| DELETE | `/media/{id}` | `media.delete`（本人上传）或 `media.delete_any` | 请求体需 `expected_version`；成功 204 |
| GET | `/media/{id}`（无 `/api` 前缀） | 匿名或 `media.read` | **公开读取**：匿名只在存在公开来源引用时返回文件；否则 404 |

错误语义：仍被任何内容引用（含草稿/私密/回收站）时 409 `media_in_use`，界面据此展示使用位置；版本过期 409 `version_conflict`；格式/尺寸/大小不合法 400 `invalid_request`。

### 9.2 权限与角色

| 权限 | Owner | Editor | Author | Admin |
|---|---|---|---|---|
| media.read | ✓ | ✓ | ✓ | |
| media.upload | ✓ | ✓ | ✓ | |
| media.delete | ✓ | | ✓ | |
| media.delete_any | ✓ | ✓ | | |

Administrator 角色面向账号与设置，不含媒体权限（与 `page.*` 的分配不同：媒体是内容链路的一环）。匿名读取公开引用的图片**不要求任何权限**。

### 9.3 浏览与阅读权限的区别

`media.read` 只授予「浏览媒体库」：可以看资产元数据与引用计数，但**使用位置按内容权限过滤**——Post 走 own/any，Page 走站点级 `page.read`，此外**公开可读的内容直接可见**（它的标题与 slug 本来就能匿名访问，不构成泄露）。媒体库是共享资源，任何人上传的图片都可能被别人的草稿或私密内容引用，因此不能靠 `media.read` 顺带给出他人内容的标题与 slug。被过滤掉的条数以 `hidden_references` 返回（引用计数是全局的，差额必须能解释）。详见 [内容生命周期 §5.3](content-lifecycle.md)。

### 9.4 缓存与传输边界

- 上传上限由 `DefaultBodyLimit` 在解析前拦住（12 MiB 请求体上限，图片本身 ≤10 MiB）；其余管理 JSON 端点仍是 2 MiB。
- 公开引用的响应是 `Cache-Control: no-cache` + ETag（可条件请求 304，但每次必须回源校验，撤回后立即失效）；后台预览是 `no-store`。媒体**绝不**使用长 max-age。
- 格式由文件内容嗅探判定，扩展名与请求 `Content-Type` 都不参与——这也是不引入 multipart 解析依赖的原因之一：按声明类型放行的路径根本不存在。
