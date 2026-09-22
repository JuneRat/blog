//! 内容上下文：文章、独立页面聚合与公开阅读的业务规则。

pub mod page;
pub mod post;

pub use page::{
    Page, PageError, PageId, PagePatch, PageSnapshot, PageStatus, RESERVED_ROOT_SLUGS,
    is_reserved_root_slug,
};
pub use post::{Post, PostError, PostId, PostPatch, PostSnapshot, PostStatus, Slug, Visibility};
