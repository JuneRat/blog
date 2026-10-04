//! 内容上下文：文章、独立页面聚合与公开阅读的业务规则。

pub mod budget;
pub mod category;
pub mod page;
pub mod post;
pub mod series;
pub mod slug;
pub mod tag;
pub mod visibility;

pub use category::{CATEGORY_NAME_MAX_CHARS, Category, CategoryError, CategorySnapshot};
pub use page::{
    Page, PageError, PageId, PagePatch, PageSnapshot, PageStatus, RESERVED_ROOT_SLUGS,
    is_reserved_root_slug,
};
pub use post::{
    Post, PostDraftMetadata, PostError, PostId, PostPatch, PostSnapshot, PostStatus,
    SeriesPlacement,
};
pub use series::{SERIES_NAME_MAX_CHARS, Series, SeriesError, SeriesSnapshot};
pub use tag::{TAG_NAME_MAX_CHARS, Tag, TagError, TagSnapshot};

pub use slug::{SLUG_MAX_BYTES, Slug, SlugError};
pub use visibility::Visibility;
