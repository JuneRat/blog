//! 应用层：用例、端口、输入输出契约与事务编排。
//! 不绑定数据库或 HTTP 框架类型。

pub mod auth;
pub mod content;
pub mod error;
pub mod identity;
pub mod page;
pub mod password;
pub mod ports;
pub mod public_site;
pub mod version;

pub use error::UseCaseError;
