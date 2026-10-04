# ADR-0001：五个 crate 的模块化单体

- 状态：已采纳
- 记录日期：原记录未注明
- 关联决策：[ADR-0015：渲染与模块边界](0015-rendered-content-runtime-and-module-boundaries.md)
- 当前参考：[架构](../architecture.md)、[Domain](../domain.md)

## 背景

项目需要用 Cargo 依赖和 Rust 可见性约束技术边界，同时保留单体部署和层内的业务模块组织。

## 决策

采用 domain、application、infrastructure、interfaces、server 五个 crate。四层遵循向内依赖，server 作为独立装配入口。

## 考虑过的方案

单 crate 和三 crate 方案更轻，但不能提供同等粒度的编译依赖边界，因此采用五个 crate 的结构。

## 后果与限制

跨 crate 的公开 API 和 DTO 转换需要维护。技术分层不等于业务上下文隔离：模块私有状态可以封装，公开聚合却不能按依赖者选择性隐藏。依赖规则需要自动检查；是否进一步提取业务上下文 crate，应由实际隔离需求决定。

## 后续变更

五个 crate 已建立；[ADR-0015](0015-rendered-content-runtime-and-module-boundaries.md)进一步落实按业务拆分端口与持久化模块、受控导出及 Cargo 依赖检查。当前允许的依赖与测试例外以架构文档为准。
