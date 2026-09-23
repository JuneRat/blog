# 架构文档

本目录记录 Rust 博客的设计基线，当前采用用户确认的 **13 张核心表**：少表、关系清晰、按功能扩展。M1 内容闭环、M2 身份/后台与本地密码认证已实现，其余能力按 [功能路线](product-roadmap.md) 推进。

- [数据库设计](database-design.md)：用户、OAuth、RBAC、分类、Series、文章、标签、独立页面和 settings 的字段及关系。
- [PostgreSQL DDL](sql/postgres-core.sql)：13 表建表草案、主外键、唯一约束、并发 version 和查询索引。
- [架构设计](architecture.md)：五个 crate、依赖方向、端口、主题与运行边界。
- [内容生命周期](content-lifecycle.md)：单份正文的编辑/发布、分类树、系列顺序、可见性、删除与 URL。
- [Domain 设计](domain.md)：业务模块、聚合、可见性与演进条件。
- [身份、RBAC 与后台](identity-and-admin.md)：权限 key、作者归属、角色委派、OAuth、本地密码（Argon2id/限流/重置）和单实例内存会话。
- [主题与渲染](themes-and-rendering.md)：MiniJinja、受控数据函数、异步桥接与后续缓存。
- [扩展与数据](extensions-and-data.md)：后续受限插件、外部搜索/统计、可靠事件与多数据库适配。
- [备份与恢复](operations-and-recovery.md)：核心数据库及资源清单、维护备份、隔离恢复与扩展任务核对。
- [功能路线](product-roadmap.md)：唯一的阶段清单与当前/后续功能边界。
- [ADR 索引](adr/README.md)：关键选择、来源、替代关系与代价。

当前核心表：

```text
users             oauth_accounts
roles             permissions       user_roles       role_permissions
categories        series            posts
tags              post_tags         pages
settings
```

Post/Page 分表，文章单分类、单系列、多标签，分类可分层。Page 无作者及分类/标签/系列，使用根路径并保留系统路由。配置按 settings.key 分组存 JSONB。

当前不建修订、路径、会话或审计表：已发布内容保存直接更新线上；无历史恢复或旧 URL 跳转。首版会话、OAuth 尝试与本地密码失败限流放单实例有界内存，重启失效；用户先由 CLI 显式创建绑定。媒体库第一版（Post/Page 正文图片）已交付 `media_assets` 与 `content_media_refs` 两张表；邀请、持久会话、事务审计和可靠任务需要时再补存储。

技术方向保持 Rust、PostgreSQL、MiniJinja SSR、React + TypeScript + Vite、通用 OIDC + GitHub。用户可以持有多个角色，作者可直接发布自己的文章，后端校验权限与真实资源归属。

当前仓库已实现五个 crate 的依赖装配、15 表迁移（13 张核心 + 媒体 2 张）与真实 PostgreSQL 集成测试；DDL 在本地/CI 的 PostgreSQL 18 上执行。文档描述的是采用的设计基线，未实现的能力（邀请、审计、媒体扩展、多实例等）以 [功能路线](product-roadmap.md) 为准，不因文档存在而视为可用。

## 维护约定

当前方案以数据库设计、专项文档及最新 ADR 为准。[ADR-0008](adr/0008-thirteen-table-blog-core.md) 替代此前 14 表及共享内容方案；旧 ADR 保留历史，不作为当前开发要求。

- 已确认：用户明确选择的功能、技术或 schema，不重复请求确认。
- 工程建议：本次为落地补充的长度、状态、事务、权限和运行时约定。
- 待验证：需真实数据库、原型或集成检查支持的方案。
- 已实现：标记以代码与验收为准。当前已实现 M1 内容闭环、M2 身份/后台（含 OAuth 与会话）以及本地密码认证（[ADR-0009](adr/0009-local-password-authentication.md)）；其余仍按“已确认/工程建议/待验证”区分。

后续修改涉及数据模型、生命周期或功能范围时，同步相关文档和路线图；重大取舍增加 ADR，不仅修改 SQL 留下相互冲突的规格。
