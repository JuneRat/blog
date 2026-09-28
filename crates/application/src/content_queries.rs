//! 后台列表读模型与授权。查询端口不加载写聚合或正文。

use std::sync::Arc;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::UseCaseError;
use crate::identity::{Actor, authorize_own_or_any};
use crate::ports::{AdminPageQuery, AdminPostQuery, UserQuery};
pub use domain::content::{Visibility, page::PageStatus, post::PostStatus};
use domain::identity::UserId;

pub const CONTENT_PER_PAGE: i64 = 20;

#[derive(Debug, Clone)]
pub struct AdminPostSummary {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    pub status: PostStatus,
    pub visibility: Visibility,
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
    pub status: PageStatus,
    pub visibility: Visibility,
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
pub struct ContentListFilter<S> {
    page: i64,
    status: Option<S>,
    visibility: Option<Visibility>,
    trash: bool,
}

pub type PostListFilter = ContentListFilter<PostStatus>;
pub type PageListFilter = ContentListFilter<PageStatus>;

impl<S: Copy> ContentListFilter<S> {
    fn parse(
        request: ContentListRequest,
        parse_status: fn(&str) -> Option<S>,
    ) -> Result<Self, UseCaseError> {
        if !(1..=i64::MAX / CONTENT_PER_PAGE).contains(&request.page) {
            return Err(UseCaseError::Invalid("页码超出范围".into()));
        }
        let status = request
            .status
            .as_deref()
            .map(|value| {
                parse_status(value).ok_or_else(|| UseCaseError::Invalid("无效的发布状态".into()))
            })
            .transpose()?;
        let visibility = request
            .visibility
            .as_deref()
            .map(|value| {
                Visibility::parse(value).ok_or_else(|| UseCaseError::Invalid("无效的可见性".into()))
            })
            .transpose()?;
        Ok(Self {
            page: request.page,
            status,
            visibility,
            trash: request.trash,
        })
    }

    pub fn page(&self) -> i64 {
        self.page
    }
    pub fn limit(&self) -> i64 {
        CONTENT_PER_PAGE
    }
    pub fn offset(&self) -> i64 {
        (self.page - 1) * CONTENT_PER_PAGE
    }
    pub fn status(&self) -> Option<S> {
        self.status
    }
    pub fn visibility(&self) -> Option<Visibility> {
        self.visibility
    }
    pub fn trash(&self) -> bool {
        self.trash
    }
}

impl TryFrom<ContentListRequest> for PostListFilter {
    type Error = UseCaseError;
    fn try_from(request: ContentListRequest) -> Result<Self, Self::Error> {
        Self::parse(request, PostStatus::parse)
    }
}

impl TryFrom<ContentListRequest> for PageListFilter {
    type Error = UseCaseError;
    fn try_from(request: ContentListRequest) -> Result<Self, Self::Error> {
        Self::parse(request, PageStatus::parse)
    }
}

pub struct ContentQueries {
    posts: Arc<dyn AdminPostQuery>,
    pages: Arc<dyn AdminPageQuery>,
    users: Arc<dyn UserQuery>,
}

impl ContentQueries {
    pub fn new(
        posts: Arc<dyn AdminPostQuery>,
        pages: Arc<dyn AdminPageQuery>,
        users: Arc<dyn UserQuery>,
    ) -> Self {
        Self {
            posts,
            pages,
            users,
        }
    }

    /// 显式作者筛选先检查 read_any，再解析用户名；普通列表与回收站共用。
    /// 缺省/空字符串代表本人。即使显式指定本人用户名，也要求 read_any，
    /// 不通过用户名是否存在、是否合法或是否停用的差异泄露账号信息。
    pub async fn posts_by_author(
        &self,
        actor: &Actor,
        author: Option<&str>,
        request: ContentListRequest,
    ) -> Result<ContentPage<AdminPostSummary>, UseCaseError> {
        let author = match author.filter(|name| !name.is_empty()) {
            None => actor.user_id,
            Some(name) => {
                if !actor.has_permission("post.read_any") {
                    return Err(UseCaseError::Forbidden);
                }
                let name = domain::identity::normalize_username(name)
                    .map_err(|error| UseCaseError::Invalid(error.to_string()))?;
                let snapshot = self
                    .users
                    .find_by_username(&name)
                    .await?
                    .ok_or_else(|| UseCaseError::NotFound(format!("用户 {name}")))?;
                let user = domain::identity::User::reconstitute(snapshot)
                    .map_err(|error| UseCaseError::Repository(error.to_string()))?;
                if !user.is_active() {
                    return Err(UseCaseError::Forbidden);
                }
                user.id()
            }
        };
        self.posts(actor, author, request).await
    }

    /// 本人须有 post.read，他人须有 post.read_any；any 同样覆盖本人。
    pub async fn posts(
        &self,
        actor: &Actor,
        author: UserId,
        request: ContentListRequest,
    ) -> Result<ContentPage<AdminPostSummary>, UseCaseError> {
        authorize_own_or_any(actor, "post.read", "post.read_any", author)?;
        let filter = PostListFilter::try_from(request)?;
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
        let filter = PageListFilter::try_from(request)?;
        let (items, total) = self.pages.list(&filter).await?;
        Ok(ContentPage {
            items,
            total,
            page: filter.page(),
            per_page: filter.limit(),
        })
    }
}
