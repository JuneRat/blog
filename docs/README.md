# 文档导航

这里区分当前实现、已采纳设计、操作指南、后续规划和决策记录。当前行为以代码及测试为准，运行数据库以迁移为执行依据；设计已采纳不代表功能已经可用。

## 按任务阅读

| 你要做什么 | 从这里开始 |
|---|---|
| 首次运行项目 | [项目首页](../README.md) → [开发指南](development.md) |
| 用 1Panel 网页部署单机站点 | [1Panel 部署](1panel.md) |
| 在后台备份、定时、远程存储和原地恢复 | [后台备份与恢复](browser-backup.md) |
| 从源码构建或管理高级容器配置 | [Docker Compose 部署](docker-compose.md) |
| Compose 停写备份、独立恢复、定时与加密异地副本 | [Compose 备份恢复](compose-backup.md) |
| 配置环境、域名、数据库或媒体目录 | [配置参考](configuration.md) |
| 修改后端或判断代码应放在哪一层 | [架构](architecture.md) → [领域模型](domain.md) |
| 查看已确认的新数据库方案 | [目标数据库设计](database-design.md) → [ADR-0016](adr/0016-confirmed-blog-schema.md) → [目标 DDL](sql/postgres-core.sql) |
| 新增表、升级结构或恢复旧版本备份 | [迁移演进](schema-migrations.md) |
| 比较连接池大小、公开页面与后台混合负载容量 | [容量验证](public-read-capacity.md) |
| 检查服务健康、错误率、延迟或运行版本 | [探针、日志与指标](observability.md) |
| 修改当前编辑、发布、删除或媒体引用实现 | [内容生命周期](content-lifecycle.md) → [当前数据库实现](database-current.md)；新方案的实施见[路线图](product-roadmap.md#已采纳数据库设计的实施) |
| 开发后台界面 | [后台开发指南](admin-development.md) → [管理 API](admin-api.md) |
| 规划后台与后端的协作优化 | [后台后端配合优化规划](admin-backend-optimizations.md) |
| 接入管理接口 | [管理 API](admin-api.md) → [身份、权限与后台](identity-and-admin.md) |
| 使用或开发原生评论 | [评论](comments.md) |
| 开发公开页面、主题或 SEO | [主题与渲染](themes-and-rendering.md) |
| 备份、恢复、清理正式媒体或处置账号问题 | [运维与恢复](operations-and-recovery.md) |
| 从空库验收安装、写作、评论及备份恢复 | [新库全链路验收](acceptance.md) |
| 为候选版本确定上线门槛并记录生产证据 | [上线验收模板](release-acceptance-template.md) → [路线图 M5](product-roadmap.md#3-里程碑与验收入口) |
| 确认交付范围或选择下一步工作 | [产品路线图](product-roadmap.md) |
| 理解决策原因及替代方案 | [ADR 索引](adr/README.md) |

## 文档职责

每类信息在一个主文档中维护，其他文档用链接引用。

| 文档 | 负责的内容 |
|---|---|
| [开发指南](development.md) | 本地环境、CLI、前端联调、检查命令与测试库 |
| [首次安装](installation.md) | 安装向导、初始 Admin、连接配置保存与中断恢复 |
| [1Panel 部署](1panel.md) | 面板编排、自动配置数据库、域名和首次安装 |
| [后台备份与恢复](browser-backup.md) | 密钥、备份、计划、远程存储、原地恢复与应急入口 |
| [Docker Compose 部署](docker-compose.md) | 镜像构建与交付、持久卷、安装、健康检查和部署维护 |
| [Compose 备份恢复](compose-backup.md) | 容器内工具、完整备份包、隔离核验、加密仓库与定时任务 |
| [后台开发指南](admin-development.md) | 组件、表单、查询缓存、失效与前端测试约定 |
| [后台后端配合优化规划](admin-backend-optimizations.md) | 后台需要后端配合的优化方案、API 规划与落地优先级 |
| [配置参考](configuration.md) | 环境变量、配置优先级、路径与命令作用域 |
| [架构](architecture.md) | 模块职责、依赖方向、装配、事务与执行边界 |
| [领域模型](domain.md) | 对象关系、业务不变量与公开可见性 |
| [领域写入约束](domain-write-invariants.md) | 值类型、持久化重建与正文容量边界；业务规则链接权威契约 |
| [内容生命周期](content-lifecycle.md) | 编辑、发布、回收站、并发与媒体生命周期 |
| [数据库设计](database-design.md) | 已采纳的 19 表目标设计、业务规则及与当前实现的差异 |
| [当前数据库实现](database-current.md) | 当前迁移对应的表结构、约束、索引与事务边界 |
| [迁移演进](schema-migrations.md) | 不可变迁移、共享清单、SQL 生成、升级与跨版本恢复 |
| [容量验证](public-read-capacity.md) | 查询路径、连接池对比和独立测试库上的持续混合读写压测 |
| [探针、日志与指标](observability.md) | 健康端点、JSON 日志、指标访问及 Prometheus 抓取 |
| [身份、权限与后台](identity-and-admin.md) | 认证、授权、会话、Admin 保护与后台交互约束 |
| [管理 API](admin-api.md) | 当前路由、请求形态、版本与错误约定 |
| [主题与渲染](themes-and-rendering.md) | 模板契约、HTML 派生、执行预算、主题与 SEO |
| [运维与恢复](operations-and-recovery.md) | 当前工具的操作步骤、验证范围与限制 |
| [新库全链路验收](acceptance.md) | 独立空库 HTTP 验收、资源清理、结果报告及 CI |
| [上线验收模板](release-acceptance-template.md) | 每次候选版本的环境、规模、量化阈值、浏览器与恢复证据格式 |
| [产品路线图](product-roadmap.md) | 交付状态、后续里程碑和验收标准 |
| [扩展与数据能力](extensions-and-data.md) | 尚未交付的扩展接口、格式和设计约束 |
| [ADR](adr/README.md) | 决策背景、理由、后果与替代关系 |

当前数据库以 [migrations/postgres](../migrations/postgres/) 的迁移链为唯一结构来源；[汇总 DDL](sql/postgres-core.sql) 由该链自动生成，只供空库 DDL 参考。已有 `0001_initial_schema.sql` 不再改写，后续按[迁移演进规则](schema-migrations.md)追加；不能与已删除的旧链叠加。桥接实验结论保存在[归档报告](template-bridge-experiment.md)，独立实验代码已移除。

## 维护约定

- 功能变更时更新对应的当前参考；功能交付状态集中在路线图维护，不往每份文档追加交付日志。
- 上手命令放开发指南，配置表放配置参考，路由与错误码放 API 参考，避免复制后逐渐失配。
- 未实现的内容区分已采纳设计与候选规划；有表或权限 key，不等于已有可用的用例、API 或界面。
- 重要架构取舍使用 [ADR 模板](adr/template.md)新增记录。旧决策被替代时同步页首、后续变更与索引，保留当时的背景与选择。
- 设计阶段更新目标 DDL 与设计说明；实施时同步应用、迁移和当前结构参考。文档中的 SQL 不替代迁移执行，目标结构不能直接覆盖现有库。
- 使用相对链接引用仓库文件与章节；移动文件或改标题后检查链接。测试数量和一次性执行日志留在变更记录中，不作为长期文档内容。
