# ADR-0001：五个 crate 的模块化单体

状态：已采纳，未实现依赖连线。来源：用户明确要求 Cargo workspace、domain/application/infrastructure/interfaces；讨论采用 server 装配入口。

采用 domain、application、infrastructure、interfaces、server 五个 crate。四层遵循向内依赖，server 装配。保留单体部署，层内按业务模块组织。

原因：用户希望用编译依赖和可见性约束技术边界。单 crate 和三 crate 方案更轻，但不能提供同等粒度的分层依赖约束。

代价：跨 crate 公开 API 和 DTO 转换需要维护；分层不等于业务上下文隔离。模块内私有状态仍可封装，公开聚合却不能按依赖者选择性隐藏。需要 CI 检查 manifest，必要时再提取业务上下文 crate。

约束详见 [架构设计](../architecture.md) 和 [Domain 设计](../domain.md)。
