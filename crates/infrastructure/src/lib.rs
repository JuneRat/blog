//! 基础设施层：持久化、渲染等出站适配器。
//! 实现应用层端口并隐藏具体库类型；数据库事务对象不暴露给 application。

pub mod persistence;
pub mod rbac;
pub mod rendering;

pub use persistence::{
    PgHealthCheck, PostgresPostRepository, PostgresPublishedPostQuery, PostgresUserRepository,
    SystemClock, connect, migrate,
};
pub use rbac::PostgresRbacStore;
pub use rendering::{MiniJinjaThemeRenderer, SanitizingMarkdownRenderer};
