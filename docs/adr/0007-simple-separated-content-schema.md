# ADR-0007：精简核心表与 Post/Page 分表

记录日期：2026-09-21。

状态：被 [ADR-0008](0008-thirteen-table-blog-core.md) 替代。下文保留原 14 表方案作为历史；用户随后选择包含 Series、权限目录与 settings 的 13 表模型，不再采用本文的修订、路径、会话和审计表。本文当时替代了 [ADR-0006](0006-relational-content-storage.md)。

原建议把 M1–M4 的身份、内容、媒体、配置和队列模型集中展开，辅助表过多，且 Post/Page 共用存储不符合本次明确要求。

推荐核心为 14 表：users、roles、user_roles、role_permissions；oauth_accounts、sessions、invitations、oauth_states；posts、post_revisions、pages、page_revisions；content_paths、audit_logs。工作副本字段并入内容主表，派生 HTML 并入各自修订表。路径仍统一登记，通过互斥真实外键引用两种内容。数据库约束继续保护路径唯一与发布修订归属。

权限目录与委派限制使用可信代码/部署策略，不持久化镜像目录；OAuth 提供商预置于部署配置，首个 Owner 用受控 CLI 预绑定稳定身份。初期从主库读取授权，以事务级 advisory lock 协调撤权、角色操作与最后 Owner 检查，不另建授权控制行或缓存世代。

媒体、分类、主题、导航、设置、固定历史和外部任务在对应功能交付时补充独立 DDL。减少当前表数不取消历史引用保护、私有媒体边界或可靠事件要求。

代价：Post/Page 重复少量字段和迁移；派生结果初期只保存一个处理器版本；权限/Owner 规则依赖全部应用入口遵守事务协议；部署者须取得稳定外部身份 ID。接受这些明确限制，后续根据已交付功能和实际查询增加结构。

未选择仅保留一份可变正文，因为它会失去线上版本隔离和历史恢复。未选择把权限、媒体关系全部塞进 JSON，因为这会弱化授权检查与引用保护。完整字段和事务见 [数据库设计](../database-design.md)，建表见 [PostgreSQL DDL 草案](../sql/postgres-core.sql)。
