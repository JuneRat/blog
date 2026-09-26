# 文档导航

这里区分当前实现、操作指南、后续规划和决策记录。当前行为以代码及测试为准，数据库结构以迁移为执行依据；规划和历史 ADR 不代表功能已经可用。

## 按任务阅读

| 你要做什么 | 从这里开始 |
|---|---|
| 首次运行项目 | [项目首页](../README.md) → [开发指南](development.md) |
| 配置环境、域名、数据库或媒体目录 | [配置参考](configuration.md) |
| 修改后端或判断代码应放在哪一层 | [架构](architecture.md) → [领域模型](domain.md) |
| 修改编辑、发布、删除或媒体引用行为 | [内容生命周期](content-lifecycle.md) → [数据库设计](database-design.md) |
| 开发后台或接入管理接口 | [管理 API](admin-api.md) → [身份、权限与后台](identity-and-admin.md) |
| 开发公开页面、主题或 SEO | [主题与渲染](themes-and-rendering.md) |
| 备份、恢复或处置账号问题 | [运维与恢复](operations-and-recovery.md) |
| 确认交付范围或选择下一步工作 | [产品路线图](product-roadmap.md) |
| 理解决策原因及替代方案 | [ADR 索引](adr/README.md) |

## 文档职责

每类信息在一个主文档中维护，其他文档用链接引用。

| 文档 | 负责的内容 |
|---|---|
| [开发指南](development.md) | 本地环境、CLI、前端联调、检查命令与测试库 |
| [配置参考](configuration.md) | 环境变量、配置优先级、路径与命令作用域 |
| [架构](architecture.md) | 模块职责、依赖方向、装配、事务与执行边界 |
| [领域模型](domain.md) | 对象关系、业务不变量与公开可见性 |
| [内容生命周期](content-lifecycle.md) | 编辑、发布、回收站、并发与媒体生命周期 |
| [数据库设计](database-design.md) | 表结构、约束、索引、迁移与 SQL 参考的关系 |
| [身份、权限与后台](identity-and-admin.md) | 认证、授权、会话、Owner 保护与后台交互约束 |
| [管理 API](admin-api.md) | 当前路由、请求形态、版本与错误约定 |
| [主题与渲染](themes-and-rendering.md) | 模板契约、HTML 派生、执行预算、主题与 SEO |
| [运维与恢复](operations-and-recovery.md) | 当前工具的操作步骤、验证范围与限制 |
| [产品路线图](product-roadmap.md) | 交付状态、后续里程碑和验收标准 |
| [扩展与数据能力](extensions-and-data.md) | 尚未交付的扩展接口、格式和设计约束 |
| [ADR](adr/README.md) | 决策背景、理由、后果与替代关系 |

数据库迁移位于 [migrations/postgres](../migrations/postgres/)，[postgres-core.sql](sql/postgres-core.sql) 是便于整体阅读的结构参考。原型验证位于 [spikes/template-bridge](../spikes/template-bridge/README.md)，不属于主程序的功能入口。

## 维护约定

- 功能变更时更新对应的当前参考；状态变化只在路线图维护，不往每份文档追加交付日志。
- 上手命令放开发指南，配置表放配置参考，路由与错误码放 API 参考，避免复制后逐渐失配。
- 未实现的设计明确标为规划；有表或权限 key，不等于已有可用的用例、API 或界面。
- 重要架构取舍新增 ADR。旧决策被替代时在索引标明关系，保留当时的背景，不把历史全文改写成现状。
- SQL 变更先写迁移，再同步数据库说明和结构参考；文档中的 SQL 不替代迁移执行。
- 使用相对链接引用仓库文件与章节；移动文件或改标题后检查链接。测试数量和一次性执行日志留在变更记录中，不作为长期文档内容。
