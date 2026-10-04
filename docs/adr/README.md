# 架构决策记录

ADR 记录问题、选择、理由和代价。每篇页首说明决策状态与后续关联，正文保留作出选择时的语境。当前功能交付见[路线图](../product-roadmap.md)，现行行为见各篇的“当前参考”。

## 决策索引

| ADR | 状态 | 演进关系 |
|---|---|---|
| [0001 五个 crate 的模块化单体](0001-workspace-boundaries.md) | 已采纳 | 分层原则由 [0015](0015-rendered-content-runtime-and-module-boundaries.md) 补充装配、模块和依赖检查 |
| [0002 MiniJinja 与受控数据函数](0002-template-data-functions.md) | 已采纳 | 桥接可行结论见[原型报告](../template-bridge-experiment.md)，执行边界由 [0015](0015-rendered-content-runtime-and-module-boundaries.md) 细化 |
| [0003 产品与外部接入基线](0003-product-baseline.md) | 已采纳 | [0008](0008-thirteen-table-blog-core.md) 细化存储与开通方式；功能分期见[路线图](../product-roadmap.md) |
| [0004 公开内容 generation](0004-public-cache-generation.md) | 提议 | 页面缓存启用前仍需单独验证，桥接原型不替代其验收 |
| [0005 跨资源备份与隔离恢复](0005-consistent-backup-and-recovery.md) | 部分被替代 | 0021 采纳一致性要求并扩展网页原地恢复；旧独立恢复流程保留 |
| [0006 共享内容存储与引用](0006-relational-content-storage.md) | 已被替代 | 被 [0007](0007-simple-separated-content-schema.md) 替代 |
| [0007 精简核心表与 Post/Page 分表](0007-simple-separated-content-schema.md) | 已被替代 | 替代 [0006](0006-relational-content-storage.md)，后被 [0008](0008-thirteen-table-blog-core.md) 替代 |
| [0008 采用 13 表博客核心](0008-thirteen-table-blog-core.md) | 部分被替代 | 密码与会话由 [0009](0009-local-password-authentication.md)、[0010](0010-persistent-postgres-sessions.md) 更新；[0016](0016-confirmed-blog-schema.md) 更新关系、生命周期和表结构 |
| [0009 本地密码认证](0009-local-password-authentication.md) | 部分被替代 | 密码方案保留，持久会话由 [0010](0010-persistent-postgres-sessions.md) 更新，版本绑定由 [0016](0016-confirmed-blog-schema.md) 更新 |
| [0010 PostgreSQL 持久会话](0010-persistent-postgres-sessions.md) | 部分被替代 | 持久化与期限原则保留，[0016](0016-confirmed-blog-schema.md) 将会话改为 auth_version 快照 |
| [0011 Ant Design 后台](0011-admin-ui-library.md) | 已采纳 | 组件使用约定见[后台开发指南](../admin-development.md) |
| [0012 公共 hook 与暂缓 Query](0012-admin-data-layer.md) | 部分被替代 | 暂缓 Query 的选择由 [0013](0013-tanstack-query.md) 替代，公共错误处理原则保留 |
| [0013 TanStack Query](0013-tanstack-query.md) | 已采纳 | 更新 [0012](0012-admin-data-layer.md) 的取数方案；当前缓存与测试约定见[后台开发指南](../admin-development.md) |
| [0014 内容提交与后台稳定身份](0014-content-commits-and-stable-admin-identity.md) | 部分被替代 | [0015](0015-rendered-content-runtime-and-module-boundaries.md) 扩展正文派生物；[0016](0016-confirmed-blog-schema.md) 更新归档、恢复及时间语义 |
| [0015 持久化 HTML、渲染与装配](0015-rendered-content-runtime-and-module-boundaries.md) | 部分被替代 | 分层与渲染原则保留，[0016](0016-confirmed-blog-schema.md) 扩展至评论 HTML；[0017](0017-explicit-html-rebuild.md) 将自动重建改为显式维护 |
| [0016 新博客数据库设计与独立会话版本](0016-confirmed-blog-schema.md) | 已采纳 | 19 表基线；局部替代 0008/0009/0010/0014，扩展 0015；0020 追加任务运行表；交付与验收状态见[实施路线](../product-roadmap.md#已采纳数据库设计的实施) |
| [0017 HTML 显式重建](0017-explicit-html-rebuild.md) | 已采纳 | 局部替代 0015 的自动重建入口，普通启动和结构迁移不再重建 HTML；0019 补充后台显式入口 |
| [0018 有界 HTML 维护与只读预检](0018-bounded-html-maintenance.md) | 已采纳 | 细化 0017 的执行控制，由应用层编排预算、游标与部分完成结果；0019 扩展后台异步任务 |
| [0019 后台显式 HTML 重建与服务进程单任务](0019-admin-html-maintenance.md) | 部分被替代 | 显式维护和事务契约保留；0020 替代内存状态、进程内去重和重启边界 |
| [0020 固定后台任务、持久计划与跨进程租约](0020-persistent-admin-tasks.md) | 已采纳 | 追加任务运行/计划两表，统一三种白名单任务，提供有界历史、一次性计划和跨进程执行协调；CLI 仍独立 |
| [0021 统一镜像、网页备份与原地恢复](0021-browser-backup-and-in-place-recovery.md) | 已采纳 | 单实例数据库外控制器、加密副本、持久阻断与网页恢复，补充 0005/0020 |

## 状态含义

| 状态 | 含义 |
|---|---|
| 提议 | 仍需确认或验证的方案；已有原型或部分工具不等于方案全部采纳、实现或验收 |
| 已采纳 | 已选择的架构方向；实现和生产验收进度单独在路线图维护 |
| 部分被替代 | 仅指定范围由后续决策更新，其余原则仍保留 |
| 已被替代 | 后续决策接管该方案，保留原记录用于解释演进 |

“细化”表示补充约束或实现边界，不自动使原决策失效。替代必须指出具体记录和范围，不能只写“其余不变”而留下相互冲突的说明。

## 新增与维护

使用[模板](template.md)记录新的独立选择。保留既有编号和文件名；新增记录采用下一个空闲编号。旧记录缺少日期时如实标注，不把整理日期补成原决策日期。

可以整理历史措辞、章节和链接，但须保留当时选择、理由与限制。更新决策状态或发生局部替代时，同时更新旧篇页首、后续变更和本索引；不要把后来实现倒写成当时已经作出的决定。

配置值、组件用法、路由表、缓存失效清单和操作步骤由对应指南维护。原型测量与一次性验证细节保留在报告或变更记录中，ADR 只保留影响决策的结论、适用条件和证据链接。
