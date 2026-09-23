# ADR-0008：采用用户提供的 13 表博客核心

记录日期：2026-09-21。

状态：已采纳，待实施。来源：用户提供包含 Series、RBAC、OAuth 的 13 表设计，并明确要求“用这个设计”。表与业务字段以该方案为基线；字段长度、状态取值、并发与登录运行时策略是本文档化的实现建议。替代 [ADR-0007](0007-simple-separated-content-schema.md)，覆盖更早文档中冲突的范围与存储建议。

核心表固定为 users、oauth_accounts、roles、permissions、user_roles、role_permissions、categories、series、posts、tags、post_tags、pages、settings。文章单分类、单系列、多标签；分类支持父节点，系列顺序存 posts.series_order；Page 无作者及分类/系列/标签。配置分组存 settings JSONB。

不建内容路径、修订、媒体、会话、OAuth token、邀请、审计、元数据或任务表。补充的 version 用于并发控制，不代表存在历史版本。正文原位修改，已发布内容保存即更新线上，取消旧方案的工作副本/发布修订隔离承诺。

Slug 在草稿创建时即唯一；首次发布后锁定，永久删除释放地址。文章使用 /posts/{slug}，Page 使用 /{slug} 并拒绝系统保留路由。不建立历史重定向和删除占位。Post 有软删除，Page 按提供字段直接物理删除。

RBAC 使用权限目录与两张关系表，权限语义由可信 key 描述符定义；作者归属、角色委派及系统 Owner 仍由应用校验。OAuth 身份唯一键为固定提供商实例加稳定用户 ID；password_hash 仅预留，不自动开放本地登录。

首版单实例会话与 OAuth 尝试使用有界内存存储，重启失效；账号由 CLI 预建绑定。邀请、持久会话、事务审计、媒体和可靠任务随相应功能增加存储，不塞进 settings 来隐藏额外模型。历史修订和 URL 改名属于后续能力，未承诺本次实现。

代价：当前不能在保留线上旧文的同时保存新稿，不能逐篇恢复历史，也不保留硬删除后的 URL；重启会登出。接受这些明确限制，以减少当前表和状态维护。工作区尚无旧 schema 实现，因此本次只替换文档/DDL，不编造生产数据迁移。

完整字段见 [数据库设计](../database-design.md)，SQL 见 [核心 DDL](../sql/postgres-core.sql)，阶段见 [路线图](../product-roadmap.md)。

后续变更：本文第 13 行「password_hash 仅预留，不自动开放本地登录」已被 [ADR-0009](0009-local-password-authentication.md) 取代——本地密码认证已启用，仍不新增表，限流与会话一样使用单实例内存存储。本文其余决策不变。

后续变更：上文「不建……会话……表」与「首版单实例会话……重启失效」中关于**会话**的部分已由 [ADR-0010](0010-persistent-postgres-sessions.md) 取代——会话持久化到 PostgreSQL 的 `sessions` 表（第 16 张），重启后仍登录。OAuth 尝试与登录失败限流仍为单实例内存。本文其余决策不变。
