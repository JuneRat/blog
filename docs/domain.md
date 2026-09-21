# Domain 设计

状态：目标规划，按实际用例逐步创建。整体依赖规则见 [架构设计](architecture.md)。

## 1. 组织原则

`domain` 首先按业务模块组织，每个模块内部围绕聚合组织。不在顶层统一堆放 `entities/`、`value_objects/`、`services/`。

模块划分是当前业务假设，不代表已经确认全部 DDD 限界上下文。简单模型保留单文件；只有复杂度增长时才拆目录。不要为了满足目录树创建空实现。

## 2. 目标目录

```text
crates/domain/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── content/
    │   ├── mod.rs
    │   ├── post/
    │   │   ├── mod.rs
    │   │   ├── aggregate.rs       # Post 聚合及业务行为
    │   │   ├── id.rs              # PostId
    │   │   ├── status.rs          # 发布状态
    │   │   ├── error.rs           # 文章规则错误
    │   │   └── tests.rs           # 聚合行为测试
    │   ├── page.rs                # 独立页面
    │   ├── slug.rs                # 内容路径值对象
    │   ├── taxonomy.rs            # 分类树和标签
    │   └── series.rs              # 有序文章系列
    ├── appearance/
    │   ├── mod.rs
    │   ├── theme_id.rs
    │   └── activation.rs          # 有实际规则时引入
    ├── media/
    │   ├── mod.rs
    │   ├── asset.rs
    │   ├── asset_id.rs
    │   └── error.rs
    ├── identity/
    │   ├── mod.rs
    │   ├── user.rs
    │   ├── user_id.rs
    │   ├── role.rs                # 内置与自定义角色
    │   ├── permission.rs          # PermissionId/Key 与授权规则；不枚举业务动作
    │   ├── role_assignment.rs
    │   ├── external_identity.rs   # 提供商稳定身份，不含 OAuth 协议
    │   └── invitation.rs          # 后续邀请能力交付时再引入
    └── site/
        ├── mod.rs
        ├── settings.rs
        └── navigation.rs
```

首条实现主线是 `content::post`。其余目录随对应功能引入；没有业务规则的技术配置无需放入 domain。

## 3. 模型与规则

| 模块 | 主要概念 | 领域职责 |
|---|---|---|
| content | Post、Page、Slug、Category、Series、Tag | 内容状态转换、分类树、系列顺序与内容约束 |
| appearance | ThemeId、激活状态 | 主题身份和实际激活业务规则 |
| media | MediaAsset、AssetId | 资产身份及生命周期约束 |
| identity | User、UserId、Role、Permission、RoleAssignment、ExternalIdentity、Invitation | 账号状态、角色授权、身份绑定与邀请规则 |
| site | SiteSettings、Navigation | 站点信息和导航约束 |

当前采用 [13 表设计](database-design.md)：Post/Page 各存一份当前内容，无独立工作副本与修订。版本变化、状态、分类树、系列顺序、删除与 Slug 规则由 [内容生命周期](content-lifecycle.md) 统一定义；媒体模型为后续扩展。

### 聚合设计

- Post 维护身份、author_id、当前标题/正文、分类/系列引用、发布状态、可见性与乐观锁 version；保存已发布内容直接更新线上。Page 使用独立聚合，无 author_id 与文章组织关系。
- 使用 `publish`、`archive`、`unpublish` 等业务方法，不提供任意修改状态的公共 setter。
- 跨聚合关联优先保存 ID，不把 User、MediaAsset 等完整聚合嵌入 Post；author_id 的建议类型为 identity::UserId。
- Category、Tag、Series 根据自身生命周期管理，Post 只存关联 ID。分类树防环、系列重排与受引用保护的删除由应用协调事务；名称读取当前值，不存在历史关系快照。
- Slug 验证单段格式；各表唯一、首次发布后锁定和 Page 系统路由保留由应用与持久化共同维护。当前无路径历史或永久占位。
- 需要当前时间时由调用者显式传入；获取时间的端口放 application，领域行为不自行读取系统时钟。
- 新建与持久化重建应有不同的受控入口，重建不重复触发创建或发布事件，同时验证必要的状态一致性。

不要按数据库表逐一生成聚合，也不要因为跨两个字段就新增领域服务。只有无法自然归属某个实体或值对象的业务规则，才考虑领域服务。

[数据库设计](database-design.md) 将 Post/Page 分别映射到 posts 和 pages，没有修订子表。共用的格式验证和发布规则可复用代码，不引入统一 contents 表；不要求数据库中的每张关系表都形成独立聚合。

### 模块间依赖（工程建议）

| 使用方 | 允许引用的其他模块概念 | 禁止 |
|---|---|---|
| content | identity::UserId | User 聚合、身份存储和权限实现 |
| media | identity::UserId | User 聚合、content 聚合 |
| identity | 无业务模块依赖 | 反向引用 content/media |
| appearance | 无业务模块依赖 | 读取 site 或用户聚合执行授权 |
| site | 默认无跨模块依赖 | 把主题激活实现放进设置聚合 |

站点启用主题、公开作者资料拼接由 application 协调用例，不靠聚合互相持有完成。媒体管理交付后也由 application 协调保留/公开引用，MediaAsset 的 Ready 状态不自动表示公开。不能因某页面同时显示两个模块的数据就添加领域依赖。

Cargo metadata 只能检查 crate 依赖，不能验证此表。模块依赖先通过私有可见性和审查约束，可增加基于语法的架构 lint；简单 use 文本扫描仅作提示，不能声称覆盖重导出、别名、宏与完整路径。需要编译器严格隔离时再拆上下文 crate。

领域模型不直接作为 HTTP/SQL/模板的序列化契约。默认在 application/interface DTO 与 infrastructure 行模型上处理序列化；并不把禁止一切 serde derive 当作 DDD 定律，若引入必须说明用途并保护构造不变量。

首期集成连接的非敏感配置可归 site 应用设置，凭据由基础设施秘密存储管理，不预建 extensions 聚合。未来即使仍是声明式扩展，也可在出现独立生命周期规则时提取模型，不以是否执行代码作为唯一条件。

新增领域模块应能说明其业务不变量及纯单元测试场景；这是防止无业务模型膨胀的评审标准，不要求每个 ID 包装类型都单独形成聚合。

## 4. 可见性与公开 API

模块入口示例：

```rust
// src/lib.rs
pub mod content;

// src/content/mod.rs
mod post;
mod slug;

pub use post::{Post, PostError, PostId, PublicationStatus};
pub use slug::Slug;

// src/content/post/mod.rs
mod aggregate;
mod error;
mod id;
mod status;

pub use aggregate::Post;
pub use error::PostError;
pub use id::PostId;
pub use status::PublicationStatus;

#[cfg(test)]
mod tests;
```

以上是目录与导出示例，不是已经落地的源码。外部使用 `domain::content::Post`，不依赖内部文件布局。

可见性约束：

- 字段、辅助函数和内部模块默认私有。
- 仅对外部用例真正需要的类型和行为使用 `pub`。
- 优先采用足够小的 `pub(super)` 或 `pub(in ...)`，不要统一开放成 `pub(crate)`。
- 公开结构体不要求公开字段。只读访问器按实际需要增加。
- 不在 crate 根目录扁平导出所有业务模型，保留模块归属。

需要特别注意：`pub use` 可以隐藏实现路径，但不是按调用者授权的机制。Post 从统一 domain 公开后，任何合法依赖 domain 的 crate 都可以使用它。

若未来 Comments 必须只能看到已发布文章摘要、不能获得 Post 聚合，应设计查询契约，并在需要编译期保证时提取独立上下文 crate。仅把文件挪进 `content/` 和 `comments/` 目录不会自动实现这种隔离。

## 5. 不属于 Domain 的内容

| 内容 | 所属层 |
|---|---|
| PublishPost、ActivateTheme 用例 | application |
| 仓储、查询、时钟、存储和渲染端口 | application，遵循本项目约定 |
| Actor 调用者契约、分页、展示 DTO | application |
| 权限检查与事务编排 | application，可使用领域规则 |
| SQLx 行类型、事务对象、数据库错误 | infrastructure |
| Markdown 渲染、HTML 清洗、图片处理 | infrastructure |
| MiniJinja、主题文件扫描、清单解析 | infrastructure |
| HTTP 请求、Cookie、路由和响应状态码 | interfaces |
| 配置加载和具体依赖装配 | server |
| 审计写入、公开缓存 generation 契约 | application；基础设施负责持久化 |

主题功能以应用流程和技术适配为主，不强行创建 ThemeAggregate。主题清单格式与模板 API 兼容性也不因包含“版本”二字就自动成为领域模型。

具体权限标识由 application 对应业务模块声明，注册表也由 application 定义，server 负责装配。identity 只维护通用标识、范围和角色授权，不反向依赖 content/media，也不拥有全量业务动作枚举；domain::content 无需引用权限实现。

RBAC 与 OAuth 的详细职责见 [身份与后台](identity-and-admin.md)。角色和身份关联规则可以进入 domain，邀请在该能力交付时再加入；OAuth code/token、HTTP 会话、OIDC 签名验证和提供商 SDK 留在外层。内置角色 slug 受保护，自定义角色不能复制 Owner 身份；委派上限、角色编辑与最后 Owner 保护需应用事务协调，不能只靠单个 User 或 Role 方法保证。

首期受限插件不需要 PluginRuntime 领域模型；搜索和统计以应用端口、配置及适配器起步。数据库种类也不进入聚合模型。扩展细节见 [扩展与数据](extensions-and-data.md)。

## 6. 错误、事件与共享概念

领域错误描述规则失败，例如内容不完整、非法状态转换或 Slug 格式无效，不包含 HTTP 状态码和 SQL 错误。

存在实际消费者时再引入领域事件，例如 `PostPublished`；由应用层收集和协调持久化，领域层不直接发布消息或刷新缓存。事件结构不能无条件等同于公开 API 或长期持久化格式，后两者需要各自的兼容性策略。

不预建 `shared`、`common`、`BaseEntity`、通用仓储或错误基类。共享前先确认概念语义是否相同、所有权属于哪个模块。不同模块可以使用不同类型并显式转换；只有稳定且确实共同拥有的少量概念，才考虑共享内核，并记录耦合代价。

## 7. 验收方式

- domain 能独立编译和测试，不需要数据库、HTTP 服务或模板环境。
- Post 状态转换测试覆盖成功路径、非法转换和重复操作策略。
- Slug 等值对象测试覆盖业务边界与非法输入。
- 聚合内部状态不能被外部直接赋值绕过规则。
- Cargo 依赖检查防止 domain 引入项目外层 crate 和禁止的框架依赖。
- 公开 API 变更按调用者需求审查，不通过扩大可见性来回避建模问题。

本目录规划不要求增加全部模型后再编写用例。先完成文章发布纵向闭环，用实际需求检验模型和端口，再逐步扩展。
