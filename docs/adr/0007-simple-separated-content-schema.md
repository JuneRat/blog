# ADR-0007：精简核心表与 Post/Page 分表

- 状态：已被替代
- 记录日期：2026-09-21
- 关联决策：替代 [ADR-0006](0006-relational-content-storage.md)；由 [ADR-0008](0008-thirteen-table-blog-core.md) 替代
- 当前参考：[数据库设计](../database-design.md)、[内容生命周期](../content-lifecycle.md)、[身份与后台](../identity-and-admin.md)

## 背景

此前方案将当时 M1–M4 规划中的身份、内容、媒体、配置和队列模型集中展开，辅助表过多。用户要求精简 DDL，且 Post/Page 共用存储不符合明确的分表要求。以下是当时的 14 表建议，不代表当前表清单。

## 决策

推荐核心为 14 表：`users`、`roles`、`user_roles`、`role_permissions`；`oauth_accounts`、`sessions`、`invitations`、`oauth_states`；`posts`、`post_revisions`、`pages`、`page_revisions`；`content_paths`、`audit_logs`。

工作副本字段并入内容主表，派生 HTML 并入各自修订表。路径统一登记，通过互斥真实外键引用两种内容；数据库约束保护路径唯一和发布修订归属。

权限目录及委派限制使用可信代码、部署策略，不持久化镜像目录。OAuth 提供商预置于部署配置，首个 Owner 用受控 CLI 预绑定稳定身份。当时建议从主库读取授权，以事务级 advisory lock 协调撤权、角色操作和最后 Owner 检查，不另建授权控制行或缓存世代。

媒体、分类、主题、导航、设置、固定历史和外部任务在对应功能交付时再补独立 DDL。减少当时的表数不取消历史引用保护、私有媒体边界或可靠事件要求。

## 考虑过的方案

- 仅保留一份可变正文：当时未选择，因为会失去线上版本隔离和历史恢复。
- 将权限、媒体关系全部存入 JSON：会弱化授权检查和引用保护，因此未采用。

## 后果与限制

Post/Page 会重复少量字段和迁移；派生结果初期只保存一个处理器版本。权限与 Owner 规则依赖所有入口遵守所建议的事务协议，部署者也须取得稳定外部身份 ID。当时接受这些限制，计划根据实际交付功能及查询需要增加结构。

## 后续变更

用户随后选择包含 Series、权限目录和 settings 的 13 表模型，[ADR-0008](0008-thirteen-table-blog-core.md) 替代本文：保留 Post/Page 分表，但改为单份当前正文，不沿用本文的修订、统一路径、邀请、OAuth 尝试和审计表。会话表后来由 [ADR-0010](0010-persistent-postgres-sessions.md) 单独采纳，不能据此推定本篇其余历史方案恢复生效。
