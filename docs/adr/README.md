# 架构决策记录

记录始于 2026-09-20，索引更新日期：2026-09-22。这里只记录重要取舍；详细规则由专项设计文档维护。

状态含义：

- 已采纳：会话中已确定的选择，或明确标注为工程设计基线的方案；不代表已实现。
- 提议：仍需产品确认或技术原型验证，不视为用户同意。
- 被替代：保留原决策与替代记录，后续不静默覆盖历史。

| ADR | 状态 | 来源 |
|---|---|---|
| [0001 Workspace 与边界](0001-workspace-boundaries.md) | 已采纳 | 用户要求 workspace 与四层，另设装配入口 |
| [0002 MiniJinja 与数据函数](0002-template-data-functions.md) | 引擎/能力已采纳；桥接待验证 | 用户提出 MiniJinja 与模板函数获取数据 |
| [0003 产品与接入基线](0003-product-baseline.md) | 已采纳 | 用户逐项回答数据库、插件、账号、登录、SPA、发布流程问题 |
| [0004 公开缓存版本](0004-public-cache-generation.md) | 提议 | 工程设计建议，需原型与一致性验证 |
| [0005 一致备份与隔离恢复](0005-consistent-backup-and-recovery.md) | 提议 | 文档审查后的工程建议，需部署规格及恢复演练验证 |
| [0006 共享内容存储与引用](0006-relational-content-storage.md) | 被替代 | 由 ADR-0007 替代，保留原建议作为历史记录 |
| [0007 精简核心表与 Post/Page 分表](0007-simple-separated-content-schema.md) | 被替代 | 由 ADR-0008 替代，保留原 14 表设计作为历史 |
| [0008 用户确认的 13 表核心](0008-thirteen-table-blog-core.md) | 已采纳，待实施 | 用户提供 Series、RBAC、OAuth 的具体表设计并要求采用 |
| [0009 启用本地密码认证](0009-local-password-authentication.md) | 已采纳并实现 | 用户要求优先启用 `users.password_hash`，并明确要 Argon2id、限流、重置与泄露处置 |

内容修订、URL、Owner 引导、授权变更、媒体/分类规则和会话存储的建议状态集中记录于 [决策登记表](../product-roadmap.md)；重大取舍可先以“提议”记录，确认后更新状态，避免把草案回填为历史事实。
