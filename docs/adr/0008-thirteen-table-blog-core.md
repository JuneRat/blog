# ADR-0008：采用用户提供的 13 表博客核心

- 状态：部分被替代
- 记录日期：2026-09-21
- 关联决策：替代 [ADR-0007](0007-simple-separated-content-schema.md)；密码与会话分别由 [ADR-0009](0009-local-password-authentication.md)、[ADR-0010](0010-persistent-postgres-sessions.md) 局部更新；内容提交及渲染进一步见 [ADR-0014](0014-content-commits-and-stable-admin-identity.md)、[ADR-0015](0015-rendered-content-runtime-and-module-boundaries.md)；[ADR-0016](0016-confirmed-blog-schema.md) 更新内容关系、生命周期、身份及表结构
- 当前参考：[数据库设计](../database-design.md)、[内容生命周期](../content-lifecycle.md)、[身份与后台](../identity-and-admin.md)、[路线图](../product-roadmap.md)

## 背景

用户提供包含 Series、RBAC、OAuth 的 13 表设计，并明确要求“用这个设计”。因此以该表结构和业务字段为基线，替代此前的 14 表建议；字段长度、状态取值、并发和登录运行时策略另行明确。

“13 表”是本次决策时的范围，不是此后禁止扩表的永久上限。以下记录初始选择，后续变化单列于文末。

## 决策

核心表为 `users`、`oauth_accounts`、`roles`、`permissions`、`user_roles`、`role_permissions`、`categories`、`series`、`posts`、`tags`、`post_tags`、`pages`、`settings`。

- 文章单分类、单系列、多标签；分类支持父节点，系列顺序存 `posts.series_order`。Page 无作者及分类、系列、标签。配置分组存入 settings JSONB。
- 当时不建内容路径、修订、媒体、会话、OAuth token、邀请、审计、元数据或任务表。`version` 用于并发控制，不代表历史版本。正文原位修改，已发布内容保存即更新线上，取消旧方案的工作副本与发布修订隔离。
- slug 在草稿创建时即唯一，首次发布后锁定，永久删除释放地址。文章使用 `/posts/{slug}`，Page 使用 `/{slug}` 并拒绝系统保留路由；不建历史重定向或删除占位。Post 软删除，Page 物理删除。
- RBAC 使用权限目录和两张关系表，权限语义由可信 key 描述符定义；作者归属、角色委派和系统 Owner 仍由应用校验。OAuth 身份唯一键为固定提供商实例加稳定用户 ID；当时 `password_hash` 仅预留，不开放本地登录。
- 当时本站会话与 OAuth 尝试使用单实例有界内存，重启失效；账号由 CLI 预建并绑定。邀请、持久会话、事务审计、媒体和可靠任务随对应功能增加存储，不借 settings 隐藏额外模型。历史修订和 URL 改名不在本次范围内。

## 后果与限制

单份正文不能在保留线上旧文的同时保存新稿，也不能逐篇恢复历史；硬删除后的 URL 不保留。当时的内存会话意味着重启后需要重新登录。接受这些限制以减少初始表和状态维护。

作出本次决策时，工作区尚无旧 schema 实现，因此变更针对文档与 DDL，不涉及已有生产数据迁移。

## 后续变更

- [ADR-0009](0009-local-password-authentication.md) 启用已有 `password_hash` 字段作为本地登录凭据，补齐哈希、限流、受控重置及凭据并发规则，替代“密码仅预留”的选择；本次密码决策未新增表。
- 媒体功能后来增加 `media_assets` 和 `content_media_refs`，落实本篇“随功能增加存储”的原则。
- [ADR-0010](0010-persistent-postgres-sessions.md) 增加 `sessions`。它在当时成为第 16 张业务表，替代本篇关于本站会话不建表、重启失效的选择；OAuth 尝试和密码限流仍在进程内存。
- [ADR-0014](0014-content-commits-and-stable-admin-identity.md) 明确内容提交与管理端 UUID 身份；[ADR-0015](0015-rendered-content-runtime-and-module-boundaries.md) 明确持久化 HTML 及渲染执行边界，均未引入另一份待发布正文。
- 原生评论后来增加三张表，当前总表数及全部迁移以 [数据库实现参考](../database-current.md) 为准。初始表清单不能用作删除或否定后续存储的依据。
- [ADR-0016](0016-confirmed-blog-schema.md) 采纳新的 19 表设计，替代单系列、Page 物理删除及原发布/目录删除规则，加入事务审计并重整身份、媒体和评论模型。该设计已确认，应用尚未切换。

单份当前正文、Post/Page 分表和公开 slug 的基本取舍仍保留；完整现行规则以页首专项文档为准。
