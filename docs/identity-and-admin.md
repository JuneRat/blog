# 身份、RBAC 与 SPA 后台

状态：采用 [13 表设计](database-design.md)。单站点、多作者、内置/自定义角色、OAuth/OIDC 与 React 后台保留；数据库不建 sessions、invitations、oauth_states 或 audit_logs。管理员邀请与持久审计转为后续扩展，当前先支持 CLI 预建并绑定的用户登录。

## 1. 前后台结构

公开站点使用 MiniJinja SSR；后台 React + TypeScript + Vite 通过版本化管理 API 调用用例。推荐同源部署：/admin/* 提供 SPA，/api/admin/v1/* 提供 JSON API，/auth/* 提供认证入口。SPA fallback 不覆盖 API、认证和公开页面。

管理响应和预览使用 Cache-Control: no-store，前端路由守卫只改善体验，权限由后端执行。OpenAPI 维护接口契约，错误提供业务码与 request ID；编辑携带 expected_version，分页限制上限，批量命令逐项授权。

## 2. RBAC 与权限目录

存储使用 users → user_roles → roles → role_permissions → permissions。角色可自定义，权限为允许集合并集，不增加用户直授权、继承、deny 优先级或任意策略脚本。

permissions.key 使用 resource.action。application 各模块声明可信权限描述符，server 装配，迁移/初始化同步 permissions 目录；domain 只维护 PermissionId/PermissionKey 和角色关系，不枚举全部业务动作。普通后台不能创建任意可执行 key，未知权限拒绝授权。

原来的数据库 action/scope 列已移除；资源范围由权限 key 的可信描述符定义，不由请求参数决定：

| 权限 | 范围/规则 |
|---|---|
| post.create | 创建本人文章 |
| post.read / post.update / post.publish / post.delete | 本人文章；delete 为回收站，不是永久删除 |
| post.read_any / post.update_any / post.publish_any / post.delete_any | 对应所有文章动作 |
| post.unpublish / post.archive / post.restore | 本人文章；各自 _any 版本覆盖其他文章 |
| post.purge / post.transfer_author | 独立敏感权限，默认只授予管理者 |
| page.read / page.create / page.update / page.publish / page.unpublish / page.archive / page.delete | 站点范围，Page 无 author_id；delete 是物理删除 |
| category.manage / tag.manage / series.manage | 分类树、标签与系列管理；跨文章修改仍核验文章授权 |
| user.manage / role.manage | 账号与角色操作，还需满足委派/Owner 限制 |
| settings.manage | 普通站点设置，不能修改受保护 OAuth 配置 |
| oauth.manage / ownership.manage | 提供商配置、所有权操作，要求受保护身份及重新认证 |

这些是建议种子权限，仅随用例注册。Author 默认获得文章 own 动作，Editor 获得内容 any 及所需 Page 管理权限；Administrator 管理普通身份和设置，Owner 另有所有权和恢复能力，Analyst 的 analytics.read 随统计功能加入。角色名称不替代动作检查，any 覆盖 own 的关系在注册表明确声明。

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

不持久化第三方 access/refresh token，不将它们、client secret 或 PKCE verifier 返回 SPA/localStorage。password_hash 可空且只预留本地登录，首版不开放密码认证和恢复；将来启用时另行实现密码哈希、限流与恢复契约。

## 5. 会话、初始化与账号开通

首版仅单实例：本站会话及一次性 OAuth 尝试采用有 TTL 和容量上限的服务端内存存储，重启全部失效，用户重新登录。不额外引入 Redis 或数据库辅助表。会话和尝试存储通过 application 端口隔离，未来多实例/持久会话再换适配器；不能让多个实例各存一份仍宣称可互通。

浏览器持有高熵不透明 Cookie，HttpOnly、Secure、合适的 SameSite；服务端保存令牌验证摘要，支持空闲/绝对过期、退出撤销与重新认证时间。回调策略须与 SameSite 兼容。写请求校验 CSRF token 和 Origin；退出也是受保护写操作。

账号软删除或解绑/离线恢复时清除相关内存会话与尝试；恢复账号不恢复旧会话。每次敏感操作重新读用户与权限，因此旧 Cookie 不可绕过撤权。进程重启失效是当前明确行为，不承诺“不登出”。

首个 Owner 由受控 CLI 配置提供商、核对稳定外部身份，并在授权排他锁内创建用户、Owner 分配及 oauth_accounts。已有 Owner 分配则拒绝重复初始化。其他用户也先由受控 CLI 创建并绑定身份；任意首次 OAuth 登录不得自动注册或成为管理员。

管理员邀请是后续能力，交付时必须同时加入一次性令牌/期限、邀请者与角色版本复核、原子账号创建和消费记录；当前不以普通 settings JSON 模拟邀请表。离线恢复需部署权限并记录运维安全日志，不开放永久公开安装入口。

## 6. 后台与验证

后台优先实现文章、页面、分类、标签、系列顺序、角色用户、外部身份绑定与站点设置。没有修订表，所以不展示历史恢复；没有媒体表，所以不宣称具备附件库。保存已发布内容会直接更新线上，不能标为“保存草稿”。

运行日志记录 request ID、actor、动作、结果与脱敏摘要，隐藏 Cookie、code、token、密码和秘密。当前 13 表不承诺事务内持久审计；如需不可遗漏的业务审计，须在该功能交付时补充专用存储和事务实现。

验收覆盖：水平越权、Author 访问他人私有文章、角色编辑/分配提权、Page 站点权限、最后 Owner 并发操作、禁用提供商后的登录方式保护、撤权和写入并发、OAuth 重放/错误 issuer、邮箱碰撞、绑定冲突、重启登录失效、CSRF 与无权限 API 直接访问。
