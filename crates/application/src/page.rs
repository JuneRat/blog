//! 页面用例：创建、读取、列表、编辑、发布、撤回、物理删除。
//!
//! 权限约定：`page.*` 是**站点级**权限——Page 没有 author_id，
//! 不套用文章的 own/any 规则（docs/content-lifecycle.md §2）。
//! 写通道仍限受控 CLI 或已认证会话（`Actor::ensure_write_channel`）。

use std::sync::Arc;

use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::UseCaseError;
use crate::ports::{Clock, PageCommitOutcome, PageDeleteOutcome, PageRepository};
use crate::version::checked_version;
use domain::content::page::{Page, PageError, PagePatch, PageSnapshot, Slug, Visibility};

/// 向接口层转出的可见性值对象（interfaces 不直接依赖 domain crate）。
pub use domain::content::page::Visibility as PageVisibility;

#[derive(Debug, Clone)]
pub struct CreatePageCmd {
    /// None 时由应用生成临时唯一 slug（草稿创建即占用）。
    pub slug: Option<String>,
    pub title: String,
    pub content: String,
    pub visibility: Visibility,
}

#[derive(Debug, Clone, Default)]
pub struct EditPageCmd {
    pub id: Uuid,
    pub new_slug: Option<String>,
    pub title: Option<String>,
    pub content: Option<String>,
    pub visibility: Option<Visibility>,
    /// None 表示使用读取到的当前版本（仍可检测读后并发修改）。
    pub expected_version: Option<i64>,
}

pub struct DeletePageCmd {
    pub id: Uuid,
    pub expected_version: i64,
}

/// 面向 CLI/后台的页面视图（含非公开状态与正文源文）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PageDto {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    pub content: String,
    pub status: &'static str,
    pub visibility: &'static str,
    pub version: i64,
    pub published_at: Option<OffsetDateTime>,
    pub updated_at: OffsetDateTime,
}

impl PageDto {
    fn from_snapshot(s: &PageSnapshot) -> Self {
        Self {
            id: s.id,
            slug: s.slug.clone(),
            title: s.title.clone(),
            content: s.content.clone(),
            status: s.status.as_str(),
            visibility: s.visibility.as_str(),
            version: s.version,
            published_at: s.published_at,
            updated_at: s.updated_at,
        }
    }
}

pub struct PageInteractor {
    pages: Arc<dyn PageRepository>,
    clock: Arc<dyn Clock>,
}

impl PageInteractor {
    pub fn new(pages: Arc<dyn PageRepository>, clock: Arc<dyn Clock>) -> Self {
        Self { pages, clock }
    }

    /// 创建草稿。Page 无作者：只校验站点级 `page.create`。
    pub async fn create(
        &self,
        actor: &crate::identity::Actor,
        cmd: CreatePageCmd,
    ) -> Result<PageDto, UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("page.create") {
            return Err(UseCaseError::Forbidden);
        }
        // 生成值用 page- 前缀，天然避开系统保留路径。
        let slug_raw = cmd
            .slug
            .unwrap_or_else(|| format!("page-{}", Uuid::now_v7().simple()));
        let slug = Slug::new(&slug_raw).map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let page = Page::create_draft(
            slug,
            cmd.title,
            cmd.content,
            cmd.visibility,
            self.clock.now(),
        )
        .map_err(map_domain)?;
        let snapshot = self.pages.insert_page(&page).await?;
        Ok(PageDto::from_snapshot(&snapshot))
    }

    /// 读取任意状态的页面。
    pub async fn find(
        &self,
        actor: &crate::identity::Actor,
        id: Uuid,
    ) -> Result<PageDto, UseCaseError> {
        let page = self.load_authorized(actor, id, "page.read").await?;
        Ok(PageDto::from_snapshot(&page.snapshot()))
    }

    /// 站点级列表：不含作者维度，返回全部页面（含草稿）。
    pub async fn list(&self, actor: &crate::identity::Actor) -> Result<Vec<PageDto>, UseCaseError> {
        if !actor.has_permission("page.read") {
            return Err(UseCaseError::Forbidden);
        }
        let snapshots = self.pages.list().await?;
        Ok(snapshots.iter().map(PageDto::from_snapshot).collect())
    }

    /// 编辑当前正文；保存已发布页面直接更新线上。
    pub async fn edit(
        &self,
        actor: &crate::identity::Actor,
        cmd: EditPageCmd,
    ) -> Result<PageDto, UseCaseError> {
        actor.ensure_write_channel()?;
        let (mut page, loaded) = self.load_versioned(actor, cmd.id, "page.update").await?;
        let expected = checked_version(loaded, cmd.expected_version)?;

        let changed = page
            .edit(PagePatch {
                slug: cmd.new_slug,
                title: cmd.title,
                content: cmd.content,
                visibility: cmd.visibility,
            })
            .map_err(map_domain)?;

        if changed {
            return self.commit(page, expected).await;
        }
        Ok(PageDto::from_snapshot(&page.snapshot()))
    }

    /// 发布：draft → published，首次发布写入 published_at；已发布幂等。
    pub async fn publish(
        &self,
        actor: &crate::identity::Actor,
        id: Uuid,
        expected_version: Option<i64>,
    ) -> Result<PageDto, UseCaseError> {
        actor.ensure_write_channel()?;
        let (mut page, loaded) = self.load_versioned(actor, id, "page.publish").await?;
        let expected = checked_version(loaded, expected_version)?;

        if page.publish(self.clock.now()).map_err(map_domain)? {
            return self.commit(page, expected).await;
        }
        Ok(PageDto::from_snapshot(&page.snapshot()))
    }

    /// 撤回：published → draft，slug 保持锁定；非发布状态幂等。
    pub async fn withdraw(
        &self,
        actor: &crate::identity::Actor,
        id: Uuid,
        expected_version: Option<i64>,
    ) -> Result<PageDto, UseCaseError> {
        actor.ensure_write_channel()?;
        let (mut page, loaded) = self.load_versioned(actor, id, "page.unpublish").await?;
        let expected = checked_version(loaded, expected_version)?;

        if page.withdraw() {
            return self.commit(page, expected).await;
        }
        Ok(PageDto::from_snapshot(&page.snapshot()))
    }

    /// Page 无回收站：物理删除后公开入口立即消失，slug 可重新使用。
    pub async fn delete(
        &self,
        actor: &crate::identity::Actor,
        cmd: DeletePageCmd,
    ) -> Result<(), UseCaseError> {
        actor.ensure_write_channel()?;
        let (_, loaded_version) = self.load_versioned(actor, cmd.id, "page.delete").await?;
        checked_version(loaded_version, Some(cmd.expected_version))?;
        match self.pages.delete(cmd.id, cmd.expected_version).await? {
            PageDeleteOutcome::Deleted => Ok(()),
            PageDeleteOutcome::StaleVersion => Err(UseCaseError::VersionConflict),
            PageDeleteOutcome::Gone => Err(UseCaseError::NotFound(format!("页面 {}", cmd.id))),
        }
    }

    /// 提交聚合变更：三态结果映射为用例错误；成功时采用数据库返回的新版本。
    async fn commit(&self, page: Page, expected: i64) -> Result<PageDto, UseCaseError> {
        match self
            .pages
            .commit_page(&page, expected, self.clock.now())
            .await?
        {
            PageCommitOutcome::Saved(snapshot) => Ok(PageDto::from_snapshot(&snapshot)),
            PageCommitOutcome::StaleConflict => Err(UseCaseError::VersionConflict),
            PageCommitOutcome::Gone => Err(UseCaseError::NotFound("页面（已被删除）".into())),
        }
    }

    async fn load_authorized(
        &self,
        actor: &crate::identity::Actor,
        id: Uuid,
        key: &str,
    ) -> Result<Page, UseCaseError> {
        if !actor.has_permission(key) {
            return Err(UseCaseError::Forbidden);
        }
        let snapshot = self
            .pages
            .find_by_id(id)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("页面 {id}")))?;
        Ok(Page::reconstitute(snapshot))
    }

    async fn load_versioned(
        &self,
        actor: &crate::identity::Actor,
        id: Uuid,
        key: &str,
    ) -> Result<(Page, i64), UseCaseError> {
        let page = self.load_authorized(actor, id, key).await?;
        let version = page.version();
        Ok((page, version))
    }
}

fn map_domain(e: PageError) -> UseCaseError {
    UseCaseError::Invalid(e.to_string())
}
