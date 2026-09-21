//! 入站适配器层：公开站点 HTTP、认证/管理 HTTP 与受控 CLI。
//! 只做传输层校验与响应映射，不创建连接池或模板引擎，不依赖 infrastructure。

pub mod cli;
pub mod http;
pub mod http_auth;
