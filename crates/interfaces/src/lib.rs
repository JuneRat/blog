//! 入站适配器层：公开站点 HTTP、认证/管理 HTTP 与受控 CLI。
//! 只做传输层校验与响应映射，不创建连接池或模板引擎，不依赖 infrastructure。

pub mod cli;
pub mod http;
pub mod http_admin;
pub mod http_audit;
pub mod http_auth;
pub mod http_client_ip;
pub mod http_comments;
pub mod http_content_preview;
pub mod http_html_rebuild;
pub mod http_identity;
pub mod http_install;
pub mod http_limits;
pub mod http_media;
pub mod http_plugins;
pub mod http_support;
pub mod http_tasks;

pub mod http_retention;

mod syndication;

pub mod observability;

pub mod http_contract;

pub mod http_registration;
