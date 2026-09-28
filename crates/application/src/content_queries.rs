//! 后台列表读模型与授权。查询端口不加载写聚合或正文。

use std::sync::Arc;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::UseCaseError;
use crate::identity::{Actor, authorize_own_or_any};
use crate::ports::{AdminPageQuery, AdminPostQuery};
use domain::identity::UserId;

pub const CONTENT_PER_PAGE: i64 = 20;

#[derive(Debug, Clone)]
pub struct AdminPostSummary {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    pub status: String,
    pub visibility: String,
    pub version: i64,
    pub published_at: Option<OffsetDateTime>,
    pub updated_at: OffsetDateTime,
    pub author_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct AdminPageSummary {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    pub status: String,
    pub visibility: String,
    pub version: i64,
    pub published_at: Option<OffsetDateTime>,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ContentPage<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

impl<T> ContentPage<T> {
    pub fn map<U>(self, convert: impl FnMut(T) -> U) -> ContentPage<U> {
        ContentPage {
            items: self.items.into_iter().map(convert).collect(),
            total: self.total,
            page: self.page,
            per_page: self.per_page,
        }
    }
}

/// 未验证的入站参数；授权后再验证，避免无权限请求探测查询行为。
#[derive(Debug, Clone)]
pub struct ContentListRequest {
    pub page: i64,
    pub status: Option<String>,
    pub visibility: Option<String>,
    pub trash: bool,
}

impl Default for ContentListRequest {
    fn default() -> Self {
        Self {
            page: 1,
            status: None,
            visibility: None,
            trash: false,
        }
    }
}

/// 应用层验证后的查询条件；适配器以绑定参数应用筛选，行与总数来自同一快照。
#[derive(Debug, Clone)]
pub struct ContentListFilter {
    request: ContentListRequest,
}

impl TryFrom<ContentListRequest> for ContentListFilter {
    type Error = UseCaseError;

    fn try_from(request: ContentListRequest) -> Result<Self, Self::Error> {
        if !(1..=i64::MAX / CONTENT_PER_PAGE).contains(&request.page) {
            return Err(UseCaseError::Invalid("页码超出范围".into()));
        }
        if request
            .status
            .as_deref()
            .is_some_and(|s| !matches!(s, "draft" | "scheduled" | "published" | "archived"))
        {
            return Err(UseCaseError::Invalid("无效的发布状态".into()));
        }
        if request
            .visibility
            .as_deref()
            .is_some_and(|s| !matches!(s, "public" | "private"))
        {
            return Err(UseCaseError::Invalid("无效的可见性".into()));
        }
        Ok(Self { request })
    }
}

impl ContentListFilter {
    pub fn page(&self) -> i64 {
        self.request.page
    }
    pub fn limit(&self) -> i64 {
        CONTENT_PER_PAGE
    }
    pub fn offset(&self) -> i64 {
        (self.request.page - 1) * CONTENT_PER_PAGE
    }
    pub fn status(&self) -> Option<&str> {
        self.request.status.as_deref()
    }
    pub fn visibility(&self) -> Option<&str> {
        self.request.visibility.as_deref()
    }
    pub fn trash(&self) -> bool {
        self.request.trash
    }
}

pub struct ContentQueries {
    posts: Arc<dyn AdminPostQuery>,
    pages: Arc<dyn AdminPageQuery>,
}

impl ContentQueries {
    pub fn new(posts: Arc<dyn AdminPostQuery>, pages: Arc<dyn AdminPageQuery>) -> Self {
        Self { posts, pages }
    }

    /// 本人须有 post.read，他人须有 post.read_any；any 同样覆盖本人。
    pub async fn posts(
        &self,
        actor: &Actor,
        author: UserId,
        request: ContentListRequest,
    ) -> Result<ContentPage<AdminPostSummary>, UseCaseError> {
        authorize_own_or_any(actor, "post.read", "post.read_any", author)?;
        let filter = ContentListFilter::try_from(request)?;
        let (items, total) = self.posts.list(author.0, &filter).await?;
        Ok(ContentPage {
            items,
            total,
            page: filter.page(),
            per_page: filter.limit(),
        })
    }

    pub async fn pages(
        &self,
        actor: &Actor,
        request: ContentListRequest,
    ) -> Result<ContentPage<AdminPageSummary>, UseCaseError> {
        if !actor.has_permission("page.read") {
            return Err(UseCaseError::Forbidden);
        }
        let filter = ContentListFilter::try_from(request)?;
        let (items, total) = self.pages.list(&filter).await?;
        Ok(ContentPage {
            items,
            total,
            page: filter.page(),
            per_page: filter.limit(),
        })
    }
}
