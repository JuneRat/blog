# 架构决策记录

索引更新：2026-09-26。ADR 记录取舍及当时背景；当前功能交付状态统一见[路线图](../product-roadmap.md)，当前规则见专项文档。

“已采纳”是决策状态，不自动代表已实现或生产验收。下表同时给出必要的实施说明。历史正文保持原样；正文开头仍写“待实施/待验证”时，以本索引记录的后续结论和关联文档理解，不把旧时间点描述当作当前状态。

## 当前决策

| ADR | 当前解释与后续变化 |
|---|---|
| [0001 Workspace 与边界](0001-workspace-boundaries.md) | 已采纳；五个 crate 已实现，依赖检查由 0015 落实。正文“未实现依赖连线”是历史状态 |
| [0002 MiniJinja 与数据函数](0002-template-data-functions.md) | 已采纳；桥接原型已验证可行，受控函数已进入生产代码。正文首段“待原型验证”由文末结论及[原型报告](../../spikes/template-bridge/README.md)更新；执行边界进一步见 0015 |
| [0003 产品与接入基线](0003-product-baseline.md) | 产品方向仍有效；存储与交付分期由 0008 及后续 ADR 细化，邀请/外部集成等不能据此推定已实现 |
| [0008 用户确认的 13 表核心](0008-thirteen-table-blog-core.md) | 已采纳并实现，替代 0007。正文“待实施”已过时；按需扩展原则下增加媒体 2 表与会话 1 表，密码与会话分别由 0009/0010 更新 |
| [0009 本地密码认证](0009-local-password-authentication.md) | 已采纳并实现；启用 Argon2id、限流及受控重置 |
| [0010 PostgreSQL 持久会话](0010-persistent-postgres-sessions.md) | 已采纳并实现；替代初期会话重启失效的边界，不代表完整多实例支持 |
| [0011 Ant Design v6 后台](0011-admin-ui-library.md) | 已采纳并实现 |
| [0013 TanStack Query](0013-tanstack-query.md) | 已采纳并实现；替代 0012 中暂不引入 Query 的决策 |
| [0014 内容提交与后台稳定身份](0014-content-commits-and-stable-admin-identity.md) | 已采纳并实现；领域写入口、完整记录及后台 UUID 身份 |
| [0015 持久化 HTML、渲染与模块边界](0015-rendered-content-runtime-and-module-boundaries.md) | 已采纳并实现；派生 HTML、集中执行、按命令装配及依赖检查 |

## 提议与尚未关闭的验证范围

| ADR | 当前边界 |
|---|---|
| [0004 公开缓存 generation](0004-public-cache-generation.md) | 仍为提议。桥接验证没有替代跨请求缓存的命中率、新鲜度与撤回验证；当前没有整页缓存 |
| [0005 一致备份与隔离恢复](0005-consistent-backup-and-recovery.md) | 原决策保留提议状态；维护工具与核心数据本机演练已落实部分方案，正文“待实施”不代表全无实现。完整媒体恢复、生产编排与 RPO/RTO 未验收，见[实际工具边界](../operations-and-recovery.md) |

## 历史与替代关系

| ADR | 替代关系 |
|---|---|
| [0006 共享内容存储与引用](0006-relational-content-storage.md) | 被 0007 替代；共享正文/修订/路径方案不再是当前模型 |
| [0007 精简核心表与 Post/Page 分表](0007-simple-separated-content-schema.md) | 被 0008 替代；保留原 14 表建议，不据此补建历史辅助表 |
| [0012 公共 hook 与暂缓 Query](0012-admin-data-layer.md) | “暂不引入 Query”被 0013 替代；公共错误处理的理由仍保留 |

新增重大选择先记录为提议，确认后再改变决策状态；实现进度在路线图更新。已被替代的正文不删除、不重新包装成当前方案。
