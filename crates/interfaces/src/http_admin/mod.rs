//! 管理 API：按资源组织内容、目录和站点设置端点。
//!
//! - 全部响应 `Cache-Control: no-store`；正文请求体上限 2 MiB，设置为 16 KiB。
//! - 认证/CSRF/Origin 由 `AdminAuth` 提取器统一执行（共用实现见 `http_support`）：
//!   读方法仅需会话，写方法（POST/PATCH/PUT/DELETE）额外校验 `X-CSRF-Token`
//!   与同源 `Origin`。
//! - 错误契约统一为 JSON：401 带 `WWW-Authenticate: Session`，内部错误只回通用文案。
//! - 权限由应用层用例执行（文章 own/any；页面站点级）；本层不做业务判断。

mod auth;
mod categories;
mod pages;
mod posts;
mod series;
mod settings;
mod support;
mod tags;

pub use auth::AdminAuth;
pub use categories::{CreateCategoryBody, UpdateCategoryBody, categories_router};
pub use pages::{CreatePageBody, DeletePageBody, EditPageBody, pages_router};
pub use posts::{CreatePostBody, EditPostBody, posts_router};
pub use series::{CreateSeriesBody, ReorderBody, UpdateSeriesBody, series_router};
pub use settings::{SaveSiteSettingsBody, SaveThemeSettingsBody, settings_router};
pub use support::double_option::deserialize as deserialize_double_option;
pub use support::{ListQuery, VersionBody};
pub use tags::{CreateTagBody, RenameTagBody, tags_router};

/// 正文写入端点的请求体上限。
pub const ADMIN_BODY_LIMIT: usize = 2 * 1024 * 1024;

pub(crate) fn export_contract(out: &mut Vec<String>) {
    posts::export_contract(out);
    pages::export_contract(out);
    tags::export_contract(out);
    categories::export_contract(out);
    series::export_contract(out);
    settings::export_contract(out);
    support::export_contract(out);
}
