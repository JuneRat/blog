//! 系列用例：目录管理与并发安全重排。
//!
//! 权限约定（docs/identity-and-admin.md §2、content-lifecycle.md §3）：
//! - 目录动作（创建/更新/删除/重排）要求 `series.manage`（Admin 与 Editor）；
//! - 目录读取对已认证会话开放；
//! - 重排会修改文章的 post_series.position：**每篇涉及文章仍按文章授权核验**
//!   （post.update own / post.update_any any）——Author 不能借重排改他人文章；
//! - 文章加入/退出系列走文章编辑授权（post.update/post.update_any）。
//!
//! 并发协议：仓储先取得内容关系事务锁，再锁系列并校验 series.version，
//! 成员文章按 id 序加锁；实际变化时递增系列及权重变化的文章版本。
//! 文章保存和目录删除遵循同一关系锁协议，删除系列只解除关系并保留文章。

use std::sync::Arc;

use uuid::Uuid;

use crate::error::UseCaseError;
use crate::identity::{Actor, authorize_own_or_any};
use crate::ports::{Clock, ReorderOutcome, SeriesDeleteOutcome, SeriesRepository, SeriesWithUsage};
use crate::version::checked_version;
use domain::content::Slug;
use domain::content::series::Series;

pub struct CreateSeriesCmd {
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct UpdateSeriesCmd {
    pub name: String,
    pub description: Option<String>,
    /// 封面三态：None 不修改；Some(None) 移除封面；Some(Some(id)) 设置封面。
    pub cover_media_id: Option<Option<Uuid>>,
    pub expected_version: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ReorderSeriesCmd {
    /// 系列内全部文章 id 按目标顺序排列（完整排列，不是增量）。
    pub ordered_post_ids: Vec<Uuid>,
    pub expected_series_version: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SeriesDto {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    /// 封面媒体资产 id（None = 无封面）；URL 由接口层按 `/media/{id}` 生成。
    pub cover_media_id: Option<Uuid>,
    pub version: i64,
    /// 成员总数（含草稿/私密/回收站——它们保留位置）。
    pub post_count: i64,
    /// 公开可见成员数（与公开系列页同口径）。
    pub public_post_count: i64,
}

impl SeriesDto {
    fn from_usage(row: &SeriesWithUsage) -> Self {
        Self {
            id: row.snapshot.id,
            name: row.snapshot.name.clone(),
            slug: row.snapshot.slug.clone(),
            description: row.snapshot.description.clone(),
            cover_media_id: row.snapshot.cover_media_id,
            version: row.snapshot.version,
            post_count: row.post_count,
            public_post_count: row.public_post_count,
        }
    }
}

/// 重排结果视图：新 series.version 与按新顺序的成员（含作者）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReorderedDto {
    pub series_version: i64,
    pub ordered_post_ids: Vec<Uuid>,
}

pub struct SeriesInteractor {
    series: Arc<dyn SeriesRepository>,
    clock: Arc<dyn Clock>,
    /// 封面附着的可用性校验（`ensure_attachable`）。
    media_guard: Arc<dyn crate::ports::MediaRefGuard>,
}

impl SeriesInteractor {
    pub fn new(
        series: Arc<dyn SeriesRepository>,
        clock: Arc<dyn Clock>,
        media_guard: Arc<dyn crate::ports::MediaRefGuard>,
    ) -> Self {
        Self {
            series,
            clock,
            media_guard,
        }
    }

    pub async fn create(
        &self,
        actor: &Actor,
        cmd: CreateSeriesCmd,
    ) -> Result<SeriesDto, UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("series.manage") {
            return Err(UseCaseError::Forbidden);
        }
        let slug = Slug::new(&cmd.slug).map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let series =
            Series::new(cmd.name, slug, cmd.description, self.clock.now()).map_err(map_domain)?;
        let snapshot = series.snapshot();
        self.series.insert(&series, actor.audit_context()).await?;
        Ok(SeriesDto {
            id: snapshot.id,
            name: snapshot.name,
            slug: snapshot.slug,
            description: snapshot.description,
            cover_media_id: snapshot.cover_media_id,
            version: snapshot.version,
            post_count: 0,
            public_post_count: 0,
        })
    }

    pub async fn update(
        &self,
        actor: &Actor,
        target_slug: &str,
        cmd: UpdateSeriesCmd,
    ) -> Result<SeriesDto, UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("series.manage") {
            return Err(UseCaseError::Forbidden);
        }
        let series = self.load(target_slug).await?;
        let expected = checked_version(series.version(), cmd.expected_version)?;
        let mut series = series;
        // 封面只有**换成新资产**时才过可用性校验：重复提交当前封面不重新授权。
        if let Some(Some(cover_media_id)) = cmd.cover_media_id
            && series.snapshot().cover_media_id != Some(cover_media_id)
        {
            crate::media::ensure_attachable(&*self.media_guard, cover_media_id).await?;
        }
        if !series
            .update(cmd.name, cmd.description, cmd.cover_media_id)
            .map_err(map_domain)?
        {
            return self.dto_of(series.id()).await;
        }
        let snapshot = series.snapshot();
        match self
            .series
            .update(
                snapshot.id,
                &snapshot.name,
                snapshot.description.as_deref(),
                snapshot.cover_media_id,
                expected,
                actor.audit_context(),
            )
            .await?
        {
            Some(updated) => Ok(self.dto_of_loaded(updated.id).await?),
            None => Err(UseCaseError::VersionConflict),
        }
    }

    pub async fn delete(
        &self,
        actor: &Actor,
        target_slug: &str,
        expected_version: Option<i64>,
    ) -> Result<(), UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("series.manage") {
            return Err(UseCaseError::Forbidden);
        }
        let series = self.load(target_slug).await?;
        let expected = checked_version(series.version(), expected_version)?;
        match self
            .series
            .delete(series.id(), expected, actor.audit_context())
            .await?
        {
            SeriesDeleteOutcome::Deleted => Ok(()),
            SeriesDeleteOutcome::StaleVersion => Err(UseCaseError::VersionConflict),
            SeriesDeleteOutcome::Gone => Err(UseCaseError::NotFound(format!("系列 {target_slug}"))),
        }
    }

    /// 全量目录（管理屏与编辑器选择器共用）。
    pub async fn list(&self, _actor: &Actor) -> Result<Vec<SeriesDto>, UseCaseError> {
        Ok(self
            .series
            .list()
            .await?
            .iter()
            .map(SeriesDto::from_usage)
            .collect())
    }

    /// 整体重排：先做**逐篇**文章授权（own/any），再进仓储锁协议。
    ///
    /// 提交的必须是完整排列：与当前成员集合不一致报 MembershipMismatch
    /// （调用方重读目录再试），避免静默丢弃或新增成员。
    pub async fn reorder(
        &self,
        actor: &Actor,
        target_slug: &str,
        cmd: ReorderSeriesCmd,
    ) -> Result<ReorderedDto, UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("series.manage") {
            return Err(UseCaseError::Forbidden);
        }
        let series = self.load(target_slug).await?;
        let expected = checked_version(series.version(), cmd.expected_series_version)?;

        // 逐篇授权：重排修改文章在本系列内的排序权重。
        let members = self.series.members_of(series.id()).await?;
        for member in &members {
            authorize_own_or_any(
                actor,
                "post.update",
                "post.update_any",
                domain::identity::UserId(member.author_id),
            )?;
        }
        // 完整排列校验（去重 + 集合相等）。
        let mut given = cmd.ordered_post_ids.clone();
        given.sort();
        given.dedup();
        if given.len() != cmd.ordered_post_ids.len() {
            return Err(UseCaseError::Invalid("顺序中存在重复文章".into()));
        }
        let mut current: Vec<Uuid> = members.iter().map(|m| m.post_id).collect();
        current.sort();
        if current != given {
            return Err(UseCaseError::Invalid(
                "提交的文章集合与系列当前成员不一致，请重读目录后再排".into(),
            ));
        }

        match self
            .series
            .reorder(
                series.id(),
                expected,
                &cmd.ordered_post_ids,
                actor.audit_context(),
            )
            .await?
        {
            ReorderOutcome::Reordered { new_version } => Ok(ReorderedDto {
                series_version: new_version,
                ordered_post_ids: cmd.ordered_post_ids,
            }),
            ReorderOutcome::StaleSeriesVersion => Err(UseCaseError::VersionConflict),
            ReorderOutcome::MembershipMismatch => Err(UseCaseError::Invalid(
                "提交的文章集合与系列当前成员不一致，请重读目录后再排".into(),
            )),
            ReorderOutcome::SeriesGone => {
                Err(UseCaseError::NotFound(format!("系列 {target_slug}")))
            }
        }
    }

    /// 管理目录视图：系列全部成员（含他人草稿/私密——重排会改动它们的位置，
    /// 目录必须完整）。要求 series.manage（持有者 Admin/Editor 均具备 post.read_any）。
    pub async fn members(
        &self,
        actor: &Actor,
        target_slug: &str,
    ) -> Result<Vec<crate::ports::SeriesMember>, UseCaseError> {
        if !actor.has_permission("series.manage") {
            return Err(UseCaseError::Forbidden);
        }
        let series = self.load(target_slug).await?;
        let members = self.series.members_of(series.id()).await?;
        // 逐篇核验读取权限：成员目录携带他人草稿/私密的标题、slug 与状态——
        // 这些正是 post.read/post.read_any 的保护对象。内置角色（Admin/Editor）
        // 恰好同时持有 series.manage 与读取权限，但自定义角色可能只有前者；
        // 权限按动作核验，不以内置角色的同时持有为依据。
        // 任一成员不可读即**整次拒绝**：残缺目录会让重排（完整排列契约）
        // 必然失败，也会误导界面以为系列缺员。
        for member in &members {
            authorize_own_or_any(
                actor,
                "post.read",
                "post.read_any",
                domain::identity::UserId(member.author_id),
            )?;
        }
        Ok(members)
    }

    async fn load(&self, slug: &str) -> Result<Series, UseCaseError> {
        let snapshot = self
            .series
            .find_by_slug(slug)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("系列 {slug}")))?;
        Series::reconstitute(snapshot).map_err(|e| UseCaseError::DataCorrupt(e.to_string()))
    }

    async fn dto_of(&self, id: Uuid) -> Result<SeriesDto, UseCaseError> {
        self.dto_of_loaded(id).await
    }

    async fn dto_of_loaded(&self, id: Uuid) -> Result<SeriesDto, UseCaseError> {
        let row = self
            .series
            .list()
            .await?
            .into_iter()
            .find(|row| row.snapshot.id == id)
            .ok_or_else(|| UseCaseError::NotFound("系列".into()))?;
        Ok(SeriesDto::from_usage(&row))
    }
}

fn map_domain(e: domain::content::series::SeriesError) -> UseCaseError {
    UseCaseError::Invalid(e.to_string())
}
